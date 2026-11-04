#![allow(unused)]
use std::{
    fs::{File, OpenOptions},
    hint::black_box,
    io::{BufWriter, Read, Write},
    time::{Duration, Instant},
};

use anyhow::Context;
use rand::{RngExt, rng};
use spillsort::{Config, Sorter};
use stryx::{
    builder::{Record, RecordReader, TableBuilder},
    config::{Compression, DuplicatePolicy, FilterType, TableOptions},
    error::DecodeError,
    reader::TableReader,
    tests::random_records,
};

fn write_to_file(file: &mut File, value: &[u8]) -> anyhow::Result<()> {
    file.write_all(&value)?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let table_options = TableOptions {
        compression: Compression::Zstd,
        key_order: stryx::config::KeyOrder::Lexicographic,
        duplicate_policy: DuplicatePolicy::Allow,
        filter_type: FilterType::BinaryFuse16,
        // block_size: 64,
        ..TableOptions::default()
    };

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("lz4.stryx")?;

    let mut table = TableBuilder::new(file, table_options)?;

    let (keys, values) = random_records(1_000, true);

    let records: Vec<Record<'_>> = keys
        .iter()
        .zip(&values)
        .map(|(key, value)| Record { key, value })
        .collect();

    println!("starting..");

    let key = records[999].key;

    table.add_batch(records);
    let (_, stats) = table.finalize()?;

    let mut reader = TableReader::open("lz4.stryx")?;

    // let key = [
    //     92, 30, 23, 10, 23, 34, 42, 24, 5, 52, 34, 29, 32, 13, 43, 5, 4,
    // ];

    let iterations = 10_000;

    let start = Instant::now();

    for _ in 0..iterations {
        let result = reader.get(std::hint::black_box(key))?;
        std::hint::black_box(result);
    }

    let elapsed = start.elapsed();

    println!("Total: {:?}", elapsed);
    println!("Average: {:?}", elapsed / iterations);

    // println!(
    //     "{:?}",
    //     reader.get_many(&[&keys[88], &keys[80], &keys[12], &keys[10]])?
    // );
    // for key in &keys {
    //     black_box(reader.get(black_box(key))?);
    // }

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
