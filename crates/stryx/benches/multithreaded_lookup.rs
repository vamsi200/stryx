use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use stryx::builder::TableBuilder;
use stryx::config::{Compression, DuplicatePolicy, FilterType, KeyOrder, TableOptions};
use stryx::reader::{ReaderOptions, TableReader};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

const NUM_THREADS: usize = 4;

struct Table {
    path: PathBuf,
    keys: Vec<Vec<u8>>,
}

impl Drop for Table {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn build_table(n: usize, compression: Compression, filter: FilterType, block_size: usize) -> Table {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("bench-stryx-mt-{}-{id}.stryx", std::process::id()));

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
            k.resize(16, b'x');
            k
        })
        .collect();
    keys.sort_unstable();

    for (i, key) in keys.iter().enumerate() {
        let value = format!("value-{i:010}-payload-padding").into_bytes();
        table.add(key, &value).unwrap();
    }

    table.finalize().unwrap();

    Table { path, keys }
}

fn sample_keys(fx: &Table, n: usize) -> Vec<Vec<u8>> {
    let step = (fx.keys.len() / n).max(1);
    fx.keys.iter().step_by(step).take(n).cloned().collect()
}

/// Consecutive keys, so a batch shares blocks (the case `get_many` is built for).
fn clustered_keys(fx: &Table, n: usize) -> Vec<Vec<u8>> {
    let start = fx.keys.len() / 2;
    fx.keys.iter().skip(start).take(n).cloned().collect()
}

fn run_threads<T, F>(
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
                work(&mut reader, chunk); // untimed warm-up pass
                barrier.wait(); // start together
                for _ in 0..iters {
                    work(&mut reader, chunk);
                }
                barrier.wait(); // all finished
            });
        }

        barrier.wait();
        let start = Instant::now();
        barrier.wait();
        start.elapsed()
    })
}

fn get_each(reader: &mut TableReader, keys: &[Vec<u8>]) {
    for k in keys {
        black_box(reader.get(black_box(k)).unwrap());
    }
}

fn get_many_chunk(reader: &mut TableReader, keys: &[&[u8]]) {
    black_box(reader.get_many(black_box(keys)).unwrap());
}

fn bench_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("mt_get");

    for (name, filter) in [
        ("no_filter", FilterType::None),
        ("fuse8", FilterType::BinaryFuse8),
        ("fuse16", FilterType::BinaryFuse16),
        ("fuse32", FilterType::BinaryFuse32),
    ] {
        let fx = build_table(100_000, Compression::None, filter, 4096);
        let keys = sample_keys(&fx, 1024);
        let base = TableReader::open_with_options(&fx.path, ReaderOptions::default()).unwrap();

        group.throughput(Throughput::Elements(keys.len() as u64));
        group.bench_function(BenchmarkId::new("hit", name), |b| {
            b.iter_custom(|iters| run_threads(&base, &keys, NUM_THREADS, iters, get_each));
        });
    }

    group.finish();
}

/// Same workload at 1, 2 and 4 threads: the scaling curve in one place.
fn bench_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("mt_scaling");

    let fx = build_table(100_000, Compression::None, FilterType::None, 4096);
    let keys = sample_keys(&fx, 1024);
    let base = TableReader::open_with_options(&fx.path, ReaderOptions::default()).unwrap();

    group.throughput(Throughput::Elements(keys.len() as u64));
    for threads in [1usize, 2, 4] {
        group.bench_function(BenchmarkId::new("hit/no_filter", threads), |b| {
            b.iter_custom(|iters| run_threads(&base, &keys, threads, iters, get_each));
        });
    }

    group.finish();
}

fn bench_misses_no_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("mt_get_miss");

    let tables = [
        ("table_bf8", FilterType::BinaryFuse8),
        ("table_bf16", FilterType::BinaryFuse16),
        ("table_bf32", FilterType::BinaryFuse32),
        ("table_none", FilterType::None),
    ];

    let misses: Vec<Vec<u8>> = (0..1024)
        .map(|i| format!("absent-{i:010}-").into_bytes())
        .collect();

    group.throughput(Throughput::Elements(misses.len() as u64));
    for (name, filter) in tables {
        let fx = build_table(100_000, Compression::None, filter, 4096);
        let base = TableReader::open_with_options(&fx.path, ReaderOptions::default()).unwrap();

        group.bench_function(BenchmarkId::new("miss", name), |b| {
            b.iter_custom(|iters| run_threads(&base, &misses, NUM_THREADS, iters, get_each));
        });
    }

    group.finish();
}

