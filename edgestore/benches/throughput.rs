use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use edgestore::memtable::{BBTreeConcurrentMemTable, BBTreeMemTable, BTreeMemTable, MemTable};
use edgestore::types::{MemEntry, Operation};
use edgestore::{Dtype, EdgestoreConfig, Engine, Metric, VectorEngine, VectorRecord};
use tempfile::TempDir;

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");

    // Put throughput
    group.bench_function("put_1000", |b| {
        b.iter_batched(
            || {
                let dir = TempDir::new().unwrap();
                Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
            },
            |mut engine| {
                for i in 0..1000 {
                    let key = format!("key{:08}", i);
                    let val = format!("val{:08}", i);
                    engine.put(b"ns", key.as_bytes(), val.as_bytes()).unwrap();
                }
                black_box(engine);
            },
            criterion::BatchSize::PerIteration,
        );
    });

    // Get throughput (hot read)
    group.bench_function("get_1000_hot", |b| {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        for i in 0..1000 {
            let key = format!("key{:08}", i);
            let val = format!("val{:08}", i);
            engine.put(b"ns", key.as_bytes(), val.as_bytes()).unwrap();
        }

        b.iter(|| {
            for i in 0..1000 {
                let key = format!("key{:08}", i);
                let _ = engine.get(b"ns", key.as_bytes());
            }
        });
    });

    // Vector put throughput
    group.bench_function("vector_put_100", |b| {
        let dims = 128usize;
        let data = vec![0xABu8; dims * 4];
        b.iter_batched(
            || {
                let dir = TempDir::new().unwrap();
                Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
            },
            |mut engine| {
                for i in 0..100 {
                    engine
                        .vector_put(b"ns", &[i as u8], dims as u16, Dtype::F32, &data)
                        .unwrap();
                }
                black_box(engine);
            },
            criterion::BatchSize::PerIteration,
        );
    });

    // Vector search throughput (flat scan)
    group.bench_function("vector_search_1000_flat", |b| {
        let dims = 32usize;
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        for i in 0..1000 {
            let v: Vec<u8> = (0..dims * 4).map(|j| ((i * 4 + j) % 256) as u8).collect();
            engine
                .vector_put(b"ns", &[i as u8], dims as u16, Dtype::F32, &v)
                .unwrap();
        }

        let query = VectorRecord {
            dims: dims as u16,
            dtype: Dtype::F32,
            data: vec![0xCDu8; dims * 4],
        };

        b.iter(|| {
            let results = engine.vector_search(b"ns", &query, 10, Metric::L2).unwrap();
            black_box(results);
        });
    });

    // Vector search throughput (HNSW)
    group.bench_function("vector_search_1000_hnsw", |b| {
        let dims = 32usize;
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        for i in 0..1000 {
            let v: Vec<u8> = (0..dims * 4).map(|j| ((i * 4 + j) % 256) as u8).collect();
            engine
                .vector_put(b"ns", &[i as u8], dims as u16, Dtype::F32, &v)
                .unwrap();
        }
        engine.build_vector_index(b"ns").unwrap();

        let query = VectorRecord {
            dims: dims as u16,
            dtype: Dtype::F32,
            data: vec![0xCDu8; dims * 4],
        };

        b.iter(|| {
            let results = engine.vector_search(b"ns", &query, 10, Metric::L2).unwrap();
            black_box(results);
        });
    });

    group.finish();
}

/// Compare put and get throughput across the three MemTable implementations.
///
/// Measures isolated memtable performance — no WAL, no segment I/O.
fn bench_memtable_variants(c: &mut Criterion) {
    const N: usize = 10_000;

    let make_entry = |i: usize| MemEntry {
        key: format!("k{:08}", i).into_bytes(),
        value: Some(format!("value_{:016}", i).into_bytes()),
        op: Operation::Put,
        lsn: i as u64,
        timestamp: 0,
        ttl: 0,
    };

    type MemFactory = fn() -> Box<dyn MemTable>;
    let variants: &[(&str, MemFactory)] = &[
        ("btree", || Box::new(BTreeMemTable::new())),
        ("bbtree_unsync", || Box::new(BBTreeMemTable::new())),
        ("bbtree_concurrent", || Box::new(BBTreeConcurrentMemTable::new())),
    ];

    let mut put_group = c.benchmark_group("memtable_put_10k");
    for &(label, factory) in variants {
        put_group.bench_with_input(BenchmarkId::new("variant", label), &label, |b, _| {
            b.iter_batched(
                factory,
                |mut m| {
                    for i in 0..N {
                        let key = format!("k{:08}", i).into_bytes();
                        m.insert(key, make_entry(i));
                    }
                    black_box(m);
                },
                criterion::BatchSize::PerIteration,
            );
        });
    }
    put_group.finish();

    let mut get_group = c.benchmark_group("memtable_get_10k_hot");
    for &(label, factory) in variants {
        get_group.bench_with_input(BenchmarkId::new("variant", label), &label, |b, _| {
            let mut m = factory();
            for i in 0..N {
                let key = format!("k{:08}", i).into_bytes();
                m.insert(key, make_entry(i));
            }
            b.iter(|| {
                for i in 0..N {
                    let key = format!("k{:08}", i);
                    black_box(m.get(key.as_bytes()));
                }
            });
        });
    }
    get_group.finish();
}

criterion_group!(benches, bench_throughput, bench_memtable_variants);
criterion_main!(benches);
