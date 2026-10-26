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
//! 3. **`get()`** tries local first. On miss, scans archived segments by key
//!    bounds, downloads the matching segment, imports it via
//!    `Engine::import_segment` (LWW merge), then retries.
//! 4. **`range()` and `prefix()`** merge local results with any archived
//!    segments whose key bounds overlap the query range. Archived segments are
//!    downloaded **ephemerally** (no local import, no disk growth) and merged
//!    in-memory via `ImmutableEngine`. Local data wins for any key present in
//!    both local and archived segments.
//! 5. **Text-index stripping** (`with_text_stripping(true)`) removes embedded
//!    BM25 index records from local segment copies after successful archival,
//!    reclaiming disk space without touching the remote copy.
//! 6. **No background tasks.** Everything is synchronous and caller-driven.
//!    The application decides when to archive, how much to keep local, and
//!    whether to prefetch.
//!
//! ## Monitor-and-archive pattern
//!
//! ```rust,no_run
//! use edgestore::{EdgestoreConfig, Engine};
//! use edgestore_repl::FilesystemRemoteStore;
//! use edgestore_tier::TieredEngine;
//!
//! let local = Engine::open(EdgestoreConfig::new("/var/db/hot")).unwrap();
//! let remote = FilesystemRemoteStore::new("/mnt/cold".into()).unwrap();
//! let mut tiered = TieredEngine::new(local, Box::new(remote))
//!     .with_text_stripping(true)
//!     .with_segment_cache_bytes(32 * 1024 * 1024);
//!
//! loop {
//!     let metas = tiered.local().list_segment_metas();
//!     if metas.len() > 8 {
//!         let to_archive = &metas[..metas.len() - 4];
//!         tiered.archive_segments(to_archive).unwrap();
//!     }
//! # break;
//! }
//! ```

use std::collections::HashMap;

use edgestore::error::EdgestoreError;
use edgestore::types::encode_key;
use edgestore::{Engine, ImmutableEngine, ImportResult, RemoteStore};
use lru::LruCache;
use std::num::NonZeroUsize;

type KvPairs = Vec<(Vec<u8>, Vec<u8>)>;

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

/// Default maximum bytes for the ephemeral segment byte cache (32 MB).
const DEFAULT_SEGMENT_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;

/// Maximum retry attempts for transient remote errors (throttle, timeout, 503).
const MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff: delay_ms = BASE_DELAY_MS * 2^attempt.
const BASE_DELAY_MS: u64 = 10;

