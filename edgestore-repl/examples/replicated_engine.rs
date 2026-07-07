//! `ReplicatedEngine` — primary + replica in one process (integration demo).
//!
//! Shows the full primary + replica topology without manual `HttpReplicationServer`
//! / `AntiEntropyLoop` wiring:
//!
//! - Primary: writable engine + HTTP server on a random port
//! - Replica: read-only engine + anti-entropy pull loop pointed at the primary
//! - Writes to primary are visible on the replica after one anti-entropy cycle
//!
//! ## Running
//!
//! ```sh
//! cargo run --example replicated_engine -p edgestore-repl
//! ```
//!
//! ## In production
//!
//! Replace the in-process primary URL with the actual remote address.
//! Each process owns exactly one `Engine` instance — all namespaces (KV,
//! vector, text) share the same WAL, lock, and flush timer.
//!
//! ```rust,no_run
//! use edgestore::EdgestoreConfig;
//! use edgestore_repl::ReplicatedEngine;
//!
//! // Primary node (machine A)
//! let primary = ReplicatedEngine::open_primary(
//!     EdgestoreConfig::new("/var/db/primary"),
//!     "0.0.0.0:8900",
//! ).unwrap();
//!
//! // Replica node (machine B) — read-only, writes return Err(ReadOnly)
//! let replica = ReplicatedEngine::open_replica(
//!     EdgestoreConfig::new("/var/db/replica"),
//!     "http://machine-a:8900",
//! ).unwrap();
//! ```

use std::time::Duration;

use edgestore::{EdgestoreConfig, EdgestoreError};
use edgestore_repl::ReplicatedEngine;
use tempfile::TempDir;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let primary_dir = TempDir::new()?;
    let replica_dir = TempDir::new()?;

    println!("=== ReplicatedEngine demo ===\n");

    // ── Primary: writable engine + HTTP server on a random port ───────────
    let primary = ReplicatedEngine::open_primary(
        EdgestoreConfig::new(primary_dir.path()),
        "127.0.0.1:0", // OS-assigned port
    )?;
    let port = primary.bound_port().unwrap();
    let primary_url = format!("http://127.0.0.1:{}", port);
    println!("Primary listening on {}", primary_url);

    // Write data to the primary.
    {
        let engine = primary.engine().lock().unwrap();
        // engine is a MutexGuard<Engine>; drop guard before re-locking below
        drop(engine);
    }
    {
        let mut engine = primary.engine().lock().unwrap();
        engine.put(b"catalog", b"item1", b"Widget A")?;
        engine.put(b"catalog", b"item2", b"Widget B")?;
        engine.put(b"catalog", b"item3", b"Gadget X")?;
        engine.flush_to_segments()?;
        println!("Primary: wrote 3 items and flushed to segment.");
    }

    // ── Replica: read-only engine + anti-entropy pull loop ─────────────────
    let mut replica_cfg = EdgestoreConfig::new(replica_dir.path());
    replica_cfg.readonly = true; // open_replica sets this automatically
    let replica = ReplicatedEngine::open_replica(
        EdgestoreConfig::new(replica_dir.path()),
        &primary_url,
    )?;
    println!("Replica connected to {}", primary_url);

    // Verify replica rejects writes.
    match replica.engine().lock().unwrap().put(b"catalog", b"rogue", b"write") {
        Err(EdgestoreError::ReadOnly) => println!("Replica: write correctly rejected (ReadOnly)."),
        other => panic!("Expected ReadOnly, got {:?}", other),
    }

    // Give anti-entropy loop one cycle to pull from the primary.
    // In production the loop runs continuously; here we just wait one interval.
    println!("Waiting for anti-entropy pull cycle...");
    std::thread::sleep(Duration::from_secs(35));

    // Replica should now have the primary's data.
    let items = replica.engine().lock().unwrap().range(b"catalog", b"", b"\xff")?;
    println!(
        "Replica: {} items visible after sync (expected 3).",
        items.len()
    );
    for (k, v) in &items {
        println!(
            "  {} = {}",
            String::from_utf8_lossy(k),
            String::from_utf8_lossy(v)
        );
    }

    println!("\nDone.");
    Ok(())
}
