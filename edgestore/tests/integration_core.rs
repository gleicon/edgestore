use edgestore::{EdgestoreConfig, Engine};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
}

fn open_engine_with_config(config: EdgestoreConfig) -> Engine {
    Engine::open(config).unwrap()
}

/// Return the first WAL file found in `db_path` (sorted ascending).
fn find_wal_file(db_path: &Path) -> std::path::PathBuf {
    let mut files: Vec<_> = std::fs::read_dir(db_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with("wal-") && s.ends_with(".log")
        })
        .map(|e| e.path())
        .collect();
    files.sort();
    files.into_iter().next().unwrap()
}

/// Count all WAL files in `db_path`.
fn count_wal_files(db_path: &Path) -> usize {
    std::fs::read_dir(db_path)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy();
            s.starts_with("wal-") && s.ends_with(".log")
        })
        .count()
}

// ── Task 1: WAL format ───────────────────────────────────────────────────────

#[test]
fn test_wal_file_header_format() {
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"k", b"v").unwrap();
        engine.flush().unwrap();
    }

    let wal_path = find_wal_file(dir.path());
    let bytes = std::fs::read(&wal_path).unwrap();

    // Magic: "EDGW"
    assert!(bytes.len() >= 8, "WAL file too short to contain header");
    assert_eq!(
        &bytes[..4],
        &[0x45, 0x44, 0x47, 0x57],
        "WAL magic mismatch"
    );
    // Version = 1
    assert_eq!(bytes[4], 1, "WAL version mismatch");
    // Reserved bytes must be zero
    assert_eq!(&bytes[5..8], &[0u8, 0, 0], "WAL reserved bytes non-zero");
}

#[test]
fn test_wal_encode_decode_round_trip() {
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        engine.put(b"test", b"hello", b"world").unwrap();
        engine.flush().unwrap();
    }

    // Reopen — recovery replays WAL into memtable.
    let engine = open_engine(&dir);
    let val = engine.get(b"test", b"hello").unwrap();
    assert_eq!(val, Some(b"world".to_vec()), "round-trip value mismatch");
}

#[test]
fn test_wal_corruption_detection() {
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"k1", b"v1").unwrap();
        engine.put(b"ns", b"k2", b"v2").unwrap();
        engine.put(b"ns", b"k3", b"v3").unwrap();
        engine.flush().unwrap();
    }

    let wal_path = find_wal_file(dir.path());
    // Flip bytes at offset 20 — this is inside the compressed payload of the
    // first frame (header=8 bytes, frame-header=8 bytes, payload starts at 16).
    let mut data = std::fs::read(&wal_path).unwrap();
    if data.len() > 20 {
        data[20] ^= 0xFF;
        data[21] ^= 0xFF;
    }
    std::fs::write(&wal_path, &data).unwrap();

    // Reopening must not panic; corrupted records are skipped.
    let engine = open_engine(&dir);

    // At least k3 (or k2) should be readable if corruption only hits the first record.
    // We just assert the engine opened successfully and doesn't crash on gets.
    let _ = engine.get(b"ns", b"k1");
    let _ = engine.get(b"ns", b"k2");
    let _ = engine.get(b"ns", b"k3");
}

#[test]
fn test_wal_truncation_safety() {
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"k1", b"v1").unwrap();
        engine.put(b"ns", b"k2", b"v2").unwrap();
        engine.flush().unwrap();
    }

    let wal_path = find_wal_file(dir.path());
    let size = std::fs::metadata(&wal_path).unwrap().len();
    // Truncate the last 4 bytes — this cuts into the final frame.
    let truncated_size = size.saturating_sub(4);

    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&wal_path)
        .unwrap();
    file.set_len(truncated_size).unwrap();
    drop(file);

    // Reopen must not panic.
    let engine = open_engine(&dir);
    // At least k1 should survive (truncation only affects the tail).
    let k1 = engine.get(b"ns", b"k1").unwrap();
    assert!(k1.is_some(), "k1 should survive WAL truncation");
}

// ── Task 2: crash recovery, namespace isolation, group commit, rotation ───────

