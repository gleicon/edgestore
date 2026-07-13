use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use edgestore::{
    EdgestoreConfig, EdgestoreError, Engine, FacetValue, RemoteStore, TextEngine, TextSearchResult,
};
use edgestore_tier::{ArchivedSegment, TieredEngine};

/// Async wrapper around the synchronous `edgestore_tier::TieredEngine` — same
/// pattern as `AsyncEngine`, all blocking work on `spawn_blocking`.
///
/// `get` reads through to `RemoteStore` on a local miss, permanently importing the
/// matching segment. `range`/`prefix` merge local results with archived segments
/// ephemerally (no import, no disk growth); see `TieredEngine` for full semantics.
#[derive(Clone)]
pub struct AsyncTieredEngine {
    inner: Arc<RwLock<TieredEngine>>,
    /// Fires (via `notify_one`) every time the local engine flushes a segment —
    /// explicit (`flush_to_segments`) or `Engine`'s own auto-flush-on-`put` once
    /// `memtable_max_bytes` is exceeded. Always wired at construction (cheap: a
    /// `Notify` with no waiters costs nothing when notified) so callers can react
    /// to a flush immediately via `flush_notify()` instead of only on their own
    /// polling interval.
    flush_notify: Arc<tokio::sync::Notify>,
}

impl AsyncTieredEngine {
    /// Opens the local engine and wraps it with the given `RemoteStore` backend.
    /// Sidecar upload and text-index stripping are both off — use
    /// `open_with_options` to enable either.
    pub async fn open(
        config: EdgestoreConfig,
        remote: Box<dyn RemoteStore>,
    ) -> Result<Self, EdgestoreError> {
        Self::open_with_options(config, remote, false, false).await
    }

    /// Same as `open`, with `TieredEngine`'s `with_sidecars`/`with_text_stripping`
    /// builder options exposed for async callers.
    pub async fn open_with_options(
        config: EdgestoreConfig,
        remote: Box<dyn RemoteStore>,
        with_sidecars: bool,
        with_text_stripping: bool,
    ) -> Result<Self, EdgestoreError> {
        let flush_notify = Arc::new(tokio::sync::Notify::new());
        let notify_for_callback = flush_notify.clone();
        let engine =
            tokio::task::spawn_blocking(move || -> Result<TieredEngine, EdgestoreError> {
                let local = Engine::open(config)?.with_on_segment_flushed(move |_meta| {
                    notify_for_callback.notify_one();
                });
                Ok(TieredEngine::new(local, remote)
                    .with_sidecars(with_sidecars)
                    .with_text_stripping(with_text_stripping))
            })
            .await
            .map_err(|e| {
                EdgestoreError::Io(std::io::Error::other(format!(
                    "spawn_blocking failed: {}",
                    e
                )))
            })??;

        Ok(AsyncTieredEngine {
            inner: Arc::new(RwLock::new(engine)),
            flush_notify,
        })
    }

    /// A handle that resolves the instant any segment is flushed locally (explicit
    /// or auto-triggered). `notify_one` semantics: if nothing is currently awaiting
    /// it, the next `.notified().await` call resolves immediately instead of
    /// missing the notification — safe for a caller that isn't always waiting.
    /// Race this against a polling interval to react to flushes without waiting up
    /// to the full interval (e.g. archiving a newly-flushed segment immediately).
    pub fn flush_notify(&self) -> Arc<tokio::sync::Notify> {
        self.flush_notify.clone()
    }

