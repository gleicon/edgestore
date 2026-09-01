use std::sync::Arc;
use tokio::sync::RwLock;

use std::collections::HashMap;

#[cfg(feature = "tier")]
mod tiered;
#[cfg(feature = "tier")]
pub use tiered::AsyncTieredEngine;

use edgestore::{
    total_cmp_f32,
    types::SegmentMeta,
    vector::distance::{distance, Metric},
    vector::search::VectorSearchResult,
    vector::types::{Dtype, VectorRecord},
    EdgestoreConfig, EdgestoreError, Engine, FacetValue, ImportResult, MetricsSnapshot,
    SearchOptions, SegmentRef, TextEngine, TextSearchResult, VectorEngine,
};

pub use edgestore::RangePage;

/// Async wrapper around the synchronous `edgestore::Engine`.
///
/// All I/O runs on `tokio::task::spawn_blocking`. The core `edgestore` crate is
/// intentionally sync; this crate is the async boundary.
///
/// ## Using with axum
///
/// Store the engine in shared state and pass it through `axum::Extension` or
/// `axum::extract::State`. Writes use `&mut self` so wrap in `Arc<RwLock<>>`:
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use tokio::sync::RwLock;
/// use edgestore::EdgestoreConfig;
/// use edgestore_tokio::AsyncEngine;
///
/// async fn axum_example() {
///     let engine = AsyncEngine::open(EdgestoreConfig::new("/var/db")).await.unwrap();
///     let shared = Arc::new(engine); // AsyncEngine already wraps Arc<RwLock<Engine>>
///
///     // In a handler:
///     // let val = shared.get(b"ns", b"key").await.unwrap();
/// }
/// ```
///
/// ## Using with actix-web
///
/// Wrap in `web::Data<AsyncEngine>`. `AsyncEngine` is `Clone + Send + Sync`.
///
/// ## Sync callers inside async
///
/// If you have blocking Engine code that needs to run inside an async context
/// without this wrapper, use `tokio::task::spawn_blocking` directly:
///
/// ```rust,no_run
/// use edgestore::{EdgestoreConfig, Engine};
///
/// async fn run_sync_code() {
///     tokio::task::spawn_blocking(|| {
///         let mut engine = Engine::open(EdgestoreConfig::new("/var/db")).unwrap();
///         engine.put(b"ns", b"key", b"val").unwrap();
///     }).await.unwrap();
/// }
/// ```
#[derive(Clone)]
pub struct AsyncEngine {
    inner: Arc<RwLock<Engine>>,
}

impl AsyncEngine {
    /// Open an engine at the given path with the provided configuration.
    ///
    /// Uses `spawn_blocking` for the initial open since it involves file I/O.
    pub async fn open(config: EdgestoreConfig) -> Result<Self, EdgestoreError> {
        let engine = tokio::task::spawn_blocking(move || Engine::open(config))
            .await
            .map_err(|e| {
                EdgestoreError::Io(std::io::Error::other(format!(
                    "spawn_blocking failed: {}",
                    e
                )))
            })??;
        Ok(AsyncEngine {
            inner: Arc::new(RwLock::new(engine)),
        })
    }

