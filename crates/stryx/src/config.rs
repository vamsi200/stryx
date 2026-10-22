//TableConfig or Options whichever

use crate::error::StryxError;

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

pub enum Compression {
    None,
    Lz4,
    Zstd,
}

pub enum Checksum {
    None,
    Crc32c,
}

pub enum IndexType {
    Sparse,
}

pub enum KeyOrder {
    Lexicographic,
}

pub enum DuplicatePolicy {
    Reject,
}

impl Default for TableOptions {
    fn default() -> Self {
        todo!()
    }
}

impl TableOptions {
    /// Validate configuration before constructing an SSTable
    pub fn validate(&self) -> Result<(), StryxError> {
        todo!()
    }
}
