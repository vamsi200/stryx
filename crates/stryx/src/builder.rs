#![allow(unused)]

use core::panic;
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

use anyhow::Context;

use crate::{
    BLOCK_SIZE, FOOTER_SIZE, MAGIC, VERSION,
    config::{Compression, DuplicatePolicy, KeyOrder, TableOptions},
    decoder::Decoder,
    error::{DecodeError, StryxError},
    index::{IndexBuilder, IndexEntry},
};

#[derive(Debug, Default)]
pub struct Record<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
}

impl<'a> Record<'a> {
    pub fn len(&self) -> usize {
        self.key.len() + self.value.len()
    }
}

#[derive(Debug)]
pub struct DataBlock<'a> {
    records: &'a [Record<'a>],
    offsets: &'a [u32],
    records_count: u32,
}

#[derive(Debug)]
struct Footer {
    pub index_offset: u64,
    pub index_size: u64,
    pub version: u16,
    pub magic_bytes: [u8; 8],
}

#[derive(Debug)]
struct RecordInfo {
    key_len: u16,
    value_len: u32,
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

        let key_len = u16::from_le_bytes([remaining[0], self.value[1]]) as usize;
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

        value.write(&key_len.to_le_bytes());
        value.write(&value_len.to_le_bytes());

        value.write(&self.key);
        value.write(&self.value);

        Ok(value)
    }
}

pub trait Encode {
    fn encode_len(&self) -> usize;
    fn encode_record(&self) -> Result<Vec<u8>, DecodeError>;
}

#[derive(Debug)]
pub struct TableBuilder<W: Write> {
    /// Destination for the encoded SSTable
    writer: W,

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
}

impl TableBuilder<BufWriter<File>> {
    /// Create an SSTable at the given path
    pub fn create(path: impl AsRef<Path>, options: TableOptions) -> Result<Self, StryxError> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .read(true)
            .open(path)?;

        Ok(Self {
            writer: BufWriter::new(file),
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
        })
    }
}

impl<'a, W: std::io::Write> TableBuilder<W> {
    /// Create an SSTable using a custom writer
    pub fn new(writer: W, options: TableOptions) -> Result<Self, StryxError> {
        Ok(Self {
            writer,
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
        })
    }

    /// Append a record
    pub fn add(&mut self, key: &[u8], value: &[u8]) -> Result<(), StryxError> {
        // Validate global key ordering
        if let Some(last_key) = self.last_key.as_deref() {
            if key < last_key {
                return Err(StryxError::InvalidKeyOrder);
            }

            if key == last_key {
                return Err(StryxError::DuplicateKey);
            }
        }

        let record = Record { key, value };
        self.record_count += 1;

        if self.min_key.is_none() {
            self.min_key = Some(key.to_vec());
        }

        self.max_key = Some(key.to_vec());

        let encoded_record = record.encode_record()?;

        // Include the additional u32 offset for the incoming record
        if !self.block.is_empty()
            && self.block.size() + encoded_record.len() + size_of::<u32>() > BLOCK_SIZE
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

    // Construct the block and flush it
    fn flush_block(&mut self) -> Result<(), StryxError> {
        if self.block.is_empty() {
            return Ok(());
        }

        let encoded_block = match self.options.compression {
            Compression::None => self.block.finish()?,
            Compression::Zstd => zstd::encode_all(self.block.finish()?.as_slice(), 1)?,
            Compression::Lz4 => {
                panic!();
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
    pub fn finalize(mut self) -> Result<TableMetadata, StryxError> {
        self.flush_block()?;

        // Encode sparse index
        let encoded_index = self.index.finish()?;

        // index block
        let index_offset = self.offset;
        let index_size = u64::try_from(encoded_index.len())?;
        let file_size = index_offset + index_size + 28;

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
        + index_size + u64::try_from(table_metadata.len())?
        + 8 // trailing metadata length
        + FOOTER_SIZE as u64;

        self.writer.write_all(&encoded_index)?;
        self.writer.write_all(&table_metadata.encode()?)?;

        // footer
        self.writer.write_all(&index_offset.to_le_bytes())?;
        self.writer.write_all(&index_size.to_le_bytes())?;
        self.writer.write_all(&VERSION.to_le_bytes())?; // version btw
        self.writer.write_all(MAGIC)?; // magic bytes

        self.writer.flush()?;

        Ok(table_metadata)
    }

    /// Return current write statistics
    pub fn statistics(&self) -> WriteStatistics {
        WriteStatistics {
            record_count: self.record_count,
            blocks_written: self.block_count,
            bytes_written: self.offset,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriteStatistics {
    pub record_count: u64,
    pub blocks_written: u32,
    pub bytes_written: u64,
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
        let min_key_size = decoder.read_u64()? as usize;
        let min_key = decoder.read_bytes(min_key_size)?;
        let max_key_size = decoder.read_u64()? as usize;
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

    /// Checksum for record
    checksum: u32,
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
