use crate::{decoder::Decoder, error::StryxError};
use std::cmp::Ordering;

#[derive(Debug)]
pub(crate) struct IndexBuilder {
    index: Index,
}

impl IndexBuilder {
    pub(crate) fn new() -> Self {
        Self {
            index: Index {
                key_data: Vec::new(),
                entries: Vec::new(),
                key_prefixes: Vec::new(),
                lcp_len: 0,
            },
        }
    }

    pub(crate) fn add(
        &mut self,
        first_key: &[u8],
        offset: u64,
        size: u32,
        uncompressed_block_size: u32,
    ) {
        let key_offset = self.index.key_data.len() as u32;
        let key_len = first_key.len() as u16;

        let mut prefix = [0u8; 8];
        let copied = first_key.len().min(8);
        prefix[..copied].copy_from_slice(&first_key[..copied]);

        self.index.key_data.extend_from_slice(first_key);
        self.index.key_prefixes.push(u64::from_be_bytes(prefix));

        self.index.entries.push(IndexEntry {
            key_offset,
            key_len,
            offset,
            size,
            uncompressed_block_size,
        });
    }

    pub(crate) fn finish(self) -> Result<Vec<u8>, StryxError> {
        let mut encoded = Vec::new();

        self.index.encode(&mut encoded)?;

        let count = u32::try_from(self.index.entries.len())?;
        encoded.extend_from_slice(&count.to_le_bytes());

        let checksum = crc32c::crc32c(&encoded);
        encoded.extend_from_slice(&checksum.to_le_bytes());

        Ok(encoded)
    }
}

#[derive(Debug)]
pub(crate) struct Index {
    /// Concatenated first keys from all index entries.
    pub key_data: Vec<u8>,

    /// Metadata for each index entry.
    pub entries: Vec<IndexEntry>,

    /// Big-endian first-eight-byte prefixes, one per entry.
    pub key_prefixes: Vec<u64>,

    /// Length of the prefix shared by every first key (first and last key bound the rest).
    pub lcp_len: usize,
}

#[inline]
fn prefix_be(b: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    let n = b.len().min(8);
    buf[..n].copy_from_slice(&b[..n]);
    u64::from_be_bytes(buf)
}

impl Index {
    pub(crate) fn build_prefixes(&mut self) {
        let n = self.entries.len();
        self.lcp_len = if n == 0 {
            0
        } else {
            let (a, b) = (self.first_key(0), self.first_key(n - 1));
            a.iter().zip(b).take_while(|(x, y)| x == y).count()
        };
        let l = self.lcp_len;
        self.key_prefixes = (0..n).map(|i| prefix_be(&self.first_key(i)[l..])).collect();
    }

    pub(crate) fn encode(&self, dst: &mut Vec<u8>) -> Result<(), StryxError> {
        for (i, entry) in self.entries.iter().enumerate() {
            let first_key = self.first_key(i);
            let key_len = u16::try_from(first_key.len())?;

            dst.extend_from_slice(&key_len.to_le_bytes());
            dst.extend_from_slice(first_key);
            dst.extend_from_slice(&entry.offset.to_le_bytes());
            dst.extend_from_slice(&entry.size.to_le_bytes());
            dst.extend_from_slice(&entry.uncompressed_block_size.to_le_bytes());
        }

        Ok(())
    }

    pub(crate) fn decode(
        decoder: &mut Decoder<'_>,
        entry_count: usize,
    ) -> Result<Self, StryxError> {
        let mut index = Self {
            key_data: Vec::new(),
            entries: Vec::with_capacity(entry_count),
            key_prefixes: Vec::with_capacity(entry_count),
            lcp_len: 0,
        };

        for _ in 0..entry_count {
            let key_len = decoder.read_u16()?;
            let first_key = decoder.read_bytes(key_len as usize)?;

            let key_offset = u32::try_from(index.key_data.len())?;

            let mut prefix = [0u8; 8];
            let copied = first_key.len().min(8);
            prefix[..copied].copy_from_slice(&first_key[..copied]);

            index.key_data.extend_from_slice(first_key);
            index.key_prefixes.push(u64::from_be_bytes(prefix));

            index.entries.push(IndexEntry {
                key_offset,
                key_len,
                offset: decoder.read_u64()?,
                size: decoder.read_u32()?,
                uncompressed_block_size: decoder.read_u32()?,
            });
        }

        Ok(index)
    }

    pub(crate) fn first_key(&self, index: usize) -> &[u8] {
        let entry = &self.entries[index];
        let start = entry.key_offset as usize;
        let end = start + entry.key_len as usize;
        &self.key_data[start..end]
    }

    pub(crate) fn partition_point_le_from(&self, start: usize, key: &[u8]) -> usize {
        let n = self.entries.len();
        if start >= n {
            return n;
        }
        let l = self.lcp_len;
        let lcp = &self.first_key(0)[..l];
        let m = key.len().min(l);
        match key[..m].cmp(&lcp[..m]) {
            Ordering::Less => return start, // key < every first key
            Ordering::Greater => return n,  // key > every first key
            Ordering::Equal if key.len() < l => return start, // proper prefix of lcp: smaller than all
            Ordering::Equal => {}
        }

        let kp = prefix_be(&key[l..]);
        let pre = &self.key_prefixes;

        // smaller prefix => strictly smaller key; equal prefix => needs a real compare
        let lo = start + pre[start..].partition_point(|&p| p < kp);
        let hi = lo + pre[lo..].partition_point(|&p| p == kp);

        let (mut a, mut b) = (lo, hi);
        while a < b {
            let mid = a + (b - a) / 2;
            if self.first_key(mid) <= key {
                a = mid + 1;
            } else {
                b = mid;
            }
        }
        a // larger prefix => strictly larger key, so everything from `hi` on stays false
    }

