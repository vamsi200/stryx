# stryx

An immutable, sorted key-value table format and reader/writer library in Rust, in the spirit of an SSTable.
You write a set of key-value pairs once, and then read it back with fast point lookups, batch lookups and ordered scans.

## Details
- **One file, read-only after writing.** Records are sorted by key and grouped into blocks.
- **Sparse index.** One entry per block, so memory use stays small even for large tables.
- **Optional membership filter** (Binary Fuse 8/16/32). Lookups for keys that aren't in the table return in tens of nanoseconds.
- **Optional block compression:** LZ4 or Zstd.
- **Integrity checks.** CRC32C on every block, the index and the filter.
- **Thread-friendly reads.** The heavy state (file, index, filter) is shared behind an `Arc`, and each thread gets its own cheap reader clone.
- **Optional decoded-block cache,** off by default.

> **Status:** early development. The on-disk format is versioned but not yet stable, so files written by one version may not be readable by the next.

## Quick start

```rust
use stryx::builder::TableBuilder;
use stryx::config::TableOptions;
use stryx::reader::TableReader;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Write a table. By default, records must already be lexicographically sorted.
    // Use KeyOrder::Lexicographic to sort records automatically before writing.
    let mut table = TableBuilder::create("fruit.stryx", TableOptions::default())?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.add(b"cherry", b"red")?;
    table.finalize()?;

    // 2. Open it and look things up.
    let mut reader = TableReader::open("fruit.stryx")?;

    assert_eq!(reader.get(b"banana")?, Some(b"yellow".to_vec()));
    assert_eq!(reader.get(b"durian")?, None);
    Ok(())
}
```

## Writing tables

Configure the table with `TableOptions`:

```rust
use stryx::builder::TableBuilder;
use stryx::config::{Compression, DuplicatePolicy, FilterType, KeyOrder, TableOptions};

let options = TableOptions {
    compression: Compression::Zstd,          // None | Lz4 | Zstd
    filter_type: FilterType::BinaryFuse16,   // None | BinaryFuse8 | BinaryFuse16 | BinaryFuse32
    block_size: 4096,                        // target size of a data block in bytes
    key_order: KeyOrder::Lexicographic,      // sort for me; Presorted = I promise it is sorted
    duplicate_policy: DuplicatePolicy::Reject, // Allow | Reject
    ..TableOptions::default()
};

let mut table = TableBuilder::create("data.stryx", options)?;
```

Batches work too:

```rust
use stryx::builder::Record;

let records = vec![
    Record { key: b"a", value: b"1" },
    Record { key: b"b", value: b"2" },
];
table.add_batch(records);
let (metadata, stats) = table.finalize()?;
```

`finalize` flushes the last block, builds the filter and writes the index, metadata and footer. It returns the table metadata and write statistics.

**Choosing options**

| Option | Guidance |
|--------|----------|
| `filter_type` | Use a filter when many lookups are for keys that aren't present. Fuse8 is the smallest. Fuse16 and Fuse32 have far fewer false positives. |
| `block_size` | Smaller blocks mean cheaper single lookups and a bigger index. Larger blocks compress better. Around 4 KB is a reasonable start for point-lookup workloads. |
| `compression` | Lz4 is fast, Zstd compresses more. Compression mostly matters for the cost of reading a block, so a block cache helps compressed tables the most. |
| `key_order` | `Presorted` skips sorting, but out-of-order keys are rejected. |

## Reading tables

### Point and batch lookups

```rust
let mut reader = TableReader::open("data.stryx")?;

// One key
let value: Option<Vec<u8>> = reader.get(b"some-key")?;

// Many keys. Results come back in the same order as the input.
let keys: Vec<&[u8]> = vec![b"a", b"zzz", b"b"];
let values: Vec<Option<Vec<u8>>> = reader.get_many(&keys)?;
```

`get_many` sorts the keys internally and loads each block only once, so batches of nearby keys are much cheaper than the same number of `get` calls.

### Sharing a table between threads

Open the table once, then clone the reader for each thread. A clone shares the file handle, index and filter, and only allocates its own scratch buffers.

```rust
let reader = TableReader::open("data.stryx")?;

std::thread::scope(|s| {
    for _ in 0..4 {
        let mut r = reader.clone();
        s.spawn(move || {
            let _ = r.get(b"some-key");
        });
    }
});
```

