use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use edgestore::{
    distance_scalar, Dtype, EdgestoreConfig, Engine, Metric, VectorEngine, VectorRecord,
};
use tempfile::TempDir;

/// Deterministic pseudo-random f32 generator (LCG).
fn lcg_sequence(seed: u64, n: usize) -> Vec<f32> {
    let mut s = seed;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        s = s.wrapping_mul(1103515245).wrapping_add(12345);
        out.push((s as f32) / (u64::MAX as f32));
    }
    out
}

/// Encode a Vec<f32> into little-endian bytes.
fn f32s_to_bytes(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes().to_vec()).collect()
}

fn bench_vector_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_search");

    for &n in &[10_000, 100_000] {
        let dims = 128usize;

        // Setup engine with n vectors
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        println!("Setup: inserting {} vectors...", n);
        for i in 0..n {
            let vals = lcg_sequence(i as u64 * 12345, dims);
            let data = f32s_to_bytes(&vals);
            let key = format!("key{:08}", i);
            engine
                .vector_put(b"ns", key.as_bytes(), dims as u16, Dtype::F32, &data)
                .unwrap();

            // Periodic flush to keep memtable from growing too large
            if i > 0 && i % 1000 == 0 {
                let _ = engine.flush_to_segments();
            }
        }
        println!("Setup: done.");

        let query_vals = lcg_sequence(99999, dims);
        let query_data = f32s_to_bytes(&query_vals);
        let query = VectorRecord {
            dims: dims as u16,
            dtype: Dtype::F32,
            data: query_data,
        };

        // Benchmark cosine search
        group.bench_with_input(
            BenchmarkId::new("cosine", n),
            &n,
            |b, _| {
                b.iter(|| {
                    let results = engine
                        .vector_search(b"ns", black_box(&query), 10, Metric::Cosine)
                        .unwrap();
                    black_box(results);
                });
            },
        );

        // Benchmark L2 search
        group.bench_with_input(
            BenchmarkId::new("l2", n),
            &n,
            |b, _| {
                b.iter(|| {
                    let results = engine
                        .vector_search(b"ns", black_box(&query), 10, Metric::L2)
                        .unwrap();
                    black_box(results);
                });
            },
        );

        // Benchmark dot product search
        group.bench_with_input(
            BenchmarkId::new("dotproduct", n),
            &n,
            |b, _| {
                b.iter(|| {
                    let results = engine
                        .vector_search(b"ns", black_box(&query), 10, Metric::DotProduct)
                        .unwrap();
                    black_box(results);
                });
            },
        );
    }

    group.finish();
}

fn bench_distance_scalar(c: &mut Criterion) {
    let mut group = c.benchmark_group("distance_scalar");

    for &dims in &[128, 512, 1024] {
        let a = lcg_sequence(1, dims);
        let b = lcg_sequence(2, dims);

        for metric in [Metric::Cosine, Metric::L2, Metric::DotProduct] {
            group.bench_with_input(
                BenchmarkId::new(format!("{:?}", metric).to_lowercase(), dims),
                &dims,
                |ben, _| {
                    ben.iter(|| {
                        let d = distance_scalar(&a, &b, metric);
                        black_box(d);
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_vector_search, bench_distance_scalar);
criterion_main!(benches);
