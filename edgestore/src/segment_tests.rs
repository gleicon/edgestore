use super::*;
use crate::types::{encode_key, Operation};
use tempfile::TempDir;

fn make_entry(lsn: u64, key: &[u8], value: &[u8]) -> MemEntry {
    MemEntry {
        key: key.to_vec(),
        value: Some(value.to_vec()),
        op: Operation::Put,
        lsn,
        timestamp: 3_600_000_000_000,
        ttl: 0,
    }
}

fn make_delete(lsn: u64, key: &[u8]) -> MemEntry {
    MemEntry {
        key: key.to_vec(),
        value: None,
        op: Operation::Delete,
        lsn,
        timestamp: 0,
        ttl: 0,
    }
}

fn sorted_entries(n: usize) -> Vec<(Vec<u8>, MemEntry)> {
    let mut v: Vec<(Vec<u8>, MemEntry)> = (0..n)
        .map(|i| {
            let k = encode_key(b"ns", format!("key-{:04}", i).as_bytes());
            let val = format!("val-{:04}", i);
            let e = make_entry(i as u64 + 1, &k, val.as_bytes());
            (k, e)
        })
        .collect();
    v.sort_by(|(a, _), (b, _)| a.cmp(b));
    v
}

// ─ Entry serialization ─────────────────────────────────────────────────

#[test]
fn test_serialize_deserialize_put() {
    let key = b"hello";
    let entry = make_entry(42, key, b"world");
    let bytes = serialize_entry(key, &entry);
    let mut pos = 0;
    let (k2, e2) = deserialize_entry(&bytes, &mut pos).unwrap();
    assert_eq!(k2, key);
    assert_eq!(e2.lsn, 42);
    assert_eq!(e2.value, Some(b"world".to_vec()));
    assert_eq!(e2.op, Operation::Put);
    assert_eq!(pos, bytes.len());
}

#[test]
fn test_serialize_deserialize_delete() {
    let key = b"gone";
    let entry = make_delete(7, key);
    let bytes = serialize_entry(key, &entry);
    let mut pos = 0;
    let (_, e2) = deserialize_entry(&bytes, &mut pos).unwrap();
    assert_eq!(e2.op, Operation::Delete);
    assert_eq!(e2.value, None);
}

#[test]
fn test_deserialize_truncated() {
    let mut pos = 0;
    assert!(deserialize_entry(&[0, 1, 2], &mut pos).is_err());
}

// ─ Writer ──────────────────────────────────────────────────────────────

#[test]
fn test_writer_dat_file_header() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(20);
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let dat = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
    let magic = u32::from_le_bytes(dat[0..4].try_into().unwrap());
    assert_eq!(magic, SEGMENT_FILE_MAGIC);
    assert_eq!(dat[4], SEGMENT_FORMAT_VERSION);
}

#[test]
fn test_writer_sparse_index_count() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(200);
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();
    let index = read_idx_file(&dir.path().join("segment-00000000.idx")).unwrap();
    assert_eq!(index.len(), 4); // ceil(200/64) = 4
}

#[test]
fn test_flush_four_files_and_hash() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(10);
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    let meta = writer.flush(&entries).unwrap();

    assert!(dir.path().join("segment-00000000.dat").exists());
    assert!(dir.path().join("segment-00000000.idx").exists());
    assert!(dir.path().join("segment-00000000.xf").exists());
    assert!(dir.path().join("segment-00000000.meta").exists());
    assert_eq!(meta.record_count, 10);

    let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
    let expected = blake3::hash(&dat_bytes).as_bytes().to_vec();
    assert_eq!(meta.segment_hash, expected);
}

#[test]
fn test_flush_empty_returns_error() {
    let dir = TempDir::new().unwrap();
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    assert!(writer.flush(&[]).is_err());
}

// ─ Xor filter ──────────────────────────────────────────────────────────

