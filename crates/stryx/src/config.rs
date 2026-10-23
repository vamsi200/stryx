//TableConfig or Options whichever

use crate::error::StryxError;

pub const BLOCK_SIZE: usize = 16 * 1024;

#[derive(Debug)]
pub struct TableOptions {
    /// Target encoded data block size in bytes
    pub block_size: usize,

    /// Compression algorithm for data blocks
    pub compression: Compression,

    /// Checksum algorithm for block integrity
    pub checksum: Checksum,

    /// Index representation
    pub index_type: IndexType,

    /// Key ordering strategy
    pub key_order: KeyOrder,

    /// Duplicate key handling
    pub duplicate_policy: DuplicatePolicy,

    /// Bloom filter bits allocated per key
    pub bloom_filter_bits_per_key: Option<usize>,

    /// Number of records between prefix-compression restart points
    pub restart_interval: usize,
}

#[derive(Default, Debug)]
pub enum Compression {
    None,
    Lz4,
    #[default]
    Zstd,
}

#[derive(Default, Debug)]
pub enum Checksum {
    None,
    #[default]
    Crc32c,
}

#[derive(Default, Debug)]
pub enum IndexType {
    #[default]
    Sparse,
}

#[derive(Default, Debug, PartialEq)]
pub enum KeyOrder {
    #[default]
    Lexicographic,
}

#[derive(Default, Debug, PartialEq)]
pub enum DuplicatePolicy {
    #[default]
    Reject,
    Allow,
}

impl Default for TableOptions {
    fn default() -> Self {
        Self {
            block_size: BLOCK_SIZE,
            bloom_filter_bits_per_key: None,
            compression: Compression::default(),
            checksum: Checksum::default(),
            index_type: IndexType::default(),
            key_order: KeyOrder::default(),
            duplicate_policy: DuplicatePolicy::default(),
            restart_interval: 0,
        }
    }
}

impl TableOptions {
    /// Validate configuration before constructing an SSTable
    pub fn validate(&self) -> Result<(), StryxError> {
        todo!()
    }
}
