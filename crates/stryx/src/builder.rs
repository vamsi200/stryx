use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
    vec,
};

use anyhow::Context;
use xorf::{BinaryFuse8, BinaryFuse16, BinaryFuse32};

use crate::{
    FOOTER_SIZE, KEY_HASH_SEED, MAGIC, UNITS, VERSION,
    config::{Compression, DuplicatePolicy, FilterType, KeyOrder, TableOptions},
    decoder::Decoder,
    error::{DecodeError, StryxError},
    helper::filter_encode,
    index::IndexBuilder,
};

#[derive(Debug, Default, Clone)]
pub struct Record<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
}

impl<'a> Record<'a> {
    pub fn len(&self) -> usize {
        self.key.len() + self.value.len()
    }
}

pub trait Encode {
    fn encode_len(&self) -> usize;
    fn encode_record(&self) -> Result<Vec<u8>, DecodeError>;
}

pub struct RecordReader<'a> {
    value: &'a [u8],
    pos: usize,
}

impl<'a> RecordReader<'a> {
    pub fn new(value: &'a [u8]) -> Self {
        Self { value, pos: 0 }
    }

    pub fn next_record(&mut self) -> Result<Option<Record<'a>>, DecodeError> {
        if self.pos == self.value.len() {
            return Ok(None);
        }

        let remaining = &self.value[self.pos..];

        if remaining.len() < 6 {
            return Err(DecodeError::Truncated);
        }

        let key_len = u16::from_le_bytes([remaining[0], remaining[1]]) as usize;
        let value_len = u32::from_le_bytes(
            remaining[2..6]
                .try_into()
                .map_err(|_| DecodeError::InvalidLength)?,
        ) as usize;

        let record_len = 6usize
            .checked_add(key_len)
            .and_then(|v| v.checked_add(value_len))
            .ok_or(DecodeError::InvalidLength)?;

        if remaining.len() < record_len {
            return Err(DecodeError::Truncated);
        }

        let key = &remaining[6..6 + key_len];
        let value = &remaining[6 + key_len..record_len];

        self.pos += record_len;

        Ok(Some(Record { key, value }))
    }

    pub fn pos(&self) -> usize {
        self.pos
    }
}

impl Encode for Record<'_> {
    fn encode_len(&self) -> usize {
        size_of::<u16>() + size_of::<u32>() + self.key.len() + self.value.len()
    }

    fn encode_record(&self) -> Result<Vec<u8>, DecodeError> {
        let mut value = Vec::with_capacity(self.encode_len());

        let key_len = u16::try_from(self.key.len())
            .context("Key Too Large")
            .unwrap();
        let value_len = u32::try_from(self.value.len())
            .context("Value Too Large")
            .unwrap();

        value.write(&key_len.to_le_bytes())?;
        value.write(&value_len.to_le_bytes())?;
        value.write(&self.key)?;
        value.write(&self.value)?;

        Ok(value)
    }
}

#[repr(u8)]
#[derive(Debug)]
pub enum FilterTypeRepr {
    None = 0,
    BinaryFuse8 = 1,
    BinaryFuse16 = 2,
    BinaryFuse32 = 3,
}

pub fn from_filter_type(val: u8) -> Option<FilterTypeRepr> {
    match val {
        1 => Some(FilterTypeRepr::BinaryFuse8),
        2 => Some(FilterTypeRepr::BinaryFuse16),
        3 => Some(FilterTypeRepr::BinaryFuse32),
        _ => None,
    }
}

#[derive(Debug)]
pub enum XorFilter {
    None,
    BinaryFuse8(BinaryFuse8),
    BinaryFuse16(BinaryFuse16),
    BinaryFuse32(BinaryFuse32),
}

pub(crate) struct FilterSection {
    pub data: Vec<u8>,
    pub checksum: u32,
}

#[derive(Debug)]
pub struct TableBuilder<W: Write> {
    /// Destination for the encoded SSTable
    writer: W,

    /// Sorter for unsorted keys
    sorter: Option<spillsort::Sorter>,

