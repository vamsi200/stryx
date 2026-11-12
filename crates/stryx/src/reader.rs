use std::{
    fmt::Debug,
    fs::File,
    io::{Read, Seek},
    os::unix::fs::FileExt,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    vec,
};

use bytes::Bytes;
use memmap2::{Mmap, MmapOptions};
use quick_cache::Weighter;
use xorf::Filter;

use crate::{
    BLOCK_SIZE, FOOTER_SIZE, KEY_HASH_SEED, MAGIC, UNITS, VERSION,
    builder::{FilterTypeRepr, Record, TableMetadata, XorFilter, from_filter_type},
    config::Compression,
    decoder::Decoder,
    error::{DecodeError, StryxError},
    helper::filter_decode,
    index::Index,
};
use zstd::bulk::Decompressor;

#[derive(Clone)]
struct BlockWeighter;

impl Weighter<u32, Arc<[u8]>> for BlockWeighter {
    fn weight(&self, _idx: &u32, block: &Arc<[u8]>) -> u64 {
        block.len() as u64
    }
}

/// A thread-safe cache for frequently accessed data blocks.
///
/// `BlockCache` is a newtype wrapper around a concurrent cache implementation from
/// the `quick_cache` crate, designed to reduce expensive disk or network reads by
/// keeping hot blocks in memory.
#[derive(Debug)]
pub struct BlockCache(quick_cache::sync::Cache<u32, Arc<[u8]>, BlockWeighter>);

impl BlockCache {
    /// Creates a new `BlockCache` configured with a maximum memory limit in bytes.
    ///
    /// # Arguments
    /// - `max_bytes`: The total maximum memory footprint allowed for cached blocks.
    ///
    /// # Implementation Details
    /// This constructor estimates the initial item capacity by assuming an average
    /// block size of 4 KB (enforcing a safe minimum of 16 items). It delegates
    /// tracking and eviction to `quick_cache` using the custom `BlockWeighter`.
    pub fn new(max_bytes: u64) -> Self {
        let est_items = ((max_bytes / 4096).max(16)) as usize;
        Self(quick_cache::sync::Cache::with_weighter(
            est_items,
            max_bytes,
            BlockWeighter,
        ))
    }

    #[inline]
    pub(crate) fn get(&self, idx: &u32) -> Option<Arc<[u8]>> {
        self.0.get(idx)
    }

    #[inline]
    pub(crate) fn insert(&self, idx: u32, block: Arc<[u8]>) {
        self.0.insert(idx, block);
    }
}

struct BlockDecoder {
    zstd: Option<Decompressor<'static>>,
    out: Vec<u8>,
}

impl Debug for BlockDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockDecoder").finish_non_exhaustive()
    }
}

impl BlockDecoder {
    fn new() -> Self {
        Self {
            zstd: None,
            out: Vec::new(),
        }
    }

    fn zstd(&mut self, src: &[u8], max_len: usize) -> Result<(&[u8], bool), StryxError> {
        if self.zstd.is_none() {
            self.zstd = Some(Decompressor::new()?);
        }

        self.out.clear();
        self.out.reserve(max_len);
        let n = self
            .zstd
            .as_mut()
            .ok_or(StryxError::InvalidState("Zstd decoder not initialized"))?
            .decompress_to_buffer(src, &mut self.out)?;
        debug_assert_eq!(n, self.out.len());
        Ok((&self.out, true))
    }

    fn lz4(&mut self, src: &[u8], uncompressed_len: usize) -> Result<(&[u8], bool), StryxError> {
        self.out.clear();
        self.out.resize(uncompressed_len, 0);
        let n = lz4_flex::block::decompress_into(src, &mut self.out)
            .map_err(|_| DecodeError::InvalidLength)?;
        if n != uncompressed_len {
            return Err(DecodeError::InvalidLength.into());
        }
        Ok((&self.out, true))
    }

    fn out(&self) -> &[u8] {
        &self.out
    }
}

#[derive(Debug)]
pub(crate) struct BlockReader<'a> {
    buf: &'a [u8],
    offsets: &'a [u8],
    record_count: usize,
}

impl<'a> BlockReader<'a> {
    #[inline]
    pub(crate) fn new(buf: &'a [u8]) -> Result<Self, StryxError> {
        let Some(footer_start) = buf.len().checked_sub(8) else {
            return Err(DecodeError::InvalidLength.into());
        };

        let record_count =
            u32::from_le_bytes(buf[footer_start..footer_start + 4].try_into().unwrap()) as usize;

        let Some(table_len) = record_count.checked_mul(size_of::<u32>()) else {
            return Err(DecodeError::InvalidLength.into());
        };

        let Some(records_end) = footer_start.checked_sub(table_len) else {
            return Err(DecodeError::InvalidLength.into());
        };

        Ok(Self {
            buf: &buf[..records_end],
            offsets: &buf[records_end..footer_start],
            record_count,
        })
    }

    #[inline]
    fn offset(&self, i: usize) -> usize {
        let start = i * 4;
        let bytes: [u8; 4] = self.offsets[start..start + 4]
            .try_into()
            .expect("invalid offset index");

        u32::from_le_bytes(bytes) as usize
    }

    fn record_at(&self, offset: usize) -> Result<(&'a [u8], &'a [u8]), StryxError> {
        let header = self
            .buf
            .get(offset..offset + 6)
            .ok_or(DecodeError::Truncated)?;
        let key_len = u16::from_le_bytes([header[0], header[1]]) as usize;
        let val_len = u32::from_le_bytes([header[2], header[3], header[4], header[5]]) as usize;

        let key_start = offset + 6;
        let val_start = key_start + key_len;
        let end = val_start
            .checked_add(val_len)
            .ok_or(DecodeError::InvalidLength)?;

        let key = self
            .buf
            .get(key_start..val_start)
            .ok_or(DecodeError::Truncated)?;

        let val = self.buf.get(val_start..end).ok_or(DecodeError::Truncated)?;

        Ok((key, val))
    }

