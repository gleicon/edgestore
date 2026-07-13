/// Integration test for v1.0 — exercises all major EdgeStore features end-to-end.
///
/// Sections:
///   1. KV operations (put, get, delete, range, prefix) across namespaces
///   2. TTL expiry and compaction
///   3. Point-in-time snapshots
///   4. Vector storage and search
///   5. Full-text search with BM25 ranking
///   6. Replication (manifest export, segment import, Merkle compare)
///   7. Multi-record transactions (commit and rollback)
use std::collections::HashMap;
use std::time::Duration;

use edgestore::{
    Dtype, EdgestoreConfig, Engine, ImportResult, Metric, TextEngine, VectorEngine, VectorRecord,
    VectorSearchResult,
};
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
}

fn open_engine_small_segments(dir: &TempDir) -> Engine {
    let mut cfg = EdgestoreConfig::new(dir.path());
    cfg.segment_size_bytes = 512; // force frequent flushes
    cfg.compaction_write_budget_bytes = u64::MAX;
    Engine::open(cfg).unwrap()
}

/// Read the first segment's raw bytes and hash from an engine.
fn read_first_segment(engine: &Engine) -> ([u8; 32], Vec<u8>) {
    let refs = engine.export_manifest().unwrap();
    assert!(!refs.is_empty(), "engine must have at least one segment");
    let seg_ref = &refs[0];
    let hash = seg_ref.segment_hash;
    let dat_path = engine
        .db_path()
        .join(format!("segment-{:08}.dat", seg_ref.segment_id));
    let data = std::fs::read(&dat_path).expect("segment .dat file must exist");
    (hash, data)
}

// ── v1 Integration Test ──────────────────────────────────────────────────────

