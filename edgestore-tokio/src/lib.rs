use std::sync::Arc;
use tokio::sync::RwLock;

use std::collections::HashMap;

#[cfg(feature = "tier")]
mod tiered;
#[cfg(feature = "tier")]
pub use tiered::AsyncTieredEngine;

use edgestore::{
    EdgestoreConfig, EdgestoreError, Engine, FacetValue, ImportResult, MetricsSnapshot, SearchOptions, SegmentRef,
    TextEngine, TextSearchResult, VectorEngine,
    types::SegmentMeta,
    vector::distance::Metric,
    vector::search::VectorSearchResult,
    vector::types::{Dtype, VectorRecord},
};

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
            .map_err(|e| EdgestoreError::Io(std::io::Error::other(
                format!("spawn_blocking failed: {}", e),
            )))??;
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
    }

    /// Heavy prefix scan — runs on spawn_blocking.
    pub async fn prefix(&self, ns: &[u8], prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let ns = ns.to_vec();
        let prefix = prefix.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.prefix(&ns, &prefix)
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
    }

    /// Vector get — lightweight read.
    pub async fn vector_get(&self, ns: &[u8], key: &[u8]) -> Result<Option<VectorRecord>, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.vector_get(&ns, &key)
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
    }

    /// Vector search — heavy operation, runs on spawn_blocking.
    pub async fn vector_search(
        &self,
        ns: &[u8],
        query: &VectorRecord,
        k: usize,
        metric: Metric,
    ) -> Result<Vec<VectorSearchResult>, EdgestoreError> {
        let ns = ns.to_vec();
        let query = query.clone();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.vector_search(&ns, &query, k, metric)
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
    }

    /// Flush WAL.
    pub async fn flush(&self) -> Result<(), EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.flush()
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
    }

    /// Flush the current memtable to a new immutable segment file — heavy I/O, runs on spawn_blocking.
    pub async fn flush_to_segments(&self) -> Result<SegmentMeta, EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut engine = inner.blocking_write();
            engine.flush_to_segments()
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
    }

    /// Local segment manifest (hash + id per segment) — used by replication/backup callers.
    pub async fn export_manifest(&self) -> Result<Vec<SegmentRef>, EdgestoreError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.export_manifest()
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
    }

    /// BM25 search — heavy operation (scoring), runs on spawn_blocking.
    pub async fn search_text(&self, ns: &[u8], query: &str, k: usize) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        let ns = ns.to_vec();
        let query = query.to_string();
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let engine = inner.blocking_read();
            engine.search_text(&ns, &query, k)
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
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
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e),
        )))?
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use edgestore::EdgestoreConfig;
    use tempfile::TempDir;

    async fn open_async_engine(dir: &TempDir) -> AsyncEngine {
        AsyncEngine::open(EdgestoreConfig::new(dir.path())).await.unwrap()
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

        engine.put_with_ttl(b"ns", b"key", b"val", 3600).await.unwrap();
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

        engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).await.unwrap();
        engine.index_text(b"ns", b"doc2", "goodbye world", std::collections::HashMap::new()).await.unwrap();

        let results = engine.search_text(b"ns", "hello", 10).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc_id, b"doc1");
    }

    #[tokio::test]
    async fn test_async_delete_text_removes_from_search() {
        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).await.unwrap();
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
            engine.vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes).await.unwrap();
        }

        let query = VectorRecord { dims, dtype: Dtype::F32, data: vec![0.5f32.to_le_bytes(); 4].concat() };
        let results = engine.vector_search(b"ns", &query, 3, Metric::L2).await.unwrap();
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
            engine.vector_put(b"ns", &[i as u8], dims, Dtype::F32, &bytes).await.unwrap();
        }

        engine.build_vector_index(b"ns").await.unwrap();

        let query = VectorRecord { dims, dtype: Dtype::F32, data: vec![0.5f32.to_le_bytes(); 4].concat() };
        let results = engine.vector_search(b"ns", &query, 3, Metric::L2).await.unwrap();
        assert!(!results.is_empty());
    }
}
