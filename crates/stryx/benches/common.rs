#![allow(dead_code)]

use std::fmt::Debug;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use stryx::builder::TableBuilder;
use stryx::config::{Compression, DuplicatePolicy, FilterType, KeyOrder, TableOptions};
use stryx::reader::{BlockCache, ReaderOptions, TableReader};

pub const KEY_LEN: usize = 16;
pub const NUM_THREADS: usize = 4;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub struct Table {
    pub path: PathBuf,
    pub keys: Vec<Vec<u8>>,
    pub value_len: usize,
}

impl Drop for Table {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn build_table(n: usize, c: Compression, f: FilterType, block_size: usize) -> Table {
    build_table_with(n, c, f, block_size, 29)
}

pub fn build_table_with(
    n: usize,
    compression: Compression,
    filter: FilterType,
    block_size: usize,
    value_len: usize,
) -> Table {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("bench-stryx-{}-{id}.stryx", std::process::id()));

    let options = TableOptions {
        compression,
        filter_type: filter,
        block_size,
        key_order: KeyOrder::Presorted,
        duplicate_policy: DuplicatePolicy::Reject,
        ..TableOptions::default()
    };
    let mut table = TableBuilder::create(&path, options).unwrap();

    let mut keys: Vec<Vec<u8>> = (0..n)
        .map(|i| {
            let mut k = format!("key-{i:010}-").into_bytes();
            k.resize(KEY_LEN, b'x');
            k
        })
        .collect();
    keys.sort_unstable();

    for (i, key) in keys.iter().enumerate() {
        let mut value = format!("value-{i:010}-").into_bytes();
        value.resize(value_len, b'p');
        table.add(key, &value).unwrap();
    }
    table.finalize().unwrap();

    Table {
        path,
        keys,
        value_len,
    }
}

pub fn sample_keys(fx: &Table, n: usize) -> Vec<Vec<u8>> {
    let step = (fx.keys.len() / n).max(1);
    fx.keys.iter().step_by(step).take(n).cloned().collect()
}

pub fn clustered_keys(fx: &Table, n: usize) -> Vec<Vec<u8>> {
    let start = fx.keys.len() / 2;
    fx.keys.iter().skip(start).take(n).cloned().collect()
}

pub fn no_cache() -> ReaderOptions {
    ReaderOptions {
        disable_block_verification: true,
        block_cache: None,
    }
}

pub fn with_cache(bytes: u64) -> ReaderOptions {
    ReaderOptions {
        disable_block_verification: true,
        block_cache: Some(BlockCache::new(bytes)),
    }
}

pub fn drain<I, T, E>(it: I) -> usize
where
    I: Iterator<Item = Result<T, E>>,
    E: Debug,
{
    let mut n = 0;
    for r in it {
        black_box(r.unwrap());
        n += 1;
    }
    n
}

pub fn run_threads<T, F>(
    base: &TableReader,
    items: &[T],
    threads: usize,
    iters: u64,
    work: F,
) -> Duration
where
    T: Sync,
    F: Fn(&mut TableReader, &[T]) + Sync,
{
    let chunk_size = items.len().div_ceil(threads).max(1);
    let chunks: Vec<&[T]> = items.chunks(chunk_size).collect();
    let n = chunks.len();

    let readers: Vec<TableReader> = (0..n).map(|_| base.clone()).collect();
    let barrier = Barrier::new(n + 1);

    thread::scope(|s| {
        for (mut reader, chunk) in readers.into_iter().zip(chunks) {
            let barrier = &barrier;
            let work = &work;
            s.spawn(move || {
                work(&mut reader, chunk); // untimed warm-up
                barrier.wait();
                for _ in 0..iters {
                    work(&mut reader, chunk);
                }
                barrier.wait();
            });
        }
        barrier.wait();
        let start = Instant::now();
        barrier.wait();
        start.elapsed()
    })
}
