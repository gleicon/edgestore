//! Basic KV example — demonstrates put, get, range, prefix, delete, and flush.
//!
//! Run with: cargo run --example basic_kv

use edgestore::{EdgestoreConfig, Engine};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = PathBuf::from("/tmp/edgestore_basic_kv_example");
    // Clean up any previous run
    let _ = std::fs::remove_dir_all(&db_path);

    println!("=== EdgeStore Basic KV Example ===\n");

    let config = EdgestoreConfig::new(&db_path);
    let mut engine = Engine::open(config)?;
    println!("Opened database at {:?}\n", db_path);

    // Put keys in three different namespaces
    engine.put(b"users", b"alice", b"admin")?;
    engine.put(b"users", b"bob", b"editor")?;
    engine.put(b"products", b"sku-42", b"Widget Pro")?;
    engine.put(b"products", b"sku-99", b"Gadget Lite")?;
    engine.put(b"metadata", b"version", b"1.0.0")?;
    println!("Put 5 keys across 3 namespaces: users, products, metadata\n");

    // Get one key
    let alice = engine.get(b"users", b"alice")?;
    println!("Get users/alice: {:?}\n", alice.as_deref().map(|v| String::from_utf8_lossy(v)));

    // Range scan within a namespace
    println!("Range scan products [sku-42, sku-99]:");
    for (key, val) in engine.range(b"products", b"sku-42", b"sku-99\x00")? {
        println!("  {} => {}", String::from_utf8_lossy(&key), String::from_utf8_lossy(&val));
    }
    println!();

    // Prefix scan
    println!("Prefix scan products (sku-):");
    for (key, val) in engine.prefix(b"products", b"sku-")? {
        println!("  {} => {}", String::from_utf8_lossy(&key), String::from_utf8_lossy(&val));
    }
    println!();

    // Delete a key
    engine.delete(b"users", b"bob")?;
    let bob = engine.get(b"users", b"bob")?;
    println!("After delete, get users/bob: {:?}\n", bob);

    // Flush WAL to disk
    engine.flush()?;
    println!("Flushed WAL to disk.\n");

    // Close by dropping — exclusive lock is released
    drop(engine);
    println!("Closed database. Cleanup complete.");

    // Clean up
    let _ = std::fs::remove_dir_all(&db_path);
    Ok(())
}
