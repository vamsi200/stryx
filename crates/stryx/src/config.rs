use crate::BLOCK_SIZE;

/// Configuration options for table creation, storage, and retrieval behavior.
#[derive(Debug, Clone)]
pub struct TableOptions {
    /// Target encoded data block size in bytes.
    pub block_size: usize,

    /// Compression algorithm used for data blocks to reduce storage.
    pub compression: Compression,

    /// Index representation strategy used for navigating the table.
    pub index_type: IndexType,

    /// Strategy for handling key ordering within the table.
    pub key_order: KeyOrder,

    /// Policy for handling duplicate keys during table construction.
    pub duplicate_policy: DuplicatePolicy,

    /// Approximate membership filter type used to accelerate point lookups.
    pub filter_type: FilterType,
}

/// Specifies the type of approximate membership query filter used for point lookups.
#[derive(Debug, Default, PartialEq, Clone)]
pub enum FilterType {
    /// Do not use any filter.
    #[default]
    None,
    /// Use a Binary Fuse 8-bit filter for space-efficient membership testing.
    BinaryFuse8,
    /// Use a Binary Fuse 16-bit filter for lower false-positive rates.
    BinaryFuse16,
    /// Use a Binary Fuse 32-bit filter for extremely low false-positive rates.
    BinaryFuse32,
}

/// Compression algorithms supported for data blocks.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// No compression applied.
    #[default]
    None,
    /// LZ4 compression for fast compression and decompression speeds.
    Lz4,
    /// Zstandard (Zstd) compression for balanced high compression ratios and speed.
    Zstd(i32),
}

// Checksum algorithms used for validating block data integrity.
#[allow(dead_code)]
#[derive(Default, Debug, Clone)]
pub(crate) enum Checksum {
    #[default]
    Crc32c,
}

/// The type of index structure used to locate data blocks within the table.
#[derive(Default, Debug, Clone)]
pub enum IndexType {
    /// Sparse index pointing to block boundaries.
    #[default]
    Sparse,
}

/// Strategy for determining or enforcing key order in the table.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrder {
    /// Require records to already be lexicographically sorted by the caller.
    #[default]
    Presorted,

    /// Automatically sort records lexicographically before writing.
    Lexicographic,
}

/// Policy for handling duplicate keys when writing records to the table.
#[derive(Default, Debug, PartialEq, Clone)]
pub enum DuplicatePolicy {
    /// Reject duplicate keys, returning an error.
    #[default]
    Reject,
    /// Allow duplicate keys to coexist within the table.
    Allow,
}

impl Default for TableOptions {
    fn default() -> Self {
        Self {
            block_size: BLOCK_SIZE,
            filter_type: FilterType::default(),
            compression: Compression::default(),
            index_type: IndexType::default(),
            key_order: KeyOrder::default(),
            duplicate_policy: DuplicatePolicy::default(),
        }
    }
}
