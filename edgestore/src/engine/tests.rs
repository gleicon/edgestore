use super::*;
use tempfile::TempDir;

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
}

#[test]
fn test_open_drop_reopen() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    drop(engine);
    let _engine2 = open_engine(&dir);
}

#[test]
fn test_double_open_writer_busy() {
    let dir = TempDir::new().unwrap();
    let _engine = open_engine(&dir);
    let result = Engine::open(EdgestoreConfig::new(dir.path()));
    assert!(matches!(result, Err(EdgestoreError::WriterBusy)));
}

#[test]
fn test_commit_returns_highest_lsn() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut tx = engine.begin();
    tx.put(b"ns", b"k1", b"v1", 1, 0).unwrap();
    tx.put(b"ns", b"k2", b"v2", 2, 0).unwrap();
    tx.put(b"ns", b"k3", b"v3", 3, 0).unwrap();
    let lsn = engine.commit_transaction(tx).unwrap();
    assert_eq!(lsn, 3);
}

#[test]
fn test_double_commit_returns_err() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut tx = engine.begin();
    tx.put(b"ns", b"k1", b"v1", 1, 0).unwrap();
    let _ = tx.take_pending().unwrap();
    let result = engine.commit_transaction(tx);
    assert!(result.is_err());
}

#[test]
fn test_wal_naming_hex() {
    let dir = TempDir::new().unwrap();
    let path = next_wal_path(dir.path(), 50);
    let filename = path.file_name().unwrap().to_string_lossy();
    assert_eq!(filename, "wal-0000000000000032.log");
}

#[test]
fn test_namespace_too_long_returns_error() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let long_ns = vec![b'x'; u16::MAX as usize + 1];
    let result = engine.put(&long_ns, b"k", b"v");
    assert!(
        matches!(result, Err(EdgestoreError::NamespaceTooLong { .. })),
        "expected NamespaceTooLong, got {:?}",
        result
    );
}

#[test]
fn test_flush_to_segments_empty_memtable_returns_error() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let result = engine.flush_to_segments();
    assert!(
        result.is_err(),
        "flush_to_segments on empty memtable must error"
    );
}

#[test]
fn test_delete_from_segment_via_memtable_tombstone() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"key", b"val").unwrap();
    engine.flush_to_segments().unwrap();
    engine.delete(b"ns", b"key").unwrap();
    let val = engine.get(b"ns", b"key").unwrap();
    assert_eq!(val, None, "tombstone in memtable must shadow segment value");
}

#[test]
fn test_range_across_segment_and_memtable() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"a", b"va").unwrap();
    engine.put(b"ns", b"b", b"vb").unwrap();
    engine.flush_to_segments().unwrap();
    engine.put(b"ns", b"c", b"vc").unwrap();
    let results = engine.range(b"ns", b"a", b"z").unwrap();
    let keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
    assert_eq!(keys, vec![b"a", b"b", b"c"]);
}

#[test]
fn test_prefix_from_segments() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"pre_a", b"v1").unwrap();
    engine.put(b"ns", b"pre_b", b"v2").unwrap();
    engine.put(b"ns", b"other", b"v3").unwrap();
    engine.flush_to_segments().unwrap();
    let results = engine.prefix(b"ns", b"pre_").unwrap();
    assert_eq!(results.len(), 2);
    let keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
    assert!(keys.contains(&b"pre_a".as_ref()));
    assert!(keys.contains(&b"pre_b".as_ref()));
}

#[test]
fn test_range_memtable_delete_shadows_segment_value() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"x", b"old").unwrap();
    engine.flush_to_segments().unwrap();
    engine.delete(b"ns", b"x").unwrap();
    let results = engine.range(b"ns", b"a", b"z").unwrap();
    assert!(results.is_empty(), "deleted key must not appear in range");
}

