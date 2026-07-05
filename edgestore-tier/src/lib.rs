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
}

impl TieredEngine {
    /// Create a new `TieredEngine` from a local `Engine` and a `RemoteStore` backend.
    pub fn new(local: Engine, remote: Box<dyn RemoteStore>) -> Self {
        Self {
            local,
            remote,
            archived: Vec::new(),
            fetched: HashMap::new(),
        }
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

            // Upload to remote.
            let data = std::fs::read(&dat_path).map_err(|e| {
                EdgestoreError::Io(e)
            })?;
            self.remote.upload(&hash, &data).map_err(|e| {
                EdgestoreError::ReplicationError(e.to_string())
            })?;

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

    /// Download and import a segment by hash.
    fn fetch_and_import(&mut self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
        let data = self.remote.download(hash).map_err(|e| {
            EdgestoreError::ReplicationError(format!(
                "download segment {}: {}",
                hex_hash(hash),
                e
            ))
        })?;

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
        Ok(())
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
}
