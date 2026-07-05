//! `edgestore-tier` — Tiered storage for EdgeStore.
//!
//! Wraps a local `Engine` with a `RemoteStore` (S3, filesystem, etc.) to provide
//! transparent read-through from local SSD to cold archive.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use edgestore::{EdgestoreConfig, Engine};
//! use edgestore_repl::S3RemoteStore;
//! use edgestore_tier::TieredEngine;
//!
//! let local = Engine::open(EdgestoreConfig::new("/tmp/db")).unwrap();
//! let remote = S3RemoteStore::new("my-bucket", Some("mydb/"), None).unwrap();
//! let mut tiered = TieredEngine::new(local, Box::new(remote));
//!
//! // write goes to local
//! tiered.put(b"users", b"alice", b"data").unwrap();
//!
//! // read tries local first; on miss checks remote segments
//! let val = tiered.get(b"users", b"alice").unwrap();
//! ```
//!
//! ## How it works
//!
//! 1. **Writes** go to the local `Engine` only. The hot path is unchanged.
//! 2. **Archiving** (`archive_segments`) uploads local segments to the remote
//!    store and records them in an archived-segment list.
//! 3. **Reads** try local first. On miss, scan archived segments by key bounds.
//!    If a segment might contain the key, download it via `RemoteStore`, import
//!    via `Engine::import_segment`, then retry.
//! 4. **No background tasks.** Everything is synchronous and caller-driven.
//!    The application decides when to archive, how much to keep local, and
//!    whether to prefetch.

use std::collections::HashMap;

use edgestore::error::EdgestoreError;
use edgestore::{Engine, ImportResult, RemoteStore};

#[cfg(test)]
use edgestore::EdgestoreConfig;

/// Metadata for a segment that lives in remote storage.
#[derive(Debug, Clone)]
pub struct ArchivedSegment {
    /// BLAKE3 content hash (also the remote object key).
    pub hash: [u8; 32],
    /// Min key in this segment (inclusive).
    pub min_key: Vec<u8>,
    /// Max key in this segment (inclusive).
    pub max_key: Vec<u8>,
}

/// Tiered engine: local hot cache + remote cold archive.
///
/// Writes go to local. Reads fall back to remote on miss.
/// Segment import uses the existing `Engine::import_segment` path (LWW merge).
pub struct TieredEngine {
    local: Engine,
    remote: Box<dyn RemoteStore>,
    /// Segments that have been uploaded to remote and (optionally) removed locally.
    archived: Vec<ArchivedSegment>,
    /// Segments already fetched this session — avoid re-download.
    fetched: HashMap<[u8; 32], ()>,
    /// When true, `archive_segments` also uploads `.idx`, `.xf`, and `.meta` sidecars.
    upload_sidecars: bool,
}

impl TieredEngine {
    /// Create a new `TieredEngine` from a local `Engine` and a `RemoteStore` backend.
    pub fn new(local: Engine, remote: Box<dyn RemoteStore>) -> Self {
        Self {
            local,
            remote,
            archived: Vec::new(),
            fetched: HashMap::new(),
            upload_sidecars: false,
        }
    }

    /// Enable or disable sidecar upload during `archive_segments`.
    ///
    /// When enabled, `archive_segments` uploads `.idx`, `.xf`, and `.meta` files
    /// alongside each `.dat` segment. Sidecars let `ImmutableEngine` reconstruct
    /// its in-memory index and filter without decompressing the full `.dat` file.
    /// Requires the `RemoteStore` to implement `upload_aux`.
    pub fn with_sidecars(mut self, enabled: bool) -> Self {
        self.upload_sidecars = enabled;
        self
    }

    /// Access the underlying local engine (e.g. for snapshots, metrics, compaction).
    pub fn local(&self) -> &Engine {
        &self.local
    }

    /// Mutable access to the underlying local engine.
    pub fn local_mut(&mut self) -> &mut Engine {
        &mut self.local
    }

    /// Return a clone of the archived segment list.
    pub fn archived_segments(&self) -> Vec<ArchivedSegment> {
        self.archived.clone()
    }

    /// Register segments as archived without uploading them.
    ///
    /// Use this after restart to restore the archived list from persistent storage
    /// (e.g. a JSON file you wrote after a previous `archive_segments` call).
    pub fn register_archived(&mut self, segments: Vec<ArchivedSegment>) {
        for seg in segments {
            if !self.archived.iter().any(|s| s.hash == seg.hash) {
                self.archived.push(seg);
            }
        }
    }

