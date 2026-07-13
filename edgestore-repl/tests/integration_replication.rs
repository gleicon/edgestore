/// Integration tests for Phase 4 replication success criteria (SC1, SC4, SC5).
///
/// SC1: Two-engine sync via HTTP. Engine A writes 10 records and flushes. AntiEntropyLoop
///      on Engine B detects Merkle divergence, pulls A's segment, B reads all 10 keys.
/// SC4: FilesystemRemoteStore round-trip — upload, download, BLAKE3 verify, delete.
/// SC5: GET /merkle?debug=json and GET /segments?debug=json return valid JSON.
///
/// SC2 and SC3 (cursor + LWW) live in edgestore/tests/integration_replication.rs
/// because they do not require the HTTP stack.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use edgestore::{EdgestoreConfig, Engine, RemoteStore};
use edgestore_repl::{AntiEntropyLoop, FilesystemRemoteStore, HttpReplicationServer};
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
}

fn open_engine_small(dir: &TempDir) -> Engine {
    let mut cfg = EdgestoreConfig::new(dir.path());
    // AVG_ENTRY_SIZE_ESTIMATE = 256; segment_size_bytes = 256 → flush after 1 entry.
    cfg.segment_size_bytes = 256;
    Engine::open(cfg).unwrap()
}

// ── SC1: Two-engine sync via HTTP ─────────────────────────────────────────────

/// Success Criterion 1 (REPL-01, REPL-02, D02, D08):
/// Engine A writes 10 records and flushes to a segment. Engine B starts with no data.
/// AntiEntropyLoop on B probes A's HTTP server, detects a Merkle root difference,
/// pulls A's missing segment, and applies it. After the loop cycle, all 10 keys
/// must be readable in B and B's Merkle root must match A's.
#[test]
fn test_sc1_two_engine_sync_via_http() {
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();

    // ── Engine A: write 10 keys, flush to segment ──────────────────────────────
    let engine_a: Arc<Mutex<Engine>> = {
        let mut a = open_engine_small(&dir_a);
        for i in 0u32..10 {
            a.put(b"ns", format!("key{:03}", i).as_bytes(), b"value_from_a")
                .unwrap();
        }
        // Flush any memtable entries to produce a segment.
        let _ = a.flush_to_segments();

        // Verify A has at least one segment.
        let refs = a.export_manifest().unwrap();
        assert!(
            !refs.is_empty(),
            "SC1: Engine A must have at least 1 segment after writes+flush"
        );

        Arc::new(Mutex::new(a))
    };

    // ── Start HTTP server for Engine A on a random port ──────────────────────
    let server = HttpReplicationServer::new(Arc::clone(&engine_a));
    let (_handle_a, port_a) = server.start("127.0.0.1:0").unwrap();
    let peer_url = format!("http://127.0.0.1:{}", port_a);

    // Brief pause so tiny_http is accepting requests before B probes.
    std::thread::sleep(Duration::from_millis(50));

    // ── Engine B: start fresh, no segments ────────────────────────────────────
    let engine_b: Arc<Mutex<Engine>> = Arc::new(Mutex::new(open_engine(&dir_b)));

    // Verify B has no segments and different Merkle root from A.
    {
        let a = engine_a.lock().unwrap();
        let b = engine_b.lock().unwrap();
        let a_root = a.range_merkle_root().unwrap();
        let b_root = b.range_merkle_root().unwrap();
        assert_ne!(
            a_root, b_root,
            "SC1: A and B must have different Merkle roots before sync"
        );
    }

    // ── Start AntiEntropyLoop on B, pointing to A's server ───────────────────
    // Use interval_secs = 1 so the first cycle completes quickly in the test.
    let loop_b = AntiEntropyLoop::new(
        Arc::clone(&engine_b),
        peer_url,
        "peer-a".to_string(),
        dir_b.path().to_path_buf(),
    )
    .with_interval(1);

    let _handle_b = loop_b.start();

    // Wait long enough for the anti-entropy loop to complete one cycle.
    // Interval is 1s; allow 4s total for the cycle to run and segment to be applied.
    std::thread::sleep(Duration::from_secs(4));

    // ── Assert B is now in sync with A ────────────────────────────────────────
    let a_root = engine_a.lock().unwrap().range_merkle_root().unwrap();
    let b_in_sync = engine_b.lock().unwrap().compare_merkle(&a_root).unwrap();
    assert!(
        b_in_sync,
        "SC1: B's Merkle root must match A's after anti-entropy sync"
    );

    // ── Verify all 10 keys are readable in B ─────────────────────────────────
    let b = engine_b.lock().unwrap();
    for i in 0u32..10 {
        let key = format!("key{:03}", i);
        let val = b.get(b"ns", key.as_bytes()).unwrap();
        assert!(
            val.is_some(),
            "SC1: key '{}' must be readable in B after sync",
            key
        );
        assert_eq!(
            val.unwrap(),
            b"value_from_a".to_vec(),
            "SC1: key '{}' value mismatch in B after sync",
            key
        );
    }
}

// ── SC4: FilesystemRemoteStore BLAKE3 round-trip ──────────────────────────────

