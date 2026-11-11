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

#[derive(Clone, Copy, Debug)]
pub enum Shape {
    Short,
    LongPrefix,
    Tied,
    Random,
    VarLen,
}

#[derive(Clone, Copy, Debug)]
pub enum Values {
    Compressible,
    Random,
}

#[derive(Clone)]
pub struct Spec {
    pub shape: Shape,
    pub value_len: usize,
    pub values: Values,
    pub compression: Compression,
    pub block_size: usize,
    pub filter: FilterType,
}

pub const TARGET_BYTES: usize = 16 << 20;

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

pub fn make_key(shape: Shape, i: usize) -> Vec<u8> {
    match shape {
        Shape::Short => {
            let mut k = format!("key-{i:010}-").into_bytes();
            k.resize(16, b'x');
            k
        }
        Shape::LongPrefix => {
            let mut k = format!("tenant-0042/collection-17/records/doc-{i:010}").into_bytes();
            k.resize(64, b'x');
            k
        }
        Shape::Tied => format!("tenant-{:04}/{i:010}", i / 5000).into_bytes(),
        Shape::Random => {
            let mut k = splitmix(i as u64).to_be_bytes().to_vec();
            k.extend_from_slice(&splitmix(!(i as u64)).to_be_bytes());
            k
        }
        // 10..=41 bytes
        Shape::VarLen => {
            let mut k = format!("k{i:09}").into_bytes();
            k.extend(std::iter::repeat_n(b'v', i % 32));
            k
        }
    }
}

pub fn build_spec(s: Spec) -> Table {
    let klen = make_key(s.shape, 0).len();
    let n = (TARGET_BYTES / (klen + s.value_len)).clamp(2_000, 500_000);

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("bench-stryx-{}-{id}.stryx", std::process::id()));
    let options = TableOptions {
        compression: s.compression,
        filter_type: s.filter,
        block_size: s.block_size,
        key_order: KeyOrder::Presorted,
        duplicate_policy: DuplicatePolicy::Reject,
        ..TableOptions::default()
    };
    let mut table = TableBuilder::create(&path, options).unwrap();

    let mut keys: Vec<Vec<u8>> = (0..n).map(|i| make_key(s.shape, i)).collect();
    keys.sort_unstable();

    let mut v = vec![0u8; s.value_len];
    for (i, key) in keys.iter().enumerate() {
        match s.values {
            Values::Compressible => {
                let head = format!("value-{i:010}-");
                v.fill(b'p');
                let m = head.len().min(v.len());
                v[..m].copy_from_slice(&head.as_bytes()[..m]);
            }
            Values::Random => {
                for (j, b) in v.iter_mut().enumerate() {
                    *b = splitmix((i * 31 + j / 8) as u64).to_le_bytes()[j % 8];
                }
            }
        }
        table.add(key, &v).unwrap();
    }
    table.finalize().unwrap();

    Table {
        path,
        keys,
        value_len: s.value_len,
    }
}
