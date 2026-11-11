mod common;

use common::*;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng}; // rand 0.9; on 0.8 use gen_range instead of random_range
use std::hint::black_box;
use stryx::config::{Compression, FilterType};
use stryx::reader::TableReader;

#[derive(Clone, Copy)]
enum Access {
    Spread,
    Clustered,
    Hot90,
}
#[derive(Clone, Copy)]
enum Cache {
    Off,
    Hot,
}
#[derive(Clone, Copy)]
enum Op {
    Get,
    Miss,
    Many(usize),
}

struct Cell {
    name: String,
    spec: Spec,
    access: Access,
    cache: Cache,
    op: Op,
}

fn base() -> Spec {
    Spec {
        shape: Shape::Short,
        value_len: 32,
        values: Values::Compressible,
        compression: Compression::None,
        block_size: 4096,
        filter: FilterType::BinaryFuse8,
    }
}

fn cells() -> Vec<Cell> {
    let mut v = Vec::new();
    let mut add = |name: &str, spec: Spec, access, cache, op| {
        v.push(Cell {
            name: name.to_string(),
            spec,
            access,
            cache,
            op,
        });
    };
    use Access::*;
    use Cache::*;

    add("base", base(), Spread, Hot, Op::Get);

    for (n, shape) in [
        ("long", Shape::LongPrefix),
        ("tied", Shape::Tied),
        ("random", Shape::Random),
        ("varlen", Shape::VarLen),
    ] {
        add(
            &format!("shape={n}"),
            Spec { shape, ..base() },
            Spread,
            Hot,
            Op::Get,
        );
    }
    for len in [16usize, 256, 4096] {
        add(
            &format!("val={len}"),
            Spec {
                value_len: len,
                ..base()
            },
            Spread,
            Hot,
            Op::Get,
        );
    }
    for bs in [1024usize, 16_384, 65_536] {
        add(
            &format!("block={bs}"),
            Spec {
                block_size: bs,
                ..base()
            },
            Spread,
            Hot,
            Op::Get,
        );
    }
    add(
        "filter=none",
        Spec {
            filter: FilterType::None,
            ..base()
        },
        Spread,
        Hot,
        Op::Get,
    );

    // cache off: the cost of read + CRC + decompress. Random values so
    // compression has real work to do.
    for (n, c) in [
        ("none", Compression::None),
        ("lz4", Compression::Lz4),
        ("zstd1", Compression::Zstd(1)),
    ] {
        add(
            &format!("cold/comp={n}"),
            Spec {
                compression: c,
                values: Values::Random,
                value_len: 256,
                ..base()
            },
            Spread,
            Off,
            Op::Get,
        );
    }

    add("access=clustered", base(), Clustered, Hot, Op::Get);
    add("access=hot90", base(), Hot90, Hot, Op::Get);
    add("cold/access=clustered", base(), Clustered, Off, Op::Get);

    add("op=miss", base(), Spread, Hot, Op::Miss);
    add("op=many64/spread", base(), Spread, Hot, Op::Many(64));
    add("op=many64/clustered", base(), Clustered, Hot, Op::Many(64));
    add(
        "cold/op=many64/clustered",
        base(),
        Clustered,
        Off,
        Op::Many(64),
    );
    v
}

fn pick(fx: &Table, access: Access, n: usize, rng: &mut StdRng) -> Vec<Vec<u8>> {
    let total = fx.keys.len();
    match access {
        Access::Spread => (0..n)
            .map(|_| fx.keys[rng.random_range(0..total)].clone())
            .collect(),
        Access::Clustered => {
            let s = rng.random_range(0..total.saturating_sub(n).max(1));
            fx.keys[s..(s + n).min(total)].to_vec()
        }
        Access::Hot90 => {
            let hot = (total / 10).max(1);
            (0..n)
                .map(|_| {
                    let i = if rng.random_range(0..100) < 90 {
                        rng.random_range(0..hot)
                    } else {
                        rng.random_range(0..total)
                    };
                    fx.keys[i].clone()
                })
                .collect()
        }
    }
}

fn bench_workloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("workloads");
    let mut rng = StdRng::seed_from_u64(0x9E37_79B9_7F4A_7C15);

    for cell in cells() {
        let fx = build_spec(cell.spec);
        let klen = fx.keys[0].len();

        let opts = match cell.cache {
            Cache::Off => no_cache(),
            Cache::Hot => with_cache((fx.keys.len() * (klen + fx.value_len) * 2) as u64),
        };
        let mut reader = TableReader::open_with_options(&fx.path, opts).unwrap();

        let hits = pick(&fx, cell.access, 4096, &mut rng);
        let keys: Vec<Vec<u8>> = match cell.op {
            // k + 0x00 sorts right after k: in range, and absent
            Op::Miss => hits
                .iter()
                .map(|k| [k.as_slice(), &[0u8]].concat())
                .collect(),
            _ => hits,
        };
        let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();

        // warm up: a block enters the cache on its 2nd load
        for _ in 0..3 {
            for k in &keys {
                reader.get(k).unwrap();
            }
        }
        let found = keys
            .iter()
            .filter(|k| reader.get(k).unwrap().is_some())
            .count();
        match cell.op {
            Op::Miss => assert_eq!(found, 0, "{}: misses must miss", cell.name),
            _ => assert_eq!(found, keys.len(), "{}: hits must hit", cell.name),
        }

        group.throughput(Throughput::Elements(keys.len() as u64));
        group.bench_function(&cell.name, |b| match cell.op {
            Op::Get | Op::Miss => b.iter(|| {
                for k in &keys {
                    black_box(reader.get(black_box(k)).unwrap());
                }
            }),
            Op::Many(batch) => b.iter(|| {
                for chunk in refs.chunks(batch) {
                    black_box(reader.get_many(black_box(chunk)).unwrap());
                }
            }),
        });
    }
    group.finish();
}

criterion_group! {
    name = workloads;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(2));
    targets = bench_workloads
}
criterion_main!(workloads);
