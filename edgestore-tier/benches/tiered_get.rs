use criterion::{black_box, criterion_group, criterion_main, Criterion};
use edgestore::{EdgestoreConfig, Engine};
use edgestore_repl::FilesystemRemoteStore;
use edgestore_tier::TieredEngine;
use tempfile::TempDir;

fn bench_local_get(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

    // Seed data
    for i in 0..1000 {
        engine.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
    }
    engine.flush_to_segments().unwrap();

    c.bench_function("local_get_hot", |b| {
        b.iter(|| {
            let _ = engine.get(black_box(b"ns"), black_box(&500u64.to_be_bytes()));
        })
    });
}

fn bench_tiered_get_local_hit(c: &mut Criterion) {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
    let remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut tiered = TieredEngine::new(local, Box::new(remote));

    // Seed data
    for i in 0..1000 {
        tiered.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
    }
    tiered.local_mut().flush_to_segments().unwrap();

    c.bench_function("tiered_get_local_hit", |b| {
        b.iter(|| {
            let _ = tiered.get(black_box(b"ns"), black_box(&500u64.to_be_bytes()));
        })
    });
}

criterion_group!(benches, bench_local_get, bench_tiered_get_local_hit);
criterion_main!(benches);
