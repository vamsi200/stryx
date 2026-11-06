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

    println!("Building table...");

    let target_key = records[99].key.to_vec();

    table.add_batch(records);
    let (_, _stats) = table.finalize()?;

    let reader = TableReader::open_with_options("lz4.stryx", true)?;

    let total_iterations = 10_000;
    let num_threads = 4;
    let iterations_per_thread = total_iterations / num_threads;

    println!(
        "Starting {} lookups across {} threads...",
        total_iterations, num_threads
    );

    let start = Instant::now();
    let mut handles = vec![];

    for _ in 0..num_threads {
        let mut thread_reader = reader.clone();

        let thread_key = target_key.clone();

        handles.push(std::thread::spawn(move || {
            for _ in 0..iterations_per_thread {
                // We use &thread_key to pass it as a slice &[u8]
                let result = thread_reader
                    .get(std::hint::black_box(&thread_key))
                    .unwrap();
                std::hint::black_box(result);
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let elapsed = start.elapsed();

    println!("Total Time: {:?}", elapsed);

    println!("Average latency: {:?}", elapsed / total_iterations);

    Ok(())
}
