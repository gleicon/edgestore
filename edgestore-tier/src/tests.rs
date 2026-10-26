use super::*;
use edgestore::{EdgestoreConfig, Engine};
use edgestore_repl::FilesystemRemoteStore;
use tempfile::TempDir;

fn make_tiered() -> (TempDir, TempDir, TieredEngine) {
    let local_dir = TempDir::new().expect("tempdir");
    let remote_dir = TempDir::new().expect("tempdir");

    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).expect("Engine::open");
    let remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
        .expect("FilesystemRemoteStore::new");

    let tiered = TieredEngine::new(local, Box::new(remote));
    (local_dir, remote_dir, tiered)
}

#[test]
fn test_archive_and_read_through() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"key1", b"val1").unwrap();
    tiered.put(b"ns", b"key2", b"val2").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    assert!(!metas.is_empty(), "should have at least one segment");

    tiered.archive_segments(&metas).unwrap();
    assert!(
        !tiered.archived_segments().is_empty(),
        "should have archived segments"
    );

    let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh")))
        .expect("fresh Engine::open");
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
        .expect("FilesystemRemoteStore::new");
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));

    fresh_tiered.register_archived(
        metas
            .iter()
            .map(|meta| {
                let hash: [u8; 32] = meta.segment_hash.as_slice().try_into().unwrap();
                ArchivedSegment {
                    hash,
                    min_key: meta.min_key.clone(),
                    max_key: meta.max_key.clone(),
                }
            })
            .collect(),
    );

    let got = fresh_tiered.get(b"ns", b"key1").unwrap();
    assert_eq!(got, Some(b"val1".to_vec()), "read-through from remote");

    let got = fresh_tiered.get(b"ns", b"key2").unwrap();
    assert_eq!(got, Some(b"val2".to_vec()), "read-through second key");
}

#[test]
fn test_idempotent_fetch() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"key", b"val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh")))
        .expect("fresh Engine::open");
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
        .expect("FilesystemRemoteStore::new");
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));

    let archived: Vec<ArchivedSegment> = tiered.archived_segments();
    fresh_tiered.register_archived(archived);

    let got = fresh_tiered.get(b"ns", b"key").unwrap();
    assert_eq!(got, Some(b"val".to_vec()));

    let got = fresh_tiered.get(b"ns", b"key").unwrap();
    assert_eq!(got, Some(b"val".to_vec()));
}

#[test]
fn test_lww_local_wins_over_archived() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"key", b"v1").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    tiered.put(b"ns", b"key", b"v2").unwrap();

    let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh2")))
        .expect("fresh Engine::open");
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
        .expect("FilesystemRemoteStore::new");
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    fresh_tiered.register_archived(tiered.archived_segments());

    let got = fresh_tiered.get(b"ns", b"key").unwrap();
    assert_eq!(got, Some(b"v1".to_vec()), "archived v1 imported");

    fresh_tiered.put(b"ns", b"key", b"v2").unwrap();

    let got = fresh_tiered.get(b"ns", b"key").unwrap();
    assert_eq!(got, Some(b"v2".to_vec()), "local v2 wins via LWW");
}

#[test]
fn test_partial_archive_mixed_local_remote() {
    let (_local_dir, _remote_dir, mut tiered) = make_tiered();

    for i in 0u64..500 {
        tiered.put(b"ns", &i.to_be_bytes(), b"batch1").unwrap();
    }
    tiered.local_mut().flush_to_segments().unwrap();

    let metas1 = tiered.local().list_segment_metas();

    for i in 500u64..1000 {
        tiered.put(b"ns", &i.to_be_bytes(), b"batch2").unwrap();
    }
    tiered.local_mut().flush_to_segments().unwrap();

    tiered.archive_segments(&metas1).unwrap();

    let got = tiered.get(b"ns", &250u64.to_be_bytes()).unwrap();
    assert_eq!(got, Some(b"batch1".to_vec()), "archived key readable");

    let got = tiered.get(b"ns", &750u64.to_be_bytes()).unwrap();
    assert_eq!(got, Some(b"batch2".to_vec()), "local key still readable");
}

