use std::sync::Arc;
use std::time::Instant;

use super::{Engine, QueryStats};
use crate::error::EdgestoreError;
use crate::types::Lsn;
use crate::vector::api::{vector_namespace, VectorEngine};
use crate::vector::distance::Metric;
use crate::vector::hnsw::HnswIndex;
use crate::vector::search::{VectorSearchResult, VectorPage};
use crate::vector::types::{decode_vector_record, encode_vector_record, Dtype, VectorRecord};

impl Engine {
    /// Build an HNSW index for all vectors in the given namespace.
    ///
    /// Scans all vector records in `__vec__{ns}`, builds the graph,
    /// serializes it to a sidecar file, and caches it in memory.
    pub fn build_vector_index(&mut self, ns: &[u8]) -> Result<(), EdgestoreError> {
        let t0 = Instant::now();
        let vec_ns = vector_namespace(ns);

        // Scan all vectors
        let all = self.prefix(&vec_ns, b"")?;
        if all.is_empty() {
            return Ok(());
        }

        // Determine dims, dtype, metric from first record
        let first_rec = decode_vector_record(&all[0].1)
            .map_err(|e| EdgestoreError::CorruptData(format!("decode vector: {}", e)))?;
        let dims = first_rec.dims;
        let dtype = first_rec.dtype;
        let metric = Metric::L2; // default; could be parameterized

        let mut index = HnswIndex::new(dims, dtype, metric).with_params(16, 100);

        for (key, val) in &all {
            // `prefix` already returns decoded raw keys (without namespace prefix)
            let rec = decode_vector_record(val)?;
            index.insert(key.clone(), rec.data)?;
        }

        // Write sidecar file
        let ns_slug = Self::ns_to_slug(ns);
        let vector_dir = self.config.path.join("vector");
        std::fs::create_dir_all(&vector_dir)?;
        let sidecar_path = vector_dir.join(format!("{}.hnsw", ns_slug));

        let serialized = index.serialize();
        std::fs::write(&sidecar_path, &serialized)?;

        // Persist segment-hash stamp so is_index_stale can detect staleness
        let current_hash = self.range_merkle_root()?;
        let stamp_path = sidecar_path.with_extension("stamp");
        std::fs::write(&stamp_path, current_hash)?;

        // Cache
        self.vector_indices
            .write()
            .unwrap()
            .insert(ns.to_vec(), std::sync::Arc::new(index));

        let elapsed_ms = t0.elapsed().as_millis() as u64;
        self.metrics.vector_index_load_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        if elapsed_ms > 2000 {
            eprintln!("warning: build_vector_index took {} ms (> 2s)", elapsed_ms);
        }

        Ok(())
    }