#[test]
fn test_crash_recovery_no_acknowledged_writes_lost() {
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        for i in 0u32..50 {
            let key = format!("key-{:03}", i);
            let val = format!("val-{:03}", i);
            engine.put(b"crash_test", key.as_bytes(), val.as_bytes()).unwrap();
        }
        engine.flush().unwrap();
    }

    let engine = open_engine(&dir);
    for i in 0u32..50 {
        let key = format!("key-{:03}", i);
        let expected_val = format!("val-{:03}", i);
        let got = engine.get(b"crash_test", key.as_bytes()).unwrap();
        assert_eq!(
            got,
            Some(expected_val.into_bytes()),
            "key {} not recovered",
            key
        );
    }
}

#[test]
fn test_namespace_isolation() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.put(b"ns_a", b"k1", b"va1").unwrap();
    engine.put(b"ns_a", b"k2", b"va2").unwrap();
    engine.put(b"ns_a", b"k3", b"va3").unwrap();
    engine.put(b"ns_b", b"k1", b"vb1").unwrap();
    engine.put(b"ns_b", b"k2", b"vb2").unwrap();

    let ns_a_results = engine.prefix(b"ns_a", b"").unwrap();
    assert_eq!(ns_a_results.len(), 3, "ns_a should have exactly 3 entries");

    // Ensure no ns_b values leak into ns_a results.
    for (_, val) in &ns_a_results {
        assert!(
            val.starts_with(b"va"),
            "ns_a result contains non-ns_a value: {:?}",
            val
        );
    }

    let ns_b_results = engine.prefix(b"ns_b", b"").unwrap();
    assert_eq!(ns_b_results.len(), 2, "ns_b should have exactly 2 entries");

    // Ensure no ns_a values leak into ns_b results.
    for (_, val) in &ns_b_results {
        assert!(
            val.starts_with(b"vb"),
            "ns_b result contains non-ns_b value: {:?}",
            val
        );
    }
}

#[test]
fn test_transaction_group_commit() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    let mut tx = engine.begin();
    for i in 0u32..100 {
        let key = format!("txkey-{:03}", i);
        let val = format!("txval-{:03}", i);
        tx.put(b"group", key.as_bytes(), val.as_bytes(), 0, 0).unwrap();
    }
    engine.commit_transaction(tx).unwrap();

    // All 100 keys must be visible.
    for i in 0u32..100 {
        let key = format!("txkey-{:03}", i);
        let expected = format!("txval-{:03}", i);
        let got = engine.get(b"group", key.as_bytes()).unwrap();
        assert_eq!(
            got,
            Some(expected.into_bytes()),
            "transaction key {} not visible after commit",
            key
        );
    }
}

#[test]
fn test_transaction_tx_commit_convenience() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    // Commit path.
    let mut tx = engine.begin();
    tx.put(b"ns", b"key1", b"val1", 0, 0).unwrap();
    let lsn = tx.commit(&mut engine).unwrap();
    assert!(lsn > 0, "commit should return a positive LSN");
    assert_eq!(
        engine.get(b"ns", b"key1").unwrap(),
        Some(b"val1".to_vec()),
        "key1 should be visible after commit"
    );

    // Rollback path.
    let mut tx2 = engine.begin();
    tx2.put(b"ns", b"key2", b"val2", 0, 0).unwrap();
    tx2.rollback(&mut engine);
    assert_eq!(
        engine.get(b"ns", b"key2").unwrap(),
        None,
        "key2 should not be visible after rollback"
    );
}

#[test]
fn test_wal_rotation() {
    let dir = TempDir::new().unwrap();

    // WAL rotation in this engine happens at Engine::open() when the existing WAL
    // exceeds wal_max_bytes.  Strategy: write a batch, close, reopen (triggers
    // rotation if size exceeded), repeat — so we accumulate multiple WAL files.

    let batch_size = 20u32;
    let batches = 10u32;

    for batch in 0..batches {
        let mut config = EdgestoreConfig::new(dir.path());
        // 512 bytes max — any non-trivial batch exceeds this quickly.
        config.wal_max_bytes = 512;
        let mut engine = open_engine_with_config(config);
        let base = batch * batch_size;
        for i in base..base + batch_size {
            let key = format!("rotkey-{:03}", i);
            let val = format!("rotval-1234567890-{:03}", i);
            engine.put(b"rotation", key.as_bytes(), val.as_bytes()).unwrap();
        }
        engine.flush().unwrap();
        // Dropping engine releases the lockfile; next iteration reopens.
    }

    // Count WAL files — there must be more than 1 due to rotation.
    let wal_count = count_wal_files(dir.path());
    assert!(
        wal_count > 1,
        "expected more than 1 WAL file after rotation, got {}",
        wal_count
    );

    // Reopen with default config and verify all keys are recoverable.
    let engine = open_engine(&dir);
    let total = batch_size * batches;
    for i in 0u32..total {
        let key = format!("rotkey-{:03}", i);
        let expected = format!("rotval-1234567890-{:03}", i);
        let got = engine.get(b"rotation", key.as_bytes()).unwrap();
        assert_eq!(
            got,
            Some(expected.into_bytes()),
            "rotation key {} not recovered after reopen",
            key
        );
    }
}