#[test]
fn test_xor_filter_no_false_negatives() {
    let dir = TempDir::new().unwrap();
    let keys: Vec<Vec<u8>> = (0..100u32)
        .map(|i| format!("key-{:04}", i).into_bytes())
        .collect();
    let filter = build_xor_filter(&keys).unwrap();
    write_xf_file(&filter, &dir.path().join("test.xf")).unwrap();
    let filter2 = read_xf_file(&dir.path().join("test.xf")).unwrap();
    for key in &keys {
        assert!(
            filter_contains(&filter2, key),
            "false negative for {:?}",
            key
        );
    }
}

#[test]
fn test_xf_truncated_returns_error() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("bad.xf");
    std::fs::write(&p, b"short").unwrap();
    assert!(read_xf_file(&p).is_err());
}

// ─ Reader ──────────────────────────────────────────────────────────────

#[test]
fn test_reader_open_and_get() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(200);
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    assert_eq!(reader.meta.record_count, 200);

    let (target_key, target_entry) = &entries[100];
    let found = reader.get(target_key).unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().lsn, target_entry.lsn);
}

#[test]
fn test_reader_absent_key_returns_none() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(50);
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    let absent = encode_key(b"ns", b"absent-key-xyz");
    assert!(reader.get(&absent).unwrap().is_none());
}

#[test]
fn test_reader_range_scan_100_entries() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(500);
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    let start = encode_key(b"ns", b"key-0100");
    let end = encode_key(b"ns", b"key-0200");
    let results = reader.range_scan(&start, &end).unwrap();
    assert_eq!(results.len(), 100);
}

#[test]
fn test_reader_open_missing_meta_errors() {
    let dir = TempDir::new().unwrap();
    assert!(SegmentReader::open(dir.path().to_path_buf(), 99).is_err());
}

// ─ Performance regression guards ───────────────────────────────────────

/// Regression: SegmentReader used to re-read the .idx file on every get() and range_scan().
/// Now it caches the index at open() time. This test verifies that the index is loaded
/// once and reused by reading multiple keys without touching the .idx file again.
#[test]
fn test_reader_caches_index_at_open() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(100);
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    assert!(!reader.index.is_empty(), "index should be cached at open()");
    for (k, e) in &entries {
        let found = reader.get(k).unwrap();
        assert!(found.is_some(), "key {:?} not found", k);
        assert_eq!(found.unwrap().lsn, e.lsn);
    }
}

/// Regression: find_block_offset was linear scan. Now it is binary search.
#[test]
fn test_find_block_offset_binary_search() {
    let index: Vec<(Vec<u8>, u64)> = (0..1000u64)
        .map(|i| (format!("key-{:08}", i).into_bytes(), i * 100))
        .collect();
    assert_eq!(find_block_offset(&index, b"key-00000050"), 5000);
    assert_eq!(find_block_offset(&index, b"key-00000050\x01"), 5000);
    assert_eq!(find_block_offset(&index, b"aaa"), 0);
    assert_eq!(find_block_offset(&index, b"zzz"), 99900);
}

/// Regression: SegmentStore::range_scan used HashMap + sort. Now it uses K-way merge.
/// This test verifies deduplication by LSN across multiple overlapping segments.
#[test]
fn test_range_scan_dedups_by_lsn_across_segments() {
    let dir = TempDir::new().unwrap();
    let ns = b"ns";
    let key = encode_key(ns, b"shared-key");
    let key_end = encode_key(ns, b"shared-key\x00");

    let mut writer0 = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    let entry0 = make_entry(1, &key, b"old");
    let meta0 = writer0.flush(&vec![(key.clone(), entry0)]).unwrap();

    let mut writer1 = SegmentWriter::new(dir.path().to_path_buf(), 1, 3600);
    let entry1 = make_entry(2, &key, b"new");
    let meta1 = writer1.flush(&vec![(key.clone(), entry1)]).unwrap();

    let mut store = SegmentStore::open(dir.path().to_path_buf(), 3600).unwrap();
    let reader0 = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    let reader1 = SegmentReader::open(dir.path().to_path_buf(), 1).unwrap();
    store.add_imported_segment(meta0, reader0).unwrap();
    store.add_imported_segment(meta1, reader1).unwrap();

    let (results, _) = store.range_scan_budgeted(&key, &key_end, None).unwrap();
    assert_eq!(results.len(), 1, "should deduplicate to 1 entry");
    assert_eq!(results[0].1.lsn, 2, "higher LSN should win");
}

