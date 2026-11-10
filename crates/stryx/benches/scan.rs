mod common;

use common::*;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use stryx::config::{Compression, FilterType};
use stryx::reader::TableReader;

/// Full scan. Throughput is logical (key + value) bytes per second, so
/// compression types compare on the same scale.
fn bench_iter(c: &mut Criterion) {
    let mut group = c.benchmark_group("iter");
    let n = 100_000;

    for compression in [Compression::None, Compression::Lz4, Compression::Zstd(1)] {
        for block_size in [4096usize, 16_384] {
            let fx = build_table(n, compression, FilterType::None, block_size);
            let mut reader = TableReader::open_with_options(&fx.path, no_cache()).unwrap();
            assert_eq!(drain(reader.iter()), n);

            group.throughput(Throughput::Bytes((n * (KEY_LEN + fx.value_len)) as u64));
            group.bench_function(
                BenchmarkId::new(format!("{compression:?}"), format!("bs{block_size}")),
                |b| b.iter(|| black_box(drain(reader.iter()))),
            );
        }
    }
    group.finish();
}

/// Does a scan benefit from the block cache? If cache == no_cache, scans bypass it.
fn bench_iter_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("iter/cache");
    let n = 100_000;
    let fx = build_table(n, Compression::Lz4, FilterType::None, 4096);

    group.throughput(Throughput::Bytes((n * (KEY_LEN + fx.value_len)) as u64));
    for (name, opts) in [
        ("no_cache", no_cache()),
        ("cache_64mb", with_cache(64 << 20)),
    ] {
        let mut reader = TableReader::open_with_options(&fx.path, opts).unwrap();
        for _ in 0..3 {
            drain(reader.iter()); // warm (promotion happens on the 2nd load)
        }
        group.bench_function(name, |b| b.iter(|| black_box(drain(reader.iter()))));
    }
    group.finish();
}

/// Range scans of 1 / 100 / 10k records. len=1 is the fixed setup cost
/// (index search + first block load); large lens show steady-state cost.
fn bench_range(c: &mut Criterion) {
    let mut group = c.benchmark_group("range");
    let n = 200_000;

    for compression in [Compression::None, Compression::Lz4] {
        let fx = build_table(n, compression, FilterType::None, 4096);
        let mut reader = TableReader::open_with_options(&fx.path, no_cache()).unwrap();

        for len in [1usize, 100, 10_000] {
            // rotate start positions so we don't re-read the same blocks every iteration
            let starts: Vec<usize> = (0..64).map(|i| i * (n - len - 1) / 64).collect();

            let probe = drain(
                reader
                    .range(&fx.keys[starts[0]], &fx.keys[starts[0] + len])
                    .unwrap(),
            );
            assert!(
                probe == len || probe == len + 1,
                "range returned {probe} for len {len}"
            );

            group.throughput(Throughput::Elements(probe as u64));
            let mut i = 0usize;
            group.bench_function(BenchmarkId::new(format!("{compression:?}"), len), |b| {
                b.iter(|| {
                    let s = starts[i % starts.len()];
                    i += 1;
                    black_box(drain(
                        reader
                            .range(black_box(&fx.keys[s]), black_box(&fx.keys[s + len]))
                            .unwrap(),
                    ))
                })
            });
        }
    }
    group.finish();
}

/// Prefix scans matching 10 / 100 / 1000 / 10000 keys, plus a prefix that matches nothing.
fn bench_prefix(c: &mut Criterion) {
    let mut group = c.benchmark_group("prefix");
    let n = 100_000;
    let fx = build_table(n, Compression::None, FilterType::None, 4096);
    let mut reader = TableReader::open_with_options(&fx.path, no_cache()).unwrap();
    let mid = &fx.keys[n / 2];

    // key = "key-" + 10 digits, so each extra digit divides the match count by 10
    for digits in [9usize, 8, 7, 6] {
        let p = &mid[..4 + digits];
        let expected = fx.keys.iter().filter(|k| k.starts_with(p)).count();
        assert_eq!(drain(reader.prefix(p).unwrap()), expected);

        group.throughput(Throughput::Elements(expected as u64));
        group.bench_function(BenchmarkId::new("match", expected), |b| {
            b.iter(|| black_box(drain(reader.prefix(black_box(p)).unwrap())))
        });
    }

    let none: &[u8] = b"zzzz";
    assert_eq!(drain(reader.prefix(none).unwrap()), 0);
    group.throughput(Throughput::Elements(1)); // per call
    group.bench_function("no_match", |b| {
        b.iter(|| black_box(drain(reader.prefix(black_box(none)).unwrap())))
    });

    group.finish();
}

fn scan_ranges(reader: &mut TableReader, ranges: &[(Vec<u8>, Vec<u8>)]) {
    for (s, e) in ranges {
        black_box(drain(reader.range(s, e).unwrap()));
    }
}

/// Same total work (8 disjoint ranges of 10k keys) at 1/2/4 threads.
fn bench_mt_range(c: &mut Criterion) {
    let mut group = c.benchmark_group("mt_range");
    let n = 200_000;
    let fx = build_table(n, Compression::Lz4, FilterType::None, 4096);
    let base = TableReader::open_with_options(&fx.path, no_cache()).unwrap();

    let ranges: Vec<(Vec<u8>, Vec<u8>)> = (0..8)
        .map(|i| {
            let s = i * 25_000;
            (fx.keys[s].clone(), fx.keys[s + 10_000].clone())
        })
        .collect();

    group.throughput(Throughput::Elements(8 * 10_000));
    for threads in [1usize, 2, 4] {
        group.bench_function(BenchmarkId::new("lz4", threads), |b| {
            b.iter_custom(|iters| run_threads(&base, &ranges, threads, iters, scan_ranges));
        });
    }
    group.finish();
}

fn benches(c: &mut Criterion) {
    bench_iter(c);
    bench_iter_cache(c);
    bench_range(c);
    bench_prefix(c);
    bench_mt_range(c);
}

criterion_group! {
    name = scan_benches;
    config = Criterion::default()
        .sample_size(30)
        .measurement_time(std::time::Duration::from_secs(5));
    targets = benches
}
criterion_main!(scan_benches);