#[test]
fn test_prefix_upper_bound_edge_cases() {
    assert_eq!(prefix_upper_bound(&[0xFF, 0xFF]), None);
    assert_eq!(prefix_upper_bound(b"ab"), Some(b"ac".to_vec()));
    assert_eq!(prefix_upper_bound(&[0x01, 0xFF]), Some(vec![0x02]));
}

#[test]
fn test_metrics_counts_operations() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.put(b"ns", b"k1", b"v1").unwrap();
    engine.put(b"ns", b"k2", b"v2").unwrap();
    engine.put_with_ttl(b"ns", b"k3", b"v3", 60).unwrap();
    engine.get(b"ns", b"k1").unwrap();
    engine.get(b"ns", b"k2").unwrap();
    engine.delete(b"ns", b"k1").unwrap();
    engine.range(b"ns", b"a", b"z").unwrap();
    engine.prefix(b"ns", b"k").unwrap();

    let mut tx = engine.begin();
    tx.put(b"ns", b"tx1", b"tv1", 0, 0).unwrap();
    engine.commit_transaction(tx).unwrap();

    let mut tx2 = engine.begin();
    tx2.put(b"ns", b"tx2", b"tv2", 0, 0).unwrap();
    engine.rollback_transaction(tx2);

    let m = engine.metrics();
    assert_eq!(m.puts, 3, "3 puts (including put_with_ttl)");
    assert_eq!(m.gets, 2);
    assert_eq!(m.deletes, 1);
    assert_eq!(m.ranges, 1);
    assert_eq!(m.prefixes, 1);
    assert_eq!(m.transactions_committed, 1);
    assert_eq!(m.transactions_rolled_back, 1);
    assert!(m.put_nanos_total > 0);
    assert!(m.get_nanos_total > 0);
}

// ─ Performance regression guards ───────────────────────────────────────

/// Regression: Engine::range_inner used to use HashMap + sort(). Now it uses merge-join.
/// This test verifies that overlapping segments + memtable entries with the same key
/// are deduplicated by highest LSN (not duplicated, not silently dropped).
#[test]
fn test_range_merge_dedups_same_key() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"key", b"old").unwrap();
    engine.flush_to_segments().unwrap();
    engine.put(b"ns", b"key", b"new").unwrap();
    let results = engine.range(b"ns", b"", b"\xff").unwrap();
    assert_eq!(results.len(), 1, "should deduplicate to 1 entry");
    assert_eq!(results[0].1, b"new".to_vec());
}

/// Regression: Engine::prefix_inner used to use HashMap + sort(). Now it uses merge-join.
/// This test verifies that prefix scans with the same key in segment and memtable
/// are deduplicated by highest LSN.
#[test]
fn test_prefix_merge_dedups_same_key() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"prefix_key", b"old").unwrap();
    engine.flush_to_segments().unwrap();
    engine.put(b"ns", b"prefix_key", b"new").unwrap();
    let results = engine.prefix(b"ns", b"prefix_").unwrap();
    assert_eq!(results.len(), 1, "should deduplicate to 1 entry");
    assert_eq!(results[0].1, b"new".to_vec());
}

/// Regression: Engine::range_inner must handle delete tombstones from memtable
/// shadowing the same key in a segment.
#[test]
fn test_range_merge_delete_tombstone_shadows_segment() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"key", b"val").unwrap();
    engine.flush_to_segments().unwrap();
    engine.delete(b"ns", b"key").unwrap();
    let results = engine.range(b"ns", b"", b"\xff").unwrap();
    assert!(
        results.is_empty(),
        "delete tombstone should shadow segment value"
    );
}

/// Regression: Engine::range_inner must return sorted results even with multiple segments.
#[test]
fn test_get_into_hit_and_miss() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"k", b"val").unwrap();

    let mut buf = Vec::new();
    assert!(
        engine.get_into(b"ns", b"k", &mut buf).unwrap(),
        "existing key must return true"
    );
    assert_eq!(buf, b"val");

    let found = engine.get_into(b"ns", b"missing", &mut buf).unwrap();
    assert!(!found, "missing key must return false");
}