    #[inline]
    pub(crate) fn partition_point_le(&self, key: &[u8]) -> usize {
        self.partition_point_le_from(0, key)
    }

    #[inline]
    pub(crate) fn partition_point_lt(&self, key: &[u8]) -> usize {
        let (mut a, mut b) = (0, self.entries.len());
        while a < b {
            let mid = a + (b - a) / 2;
            if self.first_key(mid) < key {
                a = mid + 1;
            } else {
                b = mid;
            }
        }
        a
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
    /// byte offset of the key
    pub key_offset: u32,

    /// Key len
    pub key_len: u16,

    /// Absolute byte offset of the block in the SSTable file
    pub offset: u64,

    /// Encoded size of the block in bytes
    pub size: u32,

    /// Uncompressed block size ini bytes
    pub uncompressed_block_size: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_roundtrip() -> Result<(), StryxError> {
        let mut builder = IndexBuilder::new();
        builder.add(b"apple", 1024, 4096, 8096);
        builder.add(b"banana", 5120, 2048, 4096);

        let encoded = builder.finish()?;

        // last 4 bytes are the checksum, preceding 4 are the count
        let body_len = encoded.len() - 4 - 4;
        let count =
            u32::from_le_bytes(encoded[body_len..body_len + 4].try_into().unwrap()) as usize;
        assert_eq!(count, 2);

        let mut decoder = Decoder::new(&encoded);
        let index = Index::decode(&mut decoder, count)?;

        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.first_key(0), b"apple");
        assert_eq!(index.first_key(1), b"banana");
        assert_eq!(index.entries[0].offset, 1024);
        assert_eq!(index.entries[0].size, 4096);
        assert_eq!(index.entries[0].uncompressed_block_size, 8096);
        assert_eq!(index.entries[1].offset, 5120);

        let mut expected_prefix = [0u8; 8];
        expected_prefix[..5].copy_from_slice(b"apple");
        assert_eq!(index.key_prefixes[0], u64::from_be_bytes(expected_prefix));

        let mut expected_prefix = [0u8; 8];
        expected_prefix[..6].copy_from_slice(b"banana");
        assert_eq!(index.key_prefixes[1], u64::from_be_bytes(expected_prefix));

        Ok(())
    }

    #[test]
    fn test_index_entry_empty_key() -> Result<(), StryxError> {
        let mut builder = IndexBuilder::new();
        builder.add(b"", 0, 0, 0);

        let encoded = builder.finish()?;

        let body_len = encoded.len() - 4 - 4;
        let count =
            u32::from_le_bytes(encoded[body_len..body_len + 4].try_into().unwrap()) as usize;

        let mut decoder = Decoder::new(&encoded);
        let index = Index::decode(&mut decoder, count)?;

        assert!(index.first_key(0).is_empty());
        assert_eq!(index.entries[0].offset, 0);
        assert_eq!(index.entries[0].size, 0);
        assert_eq!(index.key_prefixes[0], 0);

        Ok(())
    }

    #[test]
    fn test_index_entry_max_key_length() -> Result<(), StryxError> {
        let long_key = vec![b'v'; u16::MAX as usize];

        let mut builder = IndexBuilder::new();
        builder.add(&long_key, u64::MAX, u32::MAX, u32::MAX);

        let encoded = builder.finish()?;

        let body_len = encoded.len() - 4 - 4;
        let count =
            u32::from_le_bytes(encoded[body_len..body_len + 4].try_into().unwrap()) as usize;

        let mut decoder = Decoder::new(&encoded);
        let index = Index::decode(&mut decoder, count)?;

        assert_eq!(index.first_key(0), long_key.as_slice());
        assert_eq!(index.entries[0].offset, u64::MAX);
        assert_eq!(index.entries[0].size, u32::MAX);
        assert_eq!(index.entries[0].uncompressed_block_size, u32::MAX);

        Ok(())
    }

    #[test]
    fn test_index_entry_truncated_key_length() {
        let bytes = [0x01];
        let mut decoder = Decoder::new(&bytes);

        assert!(Index::decode(&mut decoder, 1).is_err());
    }

    #[test]
    fn test_index_entry_truncated_key() {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&5u16.to_le_bytes());
        bytes.extend_from_slice(b"msi");

        let mut decoder = Decoder::new(&bytes);

        assert!(Index::decode(&mut decoder, 1).is_err());
    }

    #[test]
    fn test_index_entry_truncated_offset() {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 7]);

        let mut decoder = Decoder::new(&bytes);

        assert!(Index::decode(&mut decoder, 1).is_err());
    }

    #[test]
    fn test_index_entry_truncated_size() {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 3]);

        let mut decoder = Decoder::new(&bytes);

        assert!(Index::decode(&mut decoder, 1).is_err());
    }
}