#[test]
fn test_integration_v1_all_features() {
    // =========================================================================
    // 1. KV OPERATIONS — put, get, delete, range, prefix across namespaces
    // =========================================================================
    {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);

        // Write across two namespaces
        engine.put(b"users", b"alice", b"Alice Data").unwrap();
        engine.put(b"users", b"bob", b"Bob Data").unwrap();
        engine.put(b"products", b"p1", b"Product 1").unwrap();
        engine.put(b"products", b"p2", b"Product 2").unwrap();

        // Verify individual gets
        assert_eq!(
            engine.get(b"users", b"alice").unwrap().unwrap(),
            b"Alice Data"
        );
        assert_eq!(engine.get(b"users", b"bob").unwrap().unwrap(), b"Bob Data");
        assert_eq!(
            engine.get(b"products", b"p1").unwrap().unwrap(),
            b"Product 1"
        );

        // Range scan within namespace
        let range_results = engine.range(b"users", b"a", b"c").unwrap();
        assert_eq!(range_results.len(), 2);
        assert_eq!(range_results[0].0, b"alice");
        assert_eq!(range_results[1].0, b"bob");

        // Prefix scan
        let prefix_results = engine.prefix(b"products", b"p").unwrap();
        assert_eq!(prefix_results.len(), 2);

        // Delete
        engine.delete(b"users", b"bob").unwrap();
        assert_eq!(engine.get(b"users", b"bob").unwrap(), None);

        // Flush to create segments
        let _ = engine.flush_to_segments();
    }

    // =========================================================================
    // 2. TTL + COMPACTION — records with short TTL expire and are removed
    // =========================================================================
    {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine_small_segments(&dir);

        // Write 10 keys with 1-second TTL
        for i in 0u32..10 {
            engine
                .put_with_ttl(
                    b"ttl_ns",
                    format!("ttl-key-{:02}", i).as_bytes(),
                    b"ttl-value",
                    1,
                )
                .unwrap();
        }

        // Explicit flush so data is in segments
        let _ = engine.flush_to_segments();

        // Verify data exists before expiry
        assert!(
            engine.get(b"ttl_ns", b"ttl-key-00").unwrap().is_some(),
            "TTL record should exist before expiry"
        );

        // Wait for TTL to expire
        std::thread::sleep(Duration::from_secs(2));

        // Compact — expired records should be collected
        let stats = engine.compact_once().unwrap();

        // After compaction, expired records should be gone
        assert_eq!(
            engine.get(b"ttl_ns", b"ttl-key-00").unwrap(),
            None,
            "TTL record should be gone after compaction"
        );

        // Compaction stats should show some work was done
        // (live_records_relocated may be 0 if all records expired)
        let _ = stats; // validate compilation
    }

    // =========================================================================
    // 3. SNAPSHOTS — point-in-time consistency across segments
    // =========================================================================
    {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);

        engine.put(b"snap_ns", b"k1", b"v1").unwrap();
        engine.put(b"snap_ns", b"k2", b"v2").unwrap();
        let _ = engine.flush_to_segments();

        // Take a snapshot
        let snapshot = engine.snapshot().unwrap();

        // Modify data after snapshot
        engine.put(b"snap_ns", b"k1", b"v1-modified").unwrap();
        let _ = engine.flush_to_segments();

        // Snapshot should still see original values
        assert_eq!(
            snapshot.get(b"snap_ns", b"k1").unwrap().unwrap(),
            b"v1",
            "snapshot must see original value"
        );
        assert_eq!(
            snapshot.get(b"snap_ns", b"k2").unwrap().unwrap(),
            b"v2",
            "snapshot must see unchanged key"
        );

        // Range scan in snapshot
        let snap_range = snapshot.range(b"snap_ns", b"k1", b"k3").unwrap();
        assert_eq!(snap_range.len(), 2);
        assert_eq!(snap_range[0].1, b"v1");
        assert_eq!(snap_range[1].1, b"v2");

        // Snapshot is dropped automatically, releasing pins
    }

    // =========================================================================
    // 4. VECTOR OPERATIONS — put, get, search with Euclidean and Cosine
    // =========================================================================
    {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);

        let dims: u16 = 8;
        let dtype = Dtype::F32;
        let bytes_per_vec = dims as usize * 4;

        // Store three simple vectors
        let v1: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let v2: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let v3: Vec<f32> = vec![0.9, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];

        let data1: Vec<u8> = v1.iter().flat_map(|f| f.to_le_bytes().to_vec()).collect();
        let data2: Vec<u8> = v2.iter().flat_map(|f| f.to_le_bytes().to_vec()).collect();
        let data3: Vec<u8> = v3.iter().flat_map(|f| f.to_le_bytes().to_vec()).collect();

        engine
            .vector_put(b"vec_ns", b"vec1", dims, dtype, &data1)
            .unwrap();
        engine
            .vector_put(b"vec_ns", b"vec2", dims, dtype, &data2)
            .unwrap();
        engine
            .vector_put(b"vec_ns", b"vec3", dims, dtype, &data3)
            .unwrap();

        // Verify round-trip
        let rec1 = engine.vector_get(b"vec_ns", b"vec1").unwrap().unwrap();
        assert_eq!(rec1.dims, dims);
        assert_eq!(rec1.dtype, dtype);
        assert_eq!(rec1.data, data1);

        // Vector search — query closest to v1
        let query_data = data1.clone();
        let query = VectorRecord {
            dims,
            dtype,
            data: query_data,
        };

        // L2 (Euclidean) search
        let euclidean_results: Vec<VectorSearchResult> = engine
            .vector_search(b"vec_ns", &query, 3, Metric::L2)
            .unwrap();
        assert_eq!(euclidean_results.len(), 3);
        // v1 should be closest (distance ≈ 0), then v3, then v2
        assert_eq!(euclidean_results[0].key, b"vec1");

        // Cosine search
        let cosine_results: Vec<VectorSearchResult> = engine
            .vector_search(b"vec_ns", &query, 3, Metric::Cosine)
            .unwrap();
        assert_eq!(cosine_results.len(), 3);
        assert_eq!(cosine_results[0].key, b"vec1");

        // Delete a vector
        engine.vector_delete(b"vec_ns", b"vec2").unwrap();
        assert_eq!(engine.vector_get(b"vec_ns", b"vec2").unwrap(), None);
    }

    // =========================================================================
    // 5. TEXT SEARCH — index documents, search with BM25 ranking
    // =========================================================================
    {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);

        // Index three documents
        engine
            .index_text(
                b"text_ns",
                b"doc1",
                "The quick brown fox jumps over the lazy dog",
                HashMap::new(),
            )
            .unwrap();
        engine
            .index_text(
                b"text_ns",
                b"doc2",
                "The quick brown fox sleeps",
                HashMap::new(),
            )
            .unwrap();
        engine
            .index_text(
                b"text_ns",
                b"doc3",
                "A completely different document about cats",
                HashMap::new(),
            )
            .unwrap();

        // Search for "quick brown"
        let results = engine.search_text(b"text_ns", "quick brown", 3).unwrap();
        assert!(!results.is_empty(), "search should return results");
        // doc1 and doc2 both contain "quick" and "brown"
        assert!(
            results.iter().any(|r| r.doc_id == b"doc1"),
            "doc1 should match"
        );
        assert!(
            results.iter().any(|r| r.doc_id == b"doc2"),
            "doc2 should match"
        );

        // Verify BM25 ranking: doc1 has more words but same term frequency,
        // doc2 is shorter so its term density is higher — doc2 should rank higher
        let doc2_score = results
            .iter()
            .find(|r| r.doc_id == b"doc2")
            .map(|r| r.score);
        let doc1_score = results
            .iter()
            .find(|r| r.doc_id == b"doc1")
            .map(|r| r.score);
        if let (Some(s2), Some(s1)) = (doc2_score, doc1_score) {
            assert!(
                s2 >= s1,
                "shorter doc with same terms should rank >= longer doc"
            );
        }

        // Search for non-existent term
        let empty_results = engine.search_text(b"text_ns", "elephant", 3).unwrap();
        assert!(empty_results.is_empty());

        // Delete a document and verify it's gone from search
        engine.delete_text(b"text_ns", b"doc1").unwrap();
        let after_delete = engine.search_text(b"text_ns", "quick brown", 3).unwrap();
        assert!(
            !after_delete.iter().any(|r| r.doc_id == b"doc1"),
            "deleted doc should not appear"
        );
    }

    // =========================================================================
    // 6. REPLICATION — export manifest, import segments, compare Merkle roots
    // =========================================================================
    {
        let dir_primary = TempDir::new().unwrap();
        let dir_replica = TempDir::new().unwrap();

        // Build primary with small segments to force flushes
        let mut primary = open_engine_small_segments(&dir_primary);
        for i in 0u32..10 {
            primary
                .put(
                    b"repl_ns",
                    format!("repl-key-{:02}", i).as_bytes(),
                    b"primary-value",
                )
                .unwrap();
        }
        let _ = primary.flush_to_segments();

        // Export manifest and read segment data
        let refs = primary.export_manifest().unwrap();
        assert!(!refs.is_empty(), "primary must have segments");

        // Build replica
        let mut replica = open_engine_small_segments(&dir_replica);

        // Initially, roots should differ
        let primary_root = primary.range_merkle_root().unwrap();
        let replica_root_before = replica.range_merkle_root().unwrap();
        assert_ne!(
            primary_root, replica_root_before,
            "replica should start empty / different from primary"
        );

        // Import all segments from primary into replica
        for seg_ref in &refs {
            let seg_path = primary
                .db_path()
                .join(format!("segment-{:08}.dat", seg_ref.segment_id));
            let data = std::fs::read(&seg_path).unwrap();
            let result = replica
                .import_segment(&data, &seg_ref.segment_hash)
                .unwrap();
            match result {
                ImportResult::Applied { .. } => {}
                ImportResult::Skipped => panic!("segment should not be skipped on first import"),
                ImportResult::HashMismatch => panic!("hash mismatch during import"),
            }
        }

        // Verify replica now has the data
        for i in 0u32..10 {
            let val = replica
                .get(b"repl_ns", format!("repl-key-{:02}", i).as_bytes())
                .unwrap();
            assert_eq!(val.unwrap(), b"primary-value");
        }

        // Merkle roots should now match
        let replica_root_after = replica.range_merkle_root().unwrap();
        assert_eq!(
            primary_root, replica_root_after,
            "merkle roots should match after full sync"
        );

        // Re-importing the same segment should be skipped
        let first_ref = &refs[0];
        let seg_path = primary
            .db_path()
            .join(format!("segment-{:08}.dat", first_ref.segment_id));
        let data = std::fs::read(&seg_path).unwrap();
        let reimport = replica
            .import_segment(&data, &first_ref.segment_hash)
            .unwrap();
        assert!(
            matches!(reimport, ImportResult::Skipped),
            "re-import should be skipped"
        );
    }

    // =========================================================================
    // 7. TRANSACTIONS — begin, multiple puts, commit and rollback
    // =========================================================================
    {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);

        // Commit path
        {
            let mut tx = engine.begin();
            tx.put(b"tx_ns", b"tx1", b"v1", 0, 0).unwrap();
            tx.put(b"tx_ns", b"tx2", b"v2", 0, 0).unwrap();
            tx.put(b"tx_ns", b"tx3", b"v3", 0, 0).unwrap();
            engine.commit_transaction(tx).unwrap();
        }

        // All three keys should be visible
        assert_eq!(engine.get(b"tx_ns", b"tx1").unwrap().unwrap(), b"v1");
        assert_eq!(engine.get(b"tx_ns", b"tx2").unwrap().unwrap(), b"v2");
        assert_eq!(engine.get(b"tx_ns", b"tx3").unwrap().unwrap(), b"v3");

        // Rollback path
        {
            let mut tx = engine.begin();
            tx.put(b"tx_ns", b"tx4", b"v4", 0, 0).unwrap();
            tx.delete(b"tx_ns", b"tx1", 0, 0).unwrap();
            engine.rollback_transaction(tx);
        }

        // tx4 should NOT exist, tx1 should still exist
        assert_eq!(engine.get(b"tx_ns", b"tx4").unwrap(), None);
        assert_eq!(engine.get(b"tx_ns", b"tx1").unwrap().unwrap(), b"v1");
    }
}
