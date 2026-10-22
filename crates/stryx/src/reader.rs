#![allow(unused)]
use crate::error::StryxError;

pub struct TableReader {}

impl TableReader {
    /// Open and validate an SSTable
    pub fn open(path: &str) -> Result<Self, StryxError> {
        todo!()
    }

    /// Open with configurable read behavior
    pub fn open_with_options(path: &str, config: ReaderOptions) -> Result<Self, StryxError> {
        todo!()
    }

    /// Find the value associated with a key
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StryxError> {
        todo!()
    }

    /// Check whether a key exists
    pub fn contains_key(&self, key: &[u8]) -> Result<bool, StryxError> {
        todo!()
    }

    /// Batch point lookups
    pub fn get_many(&self, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, StryxError> {
        todo!()
    }

    /// Iterate over all records in sorted order
    pub fn iter(&self) -> Result<TableIterator<'_>, StryxError> {
        todo!()
    }

    /// Iterate over a bounded key range
    pub fn range(&self, start: &[u8], end: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        todo!()
    }

    /// Iterate over keys sharing a prefix
    pub fn prefix(&self, prefix: &[u8]) -> Result<TableIterator<'_>, StryxError> {
        todo!()
    }

    /// Return table metadata
    pub fn metadata(&self) -> &TableMetadata {
        todo!()
    }

    /// Return runtime reader statistics
    pub fn stats(&self) -> ReadStats {
        todo!()
    }
}

pub struct ReaderOptions {
    // TODO
}

pub struct TableMetadata {
    // TODO
}

pub struct ReadStats {
    // TODO
}

pub struct TableIterator<'a> {
    _marker: std::marker::PhantomData<&'a TableReader>,
    // TODO
}
