#![allow(unused)]

use core::panic;
use std::assert_matches;
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::VERSION;
use crate::builder::{Record, TableBuilder};
use crate::config::{Compression, DuplicatePolicy, FilterType, KeyOrder, TableOptions};
use crate::error::StryxError;
use crate::reader::{OwnedRecord, TableReader};

type TestResult = Result<(), Box<dyn Error>>;

struct TestTable {
    path: PathBuf,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn test_path() -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    std::env::temp_dir().join(format!("stryx-test-{}-{id}.stryx", std::process::id()))
}

impl TestTable {
    fn new() -> Result<Self, Box<dyn Error>> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        let path =
            std::env::temp_dir().join(format!("stryx-test-{}-{id}.stryx", std::process::id()));

        let mut table = TableBuilder::create(&path, TableOptions::default())?;

        for (key, value) in [
            (b"app".as_slice(), b"root".as_slice()),
            (b"apple", b"red"),
            (b"application", b"software"),
            (b"apply", b"verb"),
            (b"banana", b"yellow"),
            (b"berry", b"purple"),
            (b"cherry", b"red"),
            (b"date", b"brown"),
        ] {
            table.add(key, value)?;
        }

        table.finalize()?;

        Ok(Self { path })
    }

    fn open(&self) -> Result<TableReader, Box<dyn Error>> {
        Ok(TableReader::open(&self.path)?)
    }
}

impl Drop for TestTable {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn test_get() -> TestResult {
    let table = TestTable::new()?;
    let mut reader = table.open()?;

    assert_eq!(reader.get(b"app")?, Some(b"root".to_vec()));
    assert_eq!(reader.get(b"apple")?, Some(b"red".to_vec()));
    assert_eq!(reader.get(b"banana")?, Some(b"yellow".to_vec()));
    assert_eq!(reader.get(b"date")?, Some(b"brown".to_vec()));

    assert_eq!(reader.get(b"missing")?, None);
    assert_eq!(reader.get(b"ap")?, None);
    assert_eq!(reader.get(b"zebra")?, None);
    assert_eq!(reader.get(b"")?, None);

    Ok(())
}

#[test]
fn test_contains_key() -> TestResult {
    let table = TestTable::new()?;
    let mut reader = table.open()?;

    assert!(reader.contains_key(b"app")?);
    assert!(reader.contains_key(b"application")?);
    assert!(reader.contains_key(b"date")?);

    assert!(!reader.contains_key(b"ap")?);
    assert!(!reader.contains_key(b"apples")?);
    assert!(!reader.contains_key(b"unknown")?);

    Ok(())
}

#[test]
fn test_prefix() -> TestResult {
    let table = TestTable::new()?;
    let mut reader = table.open()?;

    let records = reader.prefix(b"app")?.collect::<Result<Vec<_>, _>>()?;

    let keys: Vec<_> = records.iter().map(|record| record.key.as_slice()).collect();

    assert_eq!(
        keys,
        vec![b"app".as_slice(), b"apple", b"application", b"apply",]
    );

    assert_eq!(records[0].value, b"root");
    assert_eq!(records[1].value, b"red");
    assert_eq!(records[2].value, b"software");
    assert_eq!(records[3].value, b"verb");

    let records = reader.prefix(b"ban")?.collect::<Result<Vec<_>, _>>()?;

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].key, b"banana");

    let records = reader.prefix(b"xyz")?.collect::<Result<Vec<_>, _>>()?;

    assert!(records.is_empty());

    Ok(())
}

#[test]
fn test_range() -> TestResult {
    let table = TestTable::new()?;
    let mut reader = table.open()?;

    let records = reader
        .range(b"apple", b"cherry")?
        .collect::<Result<Vec<_>, _>>()?;

    let keys: Vec<_> = records.iter().map(|record| record.key.as_slice()).collect();

    assert_eq!(
        keys,
        vec![
            b"apple".as_slice(),
            b"application",
            b"apply",
            b"banana",
            b"berry",
        ]
    );

    let records = reader
        .range(b"apq", b"cat")?
        .collect::<Result<Vec<_>, _>>()?;

    let keys: Vec<_> = records.iter().map(|record| record.key.as_slice()).collect();

    assert_eq!(keys, vec![b"banana".as_slice(), b"berry",]);

    let records = reader.range(b"x", b"z")?.collect::<Result<Vec<_>, _>>()?;

    assert!(records.is_empty());

    Ok(())
}

#[test]
fn test_get_many() -> TestResult {
    let table = TestTable::new()?;
    let mut reader = table.open()?;

    let keys = [b"apple".as_slice(), b"banana", b"date"];

    let values = reader.get_many(&keys)?;

    assert_eq!(
        values,
        vec![
            Some(b"red".to_vec()),
            Some(b"yellow".to_vec()),
            Some(b"brown".to_vec()),
        ]
    );

    Ok(())
}