fn bench_get_many(c: &mut Criterion) {
    let mut group = c.benchmark_group("mt_get_many");

    let fx = build_table(100_000, Compression::None, FilterType::BinaryFuse8, 4096);
    let base = TableReader::open_with_options(&fx.path, ReaderOptions::default()).unwrap();

    // Spread keys: few keys share a block, so grouping can't help much.
    for batch in [1usize, 8, 64, 256, 1024] {
        let keys = sample_keys(&fx, batch);
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();

        group.throughput(Throughput::Elements(batch as u64));
        group.bench_function(BenchmarkId::new("hit", batch), |b| {
            b.iter_custom(|iters| {
                run_threads(&base, &key_refs, NUM_THREADS, iters, get_many_chunk)
            });
        });
    }

    // Adjacent keys: many keys per block. This is where get_many should win.
    for batch in [64usize, 256, 1024] {
        let keys = clustered_keys(&fx, batch);
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();

        group.throughput(Throughput::Elements(batch as u64));
        group.bench_function(BenchmarkId::new("clustered", batch), |b| {
            b.iter_custom(|iters| {
                run_threads(&base, &key_refs, NUM_THREADS, iters, get_many_chunk)
            });
        });
    }

    // Same clustered keys through plain get, for a direct comparison.
    for batch in [64usize, 256, 1024] {
        let keys = clustered_keys(&fx, batch);

        group.throughput(Throughput::Elements(batch as u64));
        group.bench_function(BenchmarkId::new("clustered_get_loop", batch), |b| {
            b.iter_custom(|iters| run_threads(&base, &keys, NUM_THREADS, iters, get_each));
        });
    }

    for batch in [8usize, 64, 256] {
        let mut keys: Vec<Vec<u8>> = Vec::with_capacity(batch);
        for i in 0..batch {
            if i % 2 == 0 {
                let idx = (i * 97) % fx.keys.len();
                keys.push(fx.keys[idx].clone());
            } else {
                keys.push(format!("absent-{i:010}-").into_bytes());
            }
        }
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();

        group.throughput(Throughput::Elements(batch as u64));
        group.bench_function(BenchmarkId::new("mixed", batch), |b| {
            b.iter_custom(|iters| {
                run_threads(&base, &key_refs, NUM_THREADS, iters, get_many_chunk)
            });
        });
    }

    group.finish();
}

fn bench_get_by_options(c: &mut Criterion) {
    let mut group = c.benchmark_group("mt_get_options");

    let n = 10_000;

    for compression in [Compression::None, Compression::Lz4, Compression::Zstd(1)] {
        for block_size in [1024usize, 4096, 16_384] {
            let fx = build_table(n, compression, FilterType::BinaryFuse16, block_size);
            let keys = sample_keys(&fx, 512);
            let base = TableReader::open_with_options(&fx.path, ReaderOptions::default()).unwrap();

            group.throughput(Throughput::Elements(keys.len() as u64));
            group.bench_function(
                BenchmarkId::new(format!("{compression:?}"), format!("bs{block_size}")),
                |b| {
                    b.iter_custom(|iters| run_threads(&base, &keys, NUM_THREADS, iters, get_each));
                },
            );
        }
    }

    group.finish();
}

fn mt_benches(c: &mut Criterion) {
    bench_get(c);
    bench_scaling(c);
    bench_get_many(c);
    bench_get_by_options(c);
    bench_misses_no_compression(c);
}

criterion_group! {
    name = mt_lookup_benches;
    config = Criterion::default()
        .sample_size(30)
        .measurement_time(std::time::Duration::from_secs(5));
    targets = mt_benches
}
criterion_main!(mt_lookup_benches);
