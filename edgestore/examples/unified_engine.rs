//! One Engine, many namespaces — the recommended pattern.
//!
//! EdgeStore provides KV and vector storage in a single `Engine` instance.
//! Full-text search is available via the `edgestore-text` crate.
//!
//! **Anti-pattern to avoid**: opening multiple `Engine` instances at different paths
//! (e.g. `products.db` + `products.vec`) gives you two lock files, two flush timers
//! that can diverge, and two replication targets to wire separately. Use namespaces
//! instead.
//!
//! ## Running
//!
//! ```sh
//! cargo run --example unified_engine
//! ```

use edgestore::vector::distance::Metric;
use edgestore::vector::types::Dtype;
use edgestore::{EdgestoreConfig, Engine, VectorEngine};
use tempfile::TempDir;

fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn main() {
    let dir = TempDir::new().unwrap();
    let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

    // ── KV: product catalog ────────────────────────────────────────────────
    engine.put(b"products", b"p1", b"{\"name\":\"Widget A\",\"price\":9.99}").unwrap();
    engine.put(b"products", b"p2", b"{\"name\":\"Widget B\",\"price\":19.99}").unwrap();
    engine.put(b"products", b"p3", b"{\"name\":\"Gadget X\",\"price\":49.99}").unwrap();

    let catalog = engine.range(b"products", b"", b"\xff").unwrap();
    println!("Catalog ({} products):", catalog.len());
    for (k, v) in &catalog {
        println!("  {} → {}", String::from_utf8_lossy(k), String::from_utf8_lossy(v));
    }

    // ── Vector: product embeddings ─────────────────────────────────────────
    let embed_p1 = [1.0f32, 0.0, 0.0, 0.0];
    let embed_p2 = [0.9f32, 0.1, 0.0, 0.0];
    let embed_p3 = [0.0f32, 0.0, 1.0, 0.0];

    engine.vector_put(b"products", b"p1", 4, Dtype::F32, &to_bytes(&embed_p1)).unwrap();
    engine.vector_put(b"products", b"p2", 4, Dtype::F32, &to_bytes(&embed_p2)).unwrap();
    engine.vector_put(b"products", b"p3", 4, Dtype::F32, &to_bytes(&embed_p3)).unwrap();

    let query = edgestore::vector::types::VectorRecord {
        dims: 4,
        dtype: Dtype::F32,
        data: to_bytes(&embed_p1),
    };
    let hits = engine.vector_search(b"products", &query, 2, Metric::Cosine).unwrap();
    println!("\nVector search (top 2 similar to Widget A):");
    for hit in &hits {
        println!("  key={} distance={:.3}", String::from_utf8_lossy(&hit.key), hit.distance);
    }

    engine.flush_to_segments().unwrap();
    let segments = engine.list_segment_metas();
    println!("\nSegments after flush: {}", segments.len());
    println!(
        "One Engine at '{}' — one lock, one flush, one replication target.",
        dir.path().display()
    );
}
