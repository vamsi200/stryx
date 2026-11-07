#![allow(unused)]

use std::{
    fmt::Debug,
    fs::File,
    io::{Read, Seek},
    num::NonZeroU64,
    os::unix::fs::FileExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    vec,
};

use anyhow::Result;
use bytes::Bytes;
use moka::sync::Cache;
use xorf::Filter;

use crate::{
    BLOCK_SIZE, FOOTER_SIZE, KEY_HASH_SEED, MAGIC, UNITS, VERSION,
    builder::{FilterTypeRepr, Record, TableMetadata, XorFilter, from_filter_type},
    config::Compression,
    decoder::Decoder,
    error::{DecodeError, StryxError},
    helper::filter_decode,
    index::IndexEntry,
};
use std::sync::atomic::Ordering::Relaxed;
use zstd::bulk::Decompressor;

pub type BlockCache = Cache<u32, Arc<[u8]>>;

#[derive(Debug, Clone)]
pub struct BlockCacheOptions {
    pub max_capacity: Option<u64>,
}

impl BlockCacheOptions {
    pub fn bytes(value: u64) -> Self {
        assert!(value > 0);

        Self {
            max_capacity: Some(value),
        }
    }

    pub fn build_cache(&self) -> BlockCache {
        Cache::builder()
            .max_capacity(self.max_capacity.unwrap())
            .weigher(|_idx, block: &Arc<[u8]>| u32::try_from(block.len()).unwrap_or(u32::MAX))
            .build()
    }
}

struct BlockDecoder {
    zstd: Option<Decompressor<'static>>,
    out: Vec<u8>,
}

impl Debug for BlockDecoder {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl BlockDecoder {
    fn new(compression: Compression) -> std::io::Result<Self> {
        Ok(Self {
            zstd: matches!(compression, Compression::Zstd)
                .then(Decompressor::new)
                .transpose()?,
            out: Vec::new(),
        })
    }

    fn zstd(&mut self, src: &[u8], max_len: usize) -> Result<(&[u8], bool), StryxError> {
        self.out.clear();
        self.out.reserve(max_len);
        let n = self
            .zstd
            .as_mut()
            .unwrap()
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

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct BlockReader<'a> {
    buf: &'a [u8],
    offsets: &'a [u8],
    record_count: usize,
}

impl<'a> BlockReader<'a> {
    fn new(buf: &'a [u8]) -> Result<Self, StryxError> {
        if buf.len() < 8 {
            return Err(DecodeError::InvalidLength.into());
        }

        let buf_len = buf.len();

        let record_count = u32::from_le_bytes(
            buf[buf_len - 8..buf_len - 4]
                .try_into()
                .map_err(DecodeError::from)?,
        ) as usize;

        // offset table_len = record_count * size_of::<u32>()
        let table_len = record_count
            .checked_mul(size_of::<u32>())
            .ok_or(DecodeError::InvalidLength)?;

        // buf_len - 8 is where offset table ends (last 8 are record_count and checksum)
        let records_end = (buf_len - 8)
            .checked_sub(table_len)
            .ok_or(DecodeError::InvalidLength)?;

        Ok(Self {
            buf: &buf[..records_end],
            offsets: &buf[records_end..buf_len - 8],
            record_count,
        })
    }

    #[inline]
    fn offset(&self, i: usize) -> usize {
        u32::from_le_bytes(self.offsets[i * 4..i * 4 + 4].try_into().unwrap()) as usize
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
        let stored = u32::from_le_bytes(buf[n - 4..].try_into().unwrap());
        if crc32c::crc32c(&buf[..n - 4]) != stored {
            return Err(StryxError::InvalidChecksum);
        }
        Ok(())
    }

    #[inline]
    fn key_at(&self, offset: usize) -> Result<&'a [u8], StryxError> {
        let header = self
            .buf
            .get(offset..offset + 2)
            .ok_or(DecodeError::Truncated)?;
        let key_len = u16::from_le_bytes([header[0], header[1]]) as usize;
        let key_start = offset + 6;
        self.buf
            .get(key_start..key_start + key_len)
            .ok_or_else(|| DecodeError::Truncated.into())
    }

