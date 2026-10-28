#![allow(unused)]
use std::{
    fs::{File, OpenOptions},
    hint::black_box,
    io::{BufWriter, Read, Write},
    time::{Duration, Instant},
};

use anyhow::Context;
use rand::{RngExt, rng};
use stryx::{
    builder::{Record, RecordReader, TableBuilder},
    config::TableOptions,
    error::DecodeError,
    reader::TableReader,
};

fn write_to_file(file: &mut File, value: &[u8]) -> anyhow::Result<()> {
    file.write_all(&value)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut table = TableBuilder::create("test.stryx", TableOptions::default())?;
    // let records = random_records(10);
    // let keys: Vec<_> = records.iter().step_by(1).map(|r| r.key.to_vec()).collect();

    table.add(&[1u8], &[10u8])?;
    table.add(&[1u8, 2], &[20u8])?;
    table.add(&[1u8, 4], &[20u8])?;
    table.add(&[1u8, 5], &[20u8])?;
    table.add(&[2u8, 5], &[20u8])?;
    table.add(&[3u8, 5], &[20u8])?;

    // table.add_batch(records);
    table.finalize()?;

    let mut reader = TableReader::open("test.stryx")?;

    let start = Instant::now();

    for s in reader.prefix(&[1u8])? {
        let s = s?;
        println!("{s:?}");
    }

    // println!(
    //     "{:?}",
    //     reader.get_many(&[&keys[88], &keys[80], &keys[12], &keys[10]])?
    // );
    // for key in &keys {
    //     black_box(reader.get(black_box(key))?);
    // }

    // let elapsed = start.elapsed();

    // println!("Lookups: {}", keys.len());
    // println!("Total: {:?}", elapsed);
    // println!("Average: {:?}", elapsed / keys.len() as u32);

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
