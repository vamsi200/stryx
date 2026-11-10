mod common;

use common::*;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use stryx::config::{Compression, FilterType};
use stryx::reader::TableReader;

// contains_key should beat get on hits (no value copy) and match it on filter-rejected misses.
fn bench_contains_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("contains_key");

    let misses: Vec<Vec<u8>> = (0..1024)
        .map(|i| format!("absent-{i:010}-").into_bytes())
        .collect();

    for (name, filter) in [
        ("fuse8", FilterType::BinaryFuse8),
        ("no_filter", FilterType::None),
    ] {
        let fx = build_table(100_000, Compression::None, filter, 4096);
        let hits = sample_keys(&fx, 1024);
        let mut reader = TableReader::open_with_options(&fx.path, with_cache(16 << 20)).unwrap();
        for _ in 0..2 {
            for k in &hits {
                reader.get(k).unwrap();
            }
        }

        group.throughput(Throughput::Elements(1024));
        for (kind, keys) in [("hit", &hits), ("miss", &misses)] {
            group.bench_function(BenchmarkId::new(format!("{kind}/get"), name), |b| {
                b.iter(|| {
                    for k in keys {
                        black_box(reader.get(black_box(k)).unwrap());
                    }
                })
            });
            group.bench_function(
                BenchmarkId::new(format!("{kind}/contains_key"), name),
                |b| {
                    b.iter(|| {
                        for k in keys {
                            black_box(reader.contains_key(black_box(k)).unwrap());
                        }
                    })
                },
            );
        }
    }
    group.finish();
}

// get vs get_in_bytes and get_many vs get_many_in_bytes across value sizes.
// The Bytes variants are wrappers, so they should track the Vec versions;
// the value-size axis shows where memcpy starts to dominate.
fn bench_value_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("value_size");

    for value_len in [32usize, 512, 2048] {
        let fx = build_table_with(
            10_000,
            Compression::None,
            FilterType::BinaryFuse16,
            16_384,
            value_len,
        );
        let keys = sample_keys(&fx, 256);
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        let mut reader = TableReader::open_with_options(&fx.path, with_cache(64 << 20)).unwrap();
        for _ in 0..2 {
            reader.get_many(&key_refs).unwrap(); // warm + promote
        }

        let id = |name: &str| BenchmarkId::new(name, format!("v{value_len}"));
        group.throughput(Throughput::Elements(keys.len() as u64));

        group.bench_function(id("get"), |b| {
            b.iter(|| {
                for k in &keys {
                    black_box(reader.get(black_box(k)).unwrap());
                }
            })
        });
        group.bench_function(id("get_in_bytes"), |b| {
            b.iter(|| {
                for k in &keys {
                    black_box(reader.get_in_bytes(black_box(k)).unwrap());
                }
            })
        });
        group.bench_function(id("get_many"), |b| {
            b.iter(|| black_box(reader.get_many(black_box(&key_refs)).unwrap()))
        });
        group.bench_function(id("get_many_in_bytes"), |b| {
            b.iter(|| black_box(reader.get_many_in_bytes(black_box(&key_refs)).unwrap()))
        });
    }
    group.finish();
}

// Open cost vs table size, and open + first lookup (cold index/filter pages).
fn bench_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("open");

    for n in [10_000usize, 100_000, 1_000_000] {
        let fx = build_table(n, Compression::None, FilterType::BinaryFuse8, 4096);
        let key = fx.keys[n / 2].clone();

        group.bench_function(BenchmarkId::new("open", n), |b| {
            b.iter(|| black_box(TableReader::open(black_box(&fx.path)).unwrap()))
        });
        group.bench_function(BenchmarkId::new("open_first_get", n), |b| {
            b.iter(|| {
                let mut r = TableReader::open(black_box(&fx.path)).unwrap();
                black_box(r.get(black_box(&key)).unwrap())
            })
        });
    }
    group.finish();
}

fn benches(c: &mut Criterion) {
    bench_contains_key(c);
    bench_value_sizes(c);
    bench_open(c);
}

criterion_group! {
    name = api_benches;
    config = Criterion::default()
        .sample_size(30)
        .measurement_time(std::time::Duration::from_secs(5));
    targets = benches
}
criterion_main!(api_benches);
