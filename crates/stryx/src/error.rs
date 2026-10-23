use std::{array::TryFromSliceError, num::TryFromIntError};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("Record truncated")]
    Truncated,
    #[error("Invalid record len")]
    InvalidLength,
    #[error("Failed to convert slice: {0}")]
    SliceConversion(#[from] TryFromSliceError),
}

#[derive(Debug, thiserror::Error)]
pub enum StryxError {
    #[error("Not implemented")]
    NotImplemented,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Decode error: {0}")]
    Decode(#[from] DecodeError),

    #[error("TryFromIntError")]
    TryFromIntError(#[from] TryFromIntError),

    #[error(
        "Duplicate key encountered; configure TableOptions::DuplicatePolicy to allow duplicates"
    )]
    DuplicateKey,

    #[error("Keys must be inserted in strictly ascending order")]
    UnsortedKey,

    #[error("Key violates the configured ordering; configure TableOptions::KeyOrder")]
    InvalidKeyOrder,
}