#[test]
fn test_iteration_blocks() -> TestResult {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "stryx-block-test-{}-{id}.stryx",
        std::process::id()
    ));

    let mut table = TableBuilder::create(&path, TableOptions::default())?;

    for i in 0..10_000u32 {
        let key = format!("key-{i:08}");
        let value = format!("value-{i:08}");

        table.add(key.as_bytes(), value.as_bytes())?;
    }

    let (metadata, _) = table.finalize()?;

    assert_eq!(metadata.record_count, 10_000);
    assert!(metadata.block_count > 1);

    let mut reader = TableReader::open(&path)?;

    let records = reader
        .range(b"key-00004990", b"key-00005010")?
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(records.len(), 20);

    for (i, record) in records.iter().enumerate() {
        let n = 4990 + i;
        assert_eq!(record.key, format!("key-{n:08}").as_bytes());
        assert_eq!(record.value, format!("value-{n:08}").as_bytes());
    }

    drop(reader);
    std::fs::remove_file(path)?;

    Ok(())
}

#[test]
fn test_duplicate_keys() -> TestResult {
    let path = test_path();

    let mut table = TableBuilder::create(&path, TableOptions::default())?;

    table.add(b"apple", b"first")?;

    assert!(matches!(
        table.add(b"apple", b"second"),
        Err(StryxError::DuplicateKey)
    ));

    table.add(b"banana", b"third")?;

    let (metadata, _) = table.finalize()?;
    assert_eq!(metadata.record_count, 2);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_unsorted_keys() -> TestResult {
    let path = test_path();

    let mut table = TableBuilder::create(&path, TableOptions::default())?;

    table.add(b"banana", b"first")?;

    assert!(matches!(
        table.add(b"apple", b"second"),
        Err(StryxError::InvalidKeyOrder)
    ));

    table.add(b"cherry", b"third")?;

    let (metadata, _) = table.finalize()?;
    assert_eq!(metadata.record_count, 2);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_prefix_edge_cases() -> TestResult {
    let table = TestTable::new()?;
    let mut reader = table.open()?;

    let all = reader.prefix(b"")?.collect::<Result<Vec<_>, _>>()?;

    assert_eq!(all.len(), 8);

    let records = reader.prefix(b"apple")?.collect::<Result<Vec<_>, _>>()?;

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].key, b"apple");

    let records = reader
        .prefix(b"appleeeee")?
        .collect::<Result<Vec<_>, _>>()?;

    assert!(records.is_empty());

    let records = reader.prefix(b"zzz")?.collect::<Result<Vec<_>, _>>()?;

    assert!(records.is_empty());

    Ok(())
}

#[test]
fn test_truncated_file() -> TestResult {
    let path = test_path();

    let mut table = TableBuilder::create(&path, TableOptions::default())?;
    table.add(b"hello", b"world")?;
    table.finalize()?;

    let file = std::fs::OpenOptions::new().write(true).open(&path)?;

    file.set_len(10)?;

    assert!(TableReader::open(&path).is_err());

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_binary_keys() -> TestResult {
    let path = test_path();

    let mut table = TableBuilder::create(&path, TableOptions::default())?;

    table.add(&[0x00], &[0xFF])?;
    table.add(&[0x00, 0x00], &[0x01])?;
    table.add(&[0x00, 0xFF], &[0x02])?;
    table.add(&[0x01], &[0x03])?;
    table.add(&[0xFF], &[0x04])?;

    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.get(&[0x00])?, Some(vec![0xFF]));
    assert_eq!(reader.get(&[0xFF])?, Some(vec![0x04]));

    let records = reader.prefix(&[0x00])?.collect::<Result<Vec<_>, _>>()?;

    assert_eq!(records.len(), 3);

    assert_eq!(records[0].key, vec![0x00]);
    assert_eq!(records[1].key, vec![0x00, 0x00]);
    assert_eq!(records[2].key, vec![0x00, 0xFF]);

    std::fs::remove_file(path)?;
    Ok(())
}

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

