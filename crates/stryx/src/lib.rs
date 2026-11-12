pub mod builder;
pub mod config;
pub(crate) mod decoder;
pub mod error;
pub(crate) mod helper;
pub(crate) mod index;
pub mod reader;
#[cfg(test)]
mod tests;

/// Default Block Size (16KiB)
pub const BLOCK_SIZE: usize = 16 * 1024;
pub(crate) const FOOTER_SIZE: usize = 29;
pub(crate) const MAGIC: &[u8; 8] = b"STRYX001";

/// Current Stryx Version
pub const VERSION: u32 = 1;

pub(crate) const KEY_HASH_SEED: u64 = 0;
pub(crate) const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
pub(crate) const MIN_ENTRY_LEN: usize = 2 + 8 + 4 + 4;
