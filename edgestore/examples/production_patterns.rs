//! Production patterns — flush callback, vector count, read-only guard.
//!
//! Demonstrates three patterns for production deployments:
//!
//! 1. **Flush callback** — react to segment flushes immediately (e.g. wake a
//!    replication loop) rather than polling on a fixed interval.
//!
//! 2. **`vector_count`** — query in-memory HNSW size without an O(n) prefix scan
//!    or an external counter that resets on restart.
//!
//! 3. **Read-only engine** — open a replica-side engine that rejects all writes
//!    at the API level, preventing accidental divergence from the primary.
//!
//! For the full primary + replica HTTP wiring, see `edgestore-repl`'s
//! `ReplicatedEngine::open_primary` / `open_replica`.
//!
//! ## Running
//!
//! ```sh
//! cargo run --example production_patterns
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use edgestore::vector::distance::Metric;
use edgestore::vector::types::{Dtype, VectorRecord};
use edgestore::{EdgestoreConfig, EdgestoreError, Engine, VectorEngine};
use tempfile::TempDir;

fn main() {
    pattern_flush_callback();
    pattern_vector_count();
    pattern_readonly_guard();
}

// ── Pattern 1: flush callback ──────────────────────────────────────────────

fn pattern_flush_callback() {
    println!("=== Pattern 1: flush callback ===");

    let dir = TempDir::new().unwrap();
    let flush_count = Arc::new(AtomicU64::new(0));
    let flush_count2 = flush_count.clone();

    let mut engine = Engine::open(EdgestoreConfig::new(dir.path()))
        .unwrap()
        .with_on_segment_flushed(move |meta| {
            let n = flush_count2.fetch_add(1, Ordering::Relaxed) + 1;
            println!(
                "  [callback] flush #{}: segment {} ({} bytes compressed)",
                n, meta.segment_id, meta.compressed_bytes,
            );
            // In a real service: notify a condvar, send on a channel, or
            // set an atomic flag to wake an anti-entropy thread.
            // Keep this fast — it runs synchronously on the calling thread.
        });

    for i in 0u32..5 {
        engine.put(b"events", format!("e{:04}", i).as_bytes(), b"payload").unwrap();
    }

    engine.flush_to_segments().unwrap(); // fires callback once
    engine.put(b"events", b"e0005", b"payload2").unwrap();
    engine.flush_to_segments().unwrap(); // fires callback again

    println!(
        "  Total flushes observed by callback: {}",
        flush_count.load(Ordering::Relaxed)
    );
    println!();
}

// ── Pattern 2: vector_count ────────────────────────────────────────────────

fn pattern_vector_count() {
    println!("=== Pattern 2: vector_count ===");

    let dir = TempDir::new().unwrap();
    let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

    // Before any vectors: index not loaded, count is None.
    println!(
        "  Before inserts: vector_count = {:?}",
        engine.vector_count(b"products")
    );

    // Insert 3 vectors (4-dim f32 = 16 bytes each).
    let v: Vec<u8> = (0u32..4).flat_map(|_| 1.0f32.to_le_bytes()).collect();
    engine.vector_put(b"products", b"p1", 4, Dtype::F32, &v).unwrap();
    engine.vector_put(b"products", b"p2", 4, Dtype::F32, &v).unwrap();
    engine.vector_put(b"products", b"p3", 4, Dtype::F32, &v).unwrap();

    // Index still not loaded (only stored as KV bytes so far).
    println!(
        "  After put, before search: vector_count = {:?}",
        engine.vector_count(b"products")
    );

    // Build the HNSW index. This writes a sidecar file and caches in memory.
    // Call this once after bulk-loading vectors, then search against the index.
    engine.build_vector_index(b"products").unwrap();

    // vector_count now reflects the in-memory HNSW node count.
    // No O(n) prefix scan. No external AtomicU64 that resets on restart.
    println!(
        "  After build_vector_index: vector_count = {:?}",
        engine.vector_count(b"products")
    );
    println!("  (None = index not loaded; Some(n) = HNSW node count)");

    // After a restart, preload_vector_index reloads from the sidecar file.
    // vector_count returns Some again without rebuilding or scanning.
    let query = VectorRecord { dims: 4, dtype: Dtype::F32, data: v };
    engine.vector_search(b"products", &query, 2, Metric::Cosine).unwrap();
    println!();
}

// ── Pattern 3: read-only guard ─────────────────────────────────────────────

fn pattern_readonly_guard() {
    println!("=== Pattern 3: read-only guard ===");

    let dir = TempDir::new().unwrap();

    // Write some data with a writable primary engine.
    {
        let mut primary = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        primary.put(b"catalog", b"item1", b"Widget A").unwrap();
        primary.put(b"catalog", b"item2", b"Widget B").unwrap();
        primary.flush_to_segments().unwrap();
        println!("  Primary wrote 2 items and flushed.");
    }

    // Open a read-only replica of the same directory.
    // In a real deployment this would be a different machine with segments
    // replicated by AntiEntropyLoop / ReplicatedEngine.
    let mut replica = Engine::open_readonly(EdgestoreConfig::new(dir.path())).unwrap();

    // Reads work fine.
    let val = replica.get(b"catalog", b"item1").unwrap();
    println!("  Replica reads item1: {:?}", val.map(|v| String::from_utf8_lossy(&v).into_owned()));

    // All write paths return Err(ReadOnly) — no divergence possible.
    match replica.put(b"catalog", b"item3", b"rogue write") {
        Err(EdgestoreError::ReadOnly) => println!("  replica.put → Err(ReadOnly) ✓"),
        other => panic!("expected ReadOnly, got {:?}", other),
    }
    match replica.delete(b"catalog", b"item1") {
        Err(EdgestoreError::ReadOnly) => println!("  replica.delete → Err(ReadOnly) ✓"),
        other => panic!("expected ReadOnly, got {:?}", other),
    }

    // Range scan on replica still works.
    let items = replica.range(b"catalog", b"", b"\xff").unwrap();
    println!("  Replica range scan: {} items", items.len());
    println!();

    // Suppress unused import warning
    let _ = HashMap::<String, String>::new();
}
