#![allow(unused)]
use std::{
    collections::btree_map::Values,
    fs::File,
    io::{Read, Seek},
    path::Path,
    u8, vec,
};

use crate::{
    FOOTER_SIZE, MAGIC, UNITS, VERSION,
    builder::{Record, RecordReader, TableMetadata},
    config::Compression,
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
            if records.len() == temp {
                break;
            }
        }

        if records.len() != temp {
            return Err(DecodeError::InvalidLength.into());
        }

        // Ensure record reader consumed exactly the record region.
        // No trailing or unaccounted bytes are allowed before the offsets section.
        if record_reader.pos() != buf_len - 8 - (temp * size_of::<u32>()) {
            return Err(DecodeError::InvalidLength.into());
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
    metadata: TableMetadata,
    readstats: ReadStats,
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

        // metadata starts right afer encoded_index, so offset is just:
        let metadata_offset = index_offset + index_size;
        let file_size = file.metadata()?.len();

        // (total file size - FOOTER_SIZE) gives end of metadata's position
        // metadata size = Total File Size - FOOTER_SIZE - metadata_offset
        let metadata_size = file_size
            .checked_sub(metadata_offset + FOOTER_SIZE as u64)
            .ok_or(StryxError::InvalidRange)?;

        file.seek(std::io::SeekFrom::Start(metadata_offset))?;

        let metadata_len = usize::try_from(metadata_size)?;
        let mut metadata_buf = vec![0u8; metadata_len];
        file.read_exact(&mut metadata_buf)?;

        let metadata = TableMetadata::decode(&metadata_buf)?;

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

        Ok(TableReader {
            file,
            index: index_entries,
            metadata,
            readstats: ReadStats::default(),
        })
    }

    /// Find the value associated with a key.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StryxError> {
        self.readstats.point_lookups += 1;

        // find the first index entry whose first_key is greater than requested
        // key, the preceeding block could have key
        let block = self
            .index
            .partition_point(|entry| entry.first_key.as_slice() <= key);

        if block == 0 {
            self.readstats.point_misses += 1;
            return Ok(None);
        }

        // since returned block's first_key is greater than the requested key,
        // the requested key `could be` in previous one
        let (offset, size) = {
            let entry = &self.index[block - 1];
            (entry.offset, entry.size)
        };

        let mut block_buf = vec![0u8; size as usize];

        self.file.seek(std::io::SeekFrom::Start(offset))?;
        self.file.read_exact(&mut block_buf)?;

        self.readstats.blocks_read += 1;
        self.readstats.bytes_read += u64::from(size);

        let (block_buf, decompressed) = decompress_block(self.metadata.compression, block_buf)?;

        if decompressed {
            self.readstats.blocks_decompressed += 1;
            self.readstats.bytes_decompressed += block_buf.len() as u64;
        }

        let reader = match BlockReader::new(&block_buf) {
            Ok(reader) => reader,
            Err(err) => {
                if matches!(&err, StryxError::InvalidChecksum) {
                    self.readstats.checksum_failures += 1;
                }

                return Err(err);
            }
        };

        let result = reader.get(key).map(|x| x.0.to_vec());

        if result.is_some() {
            self.readstats.point_hits += 1;
        } else {
            self.readstats.point_misses += 1;
        }

        Ok(result)
    }

    /// Check whether a key exists.
    pub fn contains_key(&mut self, key: &[u8]) -> Result<bool, StryxError> {
        Ok(self.get(key)?.is_some())
    }

    /// Batch point lookups.
    pub fn get_many(&mut self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StryxError> {
        let mut out = Vec::with_capacity(keys.len());

        for key in keys {
            out.push(self.get(key)?);
        }

        Ok(out)
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
            .index
            .partition_point(|entry| entry.first_key.as_slice() <= start);

        let block_idx = block.saturating_sub(1);
        let compression = self.metadata.compression;

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
        let compression = self.metadata.compression;

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
        &self.metadata
    }

    /// Return runtime reader statistics.
    pub fn stats(&self) -> ReadStats {
        self.readstats
    }
}

/// Decompress a data block.
/// Returns the decoded bytes and whether decompression was performed.
fn decompress_block(
    compression: Compression,
    block_buf: Vec<u8>,
) -> Result<(Vec<u8>, bool), StryxError> {
    match compression {
        Compression::None => Ok((block_buf, false)),
        Compression::Zstd => Ok((zstd::decode_all(block_buf.as_slice())?, true)),
        Compression::Lz4 => {
            let mut decoder = lz4::Decoder::new(block_buf.as_slice())?;

            let mut output = Vec::new();
            decoder.read_to_end(&mut output)?;

            let (_, result) = decoder.finish();
            result?;

            Ok((output, true))
        }
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
        let compression = reader.metadata.compression;

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

            if self.block_idx >= self.reader.index.len() {
                self.finished = true;
                return None;
            }

            let (offset, size) = {
                let entry = &self.reader.index[self.block_idx];
                (entry.offset, entry.size)
            };

            self.block_idx += 1;

            let mut block_buf = vec![0u8; size as usize];

            let result = (|| -> Result<Vec<OwnedRecord>, StryxError> {
                self.reader.file.seek(std::io::SeekFrom::Start(offset))?;

                self.reader.file.read_exact(&mut block_buf)?;

                self.reader.readstats.blocks_read += 1;
                self.reader.readstats.bytes_read += u64::from(size);

                let (block_buf, decompressed) = decompress_block(self.compression, block_buf)?;

                if decompressed {
                    self.reader.readstats.blocks_decompressed += 1;
                    self.reader.readstats.bytes_decompressed += block_buf.len() as u64;
                }

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
    use anyhow::Result;

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
    fn test_block_reader_single_record() -> Result<()> {
        let block = make_block(&[(b"apple", b"red")]);

        let reader = BlockReader::new(&block)?;

        assert_eq!(reader.record_count, 1);
        assert_eq!(reader.records.len(), 1);
        assert_eq!(reader.offsets.len(), 1);

        assert_eq!(reader.records[0].key, b"apple");
        assert_eq!(reader.records[0].value, b"red");

        Ok(())
    }

    #[test]
    fn test_block_reader_multiple_records() -> Result<()> {
        let block = make_block(&[
            (b"apple", b"red"),
            (b"banana", b"yellow"),
            (b"cherry", b"red"),
        ]);

        let reader = BlockReader::new(&block)?;

        assert_eq!(reader.record_count, 3);
        assert_eq!(reader.records.len(), 3);
        assert_eq!(reader.offsets.len(), 3);

        assert_eq!(reader.records[0].key, b"apple");
        assert_eq!(reader.records[1].key, b"banana");
        assert_eq!(reader.records[2].key, b"cherry");

        Ok(())
    }

    #[test]
    fn test_block_reader_empty_block() {
        let block = make_block(&[]);

        assert!(BlockReader::new(&block).is_err());
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

        assert!(BlockReader::new(&block).is_err());
    }
}