    fn verify_checksum(buf: &[u8]) -> Result<(), StryxError> {
        let n = buf.len();
        if n < 8 {
            return Err(DecodeError::InvalidLength.into());
        }

        // last 4 bytes contain checksum in the Data Block
        let stored = u32::from_le_bytes(
            buf.get(n - 4..)
                .ok_or(StryxError::InvalidData("missing checksum"))?
                .try_into()
                .map_err(|_| StryxError::InvalidData("invalid checksum length"))?,
        );

        if crc32c::crc32c(&buf[..n - 4]) != stored {
            return Err(StryxError::InvalidChecksum);
        }

        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn err_truncated<T>() -> Result<T, StryxError> {
        Err(DecodeError::Truncated.into())
    }

    #[inline(always)]
    pub(crate) fn key_at(&self, offset: usize) -> Result<&'a [u8], StryxError> {
        let Some(rem) = self.buf.get(offset..) else {
            return Self::err_truncated();
        };
        if rem.len() < 6 {
            return Self::err_truncated();
        }
        let key_len = u16::from_le_bytes([rem[0], rem[1]]) as usize;
        let key_end = 6 + key_len; // Cannot overflow: 6 + u16::MAX == 65541
        if rem.len() < key_end {
            return Self::err_truncated();
        }
        Ok(&rem[6..key_end])
    }

    // binary search the key, skipping `skip` already-verified shared prefix bytes
    #[inline(always)]
    fn get(&self, key: &[u8], skip: usize) -> Result<Option<(&'a [u8], u32)>, StryxError> {
        let (mut lo, mut hi) = (0usize, self.record_count);
        let mut matched_off: Option<usize> = None;
        let key_sfx = &key[skip..];

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = self.offset(mid);
            let k = self.key_at(off)?;
            let Some(k_sfx) = k.get(skip..) else {
                return Self::err_truncated();
            };

            match key_cmp(k_sfx, key_sfx) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    // if `mid == lo` or the previous key is `< key`,
                    // `mid` is guaranteed to be the FIRST matching record
                    if mid == lo {
                        return Ok(Some((self.value_at(off, key.len())?, off as u32)));
                    }
                    let prev_off = self.offset(mid - 1);
                    let prev_k = self.key_at(prev_off)?;
                    let Some(prev_sfx) = prev_k.get(skip..) else {
                        return Self::err_truncated();
                    };
                    if key_lt(prev_sfx, key_sfx) {
                        return Ok(Some((self.value_at(off, key.len())?, off as u32)));
                    }
                    // Previous key was also == key!
                    // Now we know both `mid` and `mid - 1` match `key`.
                    matched_off = Some(prev_off);
                    hi = mid - 1;
                    break;
                }
            }
        }

        // Only reached if `mid` AND `mid - 1` were both duplicates of `key`.
        // Every key in `lo..hi` is guaranteed `<= key`, so we only check `< key` vs `== key`!
        if let Some(mut first_off) = matched_off {
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let off = self.offset(mid);
                let k = self.key_at(off)?;
                let Some(k_sfx) = k.get(skip..) else {
                    return Self::err_truncated();
                };
                if key_lt(k_sfx, key_sfx) {
                    lo = mid + 1;
                } else {
                    first_off = off;
                    hi = mid;
                }
            }
            return Ok(Some((
                self.value_at(first_off, key.len())?,
                first_off as u32,
            )));
        }

        Ok(None)
    }

    #[inline(always)]
    pub(crate) fn value_at(&self, off: usize, key_len: usize) -> Result<&'a [u8], StryxError> {
        let Some(rem) = self.buf.get(off..) else {
            return Self::err_truncated();
        };

        if rem.len() < 6 {
            return Self::err_truncated();
        }

        let vlen = u32::from_le_bytes([rem[2], rem[3], rem[4], rem[5]]) as usize;

        let Some(val) = key_len.checked_add(6).and_then(|val_start| {
            let val_end = val_start.checked_add(vlen)?;
            rem.get(val_start..val_end)
        }) else {
            return Self::err_truncated();
        };

        Ok(val)
    }

    fn records(&self) -> Result<Vec<Record<'a>>, StryxError> {
        (0..self.record_count)
            .map(|i| {
                let (key, value) = self.record_at(self.offset(i))?;
                Ok(Record { key, value })
            })
            .collect()
    }
}

#[inline(always)]
pub(crate) fn key_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let n = a.len().min(b.len());
    let ap = a.as_ptr();
    let bp = b.as_ptr();

    // SAFETY: `n <= a.len()` and `n <= b.len()` by definition of `min`.
    // All reads below are at offsets `<= n - width`.
    unsafe {
        if n >= 8 {
            let mut i = 0;
            while i + 8 <= n {
                let x = ap.add(i).cast::<u64>().read_unaligned().to_be();
                let y = bp.add(i).cast::<u64>().read_unaligned().to_be();
                if x != y {
                    return x.cmp(&y);
                }
                i += 8;
            }
            let x = ap.add(n - 8).cast::<u64>().read_unaligned().to_be();
            let y = bp.add(n - 8).cast::<u64>().read_unaligned().to_be();
            if x != y {
                return x.cmp(&y);
            }
        } else if n >= 4 {
            let x1 = ap.cast::<u32>().read_unaligned().to_be();
            let y1 = bp.cast::<u32>().read_unaligned().to_be();
            if x1 != y1 {
                return x1.cmp(&y1);
            }
            let x2 = ap.add(n - 4).cast::<u32>().read_unaligned().to_be();
            let y2 = bp.add(n - 4).cast::<u32>().read_unaligned().to_be();
            if x2 != y2 {
                return x2.cmp(&y2);
            }
        } else {
            let mut i = 0;
            while i < n {
                let x = *ap.add(i);
                let y = *bp.add(i);
                if x != y {
                    return x.cmp(&y);
                }
                i += 1;
            }
        }
    }
    a.len().cmp(&b.len())
}

