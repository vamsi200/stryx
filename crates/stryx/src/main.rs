use std::{fs::OpenOptions, hint::black_box, time::Instant};

use rand::{RngExt, SeedableRng, rngs::StdRng};
use stryx::{
    builder::{Record, TableBuilder},
    config::{Compression, DuplicatePolicy, FilterType, TableOptions},
    error::StryxError,
    reader::{BlockCache, ReaderOptions, TableReader},
};

pub fn random_records(n: usize, sorted: bool) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut rng = StdRng::seed_from_u64(42);

    let mut records: Vec<(Vec<u8>, Vec<u8>)> = (0..n)
        .map(|_| {
            let key_len = rng.random_range(4..16);
            let val_len = rng.random_range(8..64);

            let key = (0..key_len).map(|_| rng.random()).collect();
            let value = (0..val_len).map(|_| rng.random()).collect();

            (key, value)
        })
        .collect();

    if sorted {
        records.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    }

    let (keys, values): (Vec<_>, Vec<_>) = records.into_iter().unzip();

    (keys, values)
}

fn main() -> Result<(), StryxError> {
    let table_options = TableOptions {
        compression: Compression::Zstd(1),
        key_order: stryx::config::KeyOrder::Lexicographic,
        duplicate_policy: DuplicatePolicy::Allow,
        filter_type: FilterType::BinaryFuse32,
        // block_size: 64,
        ..TableOptions::default()
    };

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("lz4.stryx")?;

    let mut table = TableBuilder::new(file, table_options)?;

    let (keys, values) = random_records(1_0, true);

    let records: Vec<Record<'_>> = keys
        .iter()
        .zip(&values)
        .map(|(key, value)| Record { key, value })
        .collect();

    println!("Building table...");

    // let target_key = records[99].key.to_vec();

    table.add_batch(records)?;
    let (_, _stats) = table.finalize()?;

    let read_opts = ReaderOptions {
        disable_block_verification: true,
        block_cache: Some(BlockCache::new(16 * 1024 * 1024)),
    };

    let mut reader = TableReader::open_with_options("lz4.stryx", read_opts)?;

    for record in reader.iter() {
        let record = record?;
        println!("{record:?}");
    }

    // println!(
    //     "Starting {} lookups across {} threads...",
    //     total_iterations, num_threads
    // );
    //
    // let start = Instant::now();
    //
    // let elapsed = start.elapsed();
    //
    // println!("Total Time: {:?}", elapsed);
    //
    // println!("Average latency: {:?}", elapsed / total_iterations);
    //
    Ok(())
}