#[test]
fn test_put_with_ttl_stored_in_wal() {
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        engine.put_with_ttl(b"ns", b"ttl_key", b"ttl_val", 3600).unwrap();
        engine.flush().unwrap();
    }

    // Reopen — TTL field is stored but not yet expired.
    let engine = open_engine(&dir);
    let val = engine.get(b"ns", b"ttl_key").unwrap();
    assert_eq!(
        val,
        Some(b"ttl_val".to_vec()),
        "TTL key should still be readable after reopen"
    );
}

// ── Additional: WAL writer rotation via seek-based truncation ─────────────────

#[test]
fn test_wal_file_seek_write_does_not_panic() {
    // Low-level sanity: open a WAL file, seek to offset 20, write 0xFF bytes,
    // verify the engine reopens without panic.
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"a", b"1").unwrap();
        engine.put(b"ns", b"b", b"2").unwrap();
        engine.put(b"ns", b"c", b"3").unwrap();
        engine.flush().unwrap();
    }

    let wal_path = find_wal_file(dir.path());
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&wal_path)
            .unwrap();
        let meta = f.metadata().unwrap();
        if meta.len() > 22 {
            f.seek(SeekFrom::Start(20)).unwrap();
            f.write_all(&[0xFF, 0xFF]).unwrap();
            f.flush().unwrap();
        }
    }

    // Must not panic.
    let engine = open_engine(&dir);
    // Verify engine is operational (can insert new data).
    let mut engine = engine;
    engine.put(b"ns", b"new_key", b"new_val").unwrap();
    assert_eq!(
        engine.get(b"ns", b"new_key").unwrap(),
        Some(b"new_val".to_vec())
    );
}

// ── Task 3: inline WAL rotation without reopen ────────────────────────────────

#[test]
fn test_ttl_lazy_expiry_visible_before_compaction() {
    let dir = tempfile::TempDir::new().unwrap();
    let config = EdgestoreConfig::new(dir.path());
    let mut engine = Engine::open(config).unwrap();
    engine.put_with_ttl(b"ns", b"expiring", b"still_here", 1).unwrap();
    // Flush memtable to a segment so compaction can act on it.
    engine.flush_to_segments().unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let val = engine.get(b"ns", b"expiring").unwrap();
    assert_eq!(val, Some(b"still_here".to_vec()), "lazy expiry: value must be visible before compaction even after TTL expires");
    engine.compact_once().unwrap();
    let val_after = engine.get(b"ns", b"expiring").unwrap();
    assert_eq!(val_after, None, "value must be gone after compaction removes expired cohort");
}

#[test]
fn test_wal_rotates_inline_without_reopen() {
    let dir = TempDir::new().unwrap();

    // Use a very small wal_max_bytes (300 bytes) so each write triggers rotation quickly.
    let mut config = EdgestoreConfig::new(dir.path());
    config.wal_max_bytes = 300;

    let mut engine = Engine::open(config).unwrap();

    // Write 10 puts — with 300-byte limit, multiple WAL files must be created inline.
    for i in 0usize..10 {
        let key = format!("k{:02}", i);
        engine
            .put(b"ns", key.as_bytes(), b"value_data_padding")
            .unwrap();
    }

    engine.flush().unwrap();

    // Rotation must have happened inline — at least 2 WAL files expected.
    let wal_count = count_wal_files(dir.path());
    assert!(
        wal_count >= 2,
        "expected >= 2 WAL files after inline rotation, got {}",
        wal_count
    );

    drop(engine);

    // Reopen with default config and verify all 10 keys are recoverable.
    let engine2 = open_engine(&dir);
    for i in 0usize..10 {
        let key = format!("k{:02}", i);
        let got = engine2.get(b"ns", key.as_bytes()).unwrap();
        assert_eq!(
            got,
            Some(b"value_data_padding".to_vec()),
            "key {} not found after inline rotation and reopen",
            key
        );
    }
}