Read statistics (`reader.readstats`) are kept per reader. To get totals, read them from each clone and add them up.

### Reader options

```rust
use stryx::config::{BlockCacheOptions, ReaderOptions};

let opts = ReaderOptions {
    // Verify each block's CRC the first time it is read (default). Set to true to skip.
    disable_block_verification: false,
    // Cache up to 64 MiB of decoded blocks, shared by all readers of this table.
    block_cache: Some(BlockCacheOptions::bytes(64 << 20).build_cache()),
};

let mut reader = TableReader::open_with_options("data.stryx", opts)?;
```

The block cache is **off by default**. It helps when the same blocks are read repeatedly (a hot set that fits in the cache) and when blocks are compressed. For uniform random reads over a table much larger than the cache it only adds overhead. A block is added to the cache on its second load, so blocks that are read only once never pay for a cache insert.

Block checksums are verified **once per block** per open table. Corruption that happens after a block's first read isn't noticed until the table is opened again.

## File format

An Interactive site of the format - [LINK](https://vamsi200.github.io/stryx/)

```text
+------------------------------------------+
| data block 0 ... data block N-1          |   sorted records, optionally compressed
+------------------------------------------+
| index                                    |   first key, offset, size per block
| metadata                                 |   record count, key range, compression, ...
| filter section (optional)                |   Binary Fuse filter + CRC32C
| footer (29 bytes)                        |   index offset/size, filter type, version, magic
+------------------------------------------+
```

A data block holds sorted records followed by an offset table and a checksum:

```text
[record 0][record 1] ... [record N-1]
[offset 0: u32] ... [offset N-1: u32]
[record_count: u32][crc32c: u32]

record = [key_len: u16][value_len: u32][key][value]
```

All integers are little-endian. The reader starts from the footer at the end of the file, which tells it where everything else is.

A lookup does this:

1. The filter says "definitely absent" or "maybe present".
2. The index finds the one block that could contain the key.
3. The block is read (or taken from a cache), decompressed if needed, and checksummed on first use.
4. A binary search over the offset table finds the record.

## Benchmarks

Benchmarked using Criterion on `i5-12450h`, running `linux (6.18.38-1-lts)` and rust version - `1.96.0`. Benchmarks use the optimized build profile with debug information enabled.

### Point Lookups

| Operation | Configuration | Time | Throughput |
|---|---|---:|---:|
| `get` hit | No filter | 79.860 µs | 12.82 Melem/s |
| `get` hit | XOR filter, 8 bits/key | 96.995 µs | 10.56 Melem/s |
| `get` hit | XOR filter, 16 bits/key | 97.540 µs | 10.50 Melem/s |
| `get` hit | XOR filter, 32 bits/key | 99.296 µs | 10.31 Melem/s |
| `get` miss | Bloom filter, 8 bits/key | 9.540 µs | 107.34 Melem/s |
| `get` miss | Bloom filter, 16 bits/key | 9.248 µs | 110.73 Melem/s |
| `get` miss | Bloom filter, 32 bits/key | 9.374 µs | 109.24 Melem/s |
| `get` miss | No filter | 11.973 µs | 85.53 Melem/s |

### Table Opening

| Records | `open` | `open` + first `get` |
|---:|---:|---:|
| 10,000 | 14.771 µs | 18.203 µs |
| 100,000 | 82.167 µs | 84.691 µs |
| 1,000,000 | 739.210 µs | 734.030 µs |

### Batch Lookups

| Operation | Batch size | Time | Throughput |
|---|---:|---:|---:|
| `get_many` (hits) | 8 | 770.61 ns | 10.38 Melem/s |
| `get_many` (hits) | 64 | 8.062 µs | 7.94 Melem/s |
| `get_many` (hits) | 256 | 34.702 µs | 7.38 Melem/s |
| `get_many` (hits) | 1,024 | 197.720 µs | 5.18 Melem/s |
| `get_many` (mixed) | 8 | 499.36 ns | 16.02 Melem/s |
| `get_many` (mixed) | 64 | 5.675 µs | 11.28 Melem/s |
| `get_many` (mixed) | 256 | 26.740 µs | 9.57 Melem/s |

### Sequential Scans

| Configuration | Block size | Time | Throughput |
|---|---:|---:|---:|
| No compression | 4 KiB | 6.678 ms | 642.64 MiB/s |
| No compression | 16 KiB | 9.124 ms | 470.35 MiB/s |
| LZ4 | 4 KiB | 9.306 ms | 461.14 MiB/s |
| Zstd level 1 | 4 KiB | 14.006 ms | 306.41 MiB/s |

### Range and Prefix Queries

| Operation | Configuration | Time | Throughput |
|---|---|---:|---:|
| `range` | 10,000 records, no compression | 952.39 µs | 10.50 Melem/s |
| `range` | 10,000 records, LZ4 | 1.055 ms | 9.48 Melem/s |
| `prefix` | 10,000 matching records | 931.99 µs | 10.73 Melem/s |

### Concurrent Readers

Four-thread scaling for point lookups:

| Threads | Time | Throughput |
|---:|---:|---:|
| 1 | 93.329 µs | 10.97 Melem/s |
| 2 | 41.234 µs | 24.83 Melem/s |
| 4 | 25.608 µs | 39.99 Melem/s |

### Block Cache

The block cache has workload-dependent benefits. It provides little improvement for uncompressed uniform workloads, but can substantially improve repeated access to compressed hot data.

| Compression | Access pattern | Cache capacity | Time |
|---|---|---:|---:|
| None | Uniform | Disabled | 1.865 ms |
| None | Uniform | 125% | 1.873 ms |
| None | Sequential | Disabled | 531.21 µs |
| None | Sequential | 125% | 549.01 µs |
| LZ4 | Hot | Disabled | 5.052 ms |
| LZ4 | Hot | 50% | 2.446 ms |
| LZ4 | Hot | 125% | 1.599 ms |
| LZ4 | Sequential | Disabled | 552.98 µs |
| LZ4 | Sequential | 125% | 483.15 µs |


## Limitations

### Concurrency
- **Readers are per-thread handles over shared state.** Lookups take `&mut self`, so a single `TableReader` can't be used from several threads at once. To read in parallel, give each thread its own reader with `clone()`. Clones share the open file, index, filter, metadata and block cache (all behind an `Arc`), and each clone gets its own scratch buffers.
- **Stats are per reader.** `readstats` is not aggregated across clones, so sum them yourself if you need totals.
- **The block cache and the "seen" flags are shared between clones.** A block loaded twice by any combination of readers is promoted to the cache. Concurrent readers may each decode the same cold block once before it lands in the cache.

### Read API
- **Point lookups only.** `get` and `get_many` (plus `get_in_bytes` and `get_many_in_bytes`) are the supported read paths.
- **Values are always copied out.** Every lookup copies the value into a new allocation, either a `Vec<u8>` or a `Bytes`. There is no zero-copy or borrowed-value API. `get_in_bytes` and `get_many_in_bytes` are convenience wrappers: the `Bytes` they return owns its own allocation and doesn't retain block or cache memory, but producing it is not zero-copy. Cloning or slicing it afterwards is cheap.
- **`get_many` is all-or-nothing.** The first block error aborts the call and no partial results are returned.
- **Duplicate keys in `get_many`** are allowed and each gets its own result, but they are looked up independently.

### Integrity
- **Corruption detection only.** CRCs catch accidental corruption. There is no authentication or tamper resistance, and hardening against malicious files is not a goal yet.

### Filters
- **Binary fuse filters are probabilistic.** A false positive costs an index search and a block read before the key is found to be absent. `get` and `get_many` never return a wrong value because of it.
- **The filter is optional.** Without it, every miss goes through the index and block search.

### Format
- **Immutable files.** Tables are written once. There is no update, delete or append, and no compaction or merge.
- **Sparse index with one entry per block.** Lookup cost depends on block size. Smaller blocks mean a larger index and cheaper block decodes, and larger blocks mean the opposite. Benchmarks cover 1 KB, 4 KB and 16 KB blocks.
- **Compression is per table** (None, Lz4 or Zstd), chosen at write time. It can't be mixed across blocks.

## Building and testing

```bash
cargo build --release
cargo test
```

## License

See [LICENSE](LICENSE).
