mod common;

use common::*;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::hint::black_box;
use stryx::config::{Compression, FilterType};
use stryx::reader::TableReader;

const LOOKUPS: usize = 8192;

fn make_keys(pattern: &str, fx: &Table) -> Vec<Vec<u8>> {
    let n = fx.keys.len();
    let mut rng = StdRng::seed_from_u64(0x9E37_79B9_7F4A_7C15);
    match pattern {
        "uniform" => (0..LOOKUPS)
            .map(|_| fx.keys[rng.random_range(0..n)].clone())
            .collect(),
        "hot" => {
            let hot = n / 10;
            (0..LOOKUPS)
                .map(|_| {
                    let i = if rng.random_range(0..100) < 90 {
                        rng.random_range(0..hot)
                    } else {
                        rng.random_range(0..n)
                    };
                    fx.keys[i].clone()
                })
                .collect()
        }
        "sequential" => {
            let start = rng.random_range(0..n - LOOKUPS);
            fx.keys[start..start + LOOKUPS].to_vec()
        }
        _ => unreachable!(),
    }
}

// Cache capacity is a percentage of the table's file size (0 = cache off,
// 125 = everything fits). Compression matters here: a hit skips decompression.
fn bench_cache(c: &mut Criterion) {
    let n = 200_000;

    for compression in [Compression::None, Compression::Lz4] {
        let fx = build_table_with(n, compression, FilterType::BinaryFuse8, 4096, 64);
        let file_size = std::fs::metadata(&fx.path).unwrap().len();

        for pattern in ["uniform", "hot", "sequential"] {
            let mut group = c.benchmark_group(format!("cache/{compression:?}/{pattern}"));
            let keys = make_keys(pattern, &fx);
            group.throughput(Throughput::Elements(keys.len() as u64));

            for pct in [0u64, 10, 50, 125] {
                let opts = if pct == 0 {
                    no_cache()
                } else {
                    with_cache((file_size * pct / 100).max(1 << 16))
                };
                let mut reader = TableReader::open_with_options(&fx.path, opts).unwrap();

                // blocks only enter the cache on their 2nd load, so warm over several passes
                for _ in 0..5 {
                    for k in &keys {
                        reader.get(k).unwrap();
                    }
                }
                reader.readstats.reset();

                group.bench_function(BenchmarkId::new("get", format!("cap{pct}pct")), |b| {
                    b.iter(|| {
                        for k in &keys {
                            black_box(reader.get(black_box(k)).unwrap());
                        }
                    })
                });
                println!(
                    "{compression:?} {pattern} cap{pct}%: {:?}",
                    reader.readstats
                );
            }
            group.finish();
        }
    }
}

criterion_group! {
    name = cache_benches;
    config = Criterion::default()
        .sample_size(30)
        .measurement_time(std::time::Duration::from_secs(5));
    targets = bench_cache
}
criterion_main!(cache_benches);
