#![allow(unused)]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
};

use anyhow::Context;

#[derive(Debug)]
struct Block {
    records_count: u32,
    checksum: u32,
}

#[derive(Debug)]
struct RecordInfo {
    key_len: u16,
    value_len: u32,
}

#[derive(Debug)]
struct Record<'a> {
    key: &'a [u8],
    value: &'a [u8],
}

trait Encode {
    fn encode_len(&self) -> usize;
    fn encode_record(&self, value: &mut Vec<u8>) -> anyhow::Result<()>;
}

impl Encode for Record<'_> {
    fn encode_len(&self) -> usize {
        size_of::<u16>() + size_of::<u32>() + self.key.len() + self.value.len()
    }

    fn encode_record(&self, value: &mut Vec<u8>) -> anyhow::Result<()> {
        let key_len = u16::try_from(self.key.len()).context("Key Too Large")?;
        let value_len = u32::try_from(self.value.len()).context("Value Too Large")?;

        value.reserve(self.encode_len());

        value.extend_from_slice(&key_len.to_le_bytes());
        value.extend_from_slice(&value_len.to_le_bytes());

        value.extend_from_slice(&self.key);
        value.extend_from_slice(&self.value);

        Ok(())
    }
}

trait Decode<'a>: Sized {
    fn decode(value: &'a [u8]) -> anyhow::Result<(Self, usize)>;
}

impl<'a> Decode<'a> for Record<'a> {
    fn decode(value: &'a [u8]) -> anyhow::Result<(Self, usize)> {
        let key_len = u16::from_le_bytes([value[0], value[1]]) as usize;
        let value_len = u32::from_le_bytes(value[2..6].try_into()?) as usize;

        Ok((
            Self {
                key: &value[6..6 + key_len],
                value: &value[6 + key_len..6 + key_len + value_len],
            },
            0,
        ))
    }
}

fn main() -> anyhow::Result<()> {
    let mut file = OpenOptions::new().create(true).write(true).open("test")?;
    let (key, value) = ("key", "value");
    let record = Record {
        key: key.as_bytes(),
        value: value.as_bytes(),
    };

    let mut output = Vec::new();
    record.encode_record(&mut output)?;
    println!("{}", output.len());

    let record = Record::decode(&output)?.0;
    println!("{}", str::from_utf8(record.key)?);
    println!("{}", str::from_utf8(record.value)?);
    Ok(())
}
