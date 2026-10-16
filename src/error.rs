use std::array::TryFromSliceError;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("Record truncated")]
    Truncated,
    #[error("Invalid record len")]
    InvalidLength,
    #[error("Failed to convert slice: {0}")]
    SliceConversion(#[from] TryFromSliceError),
}