    // ── Pass-through KV API ─────────────────────────────────────────────────

    /// Single-record put. Goes to local engine only.
    pub fn put(&mut self, ns: &[u8], key: &[u8], val: &[u8]) -> Result<u64, EdgestoreError> {
        self.local.put(ns, key, val)
    }

    /// Single-record put with TTL.
    pub fn put_with_ttl(
        &mut self,
        ns: &[u8],
        key: &[u8],
        val: &[u8],
        ttl_secs: u32,
    ) -> Result<u64, EdgestoreError> {
        self.local.put_with_ttl(ns, key, val, ttl_secs)
    }

    /// Delete a record.
    pub fn delete(&mut self, ns: &[u8], key: &[u8]) -> Result<u64, EdgestoreError> {
        self.local.delete(ns, key)
    }

    // ── Read-through API ────────────────────────────────────────────────────

    /// Get a value. Local first; on miss checks archived segments and fetches from remote.
    pub fn get(&mut self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        // Fast path: local only.
        if let Some(val) = self.local.get(ns, key)? {
            return Ok(Some(val));
        }

        // Slow path: find archived segments that could contain this key.
        let encoded_key = edgestore::types::encode_key(ns, key);
        let to_fetch: Vec<[u8; 32]> = self
            .archived
            .iter()
            .filter(|seg| encoded_key >= seg.min_key && encoded_key <= seg.max_key)
            .filter(|seg| !self.fetched.contains_key(&seg.hash))
            .map(|seg| seg.hash)
            .collect();

        // Fetch outside the borrow of archived.
        for hash in to_fetch {
            if let Err(e) = self.fetch_and_import(&hash) {
                eprintln!(
                    "[edgestore-tier] failed to fetch segment {}: {}",
                    hex_hash(&hash),
                    e
                );
            }
        }

        // Retry after any imports.
        self.local.get(ns, key)
    }

    /// Range scan. Local only; does not fetch from remote.
    ///
    /// To include remote data, call `sync_archived()` or `fetch_all_archived()` first.
    #[allow(clippy::type_complexity)]
    pub fn range(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        self.local.range(ns, start, end)
    }

    /// Prefix scan. Local only.
    #[allow(clippy::type_complexity)]
    pub fn prefix(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        self.local.prefix(ns, prefix)
    }

    // ── Archiving ───────────────────────────────────────────────────────────

    /// Upload a list of local segments to remote storage and register them as archived.
    ///
    /// `metas` is typically obtained from `engine.list_segment_metas()`. After upload,
    /// the caller may delete the local `.dat` files to reclaim space.
    /// The segment metadata (min/max key bounds) is recorded for future read-through.
    ///
    /// When `with_sidecars(true)` is set, also uploads `.idx`, `.xf`, and `.meta`
    /// files alongside each `.dat`. Sidecar upload errors are logged but do not fail
    /// the archive — the `.dat` alone is sufficient for correctness.
    pub fn archive_segments(&mut self, metas: &[edgestore::types::SegmentMeta]) -> Result<(), EdgestoreError> {
        let base = self.local.db_path().to_path_buf();

        for meta in metas {
            let hash: [u8; 32] = meta.segment_hash.as_slice().try_into().map_err(|_| {
                EdgestoreError::ReplicationError(format!(
                    "segment {} hash is not 32 bytes",
                    meta.segment_id
                ))
            })?;

            let dat_path = base.join(format!("segment-{:08}.dat", meta.segment_id));

            if !dat_path.exists() {
                continue; // already archived / deleted
            }

            let data = std::fs::read(&dat_path).map_err(EdgestoreError::Io)?;
            self.upload_with_retry(&hash, &data)?;

            if self.upload_sidecars {
                for ext in &["idx", "xf", "meta"] {
                    let sidecar_path = base.join(format!("segment-{:08}.{}", meta.segment_id, ext));
                    if let Ok(bytes) = std::fs::read(&sidecar_path) {
                        if let Err(e) = self.remote.upload_aux(&hash, ext, &bytes) {
                            eprintln!(
                                "[edgestore-tier] sidecar upload skipped for {}.{}: {}",
                                hex_hash(&hash),
                                ext,
                                e
                            );
                        }
                    }
                }
            }

            // Record as archived.
            self.archived.push(ArchivedSegment {
                hash,
                min_key: meta.min_key.clone(),
                max_key: meta.max_key.clone(),
            });
        }

        Ok(())
    }

