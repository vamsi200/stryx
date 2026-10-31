#![allow(unused)]

use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::builder::TableBuilder;
use crate::config::TableOptions;
use crate::error::StryxError;
use crate::reader::TableReader;

type TestResult = Result<(), Box<dyn Error>>;

struct TestTable {
    path: PathBuf,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn test_path() -> PathBuf {
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

    let metadata = table.finalize()?;

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

    let metadata = table.finalize()?;
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

    let metadata = table.finalize()?;
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