    pub async fn get(&self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let inner = self.inner.clone();

        // Fast path: under a read lock, check both the local store and whether any
        // archived segment could even contain the key. If local hit → done (1 spawn).
        // If local miss AND no archived segment contains the key → return None (1 spawn).
        // Only escalate to the write lock when an archived segment might have the key.
        let (inner2, ns2, key2) = (inner.clone(), ns.clone(), key.clone());
        let fast = tokio::task::spawn_blocking(move || {
            let engine = inner2.blocking_read();
            let needs_archive = engine.get_needs_archived_fetch(&ns2, &key2);
            let local_val = engine.local_only_get(&ns2, &key2)?;
            Ok::<_, EdgestoreError>((local_val, needs_archive))
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })??;

        let (local_val, needs_archive) = fast;
        if local_val.is_some() || !needs_archive {
            return Ok(local_val);
        }

        // Slow path: an archived segment may contain the key — escalate to the
        // write lock and the full `get()`, which handles fetch/import.
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.get(&ns, &key)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    pub async fn put(&self, ns: &[u8], key: &[u8], val: &[u8]) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let val = val.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.put(&ns, &key, &val)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    pub async fn put_with_ttl(
        &self,
        ns: &[u8],
        key: &[u8],
        val: &[u8],
        ttl_secs: u32,
    ) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let val = val.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.put_with_ttl(&ns, &key, &val, ttl_secs)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    pub async fn delete(&self, ns: &[u8], key: &[u8]) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.delete(&ns, &key)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    pub async fn range(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let ns = ns.to_vec();
        let start = start.to_vec();
        let end = end.to_vec();
        let inner = self.inner.clone();

        // Fast path: under a read lock, check whether any archived segment would
        // actually need fetching for this range; if not, the local-only scan is
        // already the complete answer — the common case (query within the local
        // retention window, nothing archived overlaps it yet).
        let (inner2, ns2, start2, end2) = (inner.clone(), ns.clone(), start.clone(), end.clone());
        let fast = tokio::task::spawn_blocking(move || {
            let engine = inner2.blocking_read();
            if engine.range_needs_archived_fetch(&ns2, &start2, &end2) {
                None
            } else {
                Some(engine.local_only_range(&ns2, &start2, &end2))
            }
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?;
        if let Some(result) = fast {
            return result;
        }

        // Slow path: an archived segment needs fetching/caching — escalate to
        // the write lock and the full `range()`.
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.range(&ns, &start, &end)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    pub async fn prefix(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let ns = ns.to_vec();
        let prefix = prefix.to_vec();
        let inner = self.inner.clone();

        // Same fast/slow split as range() — see there for the reasoning.
        let (inner2, ns2, prefix2) = (inner.clone(), ns.clone(), prefix.clone());
        let fast = tokio::task::spawn_blocking(move || {
            let engine = inner2.blocking_read();
            if engine.prefix_needs_archived_fetch(&ns2, &prefix2) {
                None
            } else {
                Some(engine.local_only_prefix(&ns2, &prefix2))
            }
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?;
        if let Some(result) = fast {
            return result;
        }

        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.prefix(&ns, &prefix)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    pub async fn flush(&self) -> Result<(), EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.flush()
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Runs one deathtime-cohort compaction pass — heavy I/O, runs on spawn_blocking.
    pub async fn compact_once(&self) -> Result<edgestore::CompactionStats, EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.compact_once()
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Removes one segment from local storage only (files + manifest entry) —
    /// does not touch the remote archive. See `TieredEngine::prune_local_segment`.
    pub async fn prune_local_segment(
        &self,
        segment_id: edgestore::types::SegmentId,
    ) -> Result<(), EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.prune_local_segment(segment_id)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Flushes the local memtable to a new immutable segment file (hot→warm).
    pub async fn flush_to_segments(&self) -> Result<edgestore::types::SegmentMeta, EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.local_mut().flush_to_segments()
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Lists local segment metadata (id, hash, key bounds) — the input `archive_segments` needs.
    pub async fn list_segment_metas(&self) -> Vec<edgestore::types::SegmentMeta> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.local().list_segment_metas()
        })
        .await
        .unwrap_or_default()
    }

    /// BM25 index — reaches through to the local `Engine` directly (`TieredEngine`
    /// doesn't wrap `TextEngine` itself); text data is not tiered,
    /// so this needs no read-through behavior.
    pub async fn index_text(
        &self,
        ns: &[u8],
        key: &[u8],
        text: &str,
        facets: HashMap<String, FacetValue>,
    ) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let text = text.to_string();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.local_mut().index_text(&ns, &key, &text, facets)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// BM25 search — reaches through to the local `Engine` directly (local-only,
    /// same as `range`/`prefix`; see struct docs).
    pub async fn search_text(
        &self,
        ns: &[u8],
        query: &str,
        k: usize,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        let ns = ns.to_vec();
        let query = query.to_string();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.local().search_text(&ns, &query, k)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Uploads the given local segments to the remote store and records them as
    /// archived. Does not delete local files — that remains the caller's decision.
    pub async fn archive_segments(
        &self,
        metas: Vec<edgestore::types::SegmentMeta>,
    ) -> Result<(), EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.archive_segments(&metas)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    pub async fn archived_segments(&self) -> Vec<ArchivedSegment> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.archived_segments()
        })
        .await
        .unwrap_or_default()
    }

    /// Fetches every archived segment into local storage — heavy, runs on spawn_blocking.
    pub async fn fetch_all_archived(&self) -> Result<(), EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.fetch_all_archived()
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Downloads+imports only the archived segments whose `[min_key, max_key]` bounds
    /// overlap the given local-namespace key range — the selective alternative to
    /// `fetch_all_archived` that range/prefix-shaped callers (e.g. time-series queries)
    /// need, since `TieredEngine` itself only does read-through for `get()`.
    pub async fn fetch_archived_overlapping(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<(), EdgestoreError> {
        let ns = ns.to_vec();
        let start = start.to_vec();
        let end = end.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.fetch_archived_overlapping(&ns, &start, &end)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Downloads a segment's raw bytes without importing it locally — for building
    /// an ephemeral, read-only `ImmutableEngine` view instead of permanently growing
    /// local storage (unlike `fetch_segment`/`fetch_all_archived`, which both import).
    pub async fn download_segment(&self, hash: [u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.download_segment(&hash)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    pub async fn register_archived(&self, segments: Vec<ArchivedSegment>) {
        let inner = self.inner.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.register_archived(segments);
        })
        .await;
    }
}