    /// Current data block being constructed
    block: BlockBuilder,

    /// Index entries for completed data blocks
    index: IndexBuilder,

    /// Configuration for this SSTable
    options: TableOptions,

    /// Last inserted key, used to enforce sorted ordering
    last_key: Option<Vec<u8>>,

    /// First Key of a block
    block_first_key: Option<Vec<u8>>,

    /// Offset of a block
    offset: u64,

    /// Min Key in the table
    min_key: Option<Vec<u8>>,

    /// Max key in the table
    max_key: Option<Vec<u8>>,

    /// Block count
    block_count: u32,

    /// Total record count
    record_count: u64,

    /// Filter Hashes
    filter_hashes: Vec<u64>,
}

impl TableBuilder<BufWriter<File>> {
    /// Create an SSTable at the given path
    pub fn create(path: impl AsRef<Path>, options: TableOptions) -> Result<Self, StryxError> {
        let sorter = match options.key_order {
            KeyOrder::Presorted => None,
            KeyOrder::Lexicographic => Some(spillsort::Sorter::new(
                spillsort::Config::default().max_bytes(64 * 1024 * 1024),
            )?),
        };

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .read(true)
            .open(path)?;

        Ok(Self {
            writer: BufWriter::new(file),
            sorter,
            block: BlockBuilder::new(),
            index: IndexBuilder::new(),
            options,
            last_key: None,
            block_first_key: None,
            offset: 0,
            min_key: None,
            max_key: None,
            block_count: 0,
            record_count: 0,
            filter_hashes: vec![],
        })
    }
}