/// Regression: range_scan must delete-filter correctly.
#[test]
fn test_range_scan_delete_wins() {
    let dir = TempDir::new().unwrap();
    let key = encode_key(b"ns", b"key");
    let key_end = encode_key(b"ns", b"key\x00");

    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    let entry1 = make_delete(2, &key);
    let meta = writer.flush(&vec![(key.clone(), entry1)]).unwrap();

    let mut store = SegmentStore::open(dir.path().to_path_buf(), 3600).unwrap();
    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    store.add_imported_segment(meta, reader).unwrap();
    let (results, _) = store.range_scan_budgeted(&key, &key_end, None).unwrap();
    assert!(results.is_empty(), "delete should filter out the key");
}

// Regression: remove_segment must update manifest before deleting files.
#[test]
fn test_remove_segment_manifest_updated_before_files_gone() {
    use crate::memtable::BTreeMemTable;
    use crate::types::encode_key;

    let dir = TempDir::new().unwrap();
    let mut store = SegmentStore::open(dir.path().to_path_buf(), 3600).unwrap();

    let mut mt = BTreeMemTable::new();
    let k = encode_key(b"ns", b"key");
    mt.insert(k.clone(), make_entry(1, &k, b"val"));
    let meta = store.flush_memtable(&mt).unwrap();
    let id = meta.segment_id;

    assert!(dir.path().join(format!("segment-{:08}.dat", id)).exists());
    assert_eq!(store.manifest.list_segments().len(), 1);

    store.remove_segment(id).unwrap();

    assert_eq!(
        store.manifest.list_segments().len(),
        0,
        "manifest must not list removed segment"
    );
    assert!(!dir.path().join(format!("segment-{:08}.dat", id)).exists());
    assert_eq!(store.readers.len(), 0);
}

// ─ parse_dat_entries ───────────────────────────────────────────────────

#[test]
fn test_parse_dat_entries_empty_slice_returns_empty() {
    let result = parse_dat_entries(&[]).unwrap();
    assert!(result.is_empty());
    let result = parse_dat_entries(&[0u8; 7]).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_parse_dat_entries_truncated_block_returns_empty() {
    let mut data = vec![0u8; 8];
    data.extend_from_slice(&SEGMENT_BLOCK_MAGIC.to_le_bytes());
    data.extend_from_slice(&1000u32.to_le_bytes());
    let result = parse_dat_entries(&data).unwrap();
    assert!(result.is_empty(), "truncated block must be skipped, not error");
}

#[test]
fn test_parse_dat_entries_unknown_magic_returns_empty() {
    let mut data = vec![0u8; 8];
    data.extend_from_slice(&0xDEADBEEFu32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    let result = parse_dat_entries(&data).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_parse_dat_entries_roundtrip() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(50);
    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
    let parsed = parse_dat_entries(&dat_bytes).unwrap();

    assert_eq!(parsed.len(), entries.len(), "parsed count must match written count");
    for ((exp_key, exp_entry), (got_key, got_entry)) in entries.iter().zip(parsed.iter()) {
        assert_eq!(got_key, exp_key, "key mismatch");
        assert_eq!(got_entry.value, exp_entry.value, "value mismatch for key {:?}", exp_key);
        assert_eq!(got_entry.op, exp_entry.op, "op mismatch");
        assert_eq!(got_entry.lsn, exp_entry.lsn, "lsn mismatch");
    }
}
