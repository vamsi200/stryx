pub mod builder;
pub mod config;
pub mod decoder;
pub mod error;
pub mod index;
pub mod reader;

pub const BLOCK_SIZE: usize = 16 * 1024;
pub const FOOTER_SIZE: usize = 28;
pub const MAGIC: &[u8; 8] = b"STRYX001";
pub const VERSION: u32 = 1;
