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
    config::{Compression, TableOptions},
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
        compression: Compression::Lz4,
        key_order: stryx::config::KeyOrder::Lexicographic,
        ..TableOptions::default()
    };

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("lz4.stryx")?;

    let mut table = TableBuilder::new(file, table_options)?;

    let (keys, values) = random_records(1_000_000, true);

    let records: Vec<Record<'_>> = keys
        .iter()
        .zip(&values)
        .map(|(key, value)| Record { key, value })
        .collect();

    println!("starting..");

    table.add_batch(records);
    table.finalize()?;

    let mut reader = TableReader::open("lz4.stryx")?;

    let start = Instant::now();

    for s in reader.prefix(&[1u8])? {
        // let s = s?;
        // println!("{s:?}");
    }

    println!("{:#?}", reader.stats());

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