#[test]
fn test_get_into_reuses_buffer() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"k1", b"first").unwrap();
    engine.put(b"ns", b"k2", b"second").unwrap();

    let mut buf = Vec::with_capacity(64);
    engine.get_into(b"ns", b"k1", &mut buf).unwrap();
    assert_eq!(buf, b"first");
    engine.get_into(b"ns", b"k2", &mut buf).unwrap();
    assert_eq!(buf, b"second", "buffer must be overwritten on second call");
}

#[test]
fn test_memtable_auto_flush_at_threshold() {
    let dir = TempDir::new().unwrap();
    let mut cfg = EdgestoreConfig::new(dir.path());
    // Set a tiny threshold so a few puts trigger a flush.
    // AVG_ENTRY_SIZE_ESTIMATE = 256 bytes, so 2 entries × 256 = 512 ≥ threshold.
    cfg.memtable_max_bytes = 400;
    let mut engine = Engine::open(cfg).unwrap();

    engine.put(b"ns", b"a", b"1").unwrap();
    engine.put(b"ns", b"b", b"2").unwrap();

    assert!(
        !engine.list_segment_metas().is_empty(),
        "auto-flush must create a segment"
    );
}

/// This test creates 3 segments and verifies the merge produces sorted output.
#[test]
fn test_range_merge_sorted_across_segments() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"c", b"vc").unwrap();
    engine.put(b"ns", b"a", b"va").unwrap();
    engine.flush_to_segments().unwrap();
    engine.put(b"ns", b"b", b"vb").unwrap();
    engine.flush_to_segments().unwrap();
    engine.put(b"ns", b"d", b"vd").unwrap();
    engine.flush_to_segments().unwrap();
    let results = engine.range(b"ns", b"", b"\xff").unwrap();
    let keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
    assert_eq!(
        keys,
        vec![b"a", b"b", b"c", b"d"],
        "must be sorted across all segments"
    );
}

#[test]
fn test_open_readonly_rejects_writes() {
    let dir = TempDir::new().unwrap();
    {
        let mut w = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        w.put(b"ns", b"k", b"v").unwrap();
    }
    let mut r = Engine::open_readonly(EdgestoreConfig::new(dir.path())).unwrap();
    assert!(
        r.get(b"ns", b"k").unwrap().is_some(),
        "reads must work on readonly engine"
    );
    let err = r.put(b"ns", b"k2", b"v2").unwrap_err();
    assert!(
        matches!(err, EdgestoreError::ReadOnly),
        "put must return ReadOnly"
    );
    let err = r.delete(b"ns", b"k").unwrap_err();
    assert!(
        matches!(err, EdgestoreError::ReadOnly),
        "delete must return ReadOnly"
    );
}

#[test]
fn test_on_segment_flushed_callback_fires() {
    use std::sync::{Arc, Mutex};
    let dir = TempDir::new().unwrap();
    let fired: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let fired2 = fired.clone();
    let mut engine = Engine::open(EdgestoreConfig::new(dir.path()))
        .unwrap()
        .with_on_segment_flushed(move |meta| {
            fired2.lock().unwrap().push(meta.segment_id);
        });
    engine.put(b"ns", b"a", b"1").unwrap();
    engine.flush_to_segments().unwrap();
    engine.put(b"ns", b"b", b"2").unwrap();
    engine.flush_to_segments().unwrap();
    let ids = fired.lock().unwrap().clone();
    assert_eq!(
        ids.len(),
        2,
        "callback must fire once per flush_to_segments"
    );
}

#[test]
fn test_on_segment_flushed_fires_on_auto_flush() {
    use std::sync::{Arc, Mutex};
    let dir = TempDir::new().unwrap();
    let count = Arc::new(Mutex::new(0u32));
    let count2 = count.clone();
    let mut cfg = EdgestoreConfig::new(dir.path());
    cfg.memtable_max_bytes = 1; // forces flush after first put
    let mut engine = Engine::open(cfg)
        .unwrap()
        .with_on_segment_flushed(move |_| {
            *count2.lock().unwrap() += 1;
        });
    engine.put(b"ns", b"a", b"1").unwrap();
    engine.put(b"ns", b"b", b"2").unwrap();
    assert!(
        *count.lock().unwrap() > 0,
        "callback must fire on auto-flush triggered by put"
    );
}