#[inline(always)]
pub(crate) fn key_lt(a: &[u8], b: &[u8]) -> bool {
    key_cmp(a, b) == std::cmp::Ordering::Less
}

/// Configuration options for controlling the behavior and performance of the reader.
#[derive(Debug, Default)]
pub struct ReaderOptions {
    /// When set to `true`, disables block-level verification (checksum validation)
    /// when reading blocks from file.
    ///
    /// # Performance vs. Safety Trade-off
    /// - **`true` (Disabled):** Improves read throughput and reduces CPU overhead by skipping
    ///   integrity checks. Use with caution if storage reliability cannot be guaranteed.
    /// - **`false` (Enabled - Recommended):** Ensures that data blocks are verified upon reading
    ///   to protect against data corruption.
    pub disable_block_verification: bool,

    /// An optional block cache (`BlockCache`) used to store frequently accessed data blocks in memory.
    ///
    /// - **`Some(BlockCache)`:** Enables caching. Subsequent reads for blocks already present in
    ///   the cache are served directly from memory, significantly reducing disk/network I/O latency.
    /// - **`None`:** Disables block caching for this reader; all data blocks will be fetched directly
    ///   from the underlying storage backend.
    pub block_cache: Option<BlockCache>,
}

#[derive(Debug)]
pub(crate) struct SharedTable {
    index: Index,
    metadata: TableMetadata,
    disable_block_verification: bool,
    crc_verification: Vec<AtomicBool>,
    filter: XorFilter,
    block_cache: Option<BlockCache>,
    block_seen: Vec<AtomicBool>,
    mmaped_file: Mmap,
}

/// A reader responsible for efficiently traversing, decoding, and reading blocks
/// from the stryx file.
#[derive(Debug)]
pub struct TableReader {
    pub readstats: ReadStats,
    shared: Arc<SharedTable>,

    // Internal reusable byte buffer
    block_buf: Vec<u8>,

    // Decoder engine responsible for decompressing and deserializing etc..
    block_decoder: BlockDecoder,

    // Index ordering vector used to determine or track the traversal sequence
    // of blocks within the table.
    order: Vec<usize>,

    loaded_idx: Option<usize>,

    last_hit: Option<(usize, Arc<[u8]>)>,
}

/// Cloning the reader creates a new scratchpad for a new thread,
/// but shares the heavy underlying index and file descriptors!
impl Clone for TableReader {
    fn clone(&self) -> Self {
        Self {
            readstats: ReadStats::default(),
            shared: Arc::clone(&self.shared),
            block_buf: Vec::with_capacity(BLOCK_SIZE),
            block_decoder: BlockDecoder::new(),
            order: Vec::new(),
            loaded_idx: None,
            last_hit: None,
        }
    }
}

impl TableReader {
    /// Open and validate an SSTable
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StryxError> {
        let reader_options = ReaderOptions::default();
        Self::open_with_options(path, reader_options)
    }

    /// Open with options and validate an SSTable
    pub fn open_with_options(
        path: impl AsRef<Path>,
        reader_options: ReaderOptions,
    ) -> Result<Self, StryxError> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();

        if file_len < FOOTER_SIZE as u64 {
            return Err(DecodeError::Truncated.into());
        }

        let mut footer = [0u8; FOOTER_SIZE];

        file.seek(std::io::SeekFrom::End(-(FOOTER_SIZE as i64)))?;
        file.read_exact(&mut footer)?;

        if &footer[21..29] != MAGIC {
            return Err(StryxError::InvalidMagic);
        }

        let mut decoder = Decoder::new(&footer);

        let index_offset = decoder.read_u64()?;
        let index_size = decoder.read_u64()?;
        let filter_type = from_filter_type(decoder.read_u8()?)?;
        let version = decoder.read_u32()?;

        if version != VERSION {
            return Err(StryxError::InvalidVersion);
        }

        let footer_offset = file_len - FOOTER_SIZE as u64;

        let metadata_offset = index_offset
            .checked_add(index_size)
            .ok_or(DecodeError::InvalidLength)?;

        if metadata_offset > footer_offset {
            return Err(DecodeError::InvalidLength.into());
        }

        file.seek(std::io::SeekFrom::Start(metadata_offset))?;

        let mut len_buf = [0u8; 8];
        file.read_exact(&mut len_buf)?;
        let metadata_size = u64::from_le_bytes(len_buf);

        // validate bounds before allocating metadata_buf
        let filter_section_offset = metadata_offset
            .checked_add(8)
            .and_then(|v| v.checked_add(metadata_size))
            .ok_or(DecodeError::InvalidLength)?;

        if matches!(filter_type, FilterTypeRepr::None) && filter_section_offset != footer_offset {
            return Err(DecodeError::InvalidLength.into());
        }

        if filter_section_offset > footer_offset {
            return Err(DecodeError::InvalidLength.into());
        }

        if !metadata_size <= file_len {
            return Err(DecodeError::InvalidLength.into());
        }

        let mut metadata_buf = vec![0u8; usize::try_from(metadata_size)?];
        file.read_exact(&mut metadata_buf)?;
        let metadata = TableMetadata::decode(&metadata_buf)?;

        let mut filter = XorFilter::None;

