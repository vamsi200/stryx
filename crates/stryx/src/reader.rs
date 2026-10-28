#![allow(unused)]
use std::{
    collections::btree_map::Values,
    fs::File,
    io::{Read, Seek},
    path::Path,
    u8, vec,
};

use crate::{
    FOOTER_SIZE, MAGIC, VERSION,
    builder::{DataBlock, Record, RecordReader, TableMetadata},
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

    fn get(&self, key: &[u8]) -> Option<(&[u8], u32)> {
        self.records
            .iter()
            .enumerate()
            .find(|(_, record)| record.key == key)
            .map(|(idx, record)| (record.value, self.offsets[idx]))
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
        Ok(reader.get(key).and_then(|x| Some(x.0.to_vec())))
    }

    /// Check whether a key exists
    pub fn contains_key(&mut self, key: &[u8]) -> Result<bool, StryxError> {
        if self.get(key)?.is_some() {
            return Ok(true);
        }

        Ok(false)
    }

    /// Batch point lookups
    pub fn get_many(&mut self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StryxError> {
        let mut out = vec![];
        for key in keys {
            out.push(self.get(*key)?);
        }

        Ok(out)
    }

    /// Iterate over all records in sorted order
    pub fn iter(&mut self) -> impl Iterator<Item = Result<OwnedRecord, StryxError>> + '_ {
        TableIterator::new(self)
    }

    /// Iterate over a bounded key range
    pub fn range(&mut self, start: &[u8], end: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        if start >= end {
            return Err(StryxError::InvalidRange);
        }

        let block = self
            .index
            .partition_point(|entry| entry.first_key.as_slice() <= start);

        let block_idx = block.saturating_sub(1);

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

    /// Iterate over keys sharing a prefix
    pub fn prefix(&mut self, prefix: &[u8]) -> Result<TableIterator<'_>, StryxError> {
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
        })
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

pub struct ReadStats {
    // TODO
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

                // return if end_key is some and is found or return early if key doesn't start with
                // prefix..
                if self
                    .end_key
                    .as_ref()
                    .is_some_and(|end| record.key.as_slice() >= end.as_slice())
                    || self
                        .prefix
                        .as_ref()
                        .is_some_and(|prefix| !record.key.starts_with(prefix))
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

            if self.block_idx >= self.reader.index.len() {
                self.finished = true;
                return None;
            }

            let entry = &self.reader.index[self.block_idx];
            self.block_idx += 1;

            let mut block_buf = vec![0u8; entry.size as usize];

            let result = (|| -> Result<Vec<OwnedRecord>, StryxError> {
                self.reader
                    .file
                    .seek(std::io::SeekFrom::Start(entry.offset))?;
                self.reader.file.read_exact(&mut block_buf)?;

                let block_reader = BlockReader::new(&block_buf)?;

                let start_pos = self
                    .start_key
                    .as_deref()
                    .or(self.prefix.as_deref())
                    .map(|key| {
                        block_reader
                            .records
                            .partition_point(|record| record.key < key)
                    })
                    .unwrap_or(0);

                let records_slice = &block_reader.records[start_pos..];

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
                    self.finished = true;
                    return Some(Err(err));
                }
            }
        }
    }
}