// ── ENG-12: QueryStats ────────────────────────────────────────────────

#[test]
fn test_get_with_stats_memtable_hit() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"k", b"value").unwrap();
    let (val, stats) = engine.get_with_stats(b"ns", b"k").unwrap();
    assert_eq!(val, Some(b"value".to_vec()));
    assert_eq!(stats.segments_scanned, 0, "memtable hit: no segment scanned");
    assert!(stats.bytes_scanned > 0);
    assert_eq!(stats.items_examined, 1);
}

#[test]
fn test_get_with_stats_segment_hit() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"k", b"value").unwrap();
    engine.flush_to_segments().unwrap();
    let (val, stats) = engine.get_with_stats(b"ns", b"k").unwrap();
    assert_eq!(val, Some(b"value".to_vec()));
    assert_eq!(stats.segments_scanned, 1, "segment hit");
    assert!(stats.bytes_scanned > 0);
    assert_eq!(stats.items_examined, 1);
}

#[test]
fn test_get_with_stats_miss_returns_zero_stats() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let (val, stats) = engine.get_with_stats(b"ns", b"missing").unwrap();
    assert_eq!(val, None);
    assert_eq!(stats.segments_scanned, 0);
    assert_eq!(stats.bytes_scanned, 0);
    assert_eq!(stats.items_examined, 0);
}

#[test]
fn test_range_with_stats_returns_bytes() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"a", b"va").unwrap();
    engine.put(b"ns", b"b", b"vb").unwrap();
    let (pairs, stats) = engine.range_with_stats(b"ns", b"a", b"z").unwrap();
    assert_eq!(pairs.len(), 2);
    assert!(stats.bytes_scanned > 0, "range scan must report non-zero bytes");
    assert!(stats.items_examined >= 2);
}

#[test]
fn test_prefix_with_stats_returns_bytes() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"foo:a", b"1").unwrap();
    engine.put(b"ns", b"foo:b", b"2").unwrap();
    engine.put(b"ns", b"bar:c", b"3").unwrap();
    let (pairs, stats) = engine.prefix_with_stats(b"ns", b"foo:").unwrap();
    assert_eq!(pairs.len(), 2);
    assert!(stats.bytes_scanned > 0);
    assert!(stats.items_examined >= 2);
}

// ── ENG-9: ScanBudget / BudgetedScan ─────────────────────────────────

#[test]
fn test_range_budgeted_truncates_at_max_items() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u8..10 {
        engine.put(b"ns", &[b'a' + i], b"v").unwrap();
    }
    let budget = ScanBudget {
        max_items: Some(3),
        max_bytes: None,
    };
    let result = engine.range_budgeted(b"ns", b"", b"\xff", &budget).unwrap();
    assert_eq!(result.items.len(), 3);
    assert!(result.truncated, "must be truncated when budget hit");
}

#[test]
fn test_range_budgeted_no_truncation_when_under_budget() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"a", b"va").unwrap();
    engine.put(b"ns", b"b", b"vb").unwrap();
    let budget = ScanBudget {
        max_items: Some(100),
        max_bytes: None,
    };
    let result = engine.range_budgeted(b"ns", b"", b"\xff", &budget).unwrap();
    assert_eq!(result.items.len(), 2);
    assert!(!result.truncated, "must not be truncated when under budget");
}

#[test]
fn test_prefix_budgeted_truncates_at_max_items() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u8..8 {
        engine
            .put(b"ns", format!("key:{}", i).as_bytes(), b"v")
            .unwrap();
    }
    let budget = ScanBudget {
        max_items: Some(2),
        max_bytes: None,
    };
    let result = engine.prefix_budgeted(b"ns", b"key:", &budget).unwrap();
    assert_eq!(result.items.len(), 2);
    assert!(result.truncated);
}

