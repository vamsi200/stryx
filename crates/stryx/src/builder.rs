#![allow(unused)]

#[derive(Debug, Default)]
pub struct Record<'a> {
    pub key: &'a [u8],
    pub value: &'a [u8],
}

impl<'a> Record<'a> {
    pub fn len(&self) -> usize {
        self.key.len() + self.value.len()
    }
}

// TableBuilder
pub struct TableBuilder {}

impl<'a> TableBuilder {
    /// Create a new SSTable builder
    pub fn create(path: &'a str) -> Self {
        todo!()
    }

    /// Append a record
    pub fn add(&mut self, key: &[u8], value: &[u8]) {
        todo!()
    }

    /// Append a batch of records
    pub fn add_batch(&mut self, records: impl IntoIterator<Item = Record<'a>>) {
        todo!()
    }

    /// Flush remaining blocks, write index and footer, finalize file
    /// and return the metadata about completed table.
    pub fn finalize(&mut self) {
        todo!()
    }

    /// Return current write statistics
    pub fn statistics(&self) {
        todo!()
    }
}