/// Success Criterion 4 (REPL-04, D04, D05, T-04-14):
/// Upload segment bytes, download them back, verify BLAKE3 hash matches.
/// Delete makes download fail.
#[test]
fn test_sc4_filesystem_remote_store_roundtrip() {
    let dir = TempDir::new().unwrap();
    let store = FilesystemRemoteStore::new(dir.path().to_path_buf())
        .expect("FilesystemRemoteStore::new must succeed");

    // Generate 1024 bytes of test data.
    let data: Vec<u8> = (0u16..1024).map(|i| (i % 256) as u8).collect();

    // Compute BLAKE3 of the test data.
    let hash: [u8; 32] = *blake3::hash(&data).as_bytes();

    // Upload.
    store.upload(&hash, &data).expect("upload must succeed");

    // Download.
    let downloaded = store.download(&hash).expect("download must succeed");

    // Assert byte equality.
    assert_eq!(
        downloaded, data,
        "SC4: downloaded bytes must exactly match uploaded bytes"
    );

    // Assert BLAKE3 of downloaded matches BLAKE3 of original (T-04-14).
    let downloaded_hash: [u8; 32] = *blake3::hash(&downloaded).as_bytes();
    assert_eq!(
        downloaded_hash, hash,
        "SC4: BLAKE3 of downloaded bytes must match original hash"
    );

    // Delete the segment.
    store.delete(&hash).expect("delete must succeed");

    // Download after delete must fail.
    let result = store.download(&hash);
    assert!(
        result.is_err(),
        "SC4: download after delete must return Err; got Ok"
    );
}

// ── SC5: ?debug=json endpoint ─────────────────────────────────────────────────

/// Success Criterion 5 (REPL-03, D07):
/// GET /merkle?debug=json returns valid JSON with a "root" key.
/// GET /segments?debug=json returns valid JSON array where each element has
/// "segment_id" and "segment_hash" keys.
#[test]
fn test_sc5_debug_json_endpoint() {
    let dir_a = TempDir::new().unwrap();

    // Write 3 records and flush to produce at least one segment.
    let engine_a: Arc<Mutex<Engine>> = {
        let mut a = open_engine(&dir_a);
        a.put(b"ns", b"k1", b"v1").unwrap();
        a.put(b"ns", b"k2", b"v2").unwrap();
        a.put(b"ns", b"k3", b"v3").unwrap();
        a.flush_to_segments().unwrap();
        Arc::new(Mutex::new(a))
    };

    // Start server on a random port.
    let server = HttpReplicationServer::new(Arc::clone(&engine_a));
    let (_handle, port) = server.start("127.0.0.1:0").unwrap();

    // Brief pause so tiny_http is accepting requests.
    std::thread::sleep(Duration::from_millis(100));

    let base = format!("http://127.0.0.1:{}", port);

    // ── GET /merkle?debug=json ────────────────────────────────────────────────
    let resp = ureq::get(&format!("{}/merkle?debug=json", base))
        .call()
        .expect("GET /merkle?debug=json must succeed");

    assert_eq!(
        resp.status(),
        200,
        "SC5: /merkle?debug=json must return 200"
    );

    // Content-Type must include application/json.
    let content_type = resp.header("Content-Type").unwrap_or("").to_string();
    assert!(
        content_type.contains("application/json"),
        "SC5: /merkle?debug=json Content-Type must be application/json; got '{}'",
        content_type
    );

    let body = resp.into_string().expect("must read body as string");
    let json: serde_json::Value =
        serde_json::from_str(&body).expect("SC5: /merkle?debug=json body must be valid JSON");

    assert!(
        json.is_object(),
        "SC5: /merkle?debug=json must return a JSON object"
    );
    assert!(
        json.get("root").is_some(),
        "SC5: /merkle?debug=json must contain 'root' key; got: {}",
        json
    );

    // ── GET /segments?debug=json ──────────────────────────────────────────────
    let resp2 = ureq::get(&format!("{}/segments?debug=json", base))
        .call()
        .expect("GET /segments?debug=json must succeed");

    assert_eq!(
        resp2.status(),
        200,
        "SC5: /segments?debug=json must return 200"
    );

    let body2 = resp2.into_string().expect("must read body as string");
    let json2: serde_json::Value =
        serde_json::from_str(&body2).expect("SC5: /segments?debug=json body must be valid JSON");

    assert!(
        json2.is_array(),
        "SC5: /segments?debug=json must return a JSON array; got: {}",
        json2
    );

    let arr = json2.as_array().unwrap();
    assert!(
        !arr.is_empty(),
        "SC5: /segments?debug=json array must have at least 1 element (we flushed 3 records)"
    );

    // Each element must have "segment_id" and "segment_hash" keys.
    for (i, element) in arr.iter().enumerate() {
        assert!(
            element.get("segment_id").is_some(),
            "SC5: segment[{}] must have 'segment_id' key; got: {}",
            i,
            element
        );
        assert!(
            element.get("segment_hash").is_some(),
            "SC5: segment[{}] must have 'segment_hash' key; got: {}",
            i,
            element
        );
    }
}