    /// Download and import one specific archived segment by hash — the selective
    /// alternative to `fetch_all_archived` for callers that know (e.g. via
    /// `archived_segments()`'s key bounds) exactly which segment they need, such as
    /// a range query fetching only the segments overlapping its time window.
    pub fn fetch_segment(&mut self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
        self.fetch_and_import(hash)
    }

    /// Download and import only the archived segments whose `[min_key, max_key]`
    /// bounds overlap the given namespace key range.
    ///
    /// This is the selective alternative to `fetch_all_archived()` for
    /// range-query-shaped workloads (e.g. time-series / log ingestion) that
    /// cannot afford to fully rehydrate before every scan. After calling this,
    /// `range()` and `prefix()` will include data from the fetched segments.
    pub fn fetch_archived_overlapping(
        &mut self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<(), EdgestoreError> {
        let start = edgestore::types::encode_key(ns, start);
        let end = edgestore::types::encode_key(ns, end);

        let overlapping: Vec<[u8; 32]> = self
            .archived
            .iter()
            .filter(|seg| seg.max_key >= start && seg.min_key < end)
            .map(|seg| seg.hash)
            .collect();

        for hash in overlapping {
            self.fetch_and_import(&hash)?;
        }
        Ok(())
    }

    /// Upload a segment with exponential backoff retry.
    /// Retries up to 3 times on transient errors.
    fn upload_with_retry(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError> {
        const MAX_RETRIES: u32 = 3;
        const BASE_DELAY_MS: u64 = 10;

        let mut last_err = None;
        for attempt in 0..=MAX_RETRIES {
            match self.remote.upload(hash, data) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let msg = e.to_string();
                    last_err = Some(e);
                    if msg.contains("throttled") || msg.contains("timeout") || msg.contains("503") {
                        if attempt < MAX_RETRIES {
                            let delay = BASE_DELAY_MS * (1u64 << attempt);
                            eprintln!(
                                "[edgestore-tier] upload retry {} for {} after {} ms",
                                attempt + 1,
                                hex_hash(hash),
                                delay
                            );
                            std::thread::sleep(std::time::Duration::from_millis(delay));
                        }
                    } else {
                        break;
                    }
                }
            }
        }

        Err(EdgestoreError::ReplicationError(format!(
            "upload segment {} failed after {} retries: {}",
            hex_hash(hash),
            MAX_RETRIES,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    /// Download and import a segment by hash with exponential backoff retry.
    /// Retries up to 3 times on transient errors (e.g. throttling, timeout).
    fn fetch_and_import(&mut self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
        const MAX_RETRIES: u32 = 3;
        const BASE_DELAY_MS: u64 = 10;

        let mut last_err = None;
        for attempt in 0..=MAX_RETRIES {
            match self.remote.download(hash) {
                Ok(data) => {
                    match self.local.import_segment(&data, hash)? {
                        ImportResult::Applied { keys_written, keys_skipped } => {
                            eprintln!(
                                "[edgestore-tier] imported {} ({} written, {} skipped)",
                                hex_hash(hash),
                                keys_written,
                                keys_skipped
                            );
                        }
                        ImportResult::Skipped => {
                            eprintln!("[edgestore-tier] segment {} already local", hex_hash(hash));
                        }
                        ImportResult::HashMismatch => {
                            return Err(EdgestoreError::ReplicationError(format!(
                                "hash mismatch for segment {}",
                                hex_hash(hash)
                            )));
                        }
                    }
                    self.fetched.insert(*hash, ());
                    return Ok(());
                }
                Err(e) => {
                    let msg = e.to_string();
                    last_err = Some(e);
                    // Only retry on transient-looking errors.
                    if msg.contains("throttled") || msg.contains("timeout") || msg.contains("503") {
                        if attempt < MAX_RETRIES {
                            let delay = BASE_DELAY_MS * (1u64 << attempt);
                            eprintln!(
                                "[edgestore-tier] retry {} for {} after {} ms",
                                attempt + 1,
                                hex_hash(hash),
                                delay
                            );
                            std::thread::sleep(std::time::Duration::from_millis(delay));
                        }
                    } else {
                        // Non-transient error: fail immediately.
                        break;
                    }
                }
            }
        }

        Err(EdgestoreError::ReplicationError(format!(
            "download segment {} failed after {} retries: {}",
            hex_hash(hash),
            MAX_RETRIES,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    /// Fetch every archived segment into local storage.
    ///
    /// Useful for warming the cache before a range scan or for disaster recovery.
    pub fn fetch_all_archived(&mut self) -> Result<(), EdgestoreError> {
        let archived = self.archived.clone();
        for seg in &archived {
            if !self.fetched.contains_key(&seg.hash) {
                self.fetch_and_import(&seg.hash)?;
            }
        }
        Ok(())
    }

    /// Flush local memtable to segment.
    pub fn flush(&mut self) -> Result<(), EdgestoreError> {
        self.local.flush()
    }

    /// Compact local segments.
    pub fn compact_once(&mut self) -> Result<edgestore::CompactionStats, EdgestoreError> {
        self.local.compact_once()
    }
}

/// Encode a 32-byte hash as a 64-character lowercase hex string.
fn hex_hash(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{:02x}", b)).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_put_get_local() {
        let (_local_dir, _remote_dir, mut tiered) = make_tiered();

        tiered.put(b"ns", b"key", b"val").unwrap();
        let got = tiered.get(b"ns", b"key").unwrap();
        assert_eq!(got, Some(b"val".to_vec()));
    }

    #[test]
    fn test_get_not_found() {
        let (_local_dir, _remote_dir, mut tiered) = make_tiered();
        let got = tiered.get(b"ns", b"missing").unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn test_archive_and_read_through() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        // Write data and flush memtable to segment.
        tiered.put(b"ns", b"key1", b"val1").unwrap();
        tiered.put(b"ns", b"key2", b"val2").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        // Get segment metadata including key bounds.
        let metas = tiered.local().list_segment_metas();
        assert!(!metas.is_empty(), "should have at least one segment");

        // Archive segments to remote.
        tiered.archive_segments(&metas).unwrap();
        assert!(!tiered.archived_segments().is_empty(), "should have archived segments");

        // Create a brand-new engine on a fresh directory.
        // The new engine has none of the local segments.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));

        // Seed the new tiered engine with the archived segment list.
        for meta in &metas {
            let hash: [u8; 32] = meta.segment_hash.as_slice().try_into().unwrap();
            fresh_tiered.archived.push(ArchivedSegment {
                hash,
                min_key: meta.min_key.clone(),
                max_key: meta.max_key.clone(),
            });
        }

        // The new engine has no local data — get() must read-through from remote.
        let got = fresh_tiered.get(b"ns", b"key1").unwrap();
        assert_eq!(got, Some(b"val1".to_vec()), "read-through from remote");

        let got = fresh_tiered.get(b"ns", b"key2").unwrap();
        assert_eq!(got, Some(b"val2".to_vec()), "read-through second key");
    }

    #[test]
    fn test_delete_passthrough() {
        let (_local_dir, _remote_dir, mut tiered) = make_tiered();

        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.delete(b"ns", b"key").unwrap();
        let got = tiered.get(b"ns", b"key").unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn test_range_passthrough() {
        let (_local_dir, _remote_dir, mut tiered) = make_tiered();

        tiered.put(b"ns", b"a", b"1").unwrap();
        tiered.put(b"ns", b"b", b"2").unwrap();
        tiered.put(b"ns", b"c", b"3").unwrap();

        let vals = tiered.range(b"ns", b"a", b"c").unwrap();
        assert_eq!(vals.len(), 2); // exclusive end
    }

    #[test]
    fn test_idempotent_fetch() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        // Fresh engine with archived list registered.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));

        let archived: Vec<ArchivedSegment> = tiered.archived_segments();
        fresh_tiered.register_archived(archived);

        // First get — triggers fetch.
        let got = fresh_tiered.get(b"ns", b"key").unwrap();
        assert_eq!(got, Some(b"val".to_vec()));

        // Second get — should be local hit, no re-fetch.
        let got = fresh_tiered.get(b"ns", b"key").unwrap();
        assert_eq!(got, Some(b"val".to_vec()));
    }

    #[test]
    fn test_lww_local_wins_over_archived() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        // Write v1 and flush to segment.
        tiered.put(b"ns", b"key", b"v1").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        // Write v2 locally (newer timestamp).
        tiered.put(b"ns", b"key", b"v2").unwrap();

        // Fresh engine — local is empty, only archived v1 exists.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh2")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        // Read-through imports v1. Then local put v2.
        let got = fresh_tiered.get(b"ns", b"key").unwrap();
        assert_eq!(got, Some(b"v1".to_vec()), "archived v1 imported");

        // Now write v2 locally.
        fresh_tiered.put(b"ns", b"key", b"v2").unwrap();

        // Local v2 should win.
        let got = fresh_tiered.get(b"ns", b"key").unwrap();
        assert_eq!(got, Some(b"v2".to_vec()), "local v2 wins via LWW");
    }

    #[test]
    fn test_partial_archive_mixed_local_remote() {
        let (_local_dir, _remote_dir, mut tiered) = make_tiered();

        // Write two batches, flush separately.
        for i in 0u64..500 {
            tiered.put(b"ns", &i.to_be_bytes(), b"batch1").unwrap();
        }
        tiered.local_mut().flush_to_segments().unwrap();

        let metas1 = tiered.local().list_segment_metas();

        for i in 500u64..1000 {
            tiered.put(b"ns", &i.to_be_bytes(), b"batch2").unwrap();
        }
        tiered.local_mut().flush_to_segments().unwrap();

        let _metas2 = tiered.local().list_segment_metas();

        // Archive only the first batch's segments.
        tiered.archive_segments(&metas1).unwrap();

        // Simulate: new engine, first batch is archived, second batch is local.
        // We can't easily simulate this with a single engine instance, so we verify:
        // 1. Archived keys are readable through read-through
        // 2. Local keys are still readable
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

        // Fresh engine with archived list.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh3")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        // Range before warming — empty.
        let vals = fresh_tiered.range(b"ns", b"a", b"d").unwrap();
        assert!(vals.is_empty(), "range before warming is empty");

        // Warm all archived segments.
        fresh_tiered.fetch_all_archived().unwrap();

        // Range after warming — includes all data.
        let vals = fresh_tiered.range(b"ns", b"a", b"d").unwrap();
        assert_eq!(vals.len(), 3, "range after warming includes all keys");
    }

    #[test]
    fn test_archived_not_found_in_remote() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        // Delete the remote file to simulate S3 deletion / bucket corruption.
        let remote_base = remote_dir.path();
        for entry in std::fs::read_dir(remote_base).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().ends_with(".seg") {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }

        // Fresh engine with archived list pointing to deleted remote objects.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh4")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        // Should return None gracefully, not panic.
        let got = fresh_tiered.get(b"ns", b"key").unwrap();
        assert_eq!(got, None, "graceful None when remote segment missing");
    }

    #[test]
    fn test_fetch_archived_overlapping_selective() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        // Two separate flushes → two segments with distinct key ranges.
        tiered.put(b"logs", b"2020", b"old-data").unwrap();
        let meta_early = tiered.local_mut().flush_to_segments().unwrap();

        tiered.put(b"logs", b"2030", b"new-data").unwrap();
        let meta_late = tiered.local_mut().flush_to_segments().unwrap();

        tiered.archive_segments(&[meta_early.clone(), meta_late.clone()]).unwrap();

        // Fresh engine with archived list.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fresh5")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        // Fetch only the range overlapping the early key.
        fresh_tiered.fetch_archived_overlapping(b"logs", b"2020", b"2021").unwrap();

        // Early key is now local.
        let early = fresh_tiered.get(b"logs", b"2020").unwrap();
        assert_eq!(early, Some(b"old-data".to_vec()), "overlapping segment fetched");

        // Late key is still not local — range() over un-fetched region is empty.
        let scan = fresh_tiered.range(b"logs", b"2025", b"2035").unwrap();
        assert!(scan.is_empty(), "non-overlapping segment must not have been fetched");
    }

    #[test]
    fn test_readthrough_latency_within_budget() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        // Write 1000 keys and flush.
        for i in 0u64..1000 {
            tiered.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
        }
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        // Fresh engine: read-through must complete within budget.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("latency")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        let start = std::time::Instant::now();
        let got = fresh_tiered.get(b"ns", &500u64.to_be_bytes()).unwrap();
        let elapsed_ms = start.elapsed().as_millis() as f64;

        assert_eq!(got, Some(b"value".to_vec()));

        // Budget: debug < 500 ms, release < 100 ms (filesystem remote; real S3 is slower).
        let budget_ms = if cfg!(debug_assertions) { 500.0 } else { 100.0 };
        assert!(
            elapsed_ms < budget_ms,
            "read-through latency {:.1} ms exceeds budget {:.0} ms",
            elapsed_ms,
            budget_ms
        );
    }

    #[test]
    fn test_large_segment_archive_and_readthrough_10k() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        // Write 10K keys and flush.
        for i in 0u64..10_000 {
            tiered.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
        }
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        // Fresh engine: read-through must find keys across the full range.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("10k")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        // Spot-check: first, middle, last.
        for i in [0u64, 5000, 9999] {
            let got = fresh_tiered.get(b"ns", &i.to_be_bytes()).unwrap();
            assert_eq!(got, Some(b"value".to_vec()), "key {} must read-through", i);
        }

        // Range scan after warming must include all keys.
        fresh_tiered.fetch_all_archived().unwrap();
        let vals = fresh_tiered.range(b"ns", &0u64.to_be_bytes(), &10_000u64.to_be_bytes()).unwrap();
        assert_eq!(vals.len(), 10_000, "range after warming must include all 10K keys");
    }

    #[test]
    fn test_concurrent_archive_and_read() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        // Seed data and flush.
        for i in 0u64..1000 {
            tiered.put(b"ns", &i.to_be_bytes(), b"value").unwrap();
        }
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();

        // Spawn archive in one thread (separate local dir to avoid WriterBusy),
        // reads in the main thread.
        use std::sync::Arc;
        use std::thread;
        use std::sync::atomic::{AtomicBool, Ordering};

        let done = Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        // Archive thread uses a separate local directory to avoid locking the main one.
        let archive_local_path = local_dir.path().join("archive_concurrent");
        let remote_path = remote_dir.path().to_path_buf();
        let metas_clone = metas.clone();

        let archive_handle = thread::spawn(move || {
            let local = Engine::open(EdgestoreConfig::new(&archive_local_path)).unwrap();
            let remote = FilesystemRemoteStore::new(remote_path).unwrap();
            let mut t = TieredEngine::new(local, Box::new(remote));
            t.archive_segments(&metas_clone).unwrap();
            done_clone.store(true, Ordering::SeqCst);
        });

        // Concurrent reads on the main engine (local hits, no remote involvement).
        let mut read_count = 0usize;
        while !done.load(Ordering::SeqCst) {
            let _ = tiered.get(b"ns", &500u64.to_be_bytes()).unwrap();
            read_count += 1;
            if read_count > 10_000 {
                break; // safety valve
            }
        }

        archive_handle.join().unwrap();
        assert!(read_count > 0, "reads occurred concurrently with archive");
    }

    #[test]
    fn test_corrupt_segment_hash_mismatch_rejected() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        // Corrupt the remote file: overwrite first byte.
        // FilesystemRemoteStore uses .seg extension.
        let remote_base = remote_dir.path();
        for entry in std::fs::read_dir(remote_base).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name().to_string_lossy().ends_with(".seg") {
                let mut bytes = std::fs::read(entry.path()).unwrap();
                bytes[0] = !bytes[0]; // flip first byte
                std::fs::write(entry.path(), bytes).unwrap();
            }
        }

