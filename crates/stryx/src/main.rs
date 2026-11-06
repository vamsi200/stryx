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

fn main() -> anyhow::Result<()> {
    let table_options = TableOptions {
        compression: Compression::Zstd,
        key_order: stryx::config::KeyOrder::Lexicographic,
        duplicate_policy: DuplicatePolicy::Allow,
        filter_type: FilterType::BinaryFuse32,
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

    let key = records[99].key;

    table.add_batch(records);
    let (_, stats) = table.finalize()?;

    let mut reader = TableReader::open_with_options("lz4.stryx", true)?;

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

    Ok(())
}
