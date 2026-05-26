//! Replication example — demonstrates export/import segment sync between two engines.
//!
//! Run with: cargo run --example replication

use edgestore::{EdgestoreConfig, Engine};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let primary_path = PathBuf::from("/tmp/edgestore_replication_primary");
    let replica_path = PathBuf::from("/tmp/edgestore_replication_replica");

    // Clean up any previous runs
    let _ = std::fs::remove_dir_all(&primary_path);
    let _ = std::fs::remove_dir_all(&replica_path);

    println!("=== EdgeStore Replication Example ===\n");

    // 1. Setup primary engine
    let mut primary = Engine::open(EdgestoreConfig::new(&primary_path))?;
    println!("Primary engine opened at {:?}", primary_path);

    // 2. Write data to primary
    for i in 0..100 {
        let key = format!("key_{:03}", i);
        let val = format!("value_{:03}", i);
        primary.put(b"sync_ns", key.as_bytes(), val.as_bytes())?;
    }
    println!("Wrote 100 keys to primary.\n");

    // 3. Flush memtable to segments so there is data to export
    let _meta = primary.flush_to_segments()?;
    println!("Flushed primary memtable to segments.\n");

    // 4. Export manifest from primary
    let manifest = primary.export_manifest()?;
    println!("Exported primary manifest: {} segment(s)", manifest.len());

    // 5. Setup replica engine
    let mut replica = Engine::open(EdgestoreConfig::new(&replica_path))?;
    println!("Replica engine opened at {:?}\n", replica_path);

    // 6. Compare merkle roots before sync
    let primary_root = primary.range_merkle_root()?;
    let in_sync_before = replica.compare_merkle(&primary_root)?;
    println!("In sync before transfer: {}", in_sync_before);

    // 7. Transfer segments: for each segment in manifest, read raw bytes from primary and import into replica
    let mut imported = 0usize;
    for seg_ref in &manifest {
        let dat_path = primary.db_path().join(format!("segment-{:08}.dat", seg_ref.segment_id));
        let data = std::fs::read(&dat_path)?;
        let result = replica.import_segment(&data, &seg_ref.segment_hash)?;
        match result {
            edgestore::ImportResult::Applied { keys_written, keys_skipped } => {
                println!("  Imported segment {} (keys_written={}, keys_skipped={})", seg_ref.segment_id, keys_written, keys_skipped);
                imported += 1;
            }
            edgestore::ImportResult::Skipped => {
                println!("  Segment {} already present — skipped", seg_ref.segment_id);
            }
            edgestore::ImportResult::HashMismatch => {
                eprintln!("  ERROR: Segment {} hash mismatch!", seg_ref.segment_id);
            }
        }
    }
    println!("\nTransferred {} segment(s) to replica.\n", imported);

    // 8. Compare merkle roots after sync
    let in_sync_after = replica.compare_merkle(&primary_root)?;
    println!("In sync after transfer: {}", in_sync_after);

    // 9. Verify a sample key on replica
    let sample = replica.get(b"sync_ns", b"key_042")?;
    println!("Replica sample get (key_042): {:?}", sample.as_deref().map(|v| String::from_utf8_lossy(v)));

    // Cleanup
    drop(primary);
    drop(replica);
    let _ = std::fs::remove_dir_all(&primary_path);
    let _ = std::fs::remove_dir_all(&replica_path);

    println!("\nReplication example complete.");
    Ok(())
}
