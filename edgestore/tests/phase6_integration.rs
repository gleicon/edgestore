use edgestore::{
    EdgestoreConfig, Engine, VectorEngine, VectorRecord,
    vector::distance::Metric,
    vector::types::{Dtype, encode_vector_record},
};
use tempfile::TempDir;

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
}

fn encode_f32_vec(v: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(v.len() * 4);
    for &f in v {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    bytes
}

fn make_rec(dims: u16, data: Vec<u8>) -> VectorRecord {
    VectorRecord { dims, dtype: Dtype::F32, data }
}

#[test]
fn test_hnsw_build_and_search() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    let dims = 8u16;
    let n = 100usize;

    // Insert vectors
    let mut seed = 12345u64;
    for i in 0..n {
        let v: Vec<f32> = (0..dims).map(|_| {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            ((seed % 100) as f32) / 100.0
        }).collect();
        let bytes = encode_f32_vec(&v);
        engine.vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes).unwrap();
    }

    // Build index
    engine.build_vector_index(b"ns").unwrap();

    // Verify index is cached
    let loaded = engine.preload_vector_index(b"ns").unwrap();
    assert!(loaded, "Index should be cached after build");

    // Search using HNSW
    let query_v: Vec<f32> = (0..dims).map(|_| {
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        ((seed % 100) as f32) / 100.0
    }).collect();
    let query = make_rec(dims, encode_f32_vec(&query_v));
    let results = engine.vector_search(b"ns", &query, 5, Metric::L2).unwrap();
    assert!(!results.is_empty(), "HNSW search should return results");
    assert!(results.len() <= 5);
}

#[test]
fn test_hnsw_survives_restart() {
    let dir = TempDir::new().unwrap();
    {
        let mut engine = open_engine(&dir);
        let dims = 4u16;
        for i in 0..20 {
            let v = vec![i as f32 * 0.1, i as f32 * 0.2, i as f32 * 0.3, i as f32 * 0.4];
            let bytes = encode_f32_vec(&v);
            engine.vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes).unwrap();
        }
        engine.build_vector_index(b"ns").unwrap();
    }

    // Reopen and search
    let mut engine = open_engine(&dir);
    let query = make_rec(4, encode_f32_vec(&[0.5, 1.0, 1.5, 2.0]));
    let results = engine.vector_search(b"ns", &query, 3, Metric::L2).unwrap();
    assert!(!results.is_empty(), "Search after restart should return results");
}

#[test]
fn test_hnsw_falls_back_to_flat_scan() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    let dims = 4u16;
    for i in 0..10 {
        let v = vec![i as f32 * 0.1, i as f32 * 0.2, i as f32 * 0.3, i as f32 * 0.4];
        let bytes = encode_f32_vec(&v);
        engine.vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes).unwrap();
    }

    // No index built — should fall back to flat scan
    let query = make_rec(4, encode_f32_vec(&[0.5, 1.0, 1.5, 2.0]));
    let results = engine.vector_search(b"ns", &query, 3, Metric::L2).unwrap();
    assert!(!results.is_empty(), "Flat scan should return results");
}

#[test]
fn test_hnsw_metrics_tracked() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    let dims = 4u16;
    for i in 0..20 {
        let v = vec![i as f32 * 0.1; 4];
        let bytes = encode_f32_vec(&v);
        engine.vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes).unwrap();
    }

    engine.build_vector_index(b"ns").unwrap();

    // Drop and reopen engine to force index reload from disk
    drop(engine);
    let mut engine = open_engine(&dir);

    // Search to trigger index load from sidecar
    let query = make_rec(4, encode_f32_vec(&[0.5; 4]));
    let _results = engine.vector_search(b"ns", &query, 3, Metric::L2).unwrap();

    let metrics = engine.metrics();
    assert!(metrics.vector_index_loads >= 1, "Index loads should be tracked after reload");
}

#[test]
fn test_storage_backend_trait() {
    use edgestore::{DefaultStorageBackend, MemoryStorageBackend, StorageBackend};

    // Default backend
    let backend = DefaultStorageBackend::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.bin");
    backend.write(&path, 0, b"hello").unwrap();
    let mut buf = [0u8; 5];
    let n = backend.read(&path, 0, &mut buf).unwrap();
    assert_eq!(n, 5);
    assert_eq!(&buf, b"hello");

    // Memory backend
    let mem = MemoryStorageBackend::new();
    let path2 = std::path::Path::new("/tmp/test.bin");
    mem.write(path2, 0, b"world").unwrap();
    let mut buf2 = [0u8; 5];
    mem.read(path2, 0, &mut buf2).unwrap();
    assert_eq!(&buf2, b"world");
}

#[test]
fn test_fdp_disabled_by_default() {
    let cfg = EdgestoreConfig::new("/tmp/test_db");
    assert_eq!(cfg.fdp_enabled, false);
}

#[test]
fn test_fdp_mock_records_hint() {
    use edgestore::{MockFdpBackend, PlacementHint, MemoryStorageBackend, StorageBackend};

    let inner = Box::new(MemoryStorageBackend::new());
    let backend = MockFdpBackend::new(inner);
    let path = std::path::Path::new("/tmp/test.bin");

    let hint = PlacementHint { cohort_bucket: 42 };
    backend.write_with_hint(path, 0, b"data", hint).unwrap();

    let hints = backend.recorded_hints.lock().unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].2.cohort_bucket, 42);
}

#[test]
fn test_hnsw_preload_vector_index() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    let dims = 4u16;
    for i in 0..20 {
        let v = vec![i as f32 * 0.1; 4];
        let bytes = encode_f32_vec(&v);
        engine.vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes).unwrap();
    }

    engine.build_vector_index(b"ns").unwrap();

    // Preload should return true
    let loaded = engine.preload_vector_index(b"ns").unwrap();
    assert!(loaded, "preload should return true when index exists");

    // Preload on non-existent namespace should return false
    let loaded2 = engine.preload_vector_index(b"other").unwrap();
    assert!(!loaded2, "preload should return false when no index exists");
}