impl<'a, W: std::io::Write> TableBuilder<W> {
    /// Create an SSTable using a custom writer
    pub fn new(writer: W, options: TableOptions) -> Result<Self, StryxError> {
        let sorter = match options.key_order {
            KeyOrder::Presorted => None,
            KeyOrder::Lexicographic => Some(spillsort::Sorter::new(
                spillsort::Config::default().max_bytes(64 * 1024 * 1024),
            )?),
        };

        Ok(Self {
            writer,
            sorter,
            block: BlockBuilder::new(),
            index: IndexBuilder::new(),
            options,
            last_key: None,
            block_first_key: None,
            offset: 0,
            min_key: None,
            max_key: None,
            block_count: 0,
            record_count: 0,
            filter_hashes: vec![],
        })
    }

    fn write_record(&mut self, key: &[u8], value: &[u8]) -> Result<(), StryxError> {
        // Validate global key ordering
        if let Some(last_key) = self.last_key.as_deref() {
            if key < last_key {
                return Err(StryxError::InvalidKeyOrder);
            }

            if key == last_key && self.options.duplicate_policy == DuplicatePolicy::Reject {
                return Err(StryxError::DuplicateKey);
            }
        }

        self.filter_hashes.push(Self::hash_key(key));

        let record = Record { key, value };
        self.record_count += 1;

        if self.min_key.is_none() {
            self.min_key = Some(key.to_vec());
        }

        self.max_key = Some(key.to_vec());

        let encoded_record = record.encode_record()?;

        // Include the additional u32 offset for the incoming record
        if !self.block.is_empty()
            && self.block.size() + encoded_record.len() + size_of::<u32>() > self.options.block_size
        {
            // println!("Exceeds Block Size, inserting into new one..");
            self.flush_block()?;
        }

        // First record of a new block
        if self.block.is_empty() {
            self.block_first_key = Some(key.to_owned());
        }

        self.block.add(&encoded_record)?;

        self.last_key = Some(key.to_owned());

        Ok(())
    }

    /// Append a record
    pub fn add(&mut self, key: &[u8], value: &[u8]) -> Result<(), StryxError> {
        if let Some(sorter) = &mut self.sorter {
            self.filter_hashes.push(Self::hash_key(key));
            sorter.push(key, value)?;
            return Ok(());
        }

        self.write_record(key, value)?;
        Ok(())
    }

    // Construct the block and flush it
    fn flush_block(&mut self) -> Result<(), StryxError> {
        if self.block.is_empty() {
            return Ok(());
        }

        let block = self.block.finish()?;

        let encoded_block = match self.options.compression {
            Compression::None => block,
            Compression::Zstd => zstd::encode_all(block.as_slice(), 1)?,
            Compression::Lz4 => {
                let mut encoder = lz4::EncoderBuilder::new().level(4).build(Vec::new())?;

                encoder.write_all(&block)?;

                let (output, result) = encoder.finish();
                result?;

                output
            }
        };

        let block_offset = self.offset;
        let block_size = u32::try_from(encoded_block.len())?;

        self.writer.write_all(&encoded_block)?;

        self.index.add(
            self.block_first_key.as_deref().unwrap(),
            block_offset,
            block_size,
        );

        self.offset += encoded_block.len() as u64;
        self.block_count += 1;

        self.block.clear();
        self.block_first_key = None;

        Ok(())
    }

    /// Append a batch of records
    pub fn add_batch(
        &mut self,
        records: impl IntoIterator<Item = Record<'a>>,
    ) -> Result<(), StryxError> {
        for record in records {
            self.add(record.key, record.value)?;
        }
        Ok(())
    }

    /// Flush remaining blocks, write index and footer, finalize file
    /// and return the metadata about completed table.
    pub fn finalize(mut self) -> Result<(TableMetadata, WriteStatistics), StryxError> {
        if let Some(sorter) = self.sorter.take() {
            let mut sorted = sorter.finish()?;

            while let Some((key, value)) = sorted.next_record()? {
                self.write_record(&key, &value)?;
            }
        }

        self.flush_block()?;

        self.filter_hashes.sort_unstable();
        self.filter_hashes.dedup();

        let filter = match self.options.filter_type {
            FilterType::None => XorFilter::None,
            FilterType::BinaryFuse8 => XorFilter::BinaryFuse8(
                BinaryFuse8::try_from(&self.filter_hashes)
                    .map_err(|_| DecodeError::FilterFailed)?,
            ),
            FilterType::BinaryFuse16 => XorFilter::BinaryFuse16(
                BinaryFuse16::try_from(&self.filter_hashes)
                    .map_err(|_| DecodeError::FilterFailed)?,
            ),
            FilterType::BinaryFuse32 => XorFilter::BinaryFuse32(
                BinaryFuse32::try_from(&self.filter_hashes)
                    .map_err(|_| DecodeError::FilterFailed)?,
            ),
        };

        let filter_type_repr = match filter {
            XorFilter::None => FilterTypeRepr::None,
            XorFilter::BinaryFuse8(_) => FilterTypeRepr::BinaryFuse8,
            XorFilter::BinaryFuse16(_) => FilterTypeRepr::BinaryFuse16,
            XorFilter::BinaryFuse32(_) => FilterTypeRepr::BinaryFuse32,
        };

        let filter_section = filter_encode(filter)?;

        let statistics = self.statistics();

        // Encode sparse index
        let encoded_index = self.index.finish()?;

        // index block
        let index_offset = self.offset;
        let index_size = u64::try_from(encoded_index.len())?;

        let mut table_metadata = TableMetadata {
            record_count: self.record_count,
            block_count: self.block_count,
            file_size: 0,
            min_key: self.min_key,
            max_key: self.max_key,
            index_offset,
            index_size,
            compression: self.options.compression,
        };

        table_metadata.file_size = index_offset
            + index_size
            + u64::try_from(table_metadata.len())?
            + 8 // trailing metadata length
            + FOOTER_SIZE as u64;

        let metadata_bytes = table_metadata.encode()?;
        self.writer.write_all(&encoded_index)?;
        self.writer
            .write_all(&(metadata_bytes.len() as u64).to_le_bytes())?;
        self.writer.write_all(&metadata_bytes)?;

        if let Some(filter_section) = filter_section {
            self.writer
                .write_all(&(filter_section.data.len() as u64).to_le_bytes())?;
            self.writer.write_all(&filter_section.data)?;
            self.writer
                .write_all(&filter_section.checksum.to_le_bytes())?;
        }

        // footer
        self.writer.write_all(&index_offset.to_le_bytes())?;
        self.writer.write_all(&index_size.to_le_bytes())?;
        self.writer
            .write_all(&(filter_type_repr as u8).to_le_bytes())?;
        self.writer.write_all(&VERSION.to_le_bytes())?; // version btw
        self.writer.write_all(MAGIC)?; // magic bytes

        self.writer.flush()?;

        Ok((table_metadata, statistics))
    }

    /// Return current write statistics
    fn statistics(&self) -> WriteStatistics {
        WriteStatistics {
            record_count: self.record_count,
            blocks_written: self.block_count,
            bytes_written: self.offset,
        }
    }

    fn hash_key(key: &[u8]) -> u64 {
        xxhash_rust::xxh3::xxh3_64_with_seed(key, KEY_HASH_SEED)
    }
}

