use std::{array::TryFromSliceError, num::TryFromIntError};

/// Errors that can occur during the decoding or deserialization of database records and filters.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The record data was abruptly truncated before decoding could finish.
    #[error("Record truncated")]
    Truncated,

    /// The record has an invalid length header or unexpected byte count.
    #[error("Invalid record length")]
    InvalidLength,

    /// The key length exceeds the maximum allowed `u16` limit.
    #[error("Key is too large (exceeds u16 limit)")]
    KeyTooLarge,

    /// The value length exceeds the maximum allowed `u32` limit.
    #[error("Value is too large (exceeds u32 limit)")]
    ValueTooLarge,

    /// An error occurred while converting a slice into a fixed-size array.
    #[error("Slice conversion failed: {0}")]
    SliceConversion(#[from] TryFromSliceError),

    /// A standard input/output error occurred during decoding.
    #[error("I/O error: {0}")]
    StdIo(#[from] std::io::Error),

    /// Failed to construct or initialize the fuse filter structure.
    #[error("Failed to create fuse filter")]
    FilterFailed,

    /// The specified filter type is invalid or unsupported.
    #[error("Invalid filter type")]
    InvalidFilterType,
}

/// The primary error type for operations across the Stryx database engine.
#[derive(Debug, thiserror::Error)]
pub enum StryxError {
    /// The requested operation or feature is not yet implemented.
    #[error("Not implemented")]
    NotImplemented,

    /// A standard input/output error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// An error occurred while decoding formats.
    #[error("Decode error: {0}")]
    Decode(#[from] DecodeError),

    /// An error occurred during primitive integer conversion.
    #[error("Integer conversion error: {0}")]
    TryFromIntError(#[from] TryFromIntError),

    /// A duplicate key was encountered while inserts require uniqueness.
    #[error(
        "Duplicate key encountered (configure TableOptions::DuplicatePolicy to allow duplicates)"
    )]
    DuplicateKey,

    /// Keys were not inserted in strictly ascending order as required.
    #[error("Keys must be inserted in strictly ascending order")]
    UnsortedKey,

    /// The key violates the configured table ordering policy.
    #[error("Key violates configured ordering (configure TableOptions::KeyOrder)")]
    InvalidKeyOrder,

    /// The file magic bytes are invalid, indicating file corruption or an invalid Stryx database.
    #[error("Invalid magic bytes (file is corrupted or not a valid Stryx database)")]
    InvalidMagic,

    /// The Stryx file version is unsupported by this build of the engine.
    #[error("Unsupported Stryx file version")]
    InvalidVersion,

    /// The computed checksum did not match the expected value, indicating data corruption.
    #[error("Invalid checksum")]
    InvalidChecksum,

    /// The requested key was not found in the database or table.
    #[error("Key not found")]
    KeyNotFound,

    /// The provided range bounds are invalid or out of order.
    #[error("Invalid range")]
    InvalidRange,

    /// The compression format or compression block is invalid.
    #[error("Invalid Compression")]
    InvalidCompression,

    /// An error occurred during external spill-sorting operations.
    #[error("Spill sort error: {0}")]
    SpillSortError(#[from] spillsort::Error),

    /// The filter block checksum is invalid or corrupted.
    #[error("Invalid filter checksum")]
    InvalidFilterChecksum,

    /// The first key of the block is missing.
    #[error("Missing first Key of the block")]
    MissingBlockFirstKey,

    /// Invalid internal state.
    #[error("{0}")]
    InvalidState(&'static str),

    /// The provided or stored data is invalid.
    #[error("{0}")]
    InvalidData(&'static str),

    #[error("Invalid Offset")]
    InvalidOffset,
}
