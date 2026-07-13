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
async fn flush_notify_resolves_after_an_explicit_flush() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();
    let engine = open(local_dir.path(), remote_dir.path()).await;
    let notify = engine.flush_notify();

    engine.put(b"ns", b"key", b"val").await.unwrap();
    engine.flush_to_segments().await.unwrap();

    // notify_one semantics: the notification is stored even though nothing was
    // awaiting it yet — this call must resolve immediately, not hang.
    tokio::time::timeout(std::time::Duration::from_secs(2), notify.notified())
        .await
        .expect("flush_notify must resolve promptly after an explicit flush");
}

#[tokio::test]
async fn range_returns_correct_results_whether_or_not_archived_overlap_exists() {
    // Proves the read-lock fast path (range_needs_archived_fetch == false ->
    // local_only_range) and the write-lock slow path (an archived segment
    // actually needs fetching) both return correct, complete results — not just
    // that one code path or the other "runs", but that they agree on the answer.
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let meta = {
        let engine = open(local_dir.path(), remote_dir.path()).await;
        engine.put(b"logs", b"archived-key", b"archived-val").await.unwrap();
        let meta = engine.flush_to_segments().await.unwrap();
        engine.archive_segments(vec![meta.clone()]).await.unwrap();
        meta
    };

    let fresh_dir = TempDir::new().unwrap();
    let fresh = open(fresh_dir.path(), remote_dir.path()).await;
    fresh
        .register_archived(vec![edgestore_tier::ArchivedSegment {
            hash: meta.segment_hash.as_slice().try_into().unwrap(),
            min_key: meta.min_key.clone(),
            max_key: meta.max_key.clone(),
        }])
        .await;
    // Local data outside the archived segment's key range entirely.
    fresh.put(b"logs", b"zzz-local-only", b"local-val").await.unwrap();

    // A range overlapping the archived segment must take the slow path and
    // still return the archived record merged with local.
    let overlapping = fresh.range(b"logs", b"archived-key", b"archived-kez").await.unwrap();
    assert_eq!(overlapping, vec![(b"archived-key".to_vec(), b"archived-val".to_vec())]);

    // A range that cannot possibly overlap the archived segment (entirely
    // above its max_key) must take the fast path and still return the correct
    // local-only result.
    let local_only = fresh.range(b"logs", b"zzz-local-only", b"zzz-local-onlz").await.unwrap();
    assert_eq!(local_only, vec![(b"zzz-local-only".to_vec(), b"local-val".to_vec())]);
}

#[tokio::test]
async fn prefix_returns_correct_results_whether_or_not_archived_overlap_exists() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let meta = {
        let engine = open(local_dir.path(), remote_dir.path()).await;
        engine.put(b"logs", b"pfx-a-1", b"a1").await.unwrap();
        let meta = engine.flush_to_segments().await.unwrap();
        engine.archive_segments(vec![meta.clone()]).await.unwrap();
        meta
    };

    let fresh_dir = TempDir::new().unwrap();
    let fresh = open(fresh_dir.path(), remote_dir.path()).await;
    fresh
        .register_archived(vec![edgestore_tier::ArchivedSegment {
            hash: meta.segment_hash.as_slice().try_into().unwrap(),
            min_key: meta.min_key.clone(),
            max_key: meta.max_key.clone(),
        }])
        .await;
    fresh.put(b"logs", b"pfx-b-1", b"b1").await.unwrap();

    // "pfx-a" overlaps the archived segment's key range -> slow path.
    let via_archived = fresh.prefix(b"logs", b"pfx-a").await.unwrap();
    assert_eq!(via_archived, vec![(b"pfx-a-1".to_vec(), b"a1".to_vec())]);

    // "pfx-b" cannot overlap the archived segment ("pfx-a-1"'s range) -> fast path.
    let local_only = fresh.prefix(b"logs", b"pfx-b").await.unwrap();
    assert_eq!(local_only, vec![(b"pfx-b-1".to_vec(), b"b1".to_vec())]);
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