#[derive(Debug, Clone, Default)]
pub struct WriteStatistics {
    pub record_count: u64,
    pub blocks_written: u32,
    pub bytes_written: u64,
}

impl std::fmt::Display for WriteStatistics {
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

        let avg_block = if self.blocks_written == 0 {
            None
        } else {
            Some(self.bytes_written / self.blocks_written as u64)
        };

        let avg_record = if self.record_count == 0 {
            None
        } else {
            Some(self.bytes_written as f64 / self.record_count as f64)
        };

        writeln!(f, "WriteStatistics {{")?;

        writeln!(f, "  Records:")?;
        writeln!(f, "    count: {}", self.record_count)?;

        writeln!(f, "  I/O:")?;
        writeln!(f, "    blocks written: {}", self.blocks_written)?;
        writeln!(
            f,
            "    bytes written:  {} ({})",
            bytes(self.bytes_written),
            self.bytes_written
        )?;

        writeln!(f, "  Averages:")?;
        match avg_block {
            Some(b) => writeln!(f, "    bytes/block:  {} ({b})", bytes(b))?,
            None => writeln!(f, "    bytes/block:  n/a")?,
        }
        match avg_record {
            Some(r) => writeln!(f, "    bytes/record: {r:.2}")?,
            None => writeln!(f, "    bytes/record: n/a")?,
        }

        write!(f, "}}")
    }
}

#[derive(Debug, Clone)]
pub struct TableMetadata {
    /// Total number of records stored in the table.
    pub record_count: u64,

    /// Number of data blocks written.
    pub block_count: u32,

    /// Total encoded SSTable size in bytes, including index and footer.
    pub file_size: u64,

    /// Smallest key in the table.
    pub min_key: Option<Vec<u8>>,

    /// Largest key in the table.
    pub max_key: Option<Vec<u8>>,

    /// Absolute file offset of the sparse index.
    pub index_offset: u64,

    /// Encoded index size in bytes.
    pub index_size: u64,

    /// Compression used
    pub compression: Compression,
}

impl TableMetadata {
    fn len(&self) -> usize {
        size_of::<u64>() // record_count
        + size_of::<u32>() // block_count
        + size_of::<u64>() // file_size
        + size_of::<u64>() // min_key length
        + self.min_key.as_ref().map_or(0, Vec::len)
        + size_of::<u64>() // max_key length
        + self.max_key.as_ref().map_or(0, Vec::len)
        + size_of::<u64>() // index_offset
        + size_of::<u64>() // index_size
    }

    fn encode(&self) -> Result<Vec<u8>, StryxError> {
        let mut out = vec![];
        out.extend((self.compression as u8).to_le_bytes());
        out.extend(self.record_count.to_le_bytes());
        out.extend(self.block_count.to_le_bytes());
        out.extend(self.file_size.to_le_bytes());

        let min_key_size = u64::try_from(self.min_key.as_ref().map(|val| val.len()).unwrap_or(0))?;
        out.extend(min_key_size.to_le_bytes());
        out.extend(self.min_key.as_deref().unwrap_or(&[]));

        let max_key_size = u64::try_from(self.max_key.as_ref().map(|val| val.len()).unwrap_or(0))?;
        out.extend(max_key_size.to_le_bytes());
        out.extend(self.max_key.as_deref().unwrap_or(&[]));

        out.extend(self.index_offset.to_le_bytes());
        out.extend(self.index_size.to_le_bytes());

        out.extend(u64::try_from(self.len())?.to_le_bytes());
        Ok(out)
    }