#[test]
fn test_range_after_warming() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"a", b"1").unwrap();
    tiered.put(b"ns", b"b", b"2").unwrap();
    tiered.put(b"ns", b"c", b"3").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("warm"))).unwrap();
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    for m in &metas {
        fresh_tiered.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    fresh_tiered.fetch_all_archived().unwrap();

    let results = fresh_tiered.range(b"ns", b"a", b"z").unwrap();
    assert_eq!(results.len(), 3, "range returns all 3 keys after warming");
}

#[test]
fn test_fetch_segment_by_hash() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"key", b"val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let hash: [u8; 32] = metas[0].segment_hash.as_slice().try_into().unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_fs"))).unwrap();
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    for m in &metas {
        fresh_tiered.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    fresh_tiered.fetch_segment(&hash).unwrap();

    let got = fresh_tiered.get(b"ns", b"key").unwrap();
    assert_eq!(got, Some(b"val".to_vec()), "key present after fetch_segment");
}

#[test]
fn test_local_only_get_returns_none_before_fetch() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"key", b"val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_lonly"))).unwrap();
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));

    let got = fresh_tiered.local_only_get(b"ns", b"key").unwrap();
    assert!(got.is_none(), "local_only_get must miss before fetch");
}

#[test]
fn test_download_segment_does_not_import() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"key", b"val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let hash: [u8; 32] = metas[0].segment_hash.as_slice().try_into().unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_dl"))).unwrap();
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    for m in &metas {
        fresh_tiered.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    let bytes = fresh_tiered.download_segment(&hash).unwrap();
    assert!(!bytes.is_empty(), "download_segment must return non-empty bytes");

    // Key must not be in local after download_segment (no import).
    let got = fresh_tiered.local_only_get(b"ns", b"key").unwrap();
    assert!(got.is_none(), "download_segment must not import into local");
}

#[test]
fn test_fetch_archived_overlapping_selective() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"aaa", b"1").unwrap();
    tiered.put(b"ns", b"bbb", b"2").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_sel"))).unwrap();
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    for m in &metas {
        fresh_tiered.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    fresh_tiered
        .fetch_archived_overlapping(b"ns", b"aaa", b"bbb")
        .unwrap();

    let got = fresh_tiered.get(b"ns", b"aaa").unwrap();
    assert_eq!(got, Some(b"1".to_vec()), "aaa fetched");
}