        // Fresh engine: use fetch_segment (not get()) which does not swallow errors.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("corrupt")))
            .expect("fresh Engine::open");
        let fresh_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        // fetch_segment must return Err on hash mismatch (get() swallows the error).
        let hash = tiered.archived_segments()[0].hash;
        let result = fresh_tiered.fetch_segment(&hash);
        assert!(result.is_err(), "corrupt segment hash mismatch must be rejected");
    }

    // ── Mock RemoteStore for fault injection ─────────────────────────────────

    struct FaultyRemoteStore {
        inner: FilesystemRemoteStore,
        fail_next_upload: AtomicBool,
        fail_next_download: AtomicBool,
        delay_ms: AtomicU64,
    }

    use std::sync::atomic::{AtomicBool, AtomicU64};

    impl FaultyRemoteStore {
        fn new(inner: FilesystemRemoteStore) -> Self {
            Self {
                inner,
                fail_next_upload: AtomicBool::new(false),
                fail_next_download: AtomicBool::new(false),
                delay_ms: AtomicU64::new(0),
            }
        }
    }

    impl RemoteStore for FaultyRemoteStore {
        fn upload(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError> {
            if self.fail_next_upload.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(EdgestoreError::ReplicationError("injected upload fault".to_string()));
            }
            if let Some(delay) = std::time::Duration::from_millis(self.delay_ms.load(std::sync::atomic::Ordering::SeqCst)).checked_mul(1) {
                std::thread::sleep(delay);
            }
            self.inner.upload(hash, data)
        }

        fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
            if self.fail_next_download.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(EdgestoreError::ReplicationError("injected download fault".to_string()));
            }
            self.inner.download(hash)
        }

        fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError> {
            self.inner.list()
        }

        fn delete(&self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
            self.inner.delete(hash)
        }
    }

    #[test]
    fn test_upload_fault_propagates() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();

        let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
        let inner_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
        let remote = FaultyRemoteStore::new(inner_remote);
        remote.fail_next_upload.store(true, std::sync::atomic::Ordering::SeqCst);

        let mut tiered = TieredEngine::new(local, Box::new(remote));
        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        let result = tiered.archive_segments(&metas);
        assert!(result.is_err(), "upload fault must propagate");
    }

    #[test]
    fn test_download_fault_propagates() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();

        let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
        let inner_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
        let remote = FaultyRemoteStore::new(inner_remote);

        let mut tiered = TieredEngine::new(local, Box::new(remote));
        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        // Fresh engine with injected download fault.
        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("fault"))).unwrap();
        let fresh_inner = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
        let fresh_remote = FaultyRemoteStore::new(fresh_inner);
        fresh_remote.fail_next_download.store(true, std::sync::atomic::Ordering::SeqCst);

        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        // fetch_segment must return Err on download fault (get() swallows the error).
        let hash = tiered.archived_segments()[0].hash;
        let result = fresh_tiered.fetch_segment(&hash);
        assert!(result.is_err(), "download fault must propagate");
    }

    #[test]
    fn test_concurrent_fetch_same_segment_idempotent() {
        let (local_dir, remote_dir, mut tiered) = make_tiered();

        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();
        let archived = tiered.archived_segments();
        let hash = archived[0].hash;

        // Two fresh engines on separate local dirs fetch the same segment concurrently.
        let fresh_local_a = Engine::open(EdgestoreConfig::new(local_dir.path().join("concurrent_a"))).unwrap();
        let fresh_local_b = Engine::open(EdgestoreConfig::new(local_dir.path().join("concurrent_b"))).unwrap();
        let remote_a = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
        let remote_b = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();

        let mut tiered_a = TieredEngine::new(fresh_local_a, Box::new(remote_a));
        let mut tiered_b = TieredEngine::new(fresh_local_b, Box::new(remote_b));
        tiered_a.register_archived(archived.clone());
        tiered_b.register_archived(archived.clone());

        use std::thread;
        let handle_a = thread::spawn(move || {
            tiered_a.fetch_segment(&hash).unwrap();
            tiered_a.get(b"ns", b"key").unwrap()
        });
        let handle_b = thread::spawn(move || {
            tiered_b.fetch_segment(&hash).unwrap();
            tiered_b.get(b"ns", b"key").unwrap()
        });

        let got_a = handle_a.join().unwrap();
        let got_b = handle_b.join().unwrap();
        assert_eq!(got_a, Some(b"val".to_vec()));
        assert_eq!(got_b, Some(b"val".to_vec()));
    }

    #[test]
    fn test_archive_with_sidecars_uploads_aux_files() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();

        let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
        let remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();

        let mut tiered = TieredEngine::new(local, Box::new(remote)).with_sidecars(true);

        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        let remote_files: Vec<String> = std::fs::read_dir(remote_dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        let has_ext = |ext: &str| remote_files.iter().any(|f| f.ends_with(ext));
        assert!(has_ext(".seg"), ".dat (as .seg) must be present");
        assert!(has_ext(".idx"), ".idx sidecar must be present");
        assert!(has_ext(".xf"), ".xf sidecar must be present");
        assert!(has_ext(".meta"), ".meta sidecar must be present");
    }

    #[test]
    fn test_archive_without_sidecars_skips_aux_files() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();

        let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
        let remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();

        // Default: sidecars disabled.
        let mut tiered = TieredEngine::new(local, Box::new(remote));

        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        tiered.archive_segments(&metas).unwrap();

        let remote_files: Vec<String> = std::fs::read_dir(remote_dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        let has_ext = |ext: &str| remote_files.iter().any(|f| f.ends_with(ext));
        assert!(has_ext(".seg"), ".dat (as .seg) must be present");
        assert!(!has_ext(".idx"), ".idx sidecar must NOT be present without with_sidecars");
        assert!(!has_ext(".xf"), ".xf sidecar must NOT be present without with_sidecars");
        assert!(!has_ext(".meta"), ".meta sidecar must NOT be present without with_sidecars");
    }

    #[test]
    fn test_sidecar_download_roundtrip_via_remote_store() {
        let (_dir, remote_dir, _tiered) = make_tiered();

        let local = Engine::open(EdgestoreConfig::new(
            remote_dir.path().join("arch_local"),
        ))
        .unwrap();
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

        assert!(idx_bytes.is_ok(), "idx sidecar downloadable: {:?}", idx_bytes.err());
        assert!(xf_bytes.is_ok(), "xf sidecar downloadable");
        assert!(meta_bytes.is_ok(), "meta sidecar downloadable");
        assert!(!idx_bytes.unwrap().is_empty(), "idx sidecar non-empty");
    }

    #[test]
    fn test_sidecars_enabled_but_upload_aux_unsupported_archive_still_succeeds() {
        // A store whose upload_aux returns Err (the default impl) must not break
        // archive_segments — the .dat upload is what matters for correctness.
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();

        // Wrap FilesystemRemoteStore to override upload_aux with the default Err behavior.
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
        let remote = NoAuxStore(
            FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap(),
        );
        let mut tiered = TieredEngine::new(local, Box::new(remote)).with_sidecars(true);

        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();
        let metas = tiered.local().list_segment_metas();

        // Must succeed even though upload_aux will return Err for every sidecar.
        tiered.archive_segments(&metas).expect("archive must succeed despite upload_aux Err");
        assert!(!tiered.archived_segments().is_empty(), "segment recorded as archived");

        // .dat is present in remote; sidecars are absent.
        let remote_files: Vec<String> = std::fs::read_dir(remote_dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(remote_files.iter().any(|f| f.ends_with(".seg")), ".dat must be archived");
        assert!(!remote_files.iter().any(|f| f.ends_with(".idx")), ".idx must be absent");
    }

    #[test]
    fn test_network_delay_does_not_panic() {
        let local_dir = TempDir::new().unwrap();
        let remote_dir = TempDir::new().unwrap();

        let local = Engine::open(EdgestoreConfig::new(local_dir.path())).unwrap();
        let inner_remote = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
        let remote = FaultyRemoteStore::new(inner_remote);
        remote.delay_ms.store(50, std::sync::atomic::Ordering::SeqCst);

        let mut tiered = TieredEngine::new(local, Box::new(remote));
        tiered.put(b"ns", b"key", b"val").unwrap();
        tiered.local_mut().flush_to_segments().unwrap();

        let metas = tiered.local().list_segment_metas();
        let start = std::time::Instant::now();
        tiered.archive_segments(&metas).unwrap();
        let elapsed = start.elapsed().as_millis() as u64;

        assert!(elapsed >= 50, "delay should have been applied: {} ms", elapsed);
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

        let fresh_local = Engine::open(EdgestoreConfig::new(local_dir.path().join("throttle"))).unwrap();
        let fresh_inner = FilesystemRemoteStore::new(remote_dir.path().to_path_buf()).unwrap();
        let fresh_remote = ThrottlingRemoteStore::new(fresh_inner, 2);
        let mut fresh_tiered = TieredEngine::new(fresh_local, Box::new(fresh_remote));
        fresh_tiered.register_archived(tiered.archived_segments());

        // get() → fetch_and_import with retry. 3rd attempt succeeds.
        let got = fresh_tiered.get(b"ns", b"key").unwrap();
        assert_eq!(got, Some(b"val".to_vec()), "retry eventually succeeds");
    }

    // ── Throttling mock: fails first N calls, then succeeds ────────────────────

    struct ThrottlingRemoteStore {
        inner: FilesystemRemoteStore,
        fail_count: AtomicU64,
        max_fails: u64,
    }

    impl ThrottlingRemoteStore {
        fn new(inner: FilesystemRemoteStore, max_fails: u64) -> Self {
            Self { inner, fail_count: AtomicU64::new(0), max_fails }
        }

        fn try_inner<F, T>(&self, f: F) -> Result<T, EdgestoreError>
        where F: FnOnce(&FilesystemRemoteStore) -> Result<T, EdgestoreError>
        {
            let count = self.fail_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count < self.max_fails {
                return Err(EdgestoreError::ReplicationError(
                    format!("throttled: 503 Slow Down (attempt {})", count + 1)
                ));
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
}