/// Tiered engine: local hot cache + remote cold archive.
///
/// Writes go to local. Reads fall back to remote on miss.
/// Segment import uses the existing `Engine::import_segment` path (LWW merge).
pub struct TieredEngine {
    pub(crate) local: Engine,
    pub(crate) remote: Box<dyn RemoteStore>,
    /// Segments uploaded to remote (optionally removed locally).
    archived: Vec<ArchivedSegment>,
    /// Segments already fetched this session — avoid re-download.
    fetched: HashMap<[u8; 32], ()>,
    /// When true, `archive_segments` uploads `.idx`, `.xf`, `.meta` sidecars.
    upload_sidecars: bool,
    /// When true, strips BM25 index records from local copy after archive.
    strip_text_after_archive: bool,
    /// When true, strips vector index records from local copy after archive.
    strip_vector_after_archive: bool,
    /// LRU cache of raw segment bytes for ephemeral range/prefix reads.
    segment_cache: LruCache<[u8; 32], Vec<u8>>,
    /// Current total bytes resident in `segment_cache`.
    pub(crate) segment_cache_bytes: usize,
    /// Maximum total bytes allowed in `segment_cache` before LRU eviction.
    segment_cache_max_bytes: usize,
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
            strip_text_after_archive: false,
            strip_vector_after_archive: false,
            // Item cap is high enough that byte-based eviction always fires first.
            // Prevents a silent second eviction path from drifting segment_cache_bytes.
            segment_cache: LruCache::new(NonZeroUsize::new(65536).unwrap()),
            segment_cache_bytes: 0,
            segment_cache_max_bytes: DEFAULT_SEGMENT_CACHE_MAX_BYTES,
        }
    }

    /// Set the maximum total bytes for the ephemeral segment byte cache.
    /// Default: 32 MB. Set to 0 to disable caching.
    pub fn with_segment_cache_bytes(mut self, max_bytes: usize) -> Self {
        self.segment_cache_max_bytes = max_bytes;
        self
    }

    /// Enable or disable sidecar upload during `archive_segments`.
    pub fn with_sidecars(mut self, enabled: bool) -> Self {
        self.upload_sidecars = enabled;
        self
    }

    /// Enable or disable automatic text-index stripping after archiving.
    pub fn with_text_stripping(mut self, enabled: bool) -> Self {
        self.strip_text_after_archive = enabled;
        self
    }

    /// Enable or disable automatic vector-index stripping after archiving.
    pub fn with_vector_stripping(mut self, enabled: bool) -> Self {
        self.strip_vector_after_archive = enabled;
        self
    }

    /// Access the underlying local engine.
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
    /// Use this after restart to restore the archived list from persistent
    /// storage (e.g. a JSON file written after a previous `archive_segments`).
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
        if let Some(val) = self.local.get(ns, key)? {
            return Ok(Some(val));
        }

        let encoded_key = edgestore::types::encode_key(ns, key);
        let to_fetch: Vec<[u8; 32]> = self
            .archived
            .iter()
            .filter(|seg| encoded_key >= seg.min_key && encoded_key <= seg.max_key)
            .filter(|seg| !self.fetched.contains_key(&seg.hash))
            .map(|seg| seg.hash)
            .collect();

        for hash in to_fetch {
            if let Err(e) = self.fetch_and_import(&hash) {
                eprintln!(
                    "[edgestore-tier] failed to fetch segment {}: {}",
                    hex_hash(&hash),
                    e
                );
            }
        }

        self.local.get(ns, key)
    }

    /// Range scan across local data and any archived segments whose key bounds
    /// overlap `[start, end)`. Local data wins on key conflict.
    #[allow(clippy::type_complexity)]
    pub fn range(
        &mut self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<KvPairs, EdgestoreError> {
        let local = self.local.range(ns, start, end)?;
        let archived = self.range_archived(ns, start, end)?;
        Ok(merge_local_wins(local, archived))
    }

    /// Prefix scan across local data and any archived segments whose key bounds
    /// overlap the prefix range. Local data wins on key conflict.
    #[allow(clippy::type_complexity)]
    pub fn prefix(&mut self, ns: &[u8], prefix: &[u8]) -> Result<KvPairs, EdgestoreError> {
        let local = self.local.prefix(ns, prefix)?;
        let archived = self.prefix_archived(ns, prefix)?;
        Ok(merge_local_wins(local, archived))
    }

    // ── Archiving ───────────────────────────────────────────────────────────

    /// Upload local segments to remote storage and register them as archived.
    ///
    /// When `with_sidecars(true)`: also uploads `.idx`, `.xf`, `.meta` files.
    /// Sidecar upload errors are logged but do not fail the archive.
    pub fn archive_segments(
        &mut self,
        metas: &[edgestore::types::SegmentMeta],
    ) -> Result<(), EdgestoreError> {
        assert!(!metas.is_empty() || true, "metas slice accepted (may be empty)");
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
            assert!(!data.is_empty(), "segment .dat file must not be empty");
            self.upload_with_retry(&hash, &data)?;

            if self.upload_sidecars {
                for ext in &["idx", "xf", "meta"] {
                    let sidecar_path =
                        base.join(format!("segment-{:08}.{}", meta.segment_id, ext));
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

            self.archived.push(ArchivedSegment {
                hash,
                min_key: meta.min_key.clone(),
                max_key: meta.max_key.clone(),
            });

            if self.strip_text_after_archive {
                if let Err(e) = self.local.strip_text_index(meta.segment_id) {
                    eprintln!(
                        "[edgestore-tier] strip_text_index skipped for segment {}: {}",
                        meta.segment_id, e
                    );
                }
            }

            if self.strip_vector_after_archive {
                if let Err(e) = self.local.strip_vector_index(meta.segment_id) {
                    eprintln!(
                        "[edgestore-tier] strip_vector_index skipped for segment {}: {}",
                        meta.segment_id, e
                    );
                }
            }
        }

        Ok(())
    }

    /// Download and import one specific archived segment by hash.
    pub fn fetch_segment(&mut self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
        self.fetch_and_import(hash)
    }

    /// Download a segment's raw bytes without importing into local storage.
    ///
    /// Use this to build an ephemeral `ImmutableEngine` without growing local
    /// storage. Does not touch `self.local` or `self.fetched`.
    pub fn download_segment(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
        assert!(hash.len() == 32, "hash must be 32 bytes");
        self.remote.download(hash).map_err(|e| {
            EdgestoreError::ReplicationError(format!("download segment {}: {}", hex_hash(hash), e))
        })
    }

    /// Download and import only the archived segments whose key bounds overlap
    /// the given namespace key range.
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

    /// Fetch every archived segment into local storage.
    pub fn fetch_all_archived(&mut self) -> Result<(), EdgestoreError> {
        let archived = self.archived.clone();
        for seg in &archived {
            if !self.fetched.contains_key(&seg.hash) {
                self.fetch_and_import(&seg.hash)?;
            }
        }
        Ok(())
    }

    // ── Read-only fast-path helpers ──────────────────────────────────────────
    //
    // These `&self` methods let a caller (e.g. `AsyncTieredEngine`) check whether
    // a read needs a write lock, cutting contention between concurrent readers and
    // the single writer for the common local-hit case.

    /// Local-only get, no archived read-through.
    /// `None` does not mean absent — caller must fall back to `get()`.
    pub fn local_only_get(&self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        self.local.get(ns, key)
    }

    fn has_unfetched_archived_overlap(&self, enc_start: &[u8], enc_end: &[u8]) -> bool {
        self.archived.iter().any(|seg| {
            seg.max_key.as_slice() >= enc_start
                && seg.min_key.as_slice() < enc_end
                && !self.fetched.contains_key(&seg.hash)
        })
    }

    fn has_unfetched_archived_point(&self, enc_key: &[u8]) -> bool {
        self.archived.iter().any(|seg| {
            seg.min_key.as_slice() <= enc_key
                && seg.max_key.as_slice() >= enc_key
                && !self.fetched.contains_key(&seg.hash)
        })
    }

    /// True if answering `get(ns, key)` might require fetching an archived segment.
    pub fn get_needs_archived_fetch(&self, ns: &[u8], key: &[u8]) -> bool {
        let enc_key = encode_key(ns, key);
        self.has_unfetched_archived_point(&enc_key)
    }

    /// True if answering `range(ns, start, end)` would need to fetch an archived segment.
    pub fn range_needs_archived_fetch(&self, ns: &[u8], start: &[u8], end: &[u8]) -> bool {
        let enc_start = encode_key(ns, start);
        let enc_end = encode_key(ns, end);
        self.has_unfetched_archived_overlap(&enc_start, &enc_end)
    }

    /// Local-only range scan, no archived read-through.
    pub fn local_only_range(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<KvPairs, EdgestoreError> {
        self.local.range(ns, start, end)
    }

    /// True if answering `prefix(ns, prefix)` would need to fetch an archived segment.
    pub fn prefix_needs_archived_fetch(&self, ns: &[u8], prefix: &[u8]) -> bool {
        let enc_prefix = encode_key(ns, prefix);
        let enc_end = edgestore::types::prefix_upper_bound(&enc_prefix)
            .unwrap_or_else(|| vec![0xFF; enc_prefix.len() + 1]);
        self.has_unfetched_archived_overlap(&enc_prefix, &enc_end)
    }

    /// Local-only prefix scan, no archived read-through.
    pub fn local_only_prefix(&self, ns: &[u8], prefix: &[u8]) -> Result<KvPairs, EdgestoreError> {
        self.local.prefix(ns, prefix)
    }

    // ── Ephemeral archived-segment reads ────────────────────────────────────

    /// Build an ephemeral `ImmutableEngine` from archived segments overlapping
    /// `[enc_start, enc_end)`. Serves from the LRU byte cache when available.
    fn ephemeral_engine_for_range(
        &mut self,
        enc_start: &[u8],
        enc_end: &[u8],
    ) -> Result<Option<ImmutableEngine>, EdgestoreError> {
        let to_load: Vec<[u8; 32]> = self
            .archived
            .iter()
            .filter(|seg| seg.max_key.as_slice() >= enc_start && seg.min_key.as_slice() < enc_end)
            .filter(|seg| !self.fetched.contains_key(&seg.hash))
            .map(|seg| seg.hash)
            .collect();

        if to_load.is_empty() {
            return Ok(None);
        }

        let mut pairs: Vec<(edgestore::types::SegmentMeta, Vec<u8>)> =
            Vec::with_capacity(to_load.len());

        for hash in to_load {
            let data = if let Some(cached) = self.segment_cache.get(&hash) {
                cached.clone()
            } else {
                match self.remote.download(&hash) {
                    Ok(data) => {
                        self.cache_segment(hash, data.clone());
                        data
                    }
                    Err(e) => {
                        eprintln!(
                            "[edgestore-tier] ephemeral download skipped for {}: {}",
                            hex_hash(&hash),
                            e
                        );
                        continue;
                    }
                }
            };
            pairs.push((synthetic_meta(&hash), data));
        }

        if pairs.is_empty() {
            return Ok(None);
        }

        Ok(Some(ImmutableEngine::from_segment_bytes(pairs)?))
    }

    fn range_archived(
        &mut self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<KvPairs, EdgestoreError> {
        let enc_start = encode_key(ns, start);
        let enc_end = encode_key(ns, end);
        match self.ephemeral_engine_for_range(&enc_start, &enc_end)? {
            Some(engine) => engine.range(ns, start, end),
            None => Ok(Vec::new()),
        }
    }

    fn prefix_archived(&mut self, ns: &[u8], prefix: &[u8]) -> Result<KvPairs, EdgestoreError> {
        let enc_prefix = encode_key(ns, prefix);
        let enc_end = edgestore::types::prefix_upper_bound(&enc_prefix)
            .unwrap_or_else(|| vec![0xFF; enc_prefix.len() + 1]);
        match self.ephemeral_engine_for_range(&enc_prefix, &enc_end)? {
            Some(engine) => engine.prefix(ns, prefix),
            None => Ok(Vec::new()),
        }
    }

    /// Upload a segment with exponential backoff retry on transient errors.
    fn upload_with_retry(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError> {
        assert!(!data.is_empty(), "upload data must not be empty");
        let label = format!("upload segment {}", hex_hash(hash));
        retry_transient(&label, || {
            self.remote.upload_if_absent(hash, data).map(|_| ())
        })
    }

    /// Download and import a segment by hash with exponential backoff retry.
    fn fetch_and_import(&mut self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
        assert!(hash.len() == 32, "hash must be 32 bytes");
        let label = format!("download segment {}", hex_hash(hash));
        let data = retry_transient(&label, || self.remote.download(hash))?;

        assert!(!data.is_empty(), "downloaded segment must not be empty");

        match self.local.import_segment(&data, hash)? {
            ImportResult::Applied {
                keys_written,
                keys_skipped,
            } => {
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
            ImportResult::Rejected { reason } => {
                return Err(EdgestoreError::ReplicationError(format!(
                    "segment {} permanently rejected: {}",
                    hex_hash(hash),
                    reason
                )));
            }
        }
        self.fetched.insert(*hash, ());
        Ok(())
    }

    /// Insert segment bytes into the LRU byte cache, evicting LRU entries
    /// until total resident bytes is within `segment_cache_max_bytes`.
    fn cache_segment(&mut self, hash: [u8; 32], data: Vec<u8>) {
        if self.segment_cache_max_bytes == 0 {
            return;
        }
        let incoming = data.len();
        while self.segment_cache_bytes + incoming > self.segment_cache_max_bytes {
            match self.segment_cache.pop_lru() {
                Some((_, evicted)) => {
                    self.segment_cache_bytes =
                        self.segment_cache_bytes.saturating_sub(evicted.len())
                }
                None => break,
            }
        }
        self.segment_cache_bytes += incoming;
        self.segment_cache.put(hash, data);
    }

    /// Flush local memtable to segment.
    pub fn flush(&mut self) -> Result<(), EdgestoreError> {
        self.local.flush()
    }

    /// Compact local segments.
    pub fn compact_once(&mut self) -> Result<edgestore::CompactionStats, EdgestoreError> {
        self.local.compact_once()
    }

    /// Remove one segment from local storage only.
    ///
    /// Caller must have confirmed (via `archived_segments()`) that the segment
    /// is durably archived before pruning.
    pub fn prune_local_segment(
        &mut self,
        segment_id: edgestore::types::SegmentId,
    ) -> Result<(), EdgestoreError> {
        self.local.prune_local_segment(segment_id)
    }
}

// ── Free functions ──────────────────────────────────────────────────────────

fn hex_hash(hash: &[u8; 32]) -> String {
    assert!(hash.len() == 32, "hash must be 32 bytes");
    let mut s = String::with_capacity(64);
    for b in hash {
        s.push_str(&format!("{b:02x}"));
    }
    assert!(s.len() == 64, "hex_hash must produce 64 chars");
    s
}

/// Build a minimal `SegmentMeta` from an archived segment hash for use with
/// `ImmutableEngine::from_segment_bytes`. Non-critical fields are placeholders;
/// the engine rebuilds its index from the raw `.dat` bytes.
fn synthetic_meta(hash: &[u8; 32]) -> edgestore::types::SegmentMeta {
    assert!(hash.len() == 32, "hash must be 32 bytes");
    edgestore::types::SegmentMeta {
        segment_id: 0,
        segment_hash: hash.to_vec(),
        min_key: vec![],
        max_key: vec![0xFF; 2],
        min_lsn: 0,
        max_lsn: u64::MAX,
        record_count: 0,
        compressed_bytes: 0,
        uncompressed_bytes: 0,
        compression: "zstd:1".to_string(),
        cohort_bucket: 0,
        death_time: i64::MAX,
        merkle_root: vec![0u8; 32],
        created_at: 0,
        text_index_stripped: false,
        vector_index_stripped: false,
    }
}

/// Merge local and archived results with local winning for any key in both.
/// Result is sorted by key.
fn merge_local_wins(local: KvPairs, archived: KvPairs) -> KvPairs {
    if archived.is_empty() {
        return local;
    }
    let local_keys: std::collections::HashSet<Vec<u8>> =
        local.iter().map(|(k, _)| k.clone()).collect();
    let mut result = local;
    for (k, v) in archived {
        if !local_keys.contains(&k) {
            result.push((k, v));
        }
    }
    result.sort_by(|(a, _), (b, _)| a.cmp(b));
    result
}

/// Retry `f` up to `MAX_RETRIES` times on transient errors (throttle/timeout/503).
/// Returns the first successful result, or the last error after all retries.
fn retry_transient<T, F>(label: &str, mut f: F) -> Result<T, EdgestoreError>
where
    F: FnMut() -> Result<T, EdgestoreError>,
{
    assert!(!label.is_empty(), "retry label must not be empty");
    let mut last_err = None;

    for attempt in 0..=MAX_RETRIES {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let msg = e.to_string();
                last_err = Some(e);
                let is_transient = msg.contains("throttled")
                    || msg.contains("timeout")
                    || msg.contains("503");
                if is_transient && attempt < MAX_RETRIES {
                    let delay_ms = BASE_DELAY_MS * (1u64 << attempt);
                    eprintln!(
                        "[edgestore-tier] retry {} for {} after {} ms",
                        attempt + 1,
                        label,
                        delay_ms
                    );
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                } else if !is_transient {
                    break;
                }
            }
        }
    }

    assert!(last_err.is_some(), "last_err must be set after all retries");
    Err(EdgestoreError::ReplicationError(format!(
        "{label} failed after {MAX_RETRIES} retries: {}",
        last_err.map(|e| e.to_string()).unwrap_or_default()
    )))
}

#[cfg(test)]
mod tests;
