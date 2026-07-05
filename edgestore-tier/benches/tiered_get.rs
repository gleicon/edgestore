use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use edgestore::{EdgestoreConfig, Engine};
use edgestore_repl::FilesystemRemoteStore;
use edgestore_tier::TieredEngine;
use tempfile::TempDir;

fn bench_local_get(c: &mut Criterion) {
    let dir = TempDir::new().unwrap();
    let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

    for i in 0u64..1000 {
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

    for i in 0u64..1000 {
        tiered.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
    }
    tiered.local_mut().flush_to_segments().unwrap();

    c.bench_function("tiered_get_local_hit", |b| {
        b.iter(|| {
            let _ = tiered.get(black_box(b"ns"), black_box(&500u64.to_be_bytes()));
        })
    });
}

fn bench_tiered_get_readthrough(c: &mut Criterion) {
    // Pre-seed: create an archived segment once.
    let seed_local = TempDir::new().unwrap();
    let seed_remote = TempDir::new().unwrap();
    let mut seed_engine = Engine::open(EdgestoreConfig::new(seed_local.path())).unwrap();
    for i in 0u64..1000 {
        seed_engine.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
    }
    seed_engine.flush_to_segments().unwrap();

    let seed_remote_store =
        FilesystemRemoteStore::new(seed_remote.path().to_path_buf()).unwrap();
    let mut seed_tiered = TieredEngine::new(seed_engine, Box::new(seed_remote_store));
    let metas = seed_tiered.local().list_segment_metas();
    seed_tiered.archive_segments(&metas).unwrap();
    let archived = seed_tiered.archived_segments();

    let counter = std::sync::atomic::AtomicUsize::new(0);

    c.bench_function("tiered_get_readthrough", |b| {
        b.iter_batched(
            || {
                // Setup: fresh local engine + fresh tiered wrapper with archived list.
                // Unique path per iteration to avoid WriterBusy from concurrent setups.
                let idx = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let fresh_local =
                    Engine::open(EdgestoreConfig::new(seed_local.path().join(format!("fresh_{idx}"))))
                        .unwrap();
                let fresh_remote =
                    FilesystemRemoteStore::new(seed_remote.path().to_path_buf()).unwrap();
                let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
                fresh_tiered.register_archived(archived.clone());
                fresh_tiered
            },
            |mut tiered| {
                // Routine: first get() triggers download + import + retry.
                let _ = tiered.get(black_box(b"ns"), black_box(&500u64.to_be_bytes()));
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_tiered_archive_segments(c: &mut Criterion) {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
    let remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut tiered = TieredEngine::new(local, Box::new(remote));

    for i in 0u64..1000 {
        tiered.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
    }
    tiered.local_mut().flush_to_segments().unwrap();

    let _metas = tiered.local().list_segment_metas();

    c.bench_function("tiered_archive_segments", |b| {
        b.iter_batched(
            || {
                // Fresh remote dir per iteration so archive doesn't collide.
                let fresh_remote =
                    FilesystemRemoteStore::new(remote_dir.path().join(format!(
                        "archive_{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                    )))
                    .unwrap();
                let mut fresh_local =
                    Engine::open(EdgestoreConfig::new(local_dir.path().join(format!(
                        "local_{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                    ))))
                    .unwrap();
                // Re-seed fresh local.
                for i in 0u64..1000 {
                    fresh_local.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
                }
                fresh_local.flush_to_segments().unwrap();
                let fresh_metas = fresh_local.list_segment_metas();
                let fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
                (fresh_tiered, fresh_metas)
            },
            |(mut tiered, metas)| {
                tiered.archive_segments(&metas).unwrap();
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_tiered_fetch_all_archived(c: &mut Criterion) {
    // Pre-seed: archive segments once.
    let seed_local = TempDir::new().unwrap();
    let seed_remote = TempDir::new().unwrap();
    let mut seed_engine = Engine::open(EdgestoreConfig::new(seed_local.path())).unwrap();
    for i in 0u64..1000 {
        seed_engine.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
    }
    seed_engine.flush_to_segments().unwrap();

    let seed_remote_store =
        FilesystemRemoteStore::new(seed_remote.path().to_path_buf()).unwrap();
    let mut seed_tiered = TieredEngine::new(seed_engine, Box::new(seed_remote_store));
    let metas = seed_tiered.local().list_segment_metas();
    seed_tiered.archive_segments(&metas).unwrap();
    let archived = seed_tiered.archived_segments();

    let counter2 = std::sync::atomic::AtomicUsize::new(0);

    c.bench_function("tiered_fetch_all_archived", |b| {
        b.iter_batched(
            || {
                let idx = counter2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let fresh_local =
                    Engine::open(EdgestoreConfig::new(seed_local.path().join(format!("fetch_fresh_{idx}"))))
                        .unwrap();
                let fresh_remote =
                    FilesystemRemoteStore::new(seed_remote.path().to_path_buf()).unwrap();
                let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
                fresh_tiered.register_archived(archived.clone());
                fresh_tiered
            },
            |mut tiered| {
                tiered.fetch_all_archived().unwrap();
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    benches,
    bench_local_get,
    bench_tiered_get_local_hit,
    bench_tiered_get_readthrough,
    bench_tiered_archive_segments,
    bench_tiered_fetch_all_archived
);
criterion_main!(benches);
