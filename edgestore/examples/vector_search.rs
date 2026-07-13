//! Vector search example — demonstrates inserting vectors and performing ANN search.
//!
//! Run with: cargo run --example vector_search

use edgestore::{Dtype, EdgestoreConfig, Engine, Metric, VectorEngine, VectorRecord};
use std::path::PathBuf;

/// Simple LCG for deterministic "random" vectors.
fn lcg_f32(seed: u64, n: usize) -> Vec<f32> {
    let mut s = seed;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        s = s.wrapping_mul(1103515245).wrapping_add(12345);
        out.push((s as f32) / (u64::MAX as f32));
    }
    out
}

fn f32s_to_bytes(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes().to_vec()).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = PathBuf::from("/tmp/edgestore_vector_search_example");
    let _ = std::fs::remove_dir_all(&db_path);

    println!("=== EdgeStore Vector Search Example ===\n");

    let config = EdgestoreConfig::new(&db_path);
    let mut engine = Engine::open(config)?;

    let dims: u16 = 32;
    let n_vectors = 1000usize;

    println!("Inserting {} {}-dim f32 vectors...", n_vectors, dims);
    for i in 0..n_vectors {
        let vals = lcg_f32(i as u64 * 7919, dims as usize);
        let bytes = f32s_to_bytes(&vals);
        let key = format!("vec_{:04}", i);
        engine.vector_put(b"embeddings", key.as_bytes(), dims, Dtype::F32, &bytes)?;
    }
    println!("Done.\n");

    // Build HNSW index for fast search
    println!("Building HNSW index...");
    engine.build_vector_index(b"embeddings")?;
    println!("Done.\n");

    // Query vector — search for something close to vec_0000's seed pattern
    let query_vals = lcg_f32(0 * 7919, dims as usize);
    let query_data = f32s_to_bytes(&query_vals);
    let query = VectorRecord {
        dims,
        dtype: Dtype::F32,
        data: query_data,
    };

    println!("Searching top-5 nearest neighbors...");
    let results = engine.vector_search(b"embeddings", &query, 5, Metric::L2)?;

    println!("\nTop-5 results:");
    println!("{:<12} {:>12}", "Key", "Distance");
    println!("{}", "-".repeat(26));
    for r in &results {
        println!(
            "{:<12} {:>12.6}",
            String::from_utf8_lossy(&r.key),
            r.distance
        );
    }

    println!("\nTotal vectors scanned: {} (HNSW index used)", n_vectors);

    drop(engine);
    let _ = std::fs::remove_dir_all(&db_path);
    Ok(())
}
