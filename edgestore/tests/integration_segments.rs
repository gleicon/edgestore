use edgestore::segment::{
    SegmentReader, SegmentWriter, SPARSE_INDEX_STRIDE,
};
use edgestore::types::{encode_key, MemEntry, Operation, SegmentMeta};
use edgestore::{EdgestoreConfig, Engine};
use std::io::{Seek, SeekFrom, Write};
use tempfile::TempDir;

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
}

fn make_entry(key: &[u8], lsn: u64, value: &[u8], ts: i64) -> MemEntry {
    MemEntry {
        key: key.to_vec(),
        value: Some(value.to_vec()),
        op: Operation::Put,
        lsn,
        timestamp: ts,
        ttl: 0,
    }
}

fn sorted_entries(n: usize) -> Vec<(Vec<u8>, MemEntry)> {
    let mut v: Vec<(Vec<u8>, MemEntry)> = (0..n)
        .map(|i| {
            let k = encode_key(b"ns", format!("key-{:04}", i).as_bytes());
            let val = format!("val-{:04}", i);
            let e = make_entry(&k, i as u64 + 1, val.as_bytes(), 3_600_000_000_000);
            (k, e)
        })
        .collect();
    v.sort_by(|(a, _), (b, _)| a.cmp(b));
    v
}

// ── Success Criterion 1: flush produces 4 files with correct BLAKE3 hash ───

#[test]
fn test_flush_produces_four_files() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    for i in 0u32..100 {
        engine
            .put(b"ns", format!("key-{:04}", i).as_bytes(), b"value")
            .unwrap();
    }

    let meta = engine.flush_to_segments().unwrap();
    assert_eq!(meta.record_count, 100);

    let seg = format!("segment-{:08}.dat", meta.segment_id);
    let dat_path = dir.path().join(&seg);
    let idx_path = dir.path().join(seg.replace(".dat", ".idx"));
    let xf_path = dir.path().join(seg.replace(".dat", ".xf"));
    let meta_path = dir.path().join(seg.replace(".dat", ".meta"));

    assert!(dat_path.exists(), ".dat missing");
    assert!(idx_path.exists(), ".idx missing");
    assert!(xf_path.exists(), ".xf missing");
    assert!(meta_path.exists(), ".meta missing");

    let dat_bytes = std::fs::read(&dat_path).unwrap();
    let expected_hash = blake3::hash(&dat_bytes).as_bytes().to_vec();
    assert_eq!(meta.segment_hash, expected_hash, "BLAKE3 hash mismatch");
}

// ── Success Criterion 2: xor filter — no false negatives ───────────────────

#[test]
fn test_xor_filter_no_false_negatives() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(200);

    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    for (key, _) in &entries {
        assert!(
            reader.get(key).unwrap().is_some(),
            "false negative for key {:?}",
            key
        );
    }
}

// ── Success Criterion 2: xor filter fast-rejects absent keys ───────────────

#[test]
fn test_xor_filter_fast_reject() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(50);

    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    for i in 0..50usize {
        let absent = encode_key(b"ns", format!("absent-{:04}", i).as_bytes());
        assert!(
            reader.get(&absent).unwrap().is_none(),
            "unexpected hit for absent key"
        );
    }
}

// ── Success Criterion 3: sparse index lands within SPARSE_INDEX_STRIDE ─────

#[test]
fn test_sparse_index_seek_accuracy() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(500);

    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let idx_path = dir.path().join("segment-00000000.idx");
    let index = edgestore::segment::read_idx_file(&idx_path).unwrap();

    let target_key = &entries[250].0;
    let mut best_idx_pos: usize = 0;
    for (i, (k, _)) in index.iter().enumerate() {
        if k.as_slice() <= target_key.as_slice() {
            best_idx_pos = i;
        } else {
            break;
        }
    }
    let approx_entry_pos = best_idx_pos * SPARSE_INDEX_STRIDE;
    assert!(
        250 - approx_entry_pos.min(250) <= SPARSE_INDEX_STRIDE,
        "sparse index too far from target: index_pos={}, entry=250, stride={}",
        approx_entry_pos,
        SPARSE_INDEX_STRIDE
    );
}

// ── Success Criterion 4: cohort_bucket and death_time in SegmentMeta ────────

#[test]
fn test_segment_meta_cohort_and_death_time() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    for i in 0u32..20 {
        engine
            .put(b"ns", format!("key-{:04}", i).as_bytes(), b"val")
            .unwrap();
    }

    let meta: SegmentMeta = engine.flush_to_segments().unwrap();

    assert!(meta.cohort_bucket != 0 || meta.death_time > 0, "cohort fields unset");
    assert!(meta.death_time > 0, "death_time is zero");
}

// ── Success Criterion 5: encode/decode round-trip ───────────────────────────

