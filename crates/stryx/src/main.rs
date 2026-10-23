#![allow(unused)]
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Read, Write},
    time::{Duration, Instant},
};

use anyhow::Context;
use rand::{RngExt, rng};
use stryx::{
    builder::{Record, RecordReader, TableBuilder},
    config::TableOptions,
    error::DecodeError,
};

fn write_to_file(file: &mut File, value: &[u8]) -> anyhow::Result<()> {
    file.write_all(&value)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut table = TableBuilder::create("test.stryx", TableOptions::default())?;
    table.add_batch(random_records(10_000))?;

    table.finalize()?;

    //
    // write_to_file(&mut file, &output)?;

    // let mut start = Instant::now();
    // let mut buf = Vec::new();
    //
    // file.read_to_end(&mut buf)?;
    //
    // let mut record_reader = RecordReader::new(&buf);
    //
    // let mut record_count = 0;
    // while let Some(record) = record_reader.next_record()? {
    //     std::hint::black_box(record);
    // }

    // println!("elapsed: {:?}", start.elapsed());
    Ok(())
}

fn random_records(n: usize) -> Vec<Record<'static>> {
    let mut rng = rand::rng();
    let mut records: Vec<Record<'static>> = (0..n)
        .map(|_| {
            let key_len = rng.random_range(4..16);
            let val_len = rng.random_range(8..64);
            let key: Vec<u8> = (0..key_len).map(|_| rng.random()).collect();
            let value: Vec<u8> = (0..val_len).map(|_| rng.random()).collect();
            let key: &'static [u8] = Box::leak(key.into_boxed_slice());
            let value: &'static [u8] = Box::leak(value.into_boxed_slice());

            Record { key, value }
        })
        .collect();

    records.sort_unstable_by(|a, b| a.key.cmp(b.key));
    records
}

#[cfg(test)]
mod tests {
    use stryx::builder::Encode;

    use super::*;

    #[test]
    fn test_decode() {
        let mut output = Vec::new();
        for record in random_records(10) {
            record.encode_record();
        }

        let mut record_reader = RecordReader::new(&output);

        while let Some(record) = record_reader.next_record().unwrap() {
            assert!(record.key.len() > 0);
            assert!(record.value.len() > 0);
        }
    }
}
