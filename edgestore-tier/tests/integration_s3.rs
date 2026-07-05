//! TieredEngine + S3RemoteStore integration test (LocalStack).
//!
//! Run with:
//! ```text
//! docker compose up -d localstack
//! EDGESTORE_S3_ENDPOINT_URL=http://localhost:4566 cargo test --package edgestore-tier --test integration_s3
//! ```

use edgestore::{EdgestoreConfig, Engine};
use edgestore_repl::S3RemoteStore;
use edgestore_tier::TieredEngine;

fn make_s3_store() -> Option<S3RemoteStore> {
    let endpoint = std::env::var("EDGESTORE_S3_ENDPOINT_URL").ok()?;
    let bucket = std::env::var("EDGESTORE_S3_BUCKET")
        .unwrap_or_else(|_| "edgestore-test".to_string());
    S3RemoteStore::new(&bucket, Some("tiered_test/"), Some(&endpoint)).ok()
}

#[test]
fn test_tiered_archive_and_readthrough_s3() {
    let Some(remote) = make_s3_store() else {
        eprintln!("skip: EDGESTORE_S3_ENDPOINT_URL not set");
        return;
    };

    let local_dir = tempfile::tempdir().unwrap();
    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
    let mut tiered = TieredEngine::new(local, Box::new(remote));

    tiered.put(b"ns", b"key", b"val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    // Fresh engine: local data gone, only archived list remains.
    let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh")))
        .expect("fresh Engine::open");
    let fresh_remote = make_s3_store().unwrap();
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    fresh_tiered.register_archived(tiered.archived_segments());

    let got = fresh_tiered.get(b"ns", b"key").unwrap();
    assert_eq!(got, Some(b"val".to_vec()), "read-through from S3 works");
}

#[test]
fn test_tiered_fetch_all_archived_s3() {
    let Some(remote) = make_s3_store() else {
        eprintln!("skip: EDGESTORE_S3_ENDPOINT_URL not set");
        return;
    };

    let local_dir = tempfile::tempdir().unwrap();
    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
    let mut tiered = TieredEngine::new(local, Box::new(remote));

    for i in 0u64..100 {
        tiered.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
    }
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    // Fresh engine + warm everything.
    let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("warm")))
        .expect("fresh Engine::open");
    let fresh_remote = make_s3_store().unwrap();
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    fresh_tiered.register_archived(tiered.archived_segments());
    fresh_tiered.fetch_all_archived().unwrap();

    // After warming, all keys should be local hits.
    for i in 0u64..100 {
        let got = fresh_tiered.get(b"ns", &i.to_be_bytes()).unwrap();
        assert_eq!(got, Some(b"value".to_vec()), "key {i} after warming");
    }
}