    /// Preload the HNSW index for a namespace into memory.
    ///
    /// Returns true if the index was loaded (or already cached), false if no index exists.
    pub fn preload_vector_index(&self, ns: &[u8]) -> Result<bool, EdgestoreError> {
        match self.get_vector_index(ns) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Get the HNSW index for a namespace, loading from sidecar if needed.
    ///
    /// Uses a two-phase (double-checked) lock: optimistic read lock on the cache,
    /// write lock only on a cache miss or stale entry. Returns an `Arc` so the
    /// caller can hold the index after the lock is released.
    pub(crate) fn get_vector_index(
        &self,
        ns: &[u8],
    ) -> Result<Option<Arc<HnswIndex>>, EdgestoreError> {
        // Fast read path: already cached and fresh
        {
            let indices = self.vector_indices.read().unwrap();
            if let Some(arc) = indices.get(ns) {
                if !self.is_index_stale(ns)? {
                    return Ok(Some(arc.clone()));
                }
                // Stale — fall through to write path
            } else {
                let ns_slug = Self::ns_to_slug(ns);
                let sidecar_path = self
                    .config
                    .path
                    .join("vector")
                    .join(format!("{}.hnsw", ns_slug));
                if !sidecar_path.exists() {
                    return Ok(None);
                }
                // Sidecar exists but not cached — fall through to write path
            }
        }

        // Write path: evict stale or load from disk
        let mut indices = self.vector_indices.write().unwrap();

        // Double-check after acquiring write lock
        if let Some(arc) = indices.get(ns) {
            if !self.is_index_stale(ns)? {
                return Ok(Some(arc.clone()));
            }
            indices.remove(ns);
            self.metrics
                .vector_index_stales
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let t0 = Instant::now();
        let ns_slug = Self::ns_to_slug(ns);
        let sidecar_path = self
            .config
            .path
            .join("vector")
            .join(format!("{}.hnsw", ns_slug));

        if !sidecar_path.exists() {
            return Ok(None);
        }

        let file_bytes = std::fs::metadata(&sidecar_path)
            .map(|m| m.len())
            .unwrap_or(0);
        if file_bytes > self.config.hnsw_max_ram_bytes {
            eprintln!(
                "[edgestore] HNSW sidecar for namespace {:?} is {} MB, exceeds hnsw_max_ram_bytes ({} MB); falling back to flat scan",
                String::from_utf8_lossy(ns),
                file_bytes / (1024 * 1024),
                self.config.hnsw_max_ram_bytes / (1024 * 1024),
            );
            return Ok(None);
        }

        let bytes = std::fs::read(&sidecar_path)?;
        let index = HnswIndex::deserialize(&bytes)?;

        if self.is_index_stale(ns)? {
            self.metrics
                .vector_index_stales
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(None);
        }

        let arc = std::sync::Arc::new(index);
        indices.insert(ns.to_vec(), arc.clone());
        self.metrics
            .vector_index_loads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.vector_index_load_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        Ok(Some(arc))
    }

    /// Try an HNSW search, returning `None` if no fresh index is available.
    ///
    /// Used by `edgestore-tokio` to attempt the fast HNSW path under a read lock
    /// before falling back to the paged flat scan. The interior `RwLock` on the
    /// index cache handles lazy loading without needing the outer engine write lock.
    pub fn try_hnsw_search(
        &self,
        ns: &[u8],
        query: &VectorRecord,
        k: usize,
    ) -> Result<Option<Vec<VectorSearchResult>>, EdgestoreError> {
        if let Some(index) = self.get_vector_index(ns)? {
            if index.dtype == query.dtype && index.dims == query.dims {
                let hnsw_results = index.search(&query.data, k, 50)?;
                return Ok(Some(
                    hnsw_results
                        .into_iter()
                        .map(|(key, distance)| VectorSearchResult { key, distance })
                        .collect(),
                ));
            }
        }
        Ok(None)
    }

    /// Check if the cached HNSW index is stale by comparing segment hashes.
    fn is_index_stale(&self, ns: &[u8]) -> Result<bool, EdgestoreError> {
        let sidecar_path = self
            .config
            .path
            .join("vector")
            .join(format!("{}.hnsw", Self::ns_to_slug(ns)));
        if !sidecar_path.exists() {
            return Ok(true);
        }
        let stamp_path = sidecar_path.with_extension("stamp");
        let Ok(stamp) = std::fs::read(&stamp_path) else {
            return Ok(true);
        };
        let current = self.range_merkle_root()?;
        Ok(stamp != current)
    }

    /// Search for the k closest vectors to the query in the given namespace.
    ///
    /// Uses HNSW when an index exists and is fresh; falls back to flat scan otherwise.
    pub fn vector_search(
        &self,
        ns: &[u8],
        query: &VectorRecord,
        k: usize,
        metric: Metric,
    ) -> Result<Vec<VectorSearchResult>, EdgestoreError> {
        // Try HNSW path
        if let Some(index) = self.get_vector_index(ns)? {
            if index.dtype == query.dtype && index.dims == query.dims {
                let hnsw_results = index.search(&query.data, k, 50)?;
                return Ok(hnsw_results
                    .into_iter()
                    .map(|(key, distance)| VectorSearchResult { key, distance })
                    .collect());
            }
        }

        // Fall back to flat scan
        crate::vector::search::vector_search(self, ns, query, k, metric)
    }

    /// Vector search with cost accounting. Returns results + [`QueryStats`].
    ///
    /// `bytes_scanned` reflects the sum of encoded vector record sizes examined during
    /// a flat scan. For HNSW paths, only the result set bytes are counted (graph
    /// traversal does not materialize all vectors).
    pub fn vector_search_with_stats(
        &self,
        ns: &[u8],
        query: &VectorRecord,
        k: usize,
        metric: Metric,
    ) -> Result<(Vec<VectorSearchResult>, QueryStats), EdgestoreError> {
        // HNSW path — stats are approximate (graph traversal not fully instrumented).
        if let Some(index) = self.get_vector_index(ns)? {
            if index.dtype == query.dtype && index.dims == query.dims {
                let hnsw_results = index.search(&query.data, k, 50)?;
                let results: Vec<VectorSearchResult> = hnsw_results
                    .into_iter()
                    .map(|(key, distance)| VectorSearchResult { key, distance })
                    .collect();
                let bytes: u64 = results
                    .iter()
                    .map(|r| r.key.len() as u64 + query.data.len() as u64)
                    .sum();
                let stats = QueryStats {
                    segments_scanned: 0,
                    bytes_scanned: bytes,
                    items_examined: results.len() as u64,
                };
                return Ok((results, stats));
            }
        }

        let vec_ns = vector_namespace(ns);
        let all = self.prefix(&vec_ns, b"")?;
        let items_examined = all.len() as u64;
        let bytes_scanned: u64 = all
            .iter()
            .map(|(k, v)| k.len() as u64 + v.len() as u64)
            .sum();
        let results = crate::vector::search::vector_search(self, ns, query, k, metric)?;
        let stats = QueryStats {
            segments_scanned: 1,
            bytes_scanned,
            items_examined,
        };
        Ok((results, stats))
    }

    /// Fetch one page of decoded vector records for cooperative async flat scans.
    ///
    /// Designed for async callers (e.g. `edgestore-tokio`) that want to iterate
    /// through a vector namespace without holding the engine lock for the full
    /// flat-scan duration. Takes `&self` (read lock only) — no HNSW mutation.
    ///
    /// Pass `None` as `cursor` to start from the beginning. On each call the
    /// returned `next_key` (if `Some`) is the cursor for the next page.
    pub fn vector_page(
        &self,
        ns: &[u8],
        cursor: Option<&[u8]>,
        page_size: usize,
    ) -> Result<VectorPage, EdgestoreError> {
        crate::vector::search::vector_page(self, ns, cursor, page_size)
    }
}

impl VectorEngine for Engine {
    fn vector_put(
        &mut self,
        ns: &[u8],
        key: &[u8],
        dims: u16,
        dtype: Dtype,
        data: &[u8],
    ) -> Result<Lsn, EdgestoreError> {
        let expected = dims as usize * dtype.element_size();
        if data.len() != expected {
            return Err(EdgestoreError::DimensionMismatch {
                expected,
                actual: data.len(),
            });
        }

        let record = VectorRecord {
            dims,
            dtype,
            data: data.to_vec(),
        };
        let encoded = encode_vector_record(&record)?;
        self.put(&vector_namespace(ns), key, &encoded)
    }

    fn vector_get(&self, ns: &[u8], key: &[u8]) -> Result<Option<VectorRecord>, EdgestoreError> {
        match self.get(&vector_namespace(ns), key)? {
            Some(bytes) => {
                let record = decode_vector_record(&bytes)
                    .map_err(|e| EdgestoreError::CorruptData(format!("decode vector: {}", e)))?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    fn vector_delete(&mut self, ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError> {
        self.delete(&vector_namespace(ns), key)
    }
}
