use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use stryx::builder::TableBuilder;
use stryx::config::{Compression, DuplicatePolicy, FilterType, KeyOrder, TableOptions};
use stryx::reader::TableReader;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

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

fn bench_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("get");

    for (name, filter) in [
        ("no_filter", FilterType::None),
        ("fuse8", FilterType::BinaryFuse8),
        ("fuse16", FilterType::BinaryFuse16),
        ("fuse32", FilterType::BinaryFuse32),
    ] {
        let fx = build_table(100_000, Compression::None, filter, 4096);
        let keys = sample_keys(&fx, 1024);

        group.throughput(Throughput::Elements(keys.len() as u64));
        group.bench_with_input(BenchmarkId::new("hit", name), &keys, |b, keys| {
            b.iter_batched_ref(
                || TableReader::open(&fx.path).unwrap(),
                |reader| {
                    for k in keys {
                        black_box(reader.get(black_box(k)).unwrap());
                    }
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_misses_no_compression(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_miss");

    let table_bf8 = build_table(100_000, Compression::None, FilterType::BinaryFuse8, 4096);
    let table_bf16 = build_table(100_000, Compression::None, FilterType::BinaryFuse16, 4096);
    let table_bf32 = build_table(100_000, Compression::None, FilterType::BinaryFuse32, 4096);
    let table_bf_none = build_table(100_000, Compression::None, FilterType::None, 4096);

    let misses: Vec<Vec<u8>> = (0..1024)
        .map(|i| format!("absent-{i:010}-").into_bytes())
        .collect();

    group.throughput(Throughput::Elements(misses.len() as u64));
    group.bench_function("miss/table_bf8", |b| {
        b.iter_batched_ref(
            || TableReader::open(&table_bf8.path).unwrap(),
            |reader| {
                for k in &misses {
                    black_box(reader.get(black_box(k)).unwrap());
                }
            },
            BatchSize::SmallInput,
        );
    });

    group.throughput(Throughput::Elements(misses.len() as u64));
    group.bench_function("miss/table_bf16", |b| {
        b.iter_batched_ref(
            || TableReader::open(&table_bf16.path).unwrap(),
            |reader| {
                for k in &misses {
                    black_box(reader.get(black_box(k)).unwrap());
                }
            },
            BatchSize::SmallInput,
        );
    });

    group.throughput(Throughput::Elements(misses.len() as u64));
    group.bench_function("miss/table_bf32", |b| {
        b.iter_batched_ref(
            || TableReader::open(&table_bf32.path).unwrap(),
            |reader| {
                for k in &misses {
                    black_box(reader.get(black_box(k)).unwrap());
                }
            },
            BatchSize::SmallInput,
        );
    });

    group.throughput(Throughput::Elements(misses.len() as u64));
    group.bench_function("miss/table_none", |b| {
        b.iter_batched_ref(
            || TableReader::open(&table_bf_none.path).unwrap(),
            |reader| {
                for k in &misses {
                    black_box(reader.get(black_box(k)).unwrap());
                }
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_get_many(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_many");

    let fx = build_table(100_000, Compression::None, FilterType::BinaryFuse8, 4096);

    for batch in [1usize, 8, 64, 256, 1024] {
        let keys = sample_keys(&fx, batch);
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();

        group.throughput(Throughput::Elements(batch as u64));
        group.bench_with_input(BenchmarkId::new("hit", batch), &key_refs, |b, ks| {
            b.iter_batched_ref(
                || TableReader::open(&fx.path).unwrap(),
                |reader| {
                    black_box(reader.get_many(black_box(ks)).unwrap());
                },
                BatchSize::SmallInput,
            );
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
        group.bench_with_input(BenchmarkId::new("mixed", batch), &key_refs, |b, ks| {
            b.iter_batched_ref(
                || TableReader::open(&fx.path).unwrap(),
                |reader| {
                    black_box(reader.get_many(black_box(ks)).unwrap());
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_get_by_options(c: &mut Criterion) {
    let mut group = c.benchmark_group("get/options");

    let n = 10_000;

    for compression in [Compression::None, Compression::Lz4, Compression::Zstd] {
        for block_size in [1024usize, 4096, 16_384] {
            let fx = build_table(n, compression, FilterType::BinaryFuse16, block_size);
            let keys = sample_keys(&fx, 512);

            group.throughput(Throughput::Elements(keys.len() as u64));
            group.bench_with_input(
                BenchmarkId::new(format!("{compression:?}"), format!("bs{block_size}")),
                &keys,
                |b, keys| {
                    b.iter_batched_ref(
                        || TableReader::open(&fx.path).unwrap(),
                        |reader| {
                            for k in keys {
                                black_box(reader.get(black_box(k)).unwrap());
                            }
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }

    group.finish();
}

fn benches(c: &mut Criterion) {
    bench_get(c);
    bench_get_many(c);
    bench_get_by_options(c);
    bench_misses_no_compression(c);
}

criterion_group! {
    name = lookup_benches;
    config = Criterion::default()
        .sample_size(50)
        .measurement_time(std::time::Duration::from_secs(10));
    targets = benches
}
criterion_main!(lookup_benches);