    /// Lightweight read — acquires read lock and returns immediately.
    pub async fn get(&self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
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

    /// Lightweight write — acquires write lock and returns immediately.
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

    /// Lightweight write with TTL — record expires via deathtime-cohort compaction.
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

    /// Lightweight delete.
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

    /// Heavy prefix scan — runs on spawn_blocking.
    pub async fn prefix(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let ns = ns.to_vec();
        let prefix = prefix.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
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

    /// Cursor-based forward range page.
    ///
    /// Each call reads only the segments whose key range overlaps `[cursor, end)` and
    /// stops at `page_size` live items. Pass `next_key` from the result as `cursor`
    /// on the next call. `next_key = None` means the scan is exhausted.
    pub async fn range_page(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
        cursor: Option<&[u8]>,
        page_size: usize,
    ) -> Result<RangePage, EdgestoreError> {
        let ns = ns.to_vec();
        let start = start.to_vec();
        let end = end.to_vec();
        let cursor = cursor.map(|c| c.to_vec());
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.range_page(&ns, &start, &end, cursor.as_deref(), page_size)
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking: {}", e))))?
    }

    /// Cursor-based reverse range page (descending key order).
    ///
    /// Returns up to `page_size` items in descending key order. Pass `next_key` from
    /// the result as `cursor` on the next call to continue going left.
    /// `next_key = None` means the scan has reached `start`.
    pub async fn range_rev_page(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
        cursor: Option<&[u8]>,
        page_size: usize,
    ) -> Result<RangePage, EdgestoreError> {
        let ns = ns.to_vec();
        let start = start.to_vec();
        let end = end.to_vec();
        let cursor = cursor.map(|c| c.to_vec());
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.range_rev_page(&ns, &start, &end, cursor.as_deref(), page_size)
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking: {}", e))))?
    }

    /// Vector put — lightweight write.
    pub async fn vector_put(
        &self,
        ns: &[u8],
        key: &[u8],
        dims: u16,
        dtype: Dtype,
        data: &[u8],
    ) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let data = data.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.vector_put(&ns, &key, dims, dtype, &data)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Vector get — lightweight read.
    pub async fn vector_get(
        &self,
        ns: &[u8],
        key: &[u8],
    ) -> Result<Option<VectorRecord>, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.vector_get(&ns, &key)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Vector delete — lightweight write.
    pub async fn vector_delete(&self, ns: &[u8], key: &[u8]) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.vector_delete(&ns, &key)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Vector search — HNSW fast path or cooperative chunked flat scan.
    ///
    /// When an HNSW index is in memory (`vector_count` returns `Some`), a single
    /// `spawn_blocking` call handles the search; HNSW completes in <5 ms so a
    /// long blocking period is not a concern.
    ///
    /// When no HNSW index is loaded the flat scan is paged through
    /// `Engine::vector_page` (read lock only, `&self`). The engine lock is
    /// released between pages and `yield_now()` gives other async tasks
    /// scheduling opportunities. No extra dependencies required.
    pub async fn vector_search(
        &self,
        ns: &[u8],
        query: &VectorRecord,
        k: usize,
        metric: Metric,
    ) -> Result<Vec<VectorSearchResult>, EdgestoreError> {
        if k == 0 {
            return Ok(vec![]);
        }

        let ns_owned = ns.to_vec();
        let query_owned = query.clone();
        let inner = self.inner.clone();

        // Read-lock check: is an HNSW index already in memory?
        let has_hnsw = {
            let engine = self.inner.read().await;
            engine.vector_count(&ns_owned).is_some()
        };

        if has_hnsw {
            // HNSW: fast (<5 ms). Uses write lock because get_vector_index may
            // lazy-load the sidecar file on first call.
            return tokio::task::spawn_blocking(move || {
                let mut engine = inner.blocking_write();
                engine.vector_search(&ns_owned, &query_owned, k, metric)
            })
            .await
            .map_err(|e| EdgestoreError::Io(std::io::Error::other(e.to_string())))?;
        }

        // Flat scan: paged with cooperative yield between pages.
        // Each page acquires a read lock (not write), fetches PAGE_SIZE records,
        // releases the lock, then computes distances on the async thread.
        const PAGE_SIZE: usize = 512;

        let mut pairs: Vec<(f32, Vec<u8>)> = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;

        loop {
            let inner2 = inner.clone();
            let ns2 = ns_owned.clone();
            let cur = cursor.clone();

            let page = tokio::task::spawn_blocking(move || {
                let engine = inner2.blocking_read();
                engine.vector_page(&ns2, cur.as_deref(), PAGE_SIZE)
            })
            .await
            .map_err(|e| EdgestoreError::Io(std::io::Error::other(e.to_string())))??;

            let has_more = page.next_key.is_some();

            // Distance computation — pure f32 math, no lock held, fast per page
            for (key, record) in page.records {
                if record.dims != query_owned.dims || record.dtype != query_owned.dtype {
                    continue;
                }
                let dist = distance(
                    &query_owned.data,
                    &record.data,
                    query_owned.dtype,
                    metric,
                )?;
                pairs.push((dist, key));
            }

            cursor = page.next_key;
            if !has_more {
                break;
            }
            tokio::task::yield_now().await;
        }

        pairs.sort_unstable_by(|(a, _), (b, _)| total_cmp_f32(*a, *b));
        pairs.truncate(k);

        Ok(pairs
            .into_iter()
            .map(|(d, key)| VectorSearchResult { key, distance: d })
            .collect())
    }

    /// Build vector index — heavy operation, runs on spawn_blocking.
    pub async fn build_vector_index(&self, ns: &[u8]) -> Result<(), EdgestoreError> {
        let ns = ns.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.build_vector_index(&ns)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Preload vector index — heavy operation, runs on spawn_blocking.
    pub async fn preload_vector_index(&self, ns: &[u8]) -> Result<bool, EdgestoreError> {
        let ns = ns.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.preload_vector_index(&ns)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Flush WAL.
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

    /// Flush the current memtable to a new immutable segment file — heavy I/O, runs on spawn_blocking.
    pub async fn flush_to_segments(&self) -> Result<SegmentMeta, EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.flush_to_segments()
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Local segment manifest (hash + id per segment) — used by replication/backup callers.
    pub async fn export_manifest(&self) -> Result<Vec<SegmentRef>, EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.export_manifest()
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Index a document for BM25 full-text search — lightweight write.
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
            engine.index_text(&ns, &key, &text, facets)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// BM25 search — heavy operation (scoring), runs on spawn_blocking.
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
            engine.search_text(&ns, &query, k)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// BM25 search with facet filters / typo tolerance — heavy, runs on spawn_blocking.
    pub async fn search_text_with_options(
        &self,
        ns: &[u8],
        query: &str,
        options: SearchOptions,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        let ns = ns.to_vec();
        let query = query.to_string();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.search_text_with_options(&ns, &query, &options)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Remove a document from the text index — lightweight write.
    pub async fn delete_text(&self, ns: &[u8], key: &[u8]) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.delete_text(&ns, &key)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }

    /// Get metrics snapshot.
    pub async fn metrics(&self) -> MetricsSnapshot {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.metrics()
        })
        .await
        .unwrap_or_default()
    }

    /// Import a segment.
    pub async fn import_segment(
        &self,
        data: &[u8],
        expected_hash: &[u8; 32],
    ) -> Result<ImportResult, EdgestoreError> {
        let data = data.to_vec();
        let expected_hash = *expected_hash;
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.import_segment(&data, &expected_hash)
        })
        .await
        .map_err(|e| {
            EdgestoreError::Io(std::io::Error::other(format!(
                "spawn_blocking failed: {}",
                e
            )))
        })?
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use edgestore::EdgestoreConfig;
    use tempfile::TempDir;

    async fn open_async_engine(dir: &TempDir) -> AsyncEngine {
        AsyncEngine::open(EdgestoreConfig::new(dir.path()))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_async_put_get() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        engine.put(b"ns", b"hello", b"world").await.unwrap();
        let val = engine.get(b"ns", b"hello").await.unwrap();
        assert_eq!(val, Some(b"world".to_vec()));
    }

    #[tokio::test]
    async fn test_async_put_with_ttl_readable_before_expiry() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        engine
            .put_with_ttl(b"ns", b"key", b"val", 3600)
            .await
            .unwrap();
        let val = engine.get(b"ns", b"key").await.unwrap();
        assert_eq!(val, Some(b"val".to_vec()));
    }

