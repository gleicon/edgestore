use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(feature = "tier")]
mod tiered;
#[cfg(feature = "tier")]
pub use tiered::AsyncTieredEngine;

use edgestore::{
    types::SegmentMeta,
    vector::distance::Metric,
    vector::search::VectorSearchResult,
    vector::types::{Dtype, VectorRecord},
    BudgetedScan, CompactionStats, EdgestoreConfig, EdgestoreError, Engine,
    ImportResult, MetricsSnapshot, QueryStats, ScanBudget, SegmentRef, VectorEngine,
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

    async fn run_read<F, T>(&self, f: F) -> Result<T, EdgestoreError>
    where
        F: FnOnce(&Engine) -> Result<T, EdgestoreError> + Send + 'static,
        T: Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || f(&inner.blocking_read()))
            .await
            .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking: {}", e))))?
    }

    async fn run_write<F, T>(&self, f: F) -> Result<T, EdgestoreError>
    where
        F: FnOnce(&mut Engine) -> Result<T, EdgestoreError> + Send + 'static,
        T: Send + 'static,
    {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || f(&mut inner.blocking_write()))
            .await
            .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking: {}", e))))?
    }

    /// Lightweight read — acquires read lock and returns immediately.
    pub async fn get(&self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        self.run_read(move |e| e.get(&ns, &key)).await
    }

    /// Lightweight write — acquires write lock and returns immediately.
    pub async fn put(&self, ns: &[u8], key: &[u8], val: &[u8]) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        let val = val.to_vec();
        self.run_write(move |e| e.put(&ns, &key, &val)).await
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
        self.run_write(move |e| e.put_with_ttl(&ns, &key, &val, ttl_secs)).await
    }

    /// Lightweight delete.
    pub async fn delete(&self, ns: &[u8], key: &[u8]) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        self.run_write(move |e| e.delete(&ns, &key)).await
    }

    /// Heavy prefix scan — runs on spawn_blocking.
    pub async fn prefix(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let ns = ns.to_vec();
        let prefix = prefix.to_vec();
        self.run_read(move |e| e.prefix(&ns, &prefix)).await
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
        self.run_read(move |e| e.range_page(&ns, &start, &end, cursor.as_deref(), page_size)).await
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
        self.run_read(move |e| e.range_rev_page(&ns, &start, &end, cursor.as_deref(), page_size)).await
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
        self.run_write(move |e| e.vector_put(&ns, &key, dims, dtype, &data)).await
    }

    /// Vector get — lightweight read.
    pub async fn vector_get(
        &self,
        ns: &[u8],
        key: &[u8],
    ) -> Result<Option<VectorRecord>, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        self.run_read(move |e| e.vector_get(&ns, &key)).await
    }

    /// Vector delete — lightweight write.
    pub async fn vector_delete(&self, ns: &[u8], key: &[u8]) -> Result<u64, EdgestoreError> {
        let ns = ns.to_vec();
        let key = key.to_vec();
        self.run_write(move |e| e.vector_delete(&ns, &key)).await
    }

    /// Vector search — HNSW fast path or flat scan. All algorithm lives in `Engine`.
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
        let ns = ns.to_vec();
        let query = query.clone();
        self.run_read(move |e| e.vector_search(&ns, &query, k, metric)).await
    }

    /// Build vector index — heavy operation, runs on spawn_blocking.
    pub async fn build_vector_index(&self, ns: &[u8]) -> Result<(), EdgestoreError> {
        let ns = ns.to_vec();
        self.run_write(move |e| e.build_vector_index(&ns)).await
    }

    /// Preload vector index — heavy operation, runs on spawn_blocking.
    pub async fn preload_vector_index(&self, ns: &[u8]) -> Result<bool, EdgestoreError> {
        let ns = ns.to_vec();
        self.run_read(move |e| e.preload_vector_index(&ns)).await
    }

    /// Flush WAL.
    pub async fn flush(&self) -> Result<(), EdgestoreError> {
        self.run_write(|e| e.flush()).await
    }

    /// Flush the current memtable to a new immutable segment file.
    ///
    /// Uses a 3-phase protocol to release the write lock during disk I/O:
    /// - Phase 1 (write lock): snapshot entries, swap in fresh memtable.
    /// - Phase 2 (no lock): write segment files to disk.
    /// - Phase 3 (write lock): register segment, retire old WAL files.
    pub async fn flush_to_segments(&self) -> Result<SegmentMeta, EdgestoreError> {
        // Phase 1: snapshot memtable entries under write lock.
        // The active memtable is NOT cleared — reads remain fully served during Phase 2.
        let inner = self.inner.clone();
        let work = tokio::task::spawn_blocking(move || {
            inner.blocking_write().begin_flush().ok_or_else(|| {
                EdgestoreError::SegmentCorrupt("memtable is empty".to_string())
            })
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking: {}", e))))??;

        let flushed_max_lsn = work.flushed_max_lsn;

        // Phase 2: disk I/O — write lock is free, reads and writes proceed normally.
        let meta = tokio::task::spawn_blocking(move || work.execute())
            .await
            .map_err(|e| {
                EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking: {}", e)))
            })??;

        // Phase 3: register segment, drop flushed memtable entries, retire WAL under write lock.
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner
                .blocking_write()
                .finalize_flush(meta.clone(), flushed_max_lsn)
                .map(|()| meta)
        })
        .await
        .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking: {}", e))))?
    }

    /// Local segment manifest (hash + id per segment) — used by replication/backup callers.
    pub async fn export_manifest(&self) -> Result<Vec<SegmentRef>, EdgestoreError> {
        self.run_read(|e| e.export_manifest()).await
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
        self.run_write(move |e| e.import_segment(&data, &expected_hash)).await
    }

    /// Open an engine in read-only mode.
    pub async fn open_readonly(config: EdgestoreConfig) -> Result<Self, EdgestoreError> {
        let engine = tokio::task::spawn_blocking(move || Engine::open_readonly(config))
            .await
            .map_err(|e| EdgestoreError::Io(std::io::Error::other(format!("spawn_blocking failed: {}", e))))??;
        Ok(AsyncEngine { inner: Arc::new(RwLock::new(engine)) })
    }

    /// Highest LSN durably flushed to a segment.
    pub async fn confirmed_lsn(&self) -> u64 {
        self.run_read(|e| Ok(e.confirmed_lsn())).await.unwrap_or(0)
    }

    /// Current LSN counter — the LSN that will be assigned to the next write.
    pub async fn current_lsn(&self) -> u64 {
        self.run_read(|e| Ok(e.current_lsn())).await.unwrap_or(0)
    }

    /// Number of WAL segment files on disk.
    pub async fn wal_segment_count(&self) -> usize {
        self.run_read(|e| Ok(e.wal_segment_count())).await.unwrap_or(0)
    }

    /// Bytes written to the current active WAL file.
    pub async fn pending_wal_bytes(&self) -> u64 {
        self.run_read(|e| Ok(e.pending_wal_bytes())).await.unwrap_or(0)
    }

    /// Current write token (fencing value for replication).
    pub async fn write_token(&self) -> u64 {
        self.run_read(|e| Ok(e.write_token())).await.unwrap_or(0)
    }

    /// Persist a new write token.
    pub async fn set_write_token(&self, token: u64) -> Result<(), EdgestoreError> {
        self.run_write(move |e| e.set_write_token(token)).await
    }

    /// Full range scan `[start, end)`.
    pub async fn range(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let ns = ns.to_vec();
        let start = start.to_vec();
        let end = end.to_vec();
        self.run_read(move |e| e.range(&ns, &start, &end)).await
    }

    /// Range scan with cost accounting.
    pub async fn range_with_stats(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, QueryStats), EdgestoreError> {
        let ns = ns.to_vec();
        let start = start.to_vec();
        let end = end.to_vec();
        self.run_read(move |e| e.range_with_stats(&ns, &start, &end)).await
    }

    /// Range scan bounded by a [`ScanBudget`].
    pub async fn range_budgeted(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
        budget: ScanBudget,
    ) -> Result<BudgetedScan<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let ns = ns.to_vec();
        let start = start.to_vec();
        let end = end.to_vec();
        self.run_read(move |e| e.range_budgeted(&ns, &start, &end, &budget)).await
    }

    /// Prefix scan with cost accounting.
    pub async fn prefix_with_stats(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, QueryStats), EdgestoreError> {
        let ns = ns.to_vec();
        let prefix = prefix.to_vec();
        self.run_read(move |e| e.prefix_with_stats(&ns, &prefix)).await
    }

    /// Prefix scan bounded by a [`ScanBudget`].
    pub async fn prefix_budgeted(
        &self,
        ns: &[u8],
        prefix: &[u8],
        budget: ScanBudget,
    ) -> Result<BudgetedScan<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let ns = ns.to_vec();
        let prefix = prefix.to_vec();
        self.run_read(move |e| e.prefix_budgeted(&ns, &prefix, &budget)).await
    }

    /// Run one compaction pass.
    pub async fn compact_once(&self) -> Result<CompactionStats, EdgestoreError> {
        self.run_write(|e| e.compact_once()).await
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

    #[tokio::test]
    async fn test_async_vector_search_concurrent_no_write_lock() {
        use edgestore::vector::distance::Metric;
        use edgestore::vector::types::{Dtype, VectorRecord};
        use edgestore::VectorEngine;

        let dir = tempfile::TempDir::new().unwrap();
        let engine = AsyncEngine::open(EdgestoreConfig::new(dir.path())).await.unwrap();

        // Write vectors and build HNSW index
        for i in 0u8..16 {
            let data = vec![i; 16 * 4];
            engine.vector_put(b"ns", &[i], 16, Dtype::F32, &data).await.unwrap();
        }
        {
            let mut e = engine.inner.write().await;
            e.flush_to_segments().unwrap();
            e.build_vector_index(b"ns").unwrap();
        }

        let query = VectorRecord {
            dims: 16,
            dtype: Dtype::F32,
            data: vec![0u8; 16 * 4],
        };

        // Launch 8 concurrent vector searches — all run under read lock
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let q = query.clone();
            let e = engine.clone();
            tasks.push(tokio::spawn(async move {
                let results = e.vector_search(b"ns", &q, 4, Metric::L2).await.unwrap();
                assert_eq!(results.len(), 4);
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
    }

    /// Verifies that the 3-phase flush releases the write lock during disk I/O.
    ///
    /// If Phase 2 still held the write lock, the reader tasks below would deadlock
    /// after Phase 1 swaps the memtable and before Phase 3 registers the segment.
    /// `tokio::time::timeout` catches the deadlock within the test.
    #[tokio::test]
    async fn test_flush_releases_lock_during_io() {
        use std::time::Duration;

        let dir = TempDir::new().unwrap();
        let engine = open_async_engine(&dir).await;

        for i in 0u32..200 {
            engine
                .put(b"ns", format!("k{:06}", i).as_bytes(), b"val")
                .await
                .unwrap();
        }

        // Spawn 16 readers that each do 20 gets. If flush holds the write lock
        // during disk I/O, these will deadlock (Tokio RwLock is fair by default).
        let mut reader_handles = vec![];
        for t in 0..16u32 {
            let e = engine.clone();
            reader_handles.push(tokio::spawn(async move {
                for i in 0u32..20 {
                    let key = format!("k{:06}", (t * 20 + i) % 200);
                    let got = e.get(b"ns", key.as_bytes()).await.unwrap();
                    assert!(got.is_some(), "key {key} must be readable during flush");
                }
            }));
        }

        // Flush concurrently with the readers. Must complete within 10 s even
        // under heavy read pressure (disk I/O with lock released ≈ <50 ms).
        let flush_handle = {
            let e = engine.clone();
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(10), e.flush_to_segments())
                    .await
                    .expect("flush must not deadlock")
                    .unwrap()
            })
        };

        let meta = flush_handle.await.unwrap();
        assert_eq!(meta.segment_hash.len(), 32, "flush must produce a valid segment");

        for h in reader_handles {
            tokio::time::timeout(Duration::from_secs(10), h)
                .await
                .expect("reader must not deadlock")
                .unwrap();
        }
    }

    /// Verifies correctness under concurrent writers, readers, and a flush.
    #[tokio::test]
    async fn test_concurrent_writes_reads_and_flush() {
        use std::sync::Arc;
        use std::time::Duration;

        let dir = TempDir::new().unwrap();
        let engine = Arc::new(open_async_engine(&dir).await);

        // Seed 50 keys so reads have something to find.
        for i in 0u32..50 {
            engine
                .put(b"w", format!("seed{:04}", i).as_bytes(), b"v0")
                .await
                .unwrap();
        }

        let mut handles = vec![];

        // 8 writer tasks
        for t in 0u32..8 {
            let e = engine.clone();
            handles.push(tokio::spawn(async move {
                for i in 0u32..25 {
                    let k = format!("w{:03}{:04}", t, i);
                    e.put(b"w", k.as_bytes(), b"vw").await.unwrap();
                }
            }));
        }

        // 8 reader tasks
        for t in 0u32..8 {
            let e = engine.clone();
            handles.push(tokio::spawn(async move {
                for i in 0u32..25 {
                    let k = format!("seed{:04}", (t * 25 + i) % 50);
                    let _ = e.get(b"w", k.as_bytes()).await.unwrap();
                }
            }));
        }

        // 1 flush task
        let flush_e = engine.clone();
        handles.push(tokio::spawn(async move {
            let _ = flush_e.flush_to_segments().await; // may find memtable empty if writers are slow
        }));

        for h in handles {
            tokio::time::timeout(Duration::from_secs(15), h)
                .await
                .expect("task must not deadlock")
                .unwrap();
        }

        // Seed keys must still be readable after all the chaos.
        for i in 0u32..50 {
            let k = format!("seed{:04}", i);
            let v = engine.get(b"w", k.as_bytes()).await.unwrap();
            assert!(v.is_some(), "seed key {k} must survive concurrent flush + writes");
        }
    }

    /// Verifies that all three MemTable variants produce identical results.
    #[tokio::test]
    async fn test_memtable_variants_correctness() {
        use edgestore::memtable::{BBTreeConcurrentMemTable, BBTreeMemTable, BTreeMemTable};

        fn run_ops(mut m: Box<dyn edgestore::memtable::MemTable>) {
            let k1 = b"aaa".to_vec();
            let k2 = b"bbb".to_vec();
            let k3 = b"ccc".to_vec();

            let entry = |v: &[u8]| edgestore::types::MemEntry {
                key: v.to_vec(),
                value: Some(v.to_vec()),
                op: edgestore::types::Operation::Put,
                lsn: 1,
                timestamp: 0,
                ttl: 0,
            };

            m.insert(k1.clone(), entry(b"v1"));
            m.insert(k2.clone(), entry(b"v2"));
            m.insert(k3.clone(), entry(b"v3"));

            assert_eq!(m.len(), 3, "len must be 3 after 3 distinct inserts");
            assert!(m.get(&k1).is_some(), "k1 must be present");
            assert!(m.get(&k2).is_some(), "k2 must be present");
            assert!(m.get(&k3).is_some(), "k3 must be present");
            assert!(m.get(b"zzz").is_none(), "absent key must return None");

            // Overwrite k1 — len stays 3
            m.insert(k1.clone(), entry(b"v1b"));
            assert_eq!(m.len(), 3, "len must stay 3 after overwrite");
            assert_eq!(
                m.get(&k1).unwrap().value,
                Some(b"v1b".to_vec()),
                "overwrite must update value"
            );

            let all = m.iter();
            assert_eq!(all.len(), 3, "iter must return all 3 entries");

            let prefix_result = m.prefix(b"a");
            assert_eq!(prefix_result.len(), 1, "prefix 'a' must return 1 entry");

            m.clear();
            assert!(m.is_empty(), "memtable must be empty after clear");
            assert_eq!(m.len(), 0, "len must be 0 after clear");
        }

        run_ops(Box::new(BTreeMemTable::new()));
        run_ops(Box::new(BBTreeMemTable::new()));
        run_ops(Box::new(BBTreeConcurrentMemTable::new()));
    }
}