    pub fn decode(data: &[u8]) -> Result<TableMetadata, StryxError> {
        let mut decoder = Decoder::new(data);
        let compression = decoder.read_u8()?;
        let record_count = decoder.read_u64()?;
        let block_count = decoder.read_u32()?;
        let file_size = decoder.read_u64()?;
        let min_key_size = usize::try_from(decoder.read_u64()?)?;
        let min_key = decoder.read_bytes(min_key_size)?;
        let max_key_size = usize::try_from(decoder.read_u64()?)?;
        let max_key = decoder.read_bytes(max_key_size)?;
        let index_offset = decoder.read_u64()?;
        let index_size = decoder.read_u64()?;

        let compression = match compression {
            1 => Compression::Lz4,
            2 => Compression::Zstd,
            _ => Compression::None,
        };

        Ok(TableMetadata {
            compression,
            record_count,
            block_count,
            file_size,
            min_key: Some(min_key.to_vec()),
            max_key: Some(max_key.to_vec()),
            index_offset,
            index_size,
        })
    }
}

/// # Data Block Format
///
/// A data block contains sorted, length-prefixed records followed by an
/// offset table, record count, and CRC32C checksum.
///
/// ## Layout
///
/// ```text
/// [record 0][record 1]...[record N]
/// [offset 0: u32][offset 1: u32]...[offset N: u32]
/// [record_count: u32]
/// [checksum: u32]
/// ```
///
/// Each record is encoded as:
///
/// ```text
/// [key_len: u16][value_len: u32][key bytes][value bytes]
/// ```
///
/// ## Details
///
/// - All integers use little-endian encoding.
/// - Records are sorted lexicographically by key.
/// - Offsets are relative to the beginning of the block.
/// - Each offset points to the beginning of its corresponding record.
/// - CRC32C covers the entire block except the checksum itself.
///
/// ## Block Size
///
/// ```text
/// records_size + (record_count * 4) + 4 + 4
/// ```
///
/// The final 8 bytes contain the record count and checksum.
#[derive(Debug, Default)]
pub struct BlockBuilder {
    /// Encoded records
    data: Vec<u8>,

    /// Byte offsets of records within the block
    offsets: Vec<u32>,
}