        if filter_type != FilterTypeRepr::None {
            let filter_overhead = filter_section_offset
                .checked_add(8 + 4 + FOOTER_SIZE as u64)
                .ok_or(DecodeError::InvalidLength)?;

            let remaining = file
                .metadata()?
                .len()
                .checked_sub(filter_overhead)
                .ok_or(DecodeError::InvalidLength)?;

            file.seek(std::io::SeekFrom::Start(filter_section_offset))?;

            let mut len_buf = [0u8; 8];
            file.read_exact(&mut len_buf)?;
            let filter_len = usize::try_from(u64::from_le_bytes(len_buf))?;

            if filter_len != usize::try_from(remaining)? {
                return Err(StryxError::InvalidRange);
            }

            if !metadata_size <= remaining {
                return Err(DecodeError::InvalidLength.into());
            }

            let mut filter_data = vec![0u8; filter_len];
            file.read_exact(&mut filter_data)?;

            let mut crc_buf = [0u8; 4];
            file.read_exact(&mut crc_buf)?;
            let stored = u32::from_le_bytes(crc_buf);

            if stored != crc32c::crc32c(&filter_data) {
                return Err(StryxError::InvalidFilterChecksum);
            }

            filter = filter_decode(&filter_data, filter_type)?;
        }

        // `index_size` is guaranteed <= `footer_offset`, so this is fine
        let index_len = usize::try_from(index_size)?;
        let mut index_buf = vec![0u8; index_len];

        file.read_exact_at(&mut index_buf, index_offset)?;

        if index_buf.len() < 8 {
            return Err(DecodeError::Truncated.into());
        }

        // Last 8 bytes contain index count and checksum.
        let entries_end = index_buf.len() - 8;

        let mut decoder = Decoder::new(&index_buf[..entries_end]);
        let mut trailer = Decoder::new(&index_buf[entries_end..]);

        let count = trailer.read_u32()?;
        let stored_checksum = trailer.read_u32()?;

        // Verify checksum before decoding entries.
        let computed_checksum = crc32c::crc32c(&index_buf[..index_len - 4]);

        if stored_checksum != computed_checksum {
            return Err(StryxError::InvalidChecksum);
        }

        let mut index = Index::decode(&mut decoder, count as usize)?;

        // Reject unexpected trailing bytes in the entry region.
        if decoder.remaining() != 0 {
            return Err(DecodeError::InvalidLength.into());
        }

        let crc_verification: Vec<AtomicBool> = (0..index.entries.len())
            .map(|_| AtomicBool::new(false))
            .collect();

        let seen: Vec<AtomicBool> = if reader_options.block_cache.is_some() {
            (0..index.entries.len())
                .map(|_| AtomicBool::new(false))
                .collect()
        } else {
            Vec::new()
        };

        index.build_prefixes();

        let mmap = unsafe { MmapOptions::new().map(&file)? };
        let _ = mmap.advise(memmap2::Advice::Random);

        let share_table = Arc::new(SharedTable {
            index,
            metadata,
            disable_block_verification: reader_options.disable_block_verification,
            crc_verification,
            filter,
            block_cache: reader_options.block_cache,
            block_seen: seen,
            mmaped_file: mmap,
        });