    #[tokio::test]
    async fn test_async_flush_to_segments_and_export_manifest() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        engine.put(b"ns", b"key", b"val").await.unwrap();
        let meta = engine.flush_to_segments().await.unwrap();
        assert_eq!(meta.segment_hash.len(), 32);

        let manifest = engine.export_manifest().await.unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest[0].segment_hash.to_vec(), meta.segment_hash);
    }

    #[tokio::test]
    async fn test_async_index_and_search_text() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        engine
            .index_text(
                b"ns",
                b"doc1",
                "hello world",
                std::collections::HashMap::new(),
            )
            .await
            .unwrap();
        engine
            .index_text(
                b"ns",
                b"doc2",
                "goodbye world",
                std::collections::HashMap::new(),
            )
            .await
            .unwrap();

        let results = engine.search_text(b"ns", "hello", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc_id, b"doc1");
    }

    #[tokio::test]
    async fn test_async_delete_text_removes_from_search() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        engine
            .index_text(
                b"ns",
                b"doc1",
                "hello world",
                std::collections::HashMap::new(),
            )
            .await
            .unwrap();
        engine.delete_text(b"ns", b"doc1").await.unwrap();

        let results = engine.search_text(b"ns", "hello", 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_async_concurrent_reads() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        engine.put(b"ns", b"key", b"val").await.unwrap();

        let mut handles = vec![];
        for _ in 0..10 {
            let engine = engine.clone();
            handles.push(tokio::spawn(async move {
                let val = engine.get(b"ns", b"key").await.unwrap();
                assert_eq!(val, Some(b"val".to_vec()));
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_async_vector_search() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        let dims = 4u16;
        for i in 0..20 {
            let v = vec![i as f32 * 0.1; 4];
            let bytes = v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
            engine
                .vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes)
                .await
                .unwrap();
        }

        let query = VectorRecord {
            dims,
            dtype: Dtype::F32,
            data: vec![0.5f32.to_le_bytes(); 4].concat(),
        };
        let results = engine
            .vector_search(b"ns", &query, 3, Metric::L2)
            .await
            .unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_async_build_index() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        let dims = 4u16;
        for i in 0..20 {
            let v = vec![i as f32 * 0.1; 4];
            let bytes = v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
            engine
                .vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes)
                .await
                .unwrap();
        }

        engine.build_vector_index(b"ns").await.unwrap();

        let query = VectorRecord {
            dims,
            dtype: Dtype::F32,
            data: vec![0.5f32.to_le_bytes(); 4].concat(),
        };
        let results = engine
            .vector_search(b"ns", &query, 3, Metric::L2)
            .await
            .unwrap();
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_async_vector_search_flat_scan_chunked() {
        // Insert more vectors than PAGE_SIZE (512) to exercise multi-page flat scan.
        // No HNSW index built — forces the cooperative chunked path.
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        let dims = 4u16;
        let n = 600usize;
        for i in 0..n {
            let v = vec![(i as f32) * 0.001; 4];
            let bytes = v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
            let key = (i as u32).to_be_bytes();
            engine
                .vector_put(b"ns", &key, dims, Dtype::F32, &bytes)
                .await
                .unwrap();
        }

        // Query close to vector 300
        let target = 300.0f32 * 0.001;
        let query = VectorRecord {
            dims,
            dtype: Dtype::F32,
            data: vec![target; 4]
                .iter()
                .flat_map(|f: &f32| f.to_le_bytes())
                .collect(),
        };
        let results = engine
            .vector_search(b"ns", &query, 5, Metric::L2)
            .await
            .unwrap();

        assert_eq!(results.len(), 5);
        // Closest vector should be key 300 (distance ~0)
        assert!(
            results[0].distance < 1e-4,
            "nearest vector should be ~0 distance, got {}",
            results[0].distance
        );
        // Results sorted ascending
        for i in 1..results.len() {
            assert!(results[i - 1].distance <= results[i].distance);
        }
    }

    #[tokio::test]
    async fn test_async_range_page_forward_pagination() {
        let dir = tempfile::TempDir::new().unwrap();
        let engine = AsyncEngine::open(EdgestoreConfig::new(dir.path())).await.unwrap();

        // Write 25 keys, flush two segments, leave some in memtable
        for i in 0u32..15 {
            engine.put(b"ns", format!("k{:04}", i).as_bytes(), b"v").await.unwrap();
        }
        {
            let mut e = engine.inner.write().await;
            e.flush_to_segments().unwrap();
        }
        for i in 15u32..25 {
            engine.put(b"ns", format!("k{:04}", i).as_bytes(), b"v").await.unwrap();
        }

        let mut all: Vec<Vec<u8>> = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let page = engine.range_page(b"ns", b"", b"\xff", cursor.as_deref(), 7).await.unwrap();
            for (k, _) in &page.items {
                all.push(k.clone());
            }
            cursor = page.next_key;
            if cursor.is_none() { break; }
        }
        assert_eq!(all.len(), 25, "all 25 keys must be returned");
        for w in all.windows(2) {
            assert!(w[0] < w[1], "forward pages must be ascending");
        }
    }

    #[tokio::test]
    async fn test_async_range_rev_page_descending_pagination() {
        let dir = tempfile::TempDir::new().unwrap();
        let engine = AsyncEngine::open(EdgestoreConfig::new(dir.path())).await.unwrap();

        for i in 0u32..12 {
            engine.put(b"ns", format!("r{:04}", i).as_bytes(), b"v").await.unwrap();
        }
        {
            let mut e = engine.inner.write().await;
            e.flush_to_segments().unwrap();
        }

        let mut all: Vec<Vec<u8>> = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let page = engine.range_rev_page(b"ns", b"", b"\xff", cursor.as_deref(), 5).await.unwrap();
            for (k, _) in &page.items {
                all.push(k.clone());
            }
            cursor = page.next_key;
            if cursor.is_none() { break; }
        }
        assert_eq!(all.len(), 12, "all 12 keys must be returned in reverse pages");
        // Globally descending
        for w in all.windows(2) {
            assert!(w[0] > w[1], "reverse pages must be globally descending");
        }
        // First key must be the lexicographically largest
        assert_eq!(all[0], b"r0011".to_vec());
    }
}