impl BlockBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an encoded record
    pub fn add(&mut self, encoded_record: &[u8]) -> Result<(), StryxError> {
        let offset = u32::try_from(self.data.len())?;

        self.offsets.push(offset);
        self.data.extend_from_slice(encoded_record);
        Ok(())
    }

    /// Current encoded block size, including offsets, record count and checksum
    pub fn size(&self) -> usize {
        self.data.len()
            + self.offsets.len() * size_of::<u32>()
            + size_of::<u32>()
            + size_of::<u32>()
    }

    /// Whether the block contains any records
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Finalize the block into encoded bytes
    pub fn finish(&mut self) -> Result<Vec<u8>, StryxError> {
        for offset in &self.offsets {
            self.data.extend_from_slice(&offset.to_le_bytes());
        }

        let count = u32::try_from(self.offsets.len())?;
        self.data.extend_from_slice(&count.to_le_bytes());

        let checksum = crc32c::crc32c(&self.data);
        self.data.extend_from_slice(&checksum.to_le_bytes());

        Ok(self.data.clone())
    }

    /// Reset the block while retaining allocated capacity
    pub fn clear(&mut self) {
        self.data.clear();
        self.offsets.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_record(key: &[u8], value: &[u8]) -> Vec<u8> {
        let mut encoded = Vec::new();

        encoded.extend_from_slice(&(key.len() as u16).to_le_bytes());
        encoded.extend_from_slice(&(value.len() as u32).to_le_bytes());
        encoded.extend_from_slice(key);
        encoded.extend_from_slice(value);

        encoded
    }

    #[test]
    fn test_record_reader_single_record() {
        let bytes = encode_record(b"apple", b"red");
        let mut reader = RecordReader::new(&bytes);

        let record = reader.next_record().unwrap().unwrap();

        assert_eq!(record.key, b"apple");
        assert_eq!(record.value, b"red");
        assert_eq!(reader.pos(), bytes.len());

        assert!(reader.next_record().unwrap().is_none());
    }

    #[test]
    fn test_record_reader_multiple_records() {
        let mut bytes = encode_record(b"apple", b"red");
        bytes.extend(encode_record(b"banana", b"yellow"));
        bytes.extend(encode_record(b"cherry", b"red"));

        let mut reader = RecordReader::new(&bytes);

        let first = reader.next_record().unwrap().unwrap();
        assert_eq!(first.key, b"apple");
        assert_eq!(first.value, b"red");

        let second = reader.next_record().unwrap().unwrap();
        assert_eq!(second.key, b"banana");
        assert_eq!(second.value, b"yellow");

        let third = reader.next_record().unwrap().unwrap();
        assert_eq!(third.key, b"cherry");
        assert_eq!(third.value, b"red");

        assert!(reader.next_record().unwrap().is_none());
        assert_eq!(reader.pos(), bytes.len());
    }

    #[test]
    fn test_record_reader_empty_key_and_value() {
        let bytes = encode_record(b"", b"");
        let mut reader = RecordReader::new(&bytes);

        let record = reader.next_record().unwrap().unwrap();

        assert!(record.key.is_empty());
        assert!(record.value.is_empty());
        assert_eq!(reader.pos(), 6);
    }

    #[test]
    fn test_record_reader_empty_key() {
        let bytes = encode_record(b"", b"value");
        let mut reader = RecordReader::new(&bytes);

        let record = reader.next_record().unwrap().unwrap();

        assert!(record.key.is_empty());
        assert_eq!(record.value, b"value");
    }

    #[test]
    fn test_record_reader_empty_value() {
        let bytes = encode_record(b"key", b"");
        let mut reader = RecordReader::new(&bytes);

        let record = reader.next_record().unwrap().unwrap();

        assert_eq!(record.key, b"key");
        assert!(record.value.is_empty());
    }

    #[test]
    fn test_record_reader_truncated_header() {
        for len in 1..6 {
            let bytes = vec![0u8; len];
            let mut reader = RecordReader::new(&bytes);

            assert!(matches!(reader.next_record(), Err(DecodeError::Truncated)));
        }
    }

    #[test]
    fn test_record_reader_truncated_key() {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&5u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"app");

        let mut reader = RecordReader::new(&bytes);

        assert!(matches!(reader.next_record(), Err(DecodeError::Truncated)));
    }

    #[test]
    fn test_record_reader_truncated_value() {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&5u32.to_le_bytes());
        bytes.extend_from_slice(b"key");
        bytes.extend_from_slice(b"ab");

        let mut reader = RecordReader::new(&bytes);

        assert!(matches!(reader.next_record(), Err(DecodeError::Truncated)));
    }

    #[test]
    fn test_record_reader_max_key_length() {
        let key = vec![b'k'; u16::MAX as usize];
        let bytes = encode_record(&key, b"value");

        let mut reader = RecordReader::new(&bytes);
        let record = reader.next_record().unwrap().unwrap();

        assert_eq!(record.key, key);
        assert_eq!(record.value, b"value");
    }

    #[test]
    fn test_record_reader_position_after_error() {
        let mut bytes = encode_record(b"apple", b"red");

        bytes.extend_from_slice(&[0x01, 0x02]);

        let mut reader = RecordReader::new(&bytes);

        assert!(reader.next_record().unwrap().is_some());

        let previous_pos = reader.pos();

        assert!(reader.next_record().is_err());
        assert_eq!(reader.pos(), previous_pos);
    }
}
