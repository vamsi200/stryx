//TableConfig or Options whichever
use crate::BLOCK_SIZE;

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

    /// Filter type
    pub filter_type: FilterType,

    /// Number of records between prefix-compression restart points
    pub restart_interval: usize,
}

#[derive(Debug, Default)]
pub enum FilterType {
    #[default]
    None,
    BinaryFuse8,
    BinaryFuse16,
    BinaryFuse32,
}

#[repr(u8)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    #[default]
    None = 0,
    Lz4 = 1,
    Zstd = 2,
}

#[derive(Default, Debug)]
pub enum Checksum {
    #[default]
    Crc32c,
}

#[derive(Default, Debug)]
pub enum IndexType {
    #[default]
    Sparse,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrder {
    /// Require records to already be lexicographically sorted.
    #[default]
    Presorted,

    /// Sort records lexicographically before writing.
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
            filter_type: FilterType::None,
            compression: Compression::default(),
            checksum: Checksum::default(),
            index_type: IndexType::default(),
            key_order: KeyOrder::default(),
            duplicate_policy: DuplicatePolicy::default(),
            restart_interval: 0,
        }
    }
}