    // binary search the key
    fn get(&self, key: &[u8]) -> Result<Option<(&'a [u8], u32)>, StryxError> {
        let (mut lo, mut hi) = (0usize, self.record_count);

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let offset = self.offset(mid);

            // ONLY parse the key, don't waste time checking value lengths/bounds
            let k = self.key_at(offset)?;

            if k < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        if lo == self.record_count {
            return Ok(None);
        }

        let off = self.offset(lo);
        let (k, v) = self.record_at(off)?;
        Ok((k == key).then_some((v, off as u32)))
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

#[derive(Debug)]
pub struct ReaderOptions {
    pub disable_block_verification: bool,
    pub block_cache: Option<BlockCache>,
}

impl Default for ReaderOptions {
    fn default() -> Self {
        Self {
            disable_block_verification: true, // change this
            block_cache: None,
        }
    }
}

/// The thread-safe core containing the file, index, and atomic stats.
#[derive(Debug)]
pub(crate) struct SharedTable {
    file: std::fs::File,
    index: Vec<IndexEntry>,
    metadata: TableMetadata,
    disable_block_verification: bool,
    crc_verification: Vec<AtomicBool>,
    filter: XorFilter,
    block_cache: Option<BlockCache>,
    block_seen: Vec<AtomicBool>,
}

#[derive(Debug)]
pub struct TableReader {
    pub readstats: ReadStats,
    shared: Arc<SharedTable>,
    block_buf: Vec<u8>,
    block_decoder: BlockDecoder,
    order: Vec<usize>,
    loaded_idx: Option<usize>, // block currently decoded
}

/// Cloning the reader creates a new scratchpad for a new thread,
/// but shares the heavy underlying index and file descriptors!
impl Clone for TableReader {
    fn clone(&self) -> Self {
        Self {
            readstats: ReadStats::default(),
            shared: Arc::clone(&self.shared),
            block_buf: Vec::with_capacity(BLOCK_SIZE),
            block_decoder: BlockDecoder::new(self.shared.metadata.compression).unwrap(),
            order: Vec::new(),
            loaded_idx: None,
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

        let mut index_entries = Vec::new();

        for _ in 0..count {
            index_entries.push(IndexEntry::decode(&mut decoder)?);
        }

        // Reject unexpected trailing bytes in the entry region.
        if decoder.remaining() != 0 {
            return Err(DecodeError::InvalidLength.into());
        }

        let crc_verification: Vec<AtomicBool> = (0..index_entries.len())
            .map(|_| AtomicBool::new(false))
            .collect();

        let seen: Vec<AtomicBool> = if reader_options.block_cache.is_some() {
            (0..index_entries.len())
                .map(|_| AtomicBool::new(false))
                .collect()
        } else {
            Vec::new()
        };

        let block_decoder = match metadata.compression {
            Compression::Zstd => BlockDecoder::new(Compression::Zstd)?,
            Compression::Lz4 => BlockDecoder::new(Compression::Lz4)?,
            Compression::None => BlockDecoder::new(Compression::None)?,
        };

        let share_table = Arc::new(SharedTable {
            file,
            index: index_entries,
            metadata,
            disable_block_verification: reader_options.disable_block_verification,
            crc_verification,
            filter,
            block_cache: reader_options.block_cache,
            block_seen: seen,
        });

        Ok(TableReader {
            readstats: ReadStats::default(),
            shared: share_table,
            block_buf: Vec::new(),
            block_decoder,
            order: Vec::new(),
            loaded_idx: None,
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

    /// Find the value associated with a key.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StryxError> {
        if !self.xorf_contains(key) {
            self.readstats.point_misses += 1;
            return Ok(None);
        }
        self.readstats.point_lookups += 1;

        // find the first index entry whose first_key is greater than requested
        // key.
        let block = self
            .shared
            .index
            .partition_point(|e| e.first_key.as_slice() <= key);

        if block == 0 {
            self.readstats.point_misses += 1;
            return Ok(None);
        }

        // since returned block's first_key is greater than the requested key,
        // the requested key `could be` in previous one
        let idx = block - 1;

        // check cache first: a hit skips read, decompress and CRC
        let cached = self
            .shared
            .block_cache
            .as_ref()
            .and_then(|c| c.get(&(idx as u32)));

        let result = if self.loaded_idx == Some(idx) {
            // same block as the previous lookup, the scratch still holds the block
            Self::search_block(self.loaded_block(), key)
        } else {
            let cached = self
                .shared
                .block_cache
                .as_ref()
                .and_then(|c| c.get(&(idx as u32)));

            if let Some(data) = cached {
                self.readstats.cache_hits += 1;
                Self::search_block(&data, key)
            } else {
                if self.shared.block_cache.is_some() {
                    self.readstats.cache_misses += 1;
                }

                self.load_block(idx)?;

                // insert only on the SECOND load of a block, so one-shot blocks
                // never pay for the allocation, the copy or the insert
                if let Some(cache) = &self.shared.block_cache {
                    if self.shared.block_seen[idx].swap(true, Ordering::Relaxed) {
                        cache.insert(idx as u32, Arc::from(self.loaded_block()));
                    }
                }

                Self::search_block(self.loaded_block(), key)
            }
        };

        let result = result.map_err(|e| {
            if matches!(e, StryxError::InvalidChecksum) {
                self.readstats.checksum_failures += 1;
            }
            e
        })?;

        match &result {
            Some(_) => self.readstats.point_hits += 1,
            None => self.readstats.point_misses += 1,
        }
        Ok(result)
    }

    /// Check whether a key exists.
    pub fn contains_key(&mut self, key: &[u8]) -> Result<bool, StryxError> {
        Ok(self.get(key)?.is_some())
    }

    /// Batch point lookups.
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
            let p = search_from
                + self.shared.index[search_from..]
                    .partition_point(|e| e.first_key.as_slice() <= key);
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

                    self.load_block(idx)?;

                    if let Some(cache) = &self.shared.block_cache {
                        let arc: Arc<[u8]> = Arc::from(self.loaded_block());
                        cache.insert(idx as u32, Arc::clone(&arc));
                        cur_arc = Some(arc);
                    }
                }
                cur_block = Some(idx);
            }

            let found: Option<Vec<u8>> = {
                let block: &[u8] = match &cur_arc {
                    Some(a) => a,
                    None => self.loaded_block(),
                };
                Self::search_block(block, key)?
            };

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

    pub fn get_bytes(&mut self, key: &[u8]) -> Result<Option<Bytes>, StryxError> {
        todo!()
    }

    pub fn get_many_bytes(&mut self, keys: &[&[u8]]) -> Result<Vec<Option<Bytes>>, StryxError> {
        todo!()
    }

    /// Iterate over all records in sorted order.
    pub fn iter(&mut self) -> impl Iterator<Item = Result<OwnedRecord, StryxError>> + '_ {
        self.readstats.iter_queries += 1;
        TableIterator::new(self)
    }

    /// Iterate over a bounded key range.
    pub fn range(&mut self, start: &[u8], end: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        if start >= end {
            return Err(StryxError::InvalidRange);
        }

        let block = self
            .shared
            .index
            .partition_point(|entry| entry.first_key.as_slice() <= start);

        let mut block_idx = block.saturating_sub(1);

        // If multiple blocks share first_key == start (allowed with duplicate keys),
        // rewind to the very first block that could contain `start`.
        while block_idx > 0 && self.shared.index[block_idx - 1].first_key.as_slice() == start {
            block_idx -= 1;
        }

        let compression = self.shared.metadata.compression;

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
            compression,
        })
    }

