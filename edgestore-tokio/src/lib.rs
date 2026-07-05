use std::sync::Arc;
use tokio::sync::RwLock;

use edgestore::{
    EdgestoreConfig, EdgestoreError, Engine, ImportResult, MetricsSnapshot, VectorEngine,
    vector::distance::Metric,
    vector::search::VectorSearchResult,
    vector::types::{Dtype, VectorRecord},
};

/// Async wrapper around the synchronous `edgestore::Engine`.
///
/// All heavy I/O operations (search, index build) run on `tokio::task::spawn_blocking`
/// to avoid blocking the async runtime. Lightweight operations return immediately.
///
/// Per architecture constraint: no async in the core `edgestore` crate; async belongs here.
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
