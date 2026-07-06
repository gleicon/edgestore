#![cfg(feature = "tier")]

use edgestore::EdgestoreConfig;
use edgestore_repl::FilesystemRemoteStore;
use edgestore_tokio::AsyncTieredEngine;
use tempfile::TempDir;

async fn open(local_dir: &std::path::Path, remote_dir: &std::path::Path) -> AsyncTieredEngine {
    let remote = FilesystemRemoteStore::new(remote_dir.to_path_buf()).unwrap();
    AsyncTieredEngine::open(EdgestoreConfig::new(local_dir), Box::new(remote)).await.unwrap()
}

#[tokio::test]
async fn put_get_local_roundtrip() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();
    let engine = open(local_dir.path(), remote_dir.path()).await;

    engine.put(b"ns", b"key", b"val").await.unwrap();
    let got = engine.get(b"ns", b"key").await.unwrap();
    assert_eq!(got, Some(b"val".to_vec()));
}

#[tokio::test]
async fn index_and_search_text_pass_through_to_local_engine() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();
    let engine = open(local_dir.path(), remote_dir.path()).await;

    engine.index_text(b"ns", b"doc1", "hello tiered world", std::collections::HashMap::new()).await.unwrap();
    let results = engine.search_text(b"ns", "hello", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].doc_id, b"doc1");
}

#[tokio::test]
async fn get_reads_through_an_archived_segment_on_a_fresh_engine() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let meta = {
        let engine = open(local_dir.path(), remote_dir.path()).await;
        engine.put(b"logs", b"key1", b"val1").await.unwrap();
        let meta = engine.flush_to_segments().await.unwrap();
        engine.archive_segments(vec![meta.clone()]).await.unwrap();
        meta
    };

    // A brand-new engine on a different local directory, seeded only with the
    // archived-segment metadata (as if restored from a manifest after a restart) —
    // it has no local data of its own, so a hit here can only come from read-through.
    let fresh_dir = TempDir::new().unwrap();
    let fresh = open(fresh_dir.path(), remote_dir.path()).await;
    fresh
        .register_archived(vec![edgestore_tier::ArchivedSegment {
            hash: meta.segment_hash.as_slice().try_into().unwrap(),
            min_key: meta.min_key.clone(),
            max_key: meta.max_key.clone(),
        }])
        .await;

    let got = fresh.get(b"logs", b"key1").await.unwrap();
    assert_eq!(got, Some(b"val1".to_vec()), "read-through must fetch the archived segment on local miss");
}

#[tokio::test]
async fn fetch_archived_overlapping_rehydrates_only_segments_in_range() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    // Two separate flushes → two separate segments with distinct key ranges.
    let (meta_early, meta_late) = {
        let engine = open(local_dir.path(), remote_dir.path()).await;
        engine.put(b"logs", b"2020", b"old-data").await.unwrap();
        let meta_early = engine.flush_to_segments().await.unwrap();

        engine.put(b"logs", b"2030", b"new-data").await.unwrap();
        let meta_late = engine.flush_to_segments().await.unwrap();

        engine.archive_segments(vec![meta_early.clone(), meta_late.clone()]).await.unwrap();
        (meta_early, meta_late)
    };

    let fresh_dir = TempDir::new().unwrap();
    let fresh = open(fresh_dir.path(), remote_dir.path()).await;
    fresh
        .register_archived(vec![
            edgestore_tier::ArchivedSegment {
                hash: meta_early.segment_hash.as_slice().try_into().unwrap(),
                min_key: meta_early.min_key.clone(),
                max_key: meta_early.max_key.clone(),
            },
            edgestore_tier::ArchivedSegment {
                hash: meta_late.segment_hash.as_slice().try_into().unwrap(),
                min_key: meta_late.min_key.clone(),
                max_key: meta_late.max_key.clone(),
            },
        ])
        .await;

    // Fetch only the range overlapping the *early* key — the late segment must
    // NOT be imported (pulled into local storage).
    fresh.fetch_archived_overlapping(b"logs", b"2020", b"2021").await.unwrap();

    let early = fresh.get(b"logs", b"2020").await.unwrap();
    assert_eq!(early, Some(b"old-data".to_vec()), "the overlapping segment must be fetched");

    // Only the overlapping (early) segment should have been imported locally — one
    // .dat file, not two. `range()` itself now reads through to archived segments
    // (1.1.4) regardless of what's been imported, so a scan finding the late key is
    // expected and correct; what this test actually guards is selective *import*,
    // not read-through reach.
    let dat_files = std::fs::read_dir(fresh_dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "dat"))
        .count();
    assert_eq!(dat_files, 1, "fetch_archived_overlapping must import only the overlapping segment, not the whole archive");
}