    /// Iterate over keys sharing a prefix.
    pub fn prefix(&mut self, prefix: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        let compression = self.shared.metadata.compression;

        self.readstats.prefix_queries += 1;

        Ok(TableIterator {
            reader: self,
            block_idx: 0,
            record_idx: 0,
            records: Vec::new(),
            start_key: None,
            end_key: None,
            prefix: Some(prefix.to_vec()),
            finished: false,
            stop_after_current_block: false,
            compression,
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

    fn search_block(block: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, StryxError> {
        let reader = BlockReader::new(block)?;
        Ok(reader.get(key)?.map(|(v, _)| v.to_vec()))
    }

    fn load_block(&mut self, idx: usize) -> Result<(), StryxError> {
        self.loaded_idx = None;

        let (offset, size, uncompressed_size) = {
            let e = &self.shared.index[idx];
            (e.offset, e.size, e.uncompressed_block_size)
        };

        self.block_buf.resize(size as usize, 0);
        self.shared
            .file
            .read_exact_at(&mut self.block_buf, offset)?;

        self.readstats.blocks_read += 1;
        self.readstats.bytes_read += u64::from(size);

        let (data, decompressed): (&[u8], bool) = match self.shared.metadata.compression {
            Compression::Zstd => self
                .block_decoder
                .zstd(&self.block_buf, uncompressed_size as usize)?,
            Compression::Lz4 => self
                .block_decoder
                .lz4(&self.block_buf, uncompressed_size as usize)?,
            Compression::None => (self.block_buf.as_slice(), false),
        };

        if decompressed {
            self.readstats.blocks_decompressed += 1;
            self.readstats.bytes_decompressed += data.len() as u64;
        }

        if !self.shared.disable_block_verification
            && !self.shared.crc_verification[idx].load(Ordering::Relaxed)
        {
            if let Err(e) = BlockReader::verify_checksum(data) {
                if matches!(e, StryxError::InvalidChecksum) {
                    self.readstats.checksum_failures += 1;
                }
                return Err(e);
            }
            self.shared.crc_verification[idx].store(true, Ordering::Relaxed);
        }

        self.loaded_idx = Some(idx);

        Ok(())
    }

    fn loaded_block(&self) -> &[u8] {
        match self.shared.metadata.compression {
            Compression::None => &self.block_buf,
            Compression::Zstd | Compression::Lz4 => self.block_decoder.out(),
        }
    }

    fn lookup_in(&mut self, idx: usize, key: &[u8]) -> Result<Option<Vec<u8>>, StryxError> {
        self.load_block(idx)?;

        let result: Option<Vec<u8>> = {
            let block = self.loaded_block();

            let reader = match BlockReader::new(&block) {
                Ok(r) => r,
                Err(err) => {
                    if matches!(err, StryxError::InvalidChecksum) {
                        self.readstats.checksum_failures += 1;
                    }
                    return Err(err);
                }
            };

            reader.get(key)?.map(|res| res.0.to_vec())
        };

        match &result {
            Some(_) => self.readstats.point_hits += 1,
            None => self.readstats.point_misses += 1,
        }
        Ok(result)
    }
}

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

#[derive(Debug)]
pub struct OwnedRecord {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

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
    compression: Compression,
}

impl<'a> TableIterator<'a> {
    fn new(reader: &'a mut TableReader) -> Self {
        let compression = reader.shared.metadata.compression;

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
            compression,
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

            if self.block_idx >= self.reader.shared.index.len() {
                self.finished = true;
                return None;
            }

            let (offset, size, uncompressed_block_size) = {
                let entry = &self.reader.shared.index[self.block_idx];
                (entry.offset, entry.size, entry.uncompressed_block_size)
            };

            self.block_idx += 1;

            let result = (|| -> Result<Vec<OwnedRecord>, StryxError> {
                self.reader.block_buf.resize(size as usize, 0);
                self.reader
                    .shared
                    .file
                    .read_exact_at(&mut self.reader.block_buf, offset)?;

                self.reader.readstats.blocks_read += 1;
                self.reader.readstats.bytes_read += u64::from(size);

                let (block_buf, decompressed) =
                    if self.reader.shared.metadata.compression == Compression::Zstd {
                        self.reader
                            .block_decoder
                            .zstd(&self.reader.block_buf, uncompressed_block_size as usize)?
                    } else if self.reader.shared.metadata.compression == Compression::Lz4 {
                        self.reader
                            .block_decoder
                            .lz4(&self.reader.block_buf, uncompressed_block_size as usize)?
                    } else {
                        (self.reader.block_buf.as_slice(), false)
                    };

                if decompressed {
                    self.reader.readstats.blocks_decompressed += 1;
                    self.reader.readstats.bytes_decompressed += block_buf.len() as u64;
                }

                let records = BlockReader::new(&block_buf)?.records()?;

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