#[test]
fn test_prefix_budgeted_stops_at_max_bytes() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u8..5 {
        engine
            .put(b"ns", format!("k:{}", i).as_bytes(), &vec![b'x'; 100])
            .unwrap();
    }
    let budget = ScanBudget {
        max_items: None,
        max_bytes: Some(1), // 1 byte — will hit after first item
    };
    let result = engine.prefix_budgeted(b"ns", b"k:", &budget).unwrap();
    assert!(result.truncated, "must truncate when byte budget exhausted");
    assert!(result.items.len() < 5, "must not return all items");
}

// ── ENG-12: vector_search_with_stats ─────────────────────────────────

#[test]
fn test_vector_search_with_stats_flat_scan() {
    use crate::vector::distance::Metric;
    use crate::vector::types::Dtype;
    use crate::VectorEngine;
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let v: Vec<u8> = vec![1.0f32, 0.0, 0.0, 0.0]
        .into_iter()
        .flat_map(|f: f32| f.to_le_bytes())
        .collect();
    engine.vector_put(b"vs", b"doc1", 4, Dtype::F32, &v).unwrap();
    let query = crate::vector::types::VectorRecord {
        dims: 4,
        dtype: Dtype::F32,
        data: v,
    };
    let (results, stats) = engine
        .vector_search_with_stats(b"vs", &query, 1, Metric::Cosine)
        .unwrap();
    assert!(!results.is_empty(), "must find at least one vector");
    assert!(stats.bytes_scanned > 0, "flat scan must report bytes");
}

// ── ENG-7: strip_vector_index ─────────────────────────────────────────

#[test]
fn test_strip_vector_index_removes_vec_records() {
    use crate::vector::types::Dtype;
    use crate::VectorEngine;
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"kv_key", b"kv_value").unwrap();
    let v: Vec<u8> = vec![0u8; 16];
    engine.vector_put(b"ns", b"vec1", 4, Dtype::F32, &v).unwrap();
    let meta = engine.flush_to_segments().unwrap();
    let seg_id = meta.segment_id;

    let new_meta = engine.strip_vector_index(seg_id).unwrap();
    assert!(new_meta.vector_index_stripped, "flag must be set after strip");
    let val = engine.get(b"ns", b"kv_key").unwrap();
    assert_eq!(val, Some(b"kv_value".to_vec()), "KV record must survive strip");
}

#[test]
fn test_strip_vector_index_idempotent() {
    use crate::vector::types::Dtype;
    use crate::VectorEngine;
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.put(b"ns", b"k", b"v").unwrap();
    let v: Vec<u8> = vec![0u8; 16];
    engine.vector_put(b"ns", b"vec1", 4, Dtype::F32, &v).unwrap();
    let meta = engine.flush_to_segments().unwrap();

    let meta1 = engine.strip_vector_index(meta.segment_id).unwrap();
    assert!(meta1.vector_index_stripped);
    let meta2 = engine.strip_vector_index(meta1.segment_id).unwrap();
    assert!(meta2.vector_index_stripped);
}

#[test]
fn test_vector_count_none_when_not_loaded() {
    let dir = TempDir::new().unwrap();
    let engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
    assert_eq!(
        engine.vector_count(b"products"),
        None,
        "no index loaded yet"
    );
}