        Ok(TableReader {
            readstats: ReadStats::default(),
            shared: share_table,
            block_buf: Vec::new(),
            block_decoder: BlockDecoder::new(),
            order: Vec::new(),
            loaded_idx: None,
            last_hit: None,
        })
    }

    fn xorf_contains(&self, key: &[u8]) -> bool {
        let key_hash = xxhash_rust::xxh3::xxh3_64_with_seed(key, KEY_HASH_SEED);

        match &self.shared.filter {
            XorFilter::BinaryFuse8(bf) => bf.contains(&key_hash),
            XorFilter::BinaryFuse16(bf) => bf.contains(&key_hash),
            XorFilter::BinaryFuse32(bf) => bf.contains(&key_hash),
            _ => true,
        }
    }

    #[cold]
    #[inline(never)]
    fn verify_block_once(
        shared: &SharedTable,
        readstats: &mut ReadStats,
        idx: usize,
        data: &[u8],
    ) -> Result<(), StryxError> {
        if let Err(e) = BlockReader::verify_checksum(data) {
            if matches!(e, StryxError::InvalidChecksum) {
                readstats.checksum_failures += 1;
            }
            return Err(e);
        }
        shared.crc_verification[idx].store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    #[inline(always)]
    fn search_block(block: &[u8], key: &[u8], skip: usize) -> Result<Option<Vec<u8>>, StryxError> {
        let reader = BlockReader::new(block)?;
        Ok(reader.get(key, skip)?.map(|(v, _)| v.to_vec()))
    }

    #[inline(always)]
    fn search_block_slice<'a>(
        block: &'a [u8],
        key: &[u8],
        skip: usize,
    ) -> Result<Option<&'a [u8]>, StryxError> {
        let reader = BlockReader::new(block)?;
        Ok(reader.get(key, skip)?.map(|(v, _)| v))
    }

    // Zero-allocation lookup returning a borrowed slice directly from the mmap,
    // block cache or decompression buffer.
    pub fn get_ref(&mut self, key: &[u8]) -> Result<Option<&[u8]>, StryxError> {
        if !self.xorf_contains(key) {
            self.readstats.point_misses += 1;
            return Ok(None);
        }
        self.readstats.point_lookups += 1;

        // find the first index entry whose first_key is greater than requested
        // key.
        let block = self.shared.index.partition_point_le(key);

        if block == 0 {
            self.readstats.point_misses += 1;
            return Ok(None);
        }

        // since returned block's first_key is greater than the requested key,
        // the requested key `could be` in previous one
        let idx = block - 1;

        let skip = if block < self.shared.index.entries.len() {
            debug_assert!(key.len() >= self.shared.index.lcp_len);
            self.shared.index.lcp_len
        } else {
            0
        };

        // Prepare the block and record whether it lives in self.last_hit
        let use_last_hit = if matches!(self.shared.metadata.compression, Compression::None) {
            let e = &self.shared.index.entries[idx];
            let start = usize::try_from(e.offset).map_err(|_| DecodeError::InvalidLength)?;
            let size = e.size as usize;

            let Some(data) = self.shared.mmaped_file.get(start..start + size) else {
                return Err(self.count_err(DecodeError::Truncated.into()));
            };

            if self.loaded_idx != Some(idx) {
                self.readstats.blocks_read += 1;
                self.readstats.bytes_read += size as u64;

                if !self.shared.disable_block_verification
                    && !self.shared.crc_verification[idx].load(std::sync::atomic::Ordering::Relaxed)
                {
                    if let Err(e) =
                        Self::verify_block_once(&self.shared, &mut self.readstats, idx, data)
                    {
                        return Err(self.count_err(e));
                    }
                }
                self.loaded_idx = Some(idx);
            }
            false
        } else if self.loaded_idx == Some(idx) {
            // same compressed block as previous uncached lookup
            false
        } else if matches!(&self.last_hit, Some((i, _)) if *i == idx) {
            // same compressed block as previous cached lookup (skips BlockCache hash/lock)
            self.readstats.cache_hits += 1;
            true
        } else {
            // check shared BlockCache first: a hit skips decompress and CRC
            let cached = self
                .shared
                .block_cache
                .as_ref()
                .and_then(|c| c.get(&(idx as u32)));

            if let Some(data) = cached {
                self.readstats.cache_hits += 1;
                self.last_hit = Some((idx, data));
                true
            } else {
                if self.shared.block_cache.is_some() {
                    self.readstats.cache_misses += 1;
                }

                self.load_block(idx).map_err(|e| self.count_err(e))?;

                // Insert only on the SECOND load of a block
                if let Some(cache) = &self.shared.block_cache
                    && self.shared.block_seen[idx].swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    let arc_block: Arc<[u8]> = Arc::from(self.loaded_block());
                    cache.insert(idx as u32, Arc::clone(&arc_block));
                    self.last_hit = Some((idx, arc_block));
                }
                false
            }
        };

        let block_slice: &[u8] = if matches!(self.shared.metadata.compression, Compression::None) {
            let e = &self.shared.index.entries[idx];
            let start = usize::try_from(e.offset).map_err(|_| DecodeError::InvalidLength)?;
            &self.shared.mmaped_file[start..start + e.size as usize]
        } else if use_last_hit {
            &self.last_hit.as_ref().unwrap().1
        } else {
            self.block_decoder.out()
        };

        let result = match Self::search_block_slice(block_slice, key, skip) {
            Ok(res) => res,
            Err(e) => {
                if matches!(e, StryxError::InvalidChecksum) {
                    self.readstats.checksum_failures += 1;
                }
                return Err(e);
            }
        };

        match result {
            Some(_) => self.readstats.point_hits += 1,
            None => self.readstats.point_misses += 1,
        }
        Ok(result)
    }

    // Reuses a caller-provided `Vec<u8>` buffer to avoid heap allocation once warmed up.
    #[inline]
    pub fn get_into(&mut self, key: &[u8], out: &mut Vec<u8>) -> Result<bool, StryxError> {
        match self.get_ref(key)? {
            Some(val) => {
                out.clear();
                out.extend_from_slice(val);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Retrieves the value associated with the given key.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Reading the underlying SSTable fails.
    /// - A data block is corrupted.
    /// - Checksum verification fails.
    /// - Block decompression or decoding fails.
    ///
    /// Returns `Ok(None)` if the key does not exist.
    #[inline]
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StryxError> {
        Ok(self.get_ref(key)?.map(|v| v.to_vec()))
    }

    /// Check whether a key exists.
    pub fn contains_key(&mut self, key: &[u8]) -> Result<bool, StryxError> {
        Ok(self.get(key)?.is_some())
    }

    #[cold]
    #[inline(never)]
    fn count_err(&mut self, e: StryxError) -> StryxError {
        if matches!(e, StryxError::InvalidChecksum) {
            self.readstats.checksum_failures += 1;
        }
        e
    }

    /// Looks up several keys in one call and returns their values in the same
    /// order as `keys`.
    ///
    /// The result has one entry per input key: `Some(value)` if the key is
    /// present, `None` otherwise. Each found value is copied out of its decoded
    /// block into its own `Vec<u8>`.
    ///
    /// Internally the keys are visited in sorted order, so keys that fall in the
    /// same block share a single block load (and a single cache lookup), and the
    /// index search only moves forward. This makes it cheaper than calling
    /// [`get`](Self::get) in a loop, especially when the keys are clustered or
    /// already sorted. Results are still returned in the caller's original order.
    /// Duplicate keys are allowed and each gets its own result.
    ///
    /// An empty `keys` slice returns an empty `Vec`.
    ///
    /// # Errors
    ///
    /// Returns an error if any block cannot be read or decompressed, or if its
    /// checksum verification fails. No partial results are returned on error.
    pub fn get_many(&mut self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StryxError> {
        if keys.len() == 1 {
            return Ok(vec![self.get(keys[0])?]);
        }

        let mut results: Vec<Option<Vec<u8>>> = vec![None; keys.len()];

        self.order.clear();
        self.order.extend(0..keys.len());
        if !keys.windows(2).all(|w| w[0] <= w[1]) {
            self.order.sort_unstable_by(|&a, &b| keys[a].cmp(keys[b]));
        }

        let mut cur_block: Option<usize> = None;
        let mut cur_arc: Option<Arc<[u8]>> = None;
        let mut search_from = 0;

        for i in 0..self.order.len() {
            let orig = self.order[i];
            let key = keys[orig];

            if !self.xorf_contains(key) {
                self.readstats.point_misses += 1;
                continue;
            }

            self.readstats.point_lookups += 1;

            // keys are sorted, so the block index never decreases
            let p = self.shared.index.partition_point_le_from(search_from, key);

            if p == 0 {
                self.readstats.point_misses += 1;
                continue;
            }
            let idx = p - 1;
            search_from = idx; // next key can't be in an earlier block

            if cur_block != Some(idx) {
                cur_arc = self
                    .shared
                    .block_cache
                    .as_ref()
                    .and_then(|c| c.get(&(idx as u32)));

                if cur_arc.is_some() {
                    self.readstats.cache_hits += 1;
                } else {
                    if self.shared.block_cache.is_some() {
                        self.readstats.cache_misses += 1;
                    }

                    self.load_block(idx).map_err(|e| self.count_err(e))?;

                    if let Some(cache) = &self.shared.block_cache
                        && self.shared.block_seen[idx]
                            .swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        let arc: Arc<[u8]> = Arc::from(self.loaded_block());
                        cache.insert(idx as u32, Arc::clone(&arc));
                        cur_arc = Some(arc);
                    }
                }
                cur_block = Some(idx);
            }

            let n = self.shared.index.entries.len();
            let skip = if p < n { self.shared.index.lcp_len } else { 0 };

            let found: Option<Vec<u8>> = {
                let block: &[u8] = match &cur_arc {
                    Some(a) => a,
                    None => self.loaded_block(),
                };
                Self::search_block(block, key, skip)
            }
            .map_err(|e| self.count_err(e))?;

            match found {
                Some(v) => {
                    self.readstats.point_hits += 1;
                    results[orig] = Some(v);
                }
                None => self.readstats.point_misses += 1,
            }
        }

        Ok(results)
    }

    /// Looks up `key` and returns its value as an owned [`Bytes`].
    ///
    /// The value is copied once out of the decoded block, so the returned
    /// `Bytes` owns its own allocation. It never borrows from, or keeps alive,
    /// the reader's scratch buffer or a cached block. This is not a zero-copy
    /// API: the benefit is downstream. Cloning or slicing the returned `Bytes`
    /// is a refcount bump, so it can be handed to other threads or consumers
    /// without further copies.
    ///
    /// Returns `Ok(None)` if the key is not present.
    ///
    /// # Errors
    ///
    /// Returns an error if the block cannot be read or decompressed, or if its
    /// checksum verification fails.
    pub fn get_in_bytes(&mut self, key: &[u8]) -> Result<Option<Bytes>, StryxError> {
        Ok(self.get(key)?.map(Bytes::from))
    }

    /// Looks up several keys and returns their values as owned [`Bytes`], in
    /// the same order as `keys`.
    ///
    /// Each found value is copied once out of its decoded block, so every
    /// returned `Bytes` owns a separate allocation and does not retain block or
    /// cache memory. Keys are processed in sorted order internally so each
    /// block is loaded at most once per call, but the results are returned in
    /// the caller's original order. Keys that are not present yield `None`.
    ///
    /// This is not a zero-copy API. Cloning or slicing the returned `Bytes`
    /// values is cheap (a refcount bump), which is the intended benefit.
    ///
    /// # Errors
    ///
    /// Returns an error if any block cannot be read or decompressed, or if its
    /// checksum verification fails. No partial results are returned on error.
    pub fn get_many_in_bytes(&mut self, keys: &[&[u8]]) -> Result<Vec<Option<Bytes>>, StryxError> {
        Ok(self
            .get_many(keys)?
            .into_iter()
            .map(|o| o.map(Bytes::from))
            .collect())
    }

    /// Iterate over all records in sorted order.
    pub fn iter(&mut self) -> impl Iterator<Item = Result<OwnedRecord, StryxError>> {
        self.readstats.iter_queries += 1;
        TableIterator::new(self)
    }

    /// Iterate over a bounded key range.
    pub fn range(&mut self, start: &[u8], end: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        if start >= end {
            return Err(StryxError::InvalidRange);
        }

        let block = self.shared.index.partition_point_le(start);

        let mut block_idx = block.saturating_sub(1);

        // If multiple blocks share first_key == start (allowed with duplicate keys),
        // rewind to the very first block that could contain `start`.
        while block_idx > 0 && self.shared.index.first_key(block_idx - 1) == start {
            block_idx -= 1;
        }

        self.readstats.range_queries += 1;

        Ok(TableIterator {
            reader: self,
            block_idx,
            record_idx: 0,
            records: Vec::new(),
            start_key: Some(start.to_vec()),
            end_key: Some(end.to_vec()),
            prefix: None,
            finished: false,
            stop_after_current_block: false,
        })
    }

    /// Iterate over keys sharing a prefix.
    pub fn prefix(&mut self, prefix: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        self.readstats.prefix_queries += 1;

        let block_idx = self
            .shared
            .index
            .partition_point_lt(prefix)
            .saturating_sub(1);

        Ok(TableIterator {
            reader: self,
            block_idx,
            record_idx: 0,
            records: Vec::new(),
            start_key: None,
            end_key: None,
            prefix: Some(prefix.to_vec()),
            finished: false,
            stop_after_current_block: false,
        })
    }

    /// Return table metadata.
    pub fn metadata(&self) -> &TableMetadata {
        &self.shared.metadata
    }

    /// Return runtime reader statistics.
    pub fn stats(&self) -> ReadStats {
        self.readstats
    }

    fn load_block(&mut self, idx: usize) -> Result<(), StryxError> {
        self.loaded_idx = None;

        let e = &self.shared.index.entries[idx];
        let start = usize::try_from(e.offset).map_err(|_| DecodeError::InvalidLength)?;
        let size = e.size as usize;
        let uncompressed_size = e.uncompressed_block_size as usize;

        let raw = self
            .shared
            .mmaped_file
            .get(start..start + size)
            .ok_or(DecodeError::Truncated)?;

        self.readstats.blocks_read += 1;
        self.readstats.bytes_read += size as u64;

        let (data, decompressed): (&[u8], bool) = match self.shared.metadata.compression {
            Compression::Zstd(_) => self.block_decoder.zstd(raw, uncompressed_size)?,
            Compression::Lz4 => self.block_decoder.lz4(raw, uncompressed_size)?,
            Compression::None => (raw, false),
        };

        if decompressed {
            self.readstats.blocks_decompressed += 1;
            self.readstats.bytes_decompressed += data.len() as u64;
        }

        if !self.shared.disable_block_verification
            && !self.shared.crc_verification[idx].load(std::sync::atomic::Ordering::Relaxed)
        {
            Self::verify_block_once(&self.shared, &mut self.readstats, idx, data)?;
        }

        self.loaded_idx = Some(idx);

        Ok(())
    }

    #[inline(always)]
    fn loaded_block(&self) -> &[u8] {
        match self.shared.metadata.compression {
            Compression::None => {
                let idx = self.loaded_idx.expect("block must be loaded");
                let e = &self.shared.index.entries[idx];
                let start = usize::try_from(e.offset).expect("Invalid offset length");
                &self.shared.mmaped_file[start..start + e.size as usize]
            }
            Compression::Zstd(_) | Compression::Lz4 => &self.block_decoder.out(),
        }
    }
}

/// Statistics tracker recording performance metrics such as read operations,
/// bytes read, and cache performance.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReadStats {
    /// Number of data blocks successfully read from the file.
    pub blocks_read: u64,

    /// Total compressed/on-disk data bytes successfully read.
    pub bytes_read: u64,

    /// Number of blocks successfully decompressed.
    pub blocks_decompressed: u64,

    /// Total uncompressed bytes produced by decompression.
    pub bytes_decompressed: u64,

    /// Number of point lookup operations.
    pub point_lookups: u64,

    /// Number of successful point lookups.
    pub point_hits: u64,

    /// Number of unsuccessful point lookups.
    pub point_misses: u64,

    /// Number of checksum verification failures.
    pub checksum_failures: u64,

    /// Number of prefix queries.
    pub prefix_queries: u64,

    /// Number of range queries.
    pub range_queries: u64,

    /// Number of full-table iteration queries.
    pub iter_queries: u64,

    /// Cache hits
    pub cache_hits: u32,

    /// Cache misses
    pub cache_misses: u32,
}

impl ReadStats {
    /// Fold `other` into `self`. Every field is merged.
    pub fn merge(&mut self, other: &ReadStats) {
        self.blocks_read += other.blocks_read;
        self.bytes_read += other.bytes_read;
        self.blocks_decompressed += other.blocks_decompressed;
        self.bytes_decompressed += other.bytes_decompressed;
        self.point_lookups += other.point_lookups;
        self.point_hits += other.point_hits;
        self.point_misses += other.point_misses;
        self.checksum_failures += other.checksum_failures;
        self.prefix_queries += other.prefix_queries;
        self.range_queries += other.range_queries;
        self.iter_queries += other.iter_queries;
        self.cache_hits += other.cache_hits;
        self.cache_misses += other.cache_misses;
    }

    /// Reset every counter to zero.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl std::fmt::Display for ReadStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes = |n: u64| -> String {
            if n < 1024 {
                return format!("{n} B");
            }
            let mut val = n as f64;
            let mut unit = 0;
            while val >= 1024.0 && unit < UNITS.len() - 1 {
                val /= 1024.0;
                unit += 1;
            }
            format!("{val:.2} {}", UNITS[unit])
        };

        let total_point = self.point_hits + self.point_misses;
        let hit_rate = if total_point == 0 {
            None
        } else {
            Some(100.0 * self.point_hits as f64 / total_point as f64)
        };

        let ratio = if self.bytes_read == 0 {
            None
        } else {
            Some(self.bytes_decompressed as f64 / self.bytes_read as f64)
        };

        writeln!(f, "ReadStats {{")?;

        writeln!(f, "  I/O:")?;
        writeln!(f, "    blocks read:        {}", self.blocks_read)?;
        writeln!(
            f,
            "    bytes read:         {} ({})",
            bytes(self.bytes_read),
            self.bytes_read
        )?;

        writeln!(f, "  Decompression:")?;
        writeln!(f, "    blocks decompressed: {}", self.blocks_decompressed)?;
        writeln!(
            f,
            "    bytes decompressed:  {} ({})",
            bytes(self.bytes_decompressed),
            self.bytes_decompressed
        )?;

        match ratio {
            Some(r) => writeln!(f, "    compression ratio:   {r:.2}x")?,
            None => writeln!(f, "    compression ratio:   n/a")?,
        }

        writeln!(f, "  Point lookups:")?;
        writeln!(f, "    total:  {}", self.point_lookups)?;
        writeln!(f, "    hits:   {}", self.point_hits)?;
        writeln!(f, "    misses: {}", self.point_misses)?;

        match hit_rate {
            Some(r) => writeln!(f, "    hit rate: {r:.1}%")?,
            None => writeln!(f, "    hit rate: n/a")?,
        }

        writeln!(f, "  Queries:")?;
        writeln!(f, "    prefix: {}", self.prefix_queries)?;
        writeln!(f, "    range:  {}", self.range_queries)?;
        writeln!(f, "    iter:   {}", self.iter_queries)?;

        writeln!(f, "  Checksum failures: {}", self.checksum_failures)?;

        write!(f, "}}")
    }
}