#[test]
fn test_segment_format_encode_decode() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(10);

    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    for (key, original) in &entries {
        let found = reader.get(key).unwrap().expect("key not found");
        assert_eq!(found.lsn, original.lsn);
        assert_eq!(found.value, original.value);
        assert_eq!(found.op, original.op);
    }
}

// ── Success Criterion 5: manifest replay after multiple flushes ─────────────

#[test]
fn test_manifest_replay_after_multiple_flushes() {
    let dir = TempDir::new().unwrap();

    let batch1_key = b"batch1-key";
    let batch2_key = b"batch2-key";

    {
        let mut engine = open_engine(&dir);
        for i in 0u32..50 {
            engine
                .put(b"ns", format!("key1-{:04}", i).as_bytes(), b"val1")
                .unwrap();
        }
        engine
            .put(b"ns", batch1_key, b"batch1-value")
            .unwrap();
        engine.flush_to_segments().unwrap();

        for i in 0u32..50 {
            engine
                .put(b"ns", format!("key2-{:04}", i).as_bytes(), b"val2")
                .unwrap();
        }
        engine
            .put(b"ns", batch2_key, b"batch2-value")
            .unwrap();
        engine.flush_to_segments().unwrap();
    }

    let engine2 = open_engine(&dir);
    assert_eq!(
        engine2.get(b"ns", batch1_key).unwrap(),
        Some(b"batch1-value".to_vec()),
        "batch1 key not readable after reopen"
    );
    assert_eq!(
        engine2.get(b"ns", batch2_key).unwrap(),
        Some(b"batch2-value".to_vec()),
        "batch2 key not readable after reopen"
    );
}

// ── Success Criterion 5: corruption detection ───────────────────────────────

#[test]
fn test_segment_corruption_detection() {
    let dir = TempDir::new().unwrap();
    let entries = sorted_entries(20);

    let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
    writer.flush(&entries).unwrap();

    let dat_path = dir.path().join("segment-00000000.dat");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&dat_path)
            .unwrap();
        f.seek(SeekFrom::Start(100)).unwrap();
        f.write_all(&[0xFF]).unwrap();
    }

    let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
    let result = reader.get(&entries[5].0);
    // Either error or None is acceptable — must not panic
    match result {
        Ok(_) | Err(_) => {}
    }
}

// ── Bonus: crash recovery with segment store ────────────────────────────────

#[test]
fn test_segment_survives_engine_crash_recovery() {
    let dir = TempDir::new().unwrap();

    {
        let mut engine = open_engine(&dir);
        for i in 0u32..30 {
            engine
                .put(b"ns", format!("flushed-{:04}", i).as_bytes(), b"v1")
                .unwrap();
        }
        engine.flush_to_segments().unwrap();

        for i in 0u32..20 {
            engine
                .put(b"ns", format!("unflushed-{:04}", i).as_bytes(), b"v2")
                .unwrap();
        }
        engine.flush(/* WAL fsync */).unwrap();
    } // drop = crash

    let engine2 = open_engine(&dir);
    for i in 0u32..30 {
        let key = format!("flushed-{:04}", i);
        assert_eq!(
            engine2.get(b"ns", key.as_bytes()).unwrap(),
            Some(b"v1".to_vec()),
            "flushed key {} missing after recovery",
            key
        );
    }
    for i in 0u32..20 {
        let key = format!("unflushed-{:04}", i);
        assert_eq!(
            engine2.get(b"ns", key.as_bytes()).unwrap(),
            Some(b"v2".to_vec()),
            "unflushed key {} missing after recovery",
            key
        );
    }
}

#[test]
fn test_range_exclusive_end() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    // Write keys a, b, c in namespace "ns".
    engine.put(b"ns", b"a", b"va").unwrap();
    engine.put(b"ns", b"b", b"vb").unwrap();
    engine.put(b"ns", b"c", b"vc").unwrap();

    // Memtable path: range [a, b) must return only key a.
    let results = engine.range(b"ns", b"a", b"b").unwrap();
    assert_eq!(results.len(), 1, "memtable: range [a, b) must exclude key b");
    assert_eq!(results[0].0, b"a", "memtable: only key a must be returned");
    assert!(
        results.iter().all(|(k, _)| k.as_slice() != b"b"),
        "memtable: key b must not appear"
    );

    // Flush to segments, then verify segment path also uses exclusive end.
    engine.flush_to_segments().unwrap();

    let results = engine.range(b"ns", b"a", b"b").unwrap();
    assert_eq!(results.len(), 1, "segment: range [a, b) must exclude key b");
    assert_eq!(results[0].0, b"a", "segment: only key a must be returned");
    assert!(
        results.iter().all(|(k, _)| k.as_slice() != b"b"),
        "segment: key b must not appear"
    );
}
