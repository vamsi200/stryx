#![allow(unused)]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    time::{Duration, Instant},
};

use anyhow::Context;
use rand::{RngExt, rng};
use stryx::{
    builder::{Record, TableBuilder},
    error::DecodeError,
};

const BLOCK_SIZE_IN_KB: u8 = 16;

#[derive(Debug)]
struct DataBlock<'a> {
    records: &'a [Record<'a>],
    offsets: &'a [u32],
    records_count: u32,
}

#[derive(Debug)]
struct IndexBlock {
    first_key: u32,
    block_offset: u32,
    block_size: u32,
    checksum: u32,
}

#[derive(Debug)]
struct Footer {
    pub index_offset: u64,
    pub index_size: u64,
    pub version: u16,
    pub magic_bytes: [u8; 8],
}

#[derive(Debug)]
struct RecordInfo {
    key_len: u16,
    value_len: u32,
}

struct RecordReader<'a> {
    value: &'a [u8],
    pos: usize,
}

impl<'a> RecordReader<'a> {
    pub fn new(value: &'a [u8]) -> Self {
        Self { value, pos: 0 }
    }

    pub fn next_record(&mut self) -> Result<Option<Record<'a>>, DecodeError> {
        if self.pos == self.value.len() {
            return Ok(None);
        }

        let remaining = &self.value[self.pos..];

        if remaining.len() < 6 {
            return Err(DecodeError::Truncated);
        }

        let key_len = u16::from_le_bytes([remaining[0], self.value[1]]) as usize;
        let value_len = u32::from_le_bytes(
            remaining[2..6]
                .try_into()
                .map_err(|_| DecodeError::InvalidLength)?,
        ) as usize;

        let record_len = 6usize
            .checked_add(key_len)
            .and_then(|v| v.checked_add(value_len))
            .ok_or(DecodeError::InvalidLength)?;

        if remaining.len() < record_len {
            return Err(DecodeError::Truncated);
        }

        let key = &remaining[6..6 + key_len];
        let value = &remaining[6 + key_len..record_len];

        self.pos += record_len;

        Ok(Some(Record { key, value }))
    }

    pub fn pos(&self) -> usize {
        self.pos
    }
}

impl Encode for Record<'_> {
    fn encode_len(&self) -> usize {
        size_of::<u16>() + size_of::<u32>() + self.key.len() + self.value.len()
    }

    fn encode_record(&self, value: &mut Vec<u8>) -> anyhow::Result<()> {
        let key_len = u16::try_from(self.key.len())
            .context("Key Too Large")
            .unwrap();
        let value_len = u32::try_from(self.value.len())
            .context("Value Too Large")
            .unwrap();

        value.reserve(self.encode_len());

        value.extend_from_slice(&key_len.to_le_bytes());
        value.extend_from_slice(&value_len.to_le_bytes());

        value.extend_from_slice(&self.key);
        value.extend_from_slice(&self.value);
        Ok(())
    }
}

trait Encode {
    fn encode_len(&self) -> usize;
    fn encode_record(&self, value: &mut Vec<u8>) -> anyhow::Result<()>;
}

fn write_to_file(file: &mut File, value: &[u8]) -> anyhow::Result<()> {
    file.write_all(&value)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut table = TableBuilder::create("test");
    let record = Record {
        key: &[0u8],
        value: &[0u8],
    };

    table.add(&[0u8], &[0u8]);

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .append(true)
        .open("test")?;

    // let mut output = Vec::new();
    // for record in random_records(100_000) {
    //     record.encode_record(&mut output);
    // }
    //
    // write_to_file(&mut file, &output)?;

    let mut start = Instant::now();
    let mut buf = Vec::new();

    file.read_to_end(&mut buf)?;

    let mut record_reader = RecordReader::new(&buf);

    let mut record_count = 0;
    while let Some(record) = record_reader.next_record()? {
        std::hint::black_box(record);
    }

    println!("elapsed: {:?}", start.elapsed());
    Ok(())
}

fn random_records(n: usize) -> Vec<Record<'static>> {
    let mut rng = rand::rng();
    (0..n)
        .map(|_| {
            let key_len = rng.random_range(4..16);
            let val_len = rng.random_range(8..64);
            let key: Vec<u8> = (0..key_len).map(|_| rng.random()).collect();
            let value: Vec<u8> = (0..val_len).map(|_| rng.random()).collect();
            let key: &'static [u8] = Box::leak(key.into_boxed_slice());
            let value: &'static [u8] = Box::leak(value.into_boxed_slice());

            Record { key, value }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode() {
        let mut output = Vec::new();
        for record in random_records(10) {
            record.encode_record(&mut output);
        }

        let mut record_reader = RecordReader::new(&output);

        while let Some(record) = record_reader.next_record().unwrap() {
            assert!(record.key.len() > 0);
            assert!(record.value.len() > 0);
        }
    }
}