#[test]
fn test_compression() -> TestResult {
    let (keys, values) = random_records(1000, true);

    for compression in [Compression::None, Compression::Lz4, Compression::Zstd] {
        let path = test_path();

        let options = TableOptions {
            compression,
            ..TableOptions::default()
        };

        let mut table = TableBuilder::create(&path, options)?;

        let records: Vec<Record<'_>> = keys
            .iter()
            .zip(&values)
            .map(|(key, value)| Record { key, value })
            .collect();

        let record_count = records.len();
        table.add_batch(records);
        table.finalize()?;

        let mut reader = TableReader::open(&path)?;

        assert_eq!(
            reader.metadata().record_count,
            record_count as u64,
            "Record count mismatch with {compression:?}"
        );

        for (key, value) in keys.iter().zip(&values) {
            assert_eq!(
                reader.get(key)?,
                Some(value.clone()),
                "get() mismatch with {compression:?}: {key:?}"
            );

            assert!(
                reader.contains_key(key)?,
                "contains_key() failed with {compression:?}: {key:?}"
            );
        }

        for key in [b"__missing_key__".as_slice(), b"not_a_real_record", b""] {
            assert_eq!(
                reader.get(key)?,
                None,
                "Unexpected get() result with {compression:?}: {key:?}"
            );

            assert!(
                !reader.contains_key(key)?,
                "Unexpected contains_key() result with {compression:?}: {key:?}"
            );
        }

        for (key, value) in keys.iter().zip(&values).take(100) {
            if key.is_empty() {
                continue;
            }

            let prefix = &key[..1];

            let actual: Vec<_> = reader
                .prefix(prefix)?
                .map(|r| r.map(|rec| (rec.key, rec.value)))
                .collect::<Result<_, _>>()?;

            let expected: Vec<_> = keys
                .iter()
                .zip(&values)
                .filter(|(k, _)| k.starts_with(prefix))
                .collect();

            assert_eq!(
                actual.len(),
                expected.len(),
                "prefix() result count mismatch with {compression:?}: {prefix:?}"
            );

            for ((actual_key, actual_value), (expected_key, expected_value)) in
                actual.iter().zip(expected)
            {
                assert_eq!(
                    actual_key, expected_key,
                    "prefix() key mismatch with {compression:?}"
                );

                assert_eq!(
                    actual_value, expected_value,
                    "prefix() value mismatch with {compression:?}"
                );
            }

            assert!(
                actual.iter().any(|(k, v)| k == key && v == value),
                "prefix() missed {key:?} with {compression:?}"
            );
        }

        assert!(
            std::fs::metadata(&path)?.len() > 0,
            "Empty SSTable with {compression:?}"
        );

        drop(reader);
        std::fs::remove_file(path)?;
    }

    Ok(())
}

#[test]
fn test_lexicographic_key_order() -> Result<(), StryxError> {
    let path = test_path();

    let mut options = TableOptions::default();
    options.key_order = crate::config::KeyOrder::Lexicographic;
    options.compression = Compression::None;

    let (keys, values) = random_records(1000, false);

    let records: Vec<Record<'_>> = keys
        .iter()
        .zip(&values)
        .map(|(key, value)| Record { key, value })
        .collect();

    assert!(
        records.windows(2).any(|w| w[0].key > w[1].key),
        "Test data unexpectedly sorted"
    );

    let expected_count = records.len();

    let mut table = TableBuilder::create(&path, options)?;

    table.add_batch(records)?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.metadata().record_count, expected_count as u64);

    let actual: Vec<OwnedRecord> = reader.iter().collect::<Result<Vec<_>, _>>()?;

    assert_eq!(actual.len(), expected_count);

    for pair in actual.windows(2) {
        assert!(
            pair[0].key <= pair[1].key,
            "Keys are not lexicographically sorted"
        );
    }

    for (key, value) in keys.iter().zip(&values) {
        let actual_value = reader.get(key)?;

        assert_eq!(
            actual_value.as_deref(),
            Some(value.as_slice()),
            "Value mismatch for key"
        );
    }

    Ok(())
}

