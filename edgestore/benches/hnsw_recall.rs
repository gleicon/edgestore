use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use edgestore::{distance, Dtype, Engine, Metric, VectorEngine, VectorRecord};
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

fn bench_hnsw_recall(c: &mut Criterion) {
    let mut group = c.benchmark_group("hnsw_recall");

    for &n in &[500, 1000, 5000] {
        let dims = 32usize;
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(edgestore::EdgestoreConfig::new(dir.path())).unwrap();

        // Insert clustered data
        let num_clusters = 5usize;
        let per_cluster = n / num_clusters;
        let mut all_data: Vec<Vec<u8>> = Vec::with_capacity(n);
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
                all_data.push(bytes.clone());
                engine
                    .vector_put(
                        b"ns",
                        &[(cluster * per_cluster + i) as u8],
                        dims as u16,
                        Dtype::F32,
                        &bytes,
                    )
                    .unwrap();
            }
        }

        engine.build_vector_index(b"ns").unwrap();

        // Measure recall across multiple queries
        group.bench_with_input(BenchmarkId::new("recall_at_10", n), &n, |b, _| {
            b.iter(|| {
                let mut total_recall = 0.0f32;
                let num_queries = 10usize;

                for q in 0..num_queries {
                    let query_vals = lcg_sequence(100000 + q as u64, dims);
                    let query_data = f32s_to_bytes(&query_vals);
                    let query = VectorRecord {
                        dims: dims as u16,
                        dtype: Dtype::F32,
                        data: query_data.clone(),
                    };

                    // HNSW results
                    let hnsw_results = engine.vector_search(b"ns", &query, 10, Metric::L2).unwrap();
                    let hnsw_keys: std::collections::HashSet<Vec<u8>> =
                        hnsw_results.iter().map(|r| r.key.clone()).collect();

                    // Brute-force reference
                    let mut brute: Vec<(Vec<u8>, f32)> = Vec::with_capacity(n);
                    for (i, rec) in all_data.iter().enumerate() {
                        let d = distance(&query_data, rec, Dtype::F32, Metric::L2).unwrap();
                        brute.push((vec![i as u8], d));
                    }
                    brute.sort_by(|a, b| edgestore::total_cmp_f32(a.1, b.1));
                    let brute_keys: std::collections::HashSet<Vec<u8>> =
                        brute.iter().take(10).map(|(k, _)| k.clone()).collect();

                    let intersection: Vec<_> = hnsw_keys.intersection(&brute_keys).collect();
                    total_recall += intersection.len() as f32 / 10.0;
                }

                black_box(total_recall / num_queries as f32);
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_hnsw_recall);
criterion_main!(benches);