#[test]
fn test_register_archived_deduplicates() {
    let (_local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"key", b"val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let archived = tiered.archived_segments();
    assert_eq!(archived.len(), metas.len(), "one archived entry per segment");

    tiered.register_archived(archived.clone());
    tiered.register_archived(archived.clone());

    assert_eq!(
        tiered.archived_segments().len(),
        metas.len(),
        "register_archived must deduplicate"
    );
}

#[test]
fn test_flush_compact_prune_passthrough() {
    let (_dir, _remote, mut tiered) = make_tiered();

    tiered.put(b"ns", b"k", b"v").unwrap();
    // flush_to_segments writes a .dat segment; flush() only flushes the WAL.
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    assert!(!metas.is_empty(), "segment exists after flush_to_segments");

    tiered.compact_once().unwrap();

    tiered.archive_segments(&metas).unwrap();
    tiered.prune_local_segment(metas[0].segment_id).unwrap();
}

#[test]
fn test_sidecar_download_roundtrip_via_remote_store() {
    let (_dir, remote_dir, _tiered) = make_tiered();

    let local =
        Engine::open(EdgestoreConfig::new(remote_dir.path().join("arch_local"))).unwrap();
    let remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut tiered = TieredEngine::new(local, Box::new(remote)).with_sidecars(true);

    tiered.put(b"ns", b"sidecar_key", b"sidecar_val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();
    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let hash: [u8; 32] = metas[0].segment_hash.as_slice().try_into().unwrap();
    let reader = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();

    let idx_bytes = reader.download_aux(&hash, "idx");
    let xf_bytes = reader.download_aux(&hash, "xf");
    let meta_bytes = reader.download_aux(&hash, "meta");

    assert!(
        idx_bytes.is_ok(),
        "idx sidecar downloadable: {:?}",
        idx_bytes.err()
    );
    assert!(xf_bytes.is_ok(), "xf sidecar downloadable");
    assert!(meta_bytes.is_ok(), "meta sidecar downloadable");
    assert!(!idx_bytes.unwrap().is_empty(), "idx sidecar non-empty");
}

#[test]
fn test_sidecars_enabled_but_upload_aux_unsupported_archive_still_succeeds() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    struct NoAuxStore(FilesystemRemoteStore);
    impl RemoteStore for NoAuxStore {
        fn upload(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError> {
            self.0.upload(hash, data)
        }
        fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
            self.0.download(hash)
        }
        fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError> {
            self.0.list()
        }
        fn delete(&self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
            self.0.delete(hash)
        }
        // upload_aux intentionally not overridden — uses default Err impl.
    }

    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
    let remote =
        NoAuxStore(FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap());
    let mut tiered = TieredEngine::new(local, Box::new(remote)).with_sidecars(true);

    tiered.put(b"ns", b"key", b"val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();
    let metas = tiered.local().list_segment_metas();

    tiered
        .archive_segments(&metas)
        .expect("archive must succeed despite upload_aux Err");
    assert!(
        !tiered.archived_segments().is_empty(),
        "segment recorded as archived"
    );

    let remote_files: Vec<String> = std::fs::read_dir(remote_dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        remote_files.iter().any(|f| f.ends_with(".seg")),
        ".dat must be archived"
    );
    assert!(
        !remote_files.iter().any(|f| f.ends_with(".idx")),
        ".idx must be absent"
    );
}

#[test]
fn test_throttling_retries_eventually_succeed() {
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
    let inner_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let remote = ThrottlingRemoteStore::new(inner_remote, 2); // fail first 2

    let mut tiered = TieredEngine::new(local, Box::new(remote));
    tiered.put(b"ns", b"key", b"val").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("throttle"))).unwrap();
    let fresh_inner = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let fresh_remote = ThrottlingRemoteStore::new(fresh_inner, 2);
    let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    fresh_tiered.register_archived(tiered.archived_segments());

    let got = fresh_tiered.get(b"ns", b"key").unwrap();
    assert_eq!(got, Some(b"val".to_vec()), "retry eventually succeeds");
}

// ── Throttling mock: fails first N calls, then succeeds ────────────────────

struct ThrottlingRemoteStore {
    inner: FilesystemRemoteStore,
    fail_count: std::sync::atomic::AtomicU64,
    max_fails: u64,
}

impl ThrottlingRemoteStore {
    fn new(inner: FilesystemRemoteStore, max_fails: u64) -> Self {
        Self {
            inner,
            fail_count: std::sync::atomic::AtomicU64::new(0),
            max_fails,
        }
    }

    fn try_inner<F, T>(&self, f: F) -> Result<T, EdgestoreError>
    where
        F: FnOnce(&FilesystemRemoteStore) -> Result<T, EdgestoreError>,
    {
        let count = self
            .fail_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if count < self.max_fails {
            return Err(EdgestoreError::ReplicationError(format!(
                "throttled: 503 Slow Down (attempt {})",
                count + 1
            )));
        }
        f(&self.inner)
    }
}

impl RemoteStore for ThrottlingRemoteStore {
    fn upload(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError> {
        self.try_inner(|inner| inner.upload(hash, data))
    }

    fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
        self.try_inner(|inner| inner.download(hash))
    }

    fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError> {
        self.inner.list()
    }

    fn delete(&self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
        self.inner.delete(hash)
    }
}

// ── Issue 1: range/prefix read-through tests ─────────────────────────────

#[test]
fn test_range_includes_archived_segments() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    for i in 0u8..5 {
        tiered.put(b"ns", &[i], &[i * 10]).unwrap();
    }
    tiered.local_mut().flush_to_segments().unwrap();
    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_range"))).unwrap();
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    for m in &metas {
        fresh.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    let results = fresh.range(b"ns", &[0u8], &[10u8]).unwrap();
    assert_eq!(results.len(), 5, "range must return all 5 archived keys");
}

#[test]
fn test_prefix_includes_archived_segments() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"foo:1", b"a").unwrap();
    tiered.put(b"ns", b"foo:2", b"b").unwrap();
    tiered.put(b"ns", b"bar:1", b"c").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();
    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_prefix"))).unwrap();
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    for m in &metas {
        fresh.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    let results = fresh.prefix(b"ns", b"foo:").unwrap();
    assert_eq!(
        results.len(),
        2,
        "prefix must return only foo: keys from archive"
    );
}