#[test]
fn test_empty_table() -> TestResult {
    let path = test_path();

    let table = TableBuilder::create(&path, TableOptions::default())?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.metadata().record_count, 0);
    assert!(reader.get(b"missing")?.is_none());

    let records: Vec<OwnedRecord> = reader.iter().collect::<Result<Vec<_>, _>>()?;

    assert!(records.is_empty());

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_duplicate_keys_allowed() -> TestResult {
    let path = test_path();

    let mut options = TableOptions::default();
    options.duplicate_policy = DuplicatePolicy::Allow;

    let mut table = TableBuilder::create(&path, options)?;

    table.add(b"apple", b"first")?;
    table.add(b"apple", b"second")?;
    table.add(b"apple", b"third")?;
    table.add(b"banana", b"yellow")?;

    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.metadata().record_count, 4);

    let records: Vec<OwnedRecord> = reader.iter().collect::<Result<Vec<_>, _>>()?;

    assert_eq!(records.len(), 4);
    assert_eq!(records[0].key, b"apple");
    assert_eq!(records[1].key, b"apple");
    assert_eq!(records[2].key, b"apple");

    assert_eq!(records[0].value, b"first");
    assert_eq!(records[1].value, b"second");
    assert_eq!(records[2].value, b"third");

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_duplicate_keys_across_blocks() -> TestResult {
    let path = test_path();

    let mut options = TableOptions::default();
    options.duplicate_policy = DuplicatePolicy::Allow;
    options.block_size = 1;

    let mut table = TableBuilder::create(&path, options)?;

    table.add(b"apple", b"first")?;
    table.add(b"apple", b"second")?;
    table.add(b"apple", b"third")?;

    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    let records: Vec<OwnedRecord> = reader.iter().collect::<Result<Vec<_>, _>>()?;

    assert_eq!(records.len(), 3);

    for record in &records {
        assert_eq!(record.key, b"apple");
    }

    assert_eq!(records[0].value, b"first");
    assert_eq!(records[1].value, b"second");
    assert_eq!(records[2].value, b"third");

    assert!(reader.get(b"apple")?.is_some());

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_oversized_record() -> TestResult {
    let path = test_path();

    let mut options = TableOptions::default();
    options.block_size = 64;

    let key = vec![b'k'; 128];
    let value = vec![b'v'; 4096];

    let mut table = TableBuilder::create(&path, options)?;
    table.add(&key, &value)?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.get(&key)?.as_deref(), Some(value.as_slice()));

    let stats = reader.stats();

    assert_eq!(stats.blocks_read, 1);
    assert_eq!(stats.blocks_decompressed, 0);
    assert_eq!(stats.point_lookups, 1);
    assert_eq!(stats.point_hits, 1);
    assert_eq!(stats.point_misses, 0);
    assert_eq!(stats.checksum_failures, 0);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_record_at_block_size_boundary() -> TestResult {
    let path = test_path();

    let key = b"key";
    let value = vec![b'x'; 64];

    let mut options = TableOptions::default();
    options.block_size = key.len() + value.len() + 6;

    let mut table = TableBuilder::create(&path, options)?;

    table.add(key, &value)?;
    table.add(b"next", b"value")?;

    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.get(key)?.as_deref(), Some(value.as_slice()));
    assert_eq!(reader.get(b"next")?.as_deref(), Some(b"value".as_slice()));

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_first_and_last_keys() -> TestResult {
    let table = TestTable::new()?;
    let mut reader = table.open()?;

    assert_eq!(reader.get(b"app")?.as_deref(), Some(b"root".as_slice()));

    assert_eq!(reader.get(b"date")?.as_deref(), Some(b"brown".as_slice()));

    assert!(reader.get(b"aaa")?.is_none());
    assert!(reader.get(b"zebra")?.is_none());

    Ok(())
}

#[test]
fn test_large_record_count() -> TestResult {
    let path = test_path();

    let mut options = TableOptions::default();
    options.block_size = 4096;

    let mut table = TableBuilder::create(&path, options)?;

    let count = 10_000usize;

    for i in 0..count {
        let key = format!("key-{i:08}");
        let value = format!("value-{i:08}");

        table.add(key.as_bytes(), value.as_bytes())?;
    }

    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.metadata().record_count, count as u64);

    for i in [0, 1, 100, 999, 5000, 9999] {
        let key = format!("key-{i:08}");
        let value = format!("value-{i:08}");

        assert_eq!(
            reader.get(key.as_bytes())?.as_deref(),
            Some(value.as_bytes())
        );
    }

    let records: Vec<OwnedRecord> = reader.iter().collect::<Result<Vec<_>, _>>()?;

    assert_eq!(records.len(), count);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_empty_batch() -> TestResult {
    let path = test_path();

    let mut table = TableBuilder::create(&path, TableOptions::default())?;

    table.add(b"apple", b"red")?;
    table.add_batch(Vec::<Record<'_>>::new())?;
    table.add(b"banana", b"yellow")?;

    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.metadata().record_count, 2);
    assert_eq!(reader.get(b"apple")?.as_deref(), Some(b"red".as_slice()));
    assert_eq!(
        reader.get(b"banana")?.as_deref(),
        Some(b"yellow".as_slice())
    );

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_lexicographic_sort_rejects_duplicates() -> TestResult {
    let path = test_path();

    let mut options = TableOptions::default();
    options.key_order = KeyOrder::Lexicographic;
    options.duplicate_policy = DuplicatePolicy::Reject;

    let mut table = TableBuilder::create(&path, options)?;

    table.add(b"banana", b"yellow")?;
    table.add(b"apple", b"red")?;
    table.add(b"apple", b"green")?;

    let result = table.finalize();

    assert!(matches!(result, Err(StryxError::DuplicateKey)));

    let _ = std::fs::remove_file(path);
    Ok(())
}

#[test]
fn test_lexicographic_sort_allows_duplicates() -> TestResult {
    let path = test_path();

    let mut options = TableOptions::default();
    options.key_order = KeyOrder::Lexicographic;
    options.duplicate_policy = DuplicatePolicy::Allow;

    let mut table = TableBuilder::create(&path, options)?;

    table.add(b"banana", b"yellow")?;
    table.add(b"apple", b"first")?;
    table.add(b"apple", b"second")?;
    table.add(b"cherry", b"red")?;

    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    let records: Vec<OwnedRecord> = reader.iter().collect::<Result<Vec<_>, _>>()?;

    assert_eq!(records.len(), 4);

    for pair in records.windows(2) {
        assert!(pair[0].key <= pair[1].key);
    }

    let apple_records: Vec<_> = records
        .iter()
        .filter(|record| record.key == b"apple")
        .collect();

    assert_eq!(apple_records.len(), 2);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_corrupted_data_block_checksum() -> TestResult {
    let path = test_path();

    let mut options = TableOptions::default();
    options.compression = Compression::None;

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let mut bytes = std::fs::read(&path)?;

    // Corrupt a byte in the data section.
    bytes[0] ^= 0xFF;

    std::fs::write(&path, bytes)?;

    let mut reader = TableReader::open(&path)?;

    let result = reader.get(b"apple");

    assert!(result.is_err());

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_corrupted_index_checksum() -> TestResult {
    let path = test_path();

    let mut table = TableBuilder::create(&path, TableOptions::default())?;

    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let reader = TableReader::open(&path)?;
    let metadata = reader.metadata().clone();

    let mut bytes = std::fs::read(&path)?;

    let checksum_offset = (metadata.index_offset + metadata.index_size - 4) as usize;

    bytes[checksum_offset] ^= 0xFF;

    std::fs::write(&path, bytes)?;

    assert_matches!(TableReader::open(&path), Err(StryxError::InvalidChecksum));

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_invalid_footer_magic() -> TestResult {
    let path = test_path();

    let mut table = TableBuilder::create(&path, TableOptions::default())?;

    table.add(b"apple", b"red")?;
    table.finalize()?;

    let mut bytes = std::fs::read(&path)?;

    let footer_offset = bytes.len() - 8;

    bytes[footer_offset..].copy_from_slice(b"INVALID!");

    std::fs::write(&path, bytes)?;

    assert_matches!(TableReader::open(&path), Err(StryxError::InvalidMagic));

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_filter_missing_key_binary_fuse8() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        filter_type: FilterType::BinaryFuse8,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.get(b"missing_key_xyz")?, None);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_filter_missing_key_binary_fuse16() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        filter_type: FilterType::BinaryFuse16,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.get(b"missing_key_xyz")?, None);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_filter_existing_key_binary_fuse8() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        filter_type: FilterType::BinaryFuse8,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.get(b"apple")?.as_deref(), Some(b"red".as_slice()));
    assert_eq!(
        reader.get(b"banana")?.as_deref(),
        Some(b"yellow".as_slice())
    );

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_filter_existing_key_binary_fuse16() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        filter_type: FilterType::BinaryFuse16,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.get(b"apple")?.as_deref(), Some(b"red".as_slice()));
    assert_eq!(
        reader.get(b"banana")?.as_deref(),
        Some(b"yellow".as_slice())
    );

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_filter_empty_table() -> TestResult {
    for filter_type in [
        FilterType::BinaryFuse8,
        FilterType::BinaryFuse16,
        FilterType::BinaryFuse32,
    ] {
        let path = test_path();

        let options = TableOptions {
            filter_type,
            ..TableOptions::default()
        };

        let table = TableBuilder::create(&path, options)?;
        table.finalize()?;

        let mut reader = TableReader::open(&path)?;

        assert_eq!(reader.metadata().record_count, 0);
        assert_eq!(reader.get(b"nonexistent")?, None);
        assert!(!reader.contains_key(b"nonexistent")?);

        std::fs::remove_file(path)?;
    }

    Ok(())
}

#[test]
fn test_filter_disabled() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        filter_type: FilterType::None,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert_eq!(reader.get(b"apple")?.as_deref(), Some(b"red".as_slice()));
    assert_eq!(
        reader.get(b"banana")?.as_deref(),
        Some(b"yellow".as_slice())
    );
    assert_eq!(reader.get(b"missing")?, None);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_filter_with_compression() -> TestResult {
    for compression in [Compression::Lz4, Compression::Zstd] {
        let path = test_path();

        let options = TableOptions {
            compression,
            filter_type: FilterType::BinaryFuse8,
            ..TableOptions::default()
        };

        let mut table = TableBuilder::create(&path, options)?;
        table.add(b"apple", b"red")?;
        table.add(b"banana", b"yellow")?;
        table.finalize()?;

        let mut reader = TableReader::open(&path)?;

        assert_eq!(reader.get(b"apple")?.as_deref(), Some(b"red".as_slice()));
        assert_eq!(
            reader.get(b"banana")?.as_deref(),
            Some(b"yellow".as_slice())
        );
        assert_eq!(reader.get(b"missing")?, None);

        std::fs::remove_file(path)?;
    }

    Ok(())
}

#[test]
fn test_filter_with_duplicate_allowance() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        duplicate_policy: DuplicatePolicy::Allow,
        filter_type: FilterType::BinaryFuse8,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"first")?;
    table.add(b"apple", b"second")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    assert!(reader.get(b"apple")?.is_some());
    assert_eq!(
        reader.get(b"banana")?.as_deref(),
        Some(b"yellow".as_slice())
    );
    assert_eq!(reader.get(b"missing")?, None);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_filter_negative_lookup_bypasses_data_and_index_reads() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        filter_type: FilterType::BinaryFuse16,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    for i in 0..1000u32 {
        let key = format!("present-key-{i:06}");
        let value = format!("value-{i:06}");
        table.add(key.as_bytes(), value.as_bytes())?;
    }
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    let initial_stats = reader.stats();
    assert_eq!(initial_stats.blocks_read, 0);

    let result = reader.get(b"definitely_missing_key_that_filter_rejects")?;
    assert_eq!(result, None);

    let stats_after = reader.stats();
    assert_eq!(stats_after.blocks_read, 0);
    assert_eq!(stats_after.blocks_decompressed, 0);
    assert_eq!(stats_after.point_misses, 1);

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_range_empty_key_mismatch() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        block_size: 64, // Small block size to force multiple blocks
        duplicate_policy: DuplicatePolicy::Allow,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;

    table.add(b"", b"val1")?;
    table.add(b"", b"val2")?;
    table.add(b"apple", b"val3")?;
    table.add(b"banana", b"val4")?;
    table.add(b"cherry", b"val5")?;
    table.finalize()?;

    let mut reader = TableReader::open(&path)?;

    let records: Vec<_> = reader
        .range(b"", b"cherry")?
        .collect::<Result<Vec<_>, _>>()?;

    // Expected: ["", "", "apple", "banana"] -> 4 records
    assert_eq!(records.len(), 4, "Records: {records:?}");

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_combo_compression_and_data_checksum_verification() -> TestResult {
    for compression in [Compression::None, Compression::Lz4, Compression::Zstd] {
        let path = test_path();

        let options = TableOptions {
            compression,
            block_size: 128,
            ..TableOptions::default()
        };

        let mut table = TableBuilder::create(&path, options)?;
        for i in 0..50u32 {
            let key = format!("key-{i:04}");
            let val = format!("value-payload-for-compression-{i:04}");
            table.add(key.as_bytes(), val.as_bytes())?;
        }
        table.finalize()?;

        {
            let mut reader = TableReader::open(&path)?;
            assert_eq!(
                reader.get(b"key-0025")?.as_deref(),
                Some(b"value-payload-for-compression-0025".as_slice())
            );
        }

        let mut bytes = std::fs::read(&path)?;
        bytes[4] ^= 0xFF;
        std::fs::write(&path, bytes)?;

        let mut reader = TableReader::open(&path)?;
        assert!(reader.get(b"key-0000").is_err());
        std::fs::remove_file(path)?;
    }

    Ok(())
}

#[test]
fn test_combo_filter_and_duplicate_spanning_blocks() -> TestResult {
    for filter_type in [
        FilterType::BinaryFuse8,
        FilterType::BinaryFuse16,
        FilterType::BinaryFuse32,
    ] {
        let path = test_path();

        let options = TableOptions {
            filter_type,
            duplicate_policy: DuplicatePolicy::Allow,
            key_order: KeyOrder::Lexicographic,
            block_size: 64,
            ..TableOptions::default()
        };

        let mut table = TableBuilder::create(&path, options)?;
        // Add unsorted duplicates to test Sorting, Duplicate, Filter and Small Block Size together
        for i in 0..20u32 {
            table.add(b"dup-key", format!("val-{i:02}").as_bytes())?;
            table.add(b"aaa-key", format!("first-{i:02}").as_bytes())?;
            table.add(b"zzz-key", format!("last-{i:02}").as_bytes())?;
        }
        table.finalize()?;

        let mut reader = TableReader::open(&path)?;

        assert_eq!(reader.metadata().record_count, 60);
        assert!(reader.contains_key(b"dup-key")?);
        assert!(reader.contains_key(b"aaa-key")?);
        assert!(reader.contains_key(b"zzz-key")?);
        assert!(!reader.contains_key(b"missing-key")?);

        let dup_records: Vec<_> = reader.prefix(b"dup-key")?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(dup_records.len(), 20);

        std::fs::remove_file(path)?;
    }

    Ok(())
}

#[test]
fn test_combo_block_size_mixed_large_and_small_records() -> TestResult {
    for compression in [Compression::None, Compression::Lz4, Compression::Zstd] {
        let path = test_path();

        let options = TableOptions {
            block_size: 128,
            compression,
            filter_type: FilterType::BinaryFuse16,
            ..TableOptions::default()
        };

        let small_val = b"tiny".to_vec();
        let huge_val = vec![b'X'; 8192]; // larger than target block_size

        let mut table = TableBuilder::create(&path, options)?;
        table.add(b"k1-small", &small_val)?;
        table.add(b"k2-huge", &huge_val)?;
        table.add(b"k3-small", &small_val)?;
        table.add(b"k4-huge", &huge_val)?;
        table.add(b"k5-small", &small_val)?;
        table.finalize()?;

        let mut reader = TableReader::open(&path)?;

        assert_eq!(reader.get(b"k1-small")?, Some(small_val.clone()));
        assert_eq!(reader.get(b"k2-huge")?, Some(huge_val.clone()));
        assert_eq!(reader.get(b"k3-small")?, Some(small_val.clone()));
        assert_eq!(reader.get(b"k4-huge")?, Some(huge_val.clone()));
        assert_eq!(reader.get(b"k5-small")?, Some(small_val.clone()));

        let range_res: Vec<_> = reader
            .range(b"k1-small", b"k5-small")?
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(range_res.len(), 4);
        assert_eq!(range_res[1].value, huge_val);
        assert_eq!(range_res[3].value, huge_val);

        std::fs::remove_file(path)?;
    }

    Ok(())
}

#[test]
fn test_combo_curated_option_matrix() -> TestResult {
    let matrix = [
        (
            64,
            Compression::None,
            FilterType::BinaryFuse8,
            DuplicatePolicy::Reject,
            KeyOrder::Presorted,
        ),
        (
            128,
            Compression::Lz4,
            FilterType::BinaryFuse16,
            DuplicatePolicy::Allow,
            KeyOrder::Lexicographic,
        ),
        (
            256,
            Compression::Zstd,
            FilterType::BinaryFuse32,
            DuplicatePolicy::Reject,
            KeyOrder::Lexicographic,
        ),
        (
            512,
            Compression::Lz4,
            FilterType::None,
            DuplicatePolicy::Allow,
            KeyOrder::Presorted,
        ),
    ];

    for (block_size, compression, filter_type, duplicate_policy, key_order) in matrix {
        let path = test_path();

        let options = TableOptions {
            block_size,
            compression,
            filter_type,
            duplicate_policy: duplicate_policy.clone(),
            key_order,
            ..TableOptions::default()
        };

        let mut table = TableBuilder::create(&path, options)?;

        let mut keys = vec![b"beta".as_slice(), b"alpha", b"gamma", b"delta"];
        if matches!(key_order, KeyOrder::Presorted) {
            keys.sort();
        }

        for k in &keys {
            table.add(k, b"v1")?;
            if matches!(duplicate_policy, DuplicatePolicy::Allow) {
                table.add(k, b"v2")?;
            }
        }

        table.finalize()?;

        let mut reader = TableReader::open(&path)?;
        for k in &keys {
            assert!(reader.contains_key(k)?);
            assert!(reader.get(k)?.is_some());
        }
        assert!(!reader.contains_key(b"omega")?);

        std::fs::remove_file(path)?;
    }

    Ok(())
}

fn replace_u64_in_slice(bytes: &mut [u8], old_val: u64, new_val: u64) -> bool {
    let target = old_val.to_le_bytes();
    if let Some(pos) = bytes.windows(8).rposition(|w| w == target) {
        bytes[pos..pos + 8].copy_from_slice(&new_val.to_le_bytes());
        true
    } else {
        false
    }
}

fn replace_u32_in_slice(bytes: &mut [u8], old_val: u32, new_val: u32) -> bool {
    let target = old_val.to_le_bytes();
    if let Some(pos) = bytes.windows(4).rposition(|w| w == target) {
        bytes[pos..pos + 4].copy_from_slice(&new_val.to_le_bytes());
        true
    } else {
        false
    }
}

#[test]
fn test_robustness_randomly_mutated_files() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        compression: Compression::Lz4,
        filter_type: FilterType::BinaryFuse8,
        block_size: 64,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    for i in 0..30u32 {
        table.add(format!("key-{i:04}").as_bytes(), b"some_value_payload")?;
    }
    table.finalize()?;

    let original_bytes = std::fs::read(&path)?;
    let mut rng = StdRng::seed_from_u64(12345);

    for _ in 0..100 {
        let mut mutated = original_bytes.clone();
        let pos = rng.random_range(0..mutated.len());
        mutated[pos] ^= rng.random_range(1..=255);

        std::fs::write(&path, &mutated)?;

        if let Ok(mut reader) = TableReader::open(&path) {
            let _ = reader.get(b"key-0005");
            let _ = reader.contains_key(b"key-0010");
            let _ = reader.prefix(b"key-").map(|it| it.collect::<Vec<_>>());
            let _ = reader
                .range(b"key-0000", b"key-0020")
                .map(|it| it.collect::<Vec<_>>());
            let _ = reader.iter().collect::<Vec<_>>();
        }
    }

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_robustness_offsets_pointing_outside_file() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        filter_type: FilterType::BinaryFuse8,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let reader = TableReader::open(&path)?;
    let meta = reader.metadata().clone();
    drop(reader);

    let original = std::fs::read(&path)?;
    let tail_start = meta.index_offset as usize;

    let mut bytes = original.clone();
    assert!(replace_u64_in_slice(
        &mut bytes[tail_start..],
        meta.index_offset,
        meta.file_size + 1024
    ));
    std::fs::write(&path, &bytes)?;
    assert!(TableReader::open(&path).is_err());

    let mut bytes = original;
    assert!(replace_u64_in_slice(
        &mut bytes[tail_start..],
        meta.index_size,
        meta.file_size + 1024
    ));
    std::fs::write(&path, &bytes)?;
    assert!(TableReader::open(&path).is_err());

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_robustness_integer_overflow_in_lengths_and_offsets() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        filter_type: FilterType::BinaryFuse8,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.add(b"banana", b"yellow")?;
    table.finalize()?;

    let reader = TableReader::open(&path)?;
    let meta = reader.metadata().clone();
    drop(reader);

    let original = std::fs::read(&path)?;
    let tail_start = meta.index_offset as usize;

    // set index_size to u64::MAX so index_offset + index_size overflows u64
    let mut bytes = original.clone();
    assert!(replace_u64_in_slice(
        &mut bytes[tail_start..],
        meta.index_size,
        u64::MAX
    ));
    std::fs::write(&path, &bytes)?;
    assert!(TableReader::open(&path).is_err());

    // set index_offset to u64::MAX so index_offset + index_size overflows u64
    let mut bytes = original;
    assert!(replace_u64_in_slice(
        &mut bytes[tail_start..],
        meta.index_offset,
        u64::MAX
    ));
    std::fs::write(&path, &bytes)?;
    assert!(TableReader::open(&path).is_err());

    std::fs::remove_file(path)?;
    Ok(())
}
#[test]
fn test_robustness_invalid_enum_discriminants() -> TestResult {
    let path = test_path();

    let options = TableOptions {
        compression: Compression::Lz4,
        filter_type: FilterType::BinaryFuse8,
        ..TableOptions::default()
    };

    let mut table = TableBuilder::create(&path, options)?;
    table.add(b"apple", b"red")?;
    table.finalize()?;

    let reader = TableReader::open(&path)?;
    let meta = reader.metadata().clone();
    drop(reader);

    let original = std::fs::read(&path)?;
    let post_index_start = (meta.index_offset + meta.index_size) as usize;
    let footer_magic_start = original.len() - 8;

    // Overwrite every single byte after the index (covering filter_type, compression, index_type) with 0xFE
    for pos in post_index_start..footer_magic_start {
        let mut bytes = original.clone();
        if bytes[pos] == 0xFE {
            continue;
        }
        bytes[pos] = 0xFE;
        std::fs::write(&path, &bytes)?;

        if let Ok(mut r) = TableReader::open(&path) {
            let _ = r.get(b"apple");
            let _ = r.iter().collect::<Vec<_>>();
        }
    }

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_robustness_inconsistent_metadata_and_index_counts() -> TestResult {
    let path = test_path();

    let mut table = TableBuilder::create(&path, TableOptions::default())?;
    for i in 0..5u32 {
        table.add(format!("key-{i}").as_bytes(), b"value")?;
    }
    table.finalize()?;

    let reader = TableReader::open(&path)?;
    let meta = reader.metadata().clone();
    drop(reader);

    let original = std::fs::read(&path)?;
    let post_index = (meta.index_offset + meta.index_size) as usize;

    // Corrupt block_count in the metadata/footer to claim 500 blocks exist when only 1 was the actual
    let mut bytes = original.clone();
    if replace_u32_in_slice(&mut bytes[post_index..], meta.block_count, 500) {
        std::fs::write(&path, &bytes)?;
        if let Ok(mut r) = TableReader::open(&path) {
            let _ = r.iter().collect::<Vec<_>>();
        }
    }

    // Corrupt block_count to 0 when 1 block exists
    let mut bytes = original;
    if replace_u32_in_slice(&mut bytes[post_index..], meta.block_count, 0) {
        std::fs::write(&path, &bytes)?;
        if let Ok(mut r) = TableReader::open(&path) {
            let _ = r.get(b"key-0");
            let _ = r.iter().collect::<Vec<_>>();
        }
    }

    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn test_robustness_filter_lengths_exceeding_remaining_file_size() -> TestResult {
    let path_no_filter = test_path();
    let path_with_filter = test_path();

    // Build identical table without filter and with filter to locate the filter section
    let mut t1 = TableBuilder::create(
        &path_no_filter,
        TableOptions {
            filter_type: FilterType::None,
            ..TableOptions::default()
        },
    )?;
    t1.add(b"apple", b"red")?;
    t1.add(b"banana", b"yellow")?;
    t1.finalize()?;

    let mut t2 = TableBuilder::create(
        &path_with_filter,
        TableOptions {
            filter_type: FilterType::BinaryFuse8,
            ..TableOptions::default()
        },
    )?;
    t2.add(b"apple", b"red")?;
    t2.add(b"banana", b"yellow")?;
    t2.finalize()?;

    let bytes_no_filter = std::fs::read(&path_no_filter)?;
    let bytes_with_filter = std::fs::read(&path_with_filter)?;

    let filter_section_len = bytes_with_filter.len() - bytes_no_filter.len();
    assert!(filter_section_len > 24);

    // Find the first byte where the two files diverge (the start of the filter section)
    let filter_start = bytes_no_filter
        .iter()
        .zip(&bytes_with_filter)
        .position(|(a, b)| a != b)
        .expect("files with and without filter must differ");

    let mut corrupted = bytes_with_filter.clone();

    // BinaryFuse header layout: seed (8) + seg_len (4) + mask (4) + seg_count_len (4) + fp_len (4)
    // Overwrite `fp_len` at filter_start + 20 with a length larger than the file
    let fp_len_offset = filter_start + 20;
    let bogus_fp_len = (corrupted.len() as u32) + 50_000;
    corrupted[fp_len_offset..fp_len_offset + 4].copy_from_slice(&bogus_fp_len.to_le_bytes());

    // Recompute the filter CRC32C checksum, so it passes checksum
    // verification and directly hits `decode_fuse8`'s remaining-length validation
    let filter_data_end = filter_start + filter_section_len - 4;
    let new_crc = crc32c::crc32c(&corrupted[filter_start..filter_data_end]);
    corrupted[filter_data_end..filter_data_end + 4].copy_from_slice(&new_crc.to_le_bytes());

    std::fs::write(&path_with_filter, &corrupted)?;

    assert!(TableReader::open(&path_with_filter).is_err());

    std::fs::remove_file(path_no_filter)?;
    std::fs::remove_file(path_with_filter)?;
    Ok(())
}
