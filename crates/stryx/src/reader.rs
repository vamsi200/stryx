#![allow(unused)]
use std::{
    fs::File,
    io::{Read, Seek},
    path::Path,
    u8,
};

use crate::{
    FOOTER_SIZE, MAGIC, VERSION,
    builder::{DataBlock, Record, RecordReader},
    decoder::Decoder,
    error::{DecodeError, StryxError},
    index::IndexEntry,
};

#[derive(Debug)]
pub(crate) struct BlockReader<'a> {
    records: Vec<Record<'a>>,
    offsets: Vec<u32>,
    record_count: u32,
    checksum: u32,
}

impl<'a> BlockReader<'a> {
    fn new(buf: &'a [u8]) -> Result<Self, StryxError> {
        if buf.len() < 8 {
            return Err(DecodeError::InvalidLength.into());
        }

        let buf_len = buf.len();

        let stored_checksum = u32::from_le_bytes(
            buf[buf_len - 4..buf_len]
                .try_into()
                .map_err(DecodeError::from)?,
        );

        let record_count = u32::from_le_bytes(
            buf[buf_len - 8..buf_len - 4]
                .try_into()
                .map_err(DecodeError::from)?,
        );

        let mut records = vec![];
        let mut record_reader = RecordReader::new(&buf);

        let temp = record_count as usize;

        while let Some(record) = record_reader.next_record()? {
            records.push(record);
            if records.len() >= temp {
                break;
            }
        }

        let mut offsets = vec![];

        let mut decoder = Decoder::new(&buf);
        decoder.pos += record_reader.pos();

        for _ in 0..record_count {
            offsets.push(decoder.read_u32()?);
        }

        Ok(Self {
            records,
            offsets,
            record_count,
            checksum: stored_checksum,
        })
    }

    fn get(&self, key: &'a [u8]) -> Option<&'a [u8]> {
        self.records
            .iter()
            .find(|x| x.key == key)
            .and_then(|r| Some(r.value))
    }
}

#[derive(Debug)]
pub struct TableReader {
    file: File,
    index: Vec<IndexEntry>,
}

impl TableReader {
    /// Open and validate an SSTable
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StryxError> {
        let mut file = File::open(path)?;
        let mut footer = [0u8; FOOTER_SIZE];

        file.seek(std::io::SeekFrom::End(-(FOOTER_SIZE as i64)))?;
        file.read_exact(&mut footer)?;

        if &footer[20..28] != MAGIC {
            return Err(StryxError::InvalidMagic);
        }

        let mut decoder = Decoder::new(&footer);

        let index_offset = decoder.read_u64()?;
        let index_size = decoder.read_u64()?;
        let version = decoder.read_u32()?;

        if version != VERSION {
            return Err(StryxError::InvalidVersion);
        }

        let index_len = usize::try_from(index_size)?;
        let mut index_buf = vec![0u8; index_len];
        file.seek(std::io::SeekFrom::Start(index_offset))?;
        file.read_exact(&mut index_buf)?;

        if index_buf.len() < 8 {
            return Err(DecodeError::Truncated.into());
        }

        // Last 8 bytes contain count and checksum
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
            println!("Remaining: {}", decoder.remaining());
            return Err(DecodeError::InvalidLength.into());
        }

        Ok(TableReader {
            file,
            index: index_entries,
        })
    }

    /// Open with configurable read behavior
    pub fn open_with_options(path: &str, config: ReaderOptions) -> Result<Self, StryxError> {
        todo!()
    }

    /// Find the value associated with a key
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StryxError> {
        // find the first index entry whose first_key is greater than requested
        // key, the preceeding block could have key
        let block = self
            .index
            .partition_point(|entry| entry.first_key.as_slice() <= key);

        if block == 0 {
            return Ok(None);
        }

        // since returned block's first_key is greater than the requested key,
        // the requested key `could be` in previous one
        let entry = &self.index[block - 1];

        let mut block_buf = vec![0u8; entry.size as usize];
        self.file.seek(std::io::SeekFrom::Start(entry.offset))?;
        self.file.read_exact(&mut block_buf)?;

        let reader = BlockReader::new(&block_buf)?;
        Ok(reader.get(key).and_then(|x| Some(x.to_vec())))
    }

    /// Check whether a key exists
    pub fn contains_key(&self, key: &[u8]) -> Result<bool, StryxError> {
        todo!()
    }

    /// Batch point lookups
    pub fn get_many(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StryxError> {
        todo!()
    }

    /// Iterate over all records in sorted order
    pub fn iter(&self) -> Result<TableIterator<'_>, StryxError> {
        todo!()
    }

    /// Iterate over a bounded key range
    pub fn range(&self, start: &[u8], end: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        todo!()
    }

    /// Iterate over keys sharing a prefix
    pub fn prefix(&self, prefix: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        todo!()
    }

    /// Return table metadata
    pub fn metadata(&self) -> &TableMetadata {
        todo!()
    }

    /// Return runtime reader statistics
    pub fn stats(&self) -> ReadStats {
        todo!()
    }
}

pub struct ReaderOptions {
    // TODO
}

pub struct TableMetadata {
    // TODO
}

pub struct ReadStats {
    // TODO
}

pub struct TableIterator<'a> {
    _marker: std::marker::PhantomData<&'a TableReader>,
    // TODO
}