/// OwnedRecord with keys and values
#[derive(Debug)]
pub struct OwnedRecord {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// Iterator for the table
pub struct TableIterator<'a> {
    reader: &'a mut TableReader,
    block_idx: usize,
    record_idx: usize,
    records: Vec<OwnedRecord>,
    start_key: Option<Vec<u8>>,
    end_key: Option<Vec<u8>>,
    prefix: Option<Vec<u8>>,
    stop_after_current_block: bool,
    finished: bool,
}

impl<'a> TableIterator<'a> {
    fn new(reader: &'a mut TableReader) -> Self {
        Self {
            reader,
            block_idx: 0,
            record_idx: 0,
            records: vec![],
            start_key: None,
            end_key: None,
            prefix: None,
            finished: false,
            stop_after_current_block: false,
        }
    }
}

impl Iterator for TableIterator<'_> {
    type Item = Result<OwnedRecord, StryxError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        loop {
            if self.record_idx < self.records.len() {
                let record = &self.records[self.record_idx];

                // return if end_key is some and is found
                if self
                    .end_key
                    .as_ref()
                    .is_some_and(|end| record.key.as_slice() >= end.as_slice())
                {
                    self.finished = true;
                    return None;
                }

                let record = std::mem::replace(
                    &mut self.records[self.record_idx],
                    OwnedRecord {
                        key: Vec::new(),
                        value: Vec::new(),
                    },
                );

                self.record_idx += 1;

                return Some(Ok(record));
            }

            // return early if cur key is greater than prefix
            if self.stop_after_current_block {
                self.finished = true;
                return None;
            }

            if self.block_idx >= self.reader.shared.index.entries.len() {
                self.finished = true;
                return None;
            }

            let (offset, size, uncompressed_block_size) = {
                let entry = &self.reader.shared.index.entries[self.block_idx];
                (entry.offset, entry.size, entry.uncompressed_block_size)
            };

            self.block_idx += 1;

            let result = (|| -> Result<Vec<OwnedRecord>, StryxError> {
                let start = usize::try_from(offset).map_err(|_| DecodeError::InvalidLength)?;
                let end = start
                    .checked_add(size as usize)
                    .ok_or(StryxError::InvalidOffset)?;

                let buf = self
                    .reader
                    .shared
                    .mmaped_file
                    .get(start..end)
                    .ok_or(StryxError::InvalidOffset)?;

                self.reader.block_buf.clear();
                self.reader.block_buf.extend_from_slice(buf);

                self.reader.readstats.blocks_read += 1;
                self.reader.readstats.bytes_read += u64::from(size);

                let (block, decompressed) = match self.reader.shared.metadata.compression {
                    Compression::Zstd(_) => self
                        .reader
                        .block_decoder
                        .zstd(&self.reader.block_buf, uncompressed_block_size as usize)?,
                    Compression::Lz4 => self
                        .reader
                        .block_decoder
                        .lz4(&self.reader.block_buf, uncompressed_block_size as usize)?,
                    Compression::None => (self.reader.block_buf.as_slice(), false),
                };

                if decompressed {
                    self.reader.readstats.blocks_decompressed += 1;
                    self.reader.readstats.bytes_decompressed += block.len() as u64;
                }

                let records = BlockReader::new(block)?.records()?;

                // take the start_key, so that subsequent blocks start at index 0
                let start_pos = self
                    .start_key
                    .take()
                    .as_deref()
                    .or(self.prefix.as_deref())
                    .map(|key| records.partition_point(|record| record.key < key))
                    .unwrap_or(0);

                let records_slice = &records[start_pos..];

                let end_pos = self
                    .prefix
                    .as_ref()
                    .map(|prefix| {
                        records_slice.partition_point(|record| record.key.starts_with(prefix))
                    })
                    .unwrap_or(records_slice.len());

                let prefix_finished = self.prefix.is_some() && end_pos < records_slice.len();

                self.stop_after_current_block = prefix_finished;

                let records: Vec<OwnedRecord> = records_slice[..end_pos]
                    .iter()
                    .map(|record| OwnedRecord {
                        key: record.key.to_vec(),
                        value: record.value.to_vec(),
                    })
                    .collect();

                Ok(records)
            })();

            match result {
                Ok(records) => {
                    self.records = records;
                    self.record_idx = 0;
                }

                Err(err) => {
                    if matches!(&err, StryxError::InvalidChecksum) {
                        self.reader.readstats.checksum_failures += 1;
                    }

                    self.finished = true;
                    return Some(Err(err));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_block(records: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut block = Vec::new();
        let mut offsets = Vec::new();

        for (key, value) in records {
            offsets.push(block.len() as u32);

            block.extend_from_slice(&(key.len() as u16).to_le_bytes());
            block.extend_from_slice(&(value.len() as u32).to_le_bytes());
            block.extend_from_slice(key);
            block.extend_from_slice(value);
        }

        for offset in offsets {
            block.extend_from_slice(&offset.to_le_bytes());
        }

        block.extend_from_slice(&(records.len() as u32).to_le_bytes());

        let checksum = crc32c::crc32c(&block);
        block.extend_from_slice(&checksum.to_le_bytes());

        block
    }

    #[test]
    fn test_block_reader_truncated_block() {
        for len in 0..8 {
            let block = vec![0u8; len];

            assert!(
                BlockReader::new(&block).is_err(),
                "unexpectedly accepted block of length {len}"
            );
        }
    }

    #[test]
    fn test_block_reader_too_many_records() {
        let mut block = make_block(&[(b"apple", b"red")]);

        let count_offset = block.len() - 8;

        block[count_offset..count_offset + 4].copy_from_slice(&2u32.to_le_bytes());

        assert!(BlockReader::verify_checksum(&block).is_err() || BlockReader::new(&block).is_err());
    }
}
