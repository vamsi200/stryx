#![allow(unused)]

use crate::{
    decoder::Decoder,
    error::{DecodeError, StryxError},
};

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

    pub fn add(&mut self, first_key: &[u8], offset: u64, size: u32, uncompressed_block_size: u32) {
        self.entries.push(IndexEntry {
            first_key: first_key.to_vec(),
            offset,
            size,
            uncompressed_block_size,
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

    /// Uncompressed block size ini bytes
    pub uncompressed_block_size: u32,
}

impl IndexEntry {
    pub fn encode(&self, dst: &mut Vec<u8>) -> Result<(), StryxError> {
        let key_len = u16::try_from(self.first_key.len())?;

        dst.extend_from_slice(&key_len.to_le_bytes());
        dst.extend_from_slice(&self.first_key);
        dst.extend_from_slice(&self.offset.to_le_bytes());
        dst.extend_from_slice(&self.size.to_le_bytes());
        dst.extend_from_slice(&self.uncompressed_block_size.to_le_bytes());

        Ok(())
    }

    pub fn decode<'a>(decoder: &mut Decoder<'a>) -> Result<Self, StryxError> {
        let key_len = decoder.read_u16()?;
        let first_key = decoder.read_bytes(key_len as usize)?;
        let block_offset = decoder.read_u64()?;
        let block_size = decoder.read_u32()?;
        let uncompressed_block_size = decoder.read_u32()?;

        Ok(Self {
            first_key: first_key.to_vec(),
            offset: block_offset,
            size: block_size,
            uncompressed_block_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_entry() -> Result<(), StryxError> {
        let entry = IndexEntry {
            first_key: b"apple".to_vec(),
            offset: 1024,
            size: 4096,
            uncompressed_block_size: 8096,
        };

        let mut encoded = Vec::new();
        entry.encode(&mut encoded)?;

        let mut decoder = Decoder::new(&encoded);
        let decoded = IndexEntry::decode(&mut decoder)?;

        assert_eq!(decoded.first_key, entry.first_key);
        assert_eq!(decoded.offset, entry.offset);
        assert_eq!(decoded.size, entry.size);
        assert_eq!(decoder.remaining(), 0);

        Ok(())
    }

    #[test]
    fn test_index_entry_empty_key() -> Result<(), StryxError> {
        let entry = IndexEntry {
            first_key: vec![],
            offset: 0,
            size: 0,
            uncompressed_block_size: 0,
        };

        let mut encoded = Vec::new();
        entry.encode(&mut encoded)?;

        let mut decoder = Decoder::new(&encoded);
        let decoded = IndexEntry::decode(&mut decoder)?;

        assert!(decoded.first_key.is_empty());
        assert_eq!(decoded.offset, 0);
        assert_eq!(decoded.size, 0);

        Ok(())
    }

    #[test]
    fn test_index_entry_max_key_length() -> Result<(), StryxError> {
        let entry = IndexEntry {
            first_key: vec![b'v'; u16::MAX as usize],
            offset: u64::MAX,
            size: u32::MAX,
            uncompressed_block_size: u32::MAX,
        };

        let mut encoded = Vec::new();
        entry.encode(&mut encoded)?;

        let mut decoder = Decoder::new(&encoded);
        let decoded = IndexEntry::decode(&mut decoder)?;

        assert_eq!(decoded.first_key, entry.first_key);
        assert_eq!(decoded.offset, u64::MAX);
        assert_eq!(decoded.size, u32::MAX);

        Ok(())
    }

    #[test]
    fn test_index_entry_key_too_large() {
        let entry = IndexEntry {
            first_key: vec![b'a'; u16::MAX as usize + 1],
            offset: 0,
            size: 0,
            uncompressed_block_size: 0,
        };

        let mut encoded = Vec::new();

        assert!(entry.encode(&mut encoded).is_err());
        assert!(encoded.is_empty());
    }

    #[test]
    fn test_index_entry_truncated_key_length() {
        let bytes = [0x01];
        let mut decoder = Decoder::new(&bytes);

        assert!(IndexEntry::decode(&mut decoder).is_err());
    }

    #[test]
    fn test_index_entry_truncated_key() {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&5u16.to_le_bytes());
        bytes.extend_from_slice(b"msi");

        let mut decoder = Decoder::new(&bytes);

        assert!(IndexEntry::decode(&mut decoder).is_err());
    }

    #[test]
    fn test_index_entry_truncated_offset() {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 7]);

        let mut decoder = Decoder::new(&bytes);

        assert!(IndexEntry::decode(&mut decoder).is_err());
    }

    #[test]
    fn test_index_entry_truncated_size() {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 3]);

        let mut decoder = Decoder::new(&bytes);

        assert!(IndexEntry::decode(&mut decoder).is_err());
    }
}