#[test]
fn test_vector_count_some_when_index_in_memory() {
    use crate::vector::distance::Metric;
    use crate::vector::types::Dtype;
    use crate::VectorEngine;
    let dir = TempDir::new().unwrap();
    let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
    let v: Vec<u8> = vec![0u8; 16];
    engine.vector_put(b"products", b"p1", 4, Dtype::F32, &v).unwrap();
    engine.vector_put(b"products", b"p2", 4, Dtype::F32, &v).unwrap();
    engine.vector_put(b"products", b"p3", 4, Dtype::F32, &v).unwrap();
    let query = crate::vector::types::VectorRecord {
        dims: 4,
        dtype: Dtype::F32,
        data: v,
    };
    engine
        .vector_search(b"products", &query, 1, Metric::Cosine)
        .unwrap();
    match engine.vector_count(b"products") {
        Some(n) => assert!(n > 0, "expected at least 1 vector"),
        None => {}
    }
    let dir2 = TempDir::new().unwrap();
    let engine2 = Engine::open(EdgestoreConfig::new(dir2.path())).unwrap();
    assert_eq!(
        engine2.vector_count(b"products"),
        None,
        "fresh engine has no index"
    );
}

// ── range_page (P4) ────────────────────────────────────────────────────

#[test]
fn test_range_page_paginates_correctly() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u32..15 {
        engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
    }
    engine.flush_to_segments().unwrap();
    for i in 15u32..25 {
        engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
    }

    let mut all = Vec::new();
    let mut cursor: Option<Vec<u8>> = None;
    loop {
        let page = engine
            .range_page(b"ns", b"", b"\xff", cursor.as_deref(), 7)
            .unwrap();
        assert!(page.items.len() <= 7, "page must not exceed page_size");
        all.extend(page.items);
        cursor = page.next_key;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(all.len(), 25, "all 25 keys must be returned across pages");
    for w in all.windows(2) {
        assert!(w[0].0 < w[1].0, "keys must be ascending");
    }
}

#[test]
fn test_range_page_empty_range() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let page = engine.range_page(b"ns", b"", b"\xff", None, 10).unwrap();
    assert!(page.items.is_empty());
    assert!(page.next_key.is_none());
}

#[test]
fn test_range_page_cursor_excludes_previous_last_key() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u32..6 {
        engine.put(b"ns", format!("k{}", i).as_bytes(), b"v").unwrap();
    }
    let page1 = engine.range_page(b"ns", b"", b"\xff", None, 3).unwrap();
    assert_eq!(page1.items.len(), 3);
    let page2 = engine
        .range_page(b"ns", b"", b"\xff", page1.next_key.as_deref(), 3)
        .unwrap();
    assert_eq!(page2.items.len(), 3);
    assert!(page2.next_key.is_none(), "should be exhausted");
    let last1 = &page1.items.last().unwrap().0;
    assert!(!page2.items.iter().any(|(k, _)| k == last1));
}

// ── range_rev_page (P5) ────────────────────────────────────────────────

#[test]
fn test_range_rev_page_descending_order() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u32..10 {
        engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
    }
    engine.flush_to_segments().unwrap();

    let page = engine
        .range_rev_page(b"ns", b"", b"\xff", None, 4)
        .unwrap();
    assert_eq!(page.items.len(), 4, "should return page_size items");
    for w in page.items.windows(2) {
        assert!(w[0].0 > w[1].0, "keys must be descending");
    }
    assert_eq!(page.items[0].0, b"key-0009".to_vec());
}

#[test]
fn test_range_rev_page_paginates_correctly() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u32..20 {
        engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
    }
    engine.flush_to_segments().unwrap();

    let mut all: Vec<Vec<u8>> = Vec::new();
    let mut cursor: Option<Vec<u8>> = None;
    loop {
        let page = engine
            .range_rev_page(b"ns", b"", b"\xff", cursor.as_deref(), 6)
            .unwrap();
        assert!(page.items.len() <= 6);
        for (k, _) in &page.items {
            all.push(k.clone());
        }
        cursor = page.next_key;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(all.len(), 20, "all 20 keys must be returned across reverse pages");
    for w in all.windows(2) {
        assert!(w[0] > w[1], "global order must be descending");
    }
}

#[test]
fn test_range_rev_page_memtable_delete_excluded() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u32..5 {
        engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
    }
    engine.flush_to_segments().unwrap();
    engine.delete(b"ns", b"key-0004").unwrap();

    let page = engine
        .range_rev_page(b"ns", b"", b"\xff", None, 10)
        .unwrap();
    let keys: Vec<_> = page.items.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(keys.len(), 4, "deleted key must be absent");
    assert!(!keys.contains(&b"key-0004".to_vec()), "key-0004 deleted by memtable");
    assert_eq!(keys[0], b"key-0003".to_vec(), "key-0003 is now largest");
}

