#![allow(unused)]

use crate::error::StryxError;

#[derive(Debug)]
pub(crate) struct IndexBuilder {
    entries: Vec<IndexEntry>,
}

impl IndexBuilder {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn add(&mut self, first_key: &[u8], offset: u64, size: u32) {
        self.entries.push(IndexEntry {
            first_key: first_key.to_vec(),
            offset,
            size,
        });
    }

    pub fn finish(self) -> Result<Vec<u8>, StryxError> {
        let mut encoded = Vec::new();

        for entry in &self.entries {
            entry.encode(&mut encoded)?;
        }

        let count = u32::try_from(self.entries.len())?;
        encoded.extend_from_slice(&count.to_le_bytes());

        let checksum = crc32c::crc32c(&encoded);
        encoded.extend_from_slice(&checksum.to_le_bytes());

        Ok(encoded)
    }
}

/// Sparse index entry identifying a data block within the SSTable.
/// # Format (in order):
/// - `key_len`: Length of the first key in bytes.
/// - `first_key`: First key in the corresponding data block.
/// - `offset`: Absolute byte offset of the block in the SSTable file.
/// - `size`: Total encoded size of the block in bytes.
///
/// Total encoded size: `2 + key_len + 8 + 4` bytes.
#[derive(Debug)]
pub struct IndexEntry {
    /// First key in the corresponding data block
    pub first_key: Vec<u8>,

    /// Absolute byte offset of the block in the SSTable file
    pub offset: u64,

    /// Encoded size of the block in bytes
    pub size: u32,
}

impl IndexEntry {
    pub fn encode(&self, dst: &mut Vec<u8>) -> Result<(), StryxError> {
        let key_len = u16::try_from(self.first_key.len())?;

        dst.extend_from_slice(&key_len.to_le_bytes());
        dst.extend_from_slice(&self.first_key);
        dst.extend_from_slice(&self.offset.to_le_bytes());
        dst.extend_from_slice(&self.size.to_le_bytes());

        Ok(())
    }
}