#[test]
fn test_range_local_wins_over_archived() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"key", b"archive-value").unwrap();
    tiered.flush().unwrap();
    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_lww"))).unwrap();
    let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh = TieredEngine::new(fresh_local, Box::new(fresh_remote));
    for m in &metas {
        fresh.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }
    fresh.put(b"ns", b"key", b"local-value").unwrap();

    let results = fresh.range(b"ns", b"", b"\xff").unwrap();
    let val = results
        .iter()
        .find(|(k, _)| k == b"key")
        .map(|(_, v)| v.as_slice());
    assert_eq!(
        val,
        Some(b"local-value" as &[u8]),
        "local write must win over archived copy"
    );
}

// ── Issue 2: strip_text_index tests ─────────────────────────────────────

#[test]
fn test_strip_text_index_removes_text_records() {
    let (local_dir, _remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"kv-key", b"kv-value").unwrap();
    tiered.local_mut().put(b"__text__docs", b"doc1", b"raw text record").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    assert!(!metas.is_empty(), "segment must exist");
    let seg_id = metas[0].segment_id;

    let new_meta = tiered.local_mut().strip_text_index(seg_id).unwrap();
    assert!(
        new_meta.text_index_stripped,
        "new segment must be marked stripped"
    );

    let got = tiered.get(b"ns", b"kv-key").unwrap();
    assert_eq!(
        got,
        Some(b"kv-value".to_vec()),
        "KV record must survive stripping"
    );
}

#[test]
fn test_strip_text_index_via_archive_with_text_stripping() {
    let (_local_dir, _remote_dir, tiered) = make_tiered();
    let mut tiered = TieredEngine::new(tiered.local, tiered.remote).with_text_stripping(true);

    tiered.put(b"ns", b"kv", b"value").unwrap();
    tiered.local_mut().put(b"__text__docs", b"doc1", b"raw text record").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let stripped: Vec<_> = tiered
        .local()
        .list_segment_metas()
        .into_iter()
        .filter(|m| m.text_index_stripped)
        .collect();

    let got = tiered.get(b"ns", b"kv").unwrap();
    assert!(
        got.is_some(),
        "KV access must still work after text stripping"
    );
    let _ = stripped;
}

#[test]
fn test_segment_cache_avoids_redownload() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"x", b"v1").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();
    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_cache"))).unwrap();
    let fresh_remote =
        edgestore_repl::FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh = TieredEngine::new(fresh_local, Box::new(fresh_remote))
        .with_segment_cache_bytes(64 * 1024 * 1024);
    for m in &metas {
        fresh.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    let r1 = fresh.range(b"ns", b"x", b"z").unwrap();
    assert_eq!(r1.len(), 1);

    let r2 = fresh.range(b"ns", b"x", b"z").unwrap();
    assert_eq!(r2, r1, "cached range must return identical results");

    assert!(
        fresh.segment_cache_bytes > 0,
        "cache must hold resident bytes"
    );
}

