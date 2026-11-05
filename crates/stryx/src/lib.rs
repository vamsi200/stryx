pub mod builder;
pub mod config;
pub mod decoder;
pub mod error;
pub mod helper;
pub mod index;
pub mod reader;
pub mod tests;

pub const BLOCK_SIZE: usize = 16 * 1024;
pub const FOOTER_SIZE: usize = 29;
pub const MAGIC: &[u8; 8] = b"STRYX001";
pub const VERSION: u32 = 1;
pub const KEY_HASH_SEED: u64 = 0;
pub const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
