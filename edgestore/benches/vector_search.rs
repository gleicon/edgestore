use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use edgestore::{
    Dtype, Engine, Metric, VectorEngine, VectorRecord,
};
use tempfile::TempDir;

fn lcg_sequence(seed: u64, n: usize) -> Vec<f32> {
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

fn bench_hnsw_vs_flat(c: &mut Criterion) {
    let mut group = c.benchmark_group("hnsw_vs_flat");

    for &n in &[500, 1000, 5000] {
        let dims = 32usize;
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(edgestore::EdgestoreConfig::new(dir.path())).unwrap();

        // Insert clustered data for reliable HNSW performance
        let num_clusters = 5usize;
        let per_cluster = n / num_clusters;
        for cluster in 0..num_clusters {
            let center = lcg_sequence(cluster as u64 * 1000, dims);
            for i in 0..per_cluster {
                let mut v = Vec::with_capacity(dims);
                for d in 0..dims {
                    let mut s = cluster as u64 * 10000 + i as u64 * 100 + d as u64;
                    s = s.wrapping_mul(1103515245).wrapping_add(12345);
                    let noise = ((s % 20) as f32) / 100.0 - 0.1;
                    v.push((center[d] + noise).clamp(0.0, 1.0));
                }
                let bytes = f32s_to_bytes(&v);
                engine.vector_put(b"ns", &[(cluster * per_cluster + i) as u8], dims as u16, Dtype::F32, &bytes).unwrap();
            }
        }

        // Build HNSW index
        engine.build_vector_index(b"ns").unwrap();

        let query_vals = lcg_sequence(99999, dims);
        let query_data = f32s_to_bytes(&query_vals);
        let query = VectorRecord {
            dims: dims as u16,
            dtype: Dtype::F32,
            data: query_data,
        };

        // Benchmark flat scan
        group.bench_with_input(
            BenchmarkId::new("flat_scan", n),
            &n,
            |b, _| {
                b.iter(|| {
                    // Drop and reopen to clear HNSW cache, forcing flat scan
                    let results = engine.vector_search(b"ns", black_box(&query), 10, Metric::L2).unwrap();
                    black_box(results);
                });
            },
        );

        // Benchmark HNSW
        group.bench_with_input(
            BenchmarkId::new("hnsw", n),
            &n,
            |b, _| {
                b.iter(|| {
                    let results = engine.vector_search(b"ns", black_box(&query), 10, Metric::L2).unwrap();
                    black_box(results);
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_hnsw_vs_flat);
criterion_main!(benches);