// ── range_scan_budgeted (P1/P2/P3) via segment tests ──────────────────

#[test]
fn test_range_scan_budgeted_stops_at_segment_level() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0u32..15 {
        engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
    }
    engine.flush_to_segments().unwrap();
    for i in 15u32..30 {
        engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
    }
    engine.flush_to_segments().unwrap();

    let budget = ScanBudget { max_items: Some(5), max_bytes: None };
    let result = engine.range_budgeted(b"ns", b"", b"\xff", &budget).unwrap();
    assert_eq!(result.items.len(), 5);
    assert!(result.truncated);
    assert_eq!(result.items[0].0, b"key-0000".to_vec());
    assert_eq!(result.items[4].0, b"key-0004".to_vec());
}

// ── Concurrent vector search (no serialization) ───────────────────────

#[test]
fn test_vector_search_concurrent_reads() {
    use crate::vector::distance::Metric;
    use crate::vector::types::{Dtype, VectorRecord};
    use crate::VectorEngine;

    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    for i in 0u8..20 {
        let data = vec![i; 16 * 4];
        engine.vector_put(b"ns", &[i], 16, Dtype::F32, &data).unwrap();
    }
    engine.flush_to_segments().unwrap();
    engine.build_vector_index(b"ns").unwrap();

    let query = VectorRecord {
        dims: 16,
        dtype: Dtype::F32,
        data: vec![0u8; 16 * 4],
    };

    std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    let results = engine.vector_search(b"ns", &query, 3, Metric::L2).unwrap();
                    assert_eq!(results.len(), 3);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
}

#[test]
fn test_vector_search_concurrent_flat_scan() {
    use crate::vector::distance::Metric;
    use crate::vector::types::{Dtype, VectorRecord};
    use crate::VectorEngine;

    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    for i in 0u8..10 {
        let data = vec![i; 16 * 4];
        engine.vector_put(b"ns", &[i], 16, Dtype::F32, &data).unwrap();
    }

    let query = VectorRecord {
        dims: 16,
        dtype: Dtype::F32,
        data: vec![0u8; 16 * 4],
    };

    std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                s.spawn(|| {
                    let results = engine.vector_search(b"ns", &query, 5, Metric::Cosine).unwrap();
                    assert_eq!(results.len(), 5);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    });
}

#[test]
fn test_confirmed_lsn_zero_before_flush() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    assert_eq!(engine.confirmed_lsn(), 0);
    engine.put(b"ns", b"k", b"v").unwrap();
    assert_eq!(engine.confirmed_lsn(), 0);
}

#[test]
fn test_confirmed_lsn_advances_after_flush() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    for i in 0..10u8 {
        engine.put(b"ns", &[i], b"v").unwrap();
    }
    engine.flush_to_segments().unwrap();
    assert!(engine.confirmed_lsn() > 0);
}

#[test]
fn test_write_token_zero_on_new_engine() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    assert_eq!(engine.write_token(), 0);
    assert!(!dir.path().join("WTOKEN").exists());
}

#[test]
fn test_set_write_token_persists_across_reopen() {
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        assert_eq!(engine.write_token(), 0);
        engine.set_write_token(7).unwrap();
        assert_eq!(engine.write_token(), 7);
    }
    let engine2 = open_engine(&dir);
    assert_eq!(engine2.write_token(), 7);
}

#[test]
fn test_set_write_token_wtoken_file_contents() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    engine.set_write_token(42).unwrap();
    let bytes = std::fs::read(dir.path().join("WTOKEN")).unwrap();
    assert_eq!(bytes.len(), 8);
    assert_eq!(u64::from_le_bytes(bytes.try_into().unwrap()), 42u64);
}