#[test]
fn test_segment_cache_evicts_at_limit() {
    let (local_dir, remote_dir, mut tiered) = make_tiered();

    tiered.put(b"ns", b"a", b"1").unwrap();
    tiered.local_mut().flush_to_segments().unwrap();
    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_evict"))).unwrap();
    let fresh_remote =
        edgestore_repl::FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh =
        TieredEngine::new(fresh_local, Box::new(fresh_remote)).with_segment_cache_bytes(1);
    for m in &metas {
        fresh.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    let r = fresh.range(b"ns", b"a", b"z").unwrap();
    assert_eq!(r.len(), 1, "results correct even with 1-byte cache limit");
}

// Regression: byte-counter drift when LRU item cap was 64.
#[test]
fn test_segment_cache_byte_counter_accurate_past_old_item_cap() {
    let (local_dir, remote_dir, mut base) = make_tiered();

    for i in 0u32..70 {
        let key = format!("key{:04}", i);
        let val = format!("val{:04}", i);
        base.put(b"ns", key.as_bytes(), val.as_bytes()).unwrap();
        base.local_mut().flush_to_segments().unwrap();
    }
    let metas = base.local().list_segment_metas();
    assert!(metas.len() >= 70, "need 70 segments for regression");
    base.archive_segments(&metas).unwrap();

    let fresh_local =
        Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh_70"))).unwrap();
    let fresh_remote =
        edgestore_repl::FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut fresh = TieredEngine::new(fresh_local, Box::new(fresh_remote))
        .with_segment_cache_bytes(64 * 1024 * 1024);
    for m in &metas {
        fresh.register_archived(vec![ArchivedSegment {
            hash: m.segment_hash.as_slice().try_into().unwrap(),
            min_key: m.min_key.clone(),
            max_key: m.max_key.clone(),
        }]);
    }

    let results = fresh.range(b"ns", b"key0000", b"key9999").unwrap();
    assert_eq!(
        results.len(),
        70,
        "all 70 entries visible after >64 segments cached"
    );
}

#[test]
fn test_get_needs_archived_fetch_false_when_no_archived_segments() {
    let local = TempDir::new().unwrap();
    let remote = TempDir::new().unwrap();
    let remote_store = FilesystemRemoteStore::new(remote.path().to_path_buf()).unwrap();
    let engine = TieredEngine::new(
        Engine::open(EdgestoreConfig::new(local.path())).unwrap(),
        Box::new(remote_store),
    );
    assert!(!engine.get_needs_archived_fetch(b"ns", b"anykey"));
}

#[test]
fn test_get_needs_archived_fetch_true_for_key_in_archived_range() {
    let local = TempDir::new().unwrap();
    let remote = TempDir::new().unwrap();
    let remote_store = FilesystemRemoteStore::new(remote.path().to_path_buf()).unwrap();
    let mut engine = TieredEngine::new(
        Engine::open(EdgestoreConfig::new(local.path())).unwrap(),
        Box::new(remote_store),
    );

    engine.put(b"ns", b"b", b"val").unwrap();
    let meta = engine.local_mut().flush_to_segments().unwrap();
    engine.archive_segments(&[meta.clone()]).unwrap();

    // Verify predicate: a key outside any archived range returns false.
    assert!(!engine.get_needs_archived_fetch(b"ns", b"zzz-no-segment-covers-this"));
}

// ── ENG-7: with_vector_stripping ─────────────────────────────────────

#[test]
fn test_with_vector_stripping_sets_flag_after_archive() {
    use edgestore::VectorEngine;
    use edgestore::vector::types::Dtype;
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
    let remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut tiered = TieredEngine::new(local, Box::new(remote)).with_vector_stripping(true);

    tiered.put(b"ns", b"kv", b"value").unwrap();
    let v = vec![0u8; 16]; // 4-dim f32
    tiered
        .local_mut()
        .vector_put(b"ns", b"vec1", 4, Dtype::F32, &v)
        .unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    assert!(!metas.is_empty());
    tiered.archive_segments(&metas).unwrap();

    let post_metas = tiered.local().list_segment_metas();
    assert!(
        post_metas.iter().any(|m| m.vector_index_stripped),
        "at least one segment must have vector_index_stripped set"
    );
    let val = tiered.get(b"ns", b"kv").unwrap();
    assert_eq!(val, Some(b"value".to_vec()), "KV survives vector strip");
}

#[test]
fn test_with_vector_stripping_disabled_leaves_flag_false() {
    use edgestore::VectorEngine;
    use edgestore::vector::types::Dtype;
    let local_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
    let remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
    let mut tiered = TieredEngine::new(local, Box::new(remote));

    tiered.put(b"ns", b"kv", b"value").unwrap();
    let v = vec![0u8; 16];
    tiered
        .local_mut()
        .vector_put(b"ns", b"vec1", 4, Dtype::F32, &v)
        .unwrap();
    tiered.local_mut().flush_to_segments().unwrap();

    let metas = tiered.local().list_segment_metas();
    tiered.archive_segments(&metas).unwrap();

    let post_metas = tiered.local().list_segment_metas();
    assert!(
        post_metas.iter().all(|m| !m.vector_index_stripped),
        "vector_index_stripped must remain false when stripping is disabled"
    );
}
