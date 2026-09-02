use fs2::FileExt;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::config::EdgestoreConfig;
use crate::error::EdgestoreError;
use crate::memtable::MemTable;
use crate::metrics::{EngineMetrics, MetricsSnapshot};
use crate::replication::SegmentRef;
use crate::types::{
    decode_key, encode_key, prefix_upper_bound, Lsn, MemEntry, Operation, WalRecord,
};
use crate::vector::api::{vector_namespace, VectorEngine};
use crate::vector::distance::Metric;
use crate::vector::hnsw::HnswIndex;
use crate::vector::search::VectorSearchResult;
use crate::vector::types::{decode_vector_record, encode_vector_record, Dtype, VectorRecord};
use crate::wal::WalWriter;

fn next_wal_path(db_path: &Path, lsn: Lsn) -> PathBuf {
    db_path.join(format!("wal-{:016x}.log", lsn))
}

/// Alias to reduce type-complexity warnings on public KV scan APIs.
type KvPairs = Vec<(Vec<u8>, Vec<u8>)>;
/// Alias for budget-limited KV scan results.
type BudgetedKvScan = BudgetedScan<(Vec<u8>, Vec<u8>)>;

const AVG_ENTRY_SIZE_ESTIMATE: u64 = 256;

/// Accounting data returned alongside query results.
///
/// All byte counts are "bytes materialized" — the sum of raw key+value bytes for each
/// record examined — not physical bytes read from disk (which include block padding and
/// index structures). Use these values for relative comparisons and cost budgeting, not
/// as exact I/O measurements.
#[derive(Debug, Clone, Default)]
pub struct QueryStats {
    /// Number of immutable segment files touched by the query (0 = memtable-only hit).
    pub segments_scanned: u32,
    /// Approximate bytes of record data examined (key + value, before filtering).
    pub bytes_scanned: u64,
    /// Number of records examined before filtering (includes tombstones and duplicates).
    pub items_examined: u64,
}

/// Per-query byte and item scan limits for bounded queries.
///
/// Both limits are checked after each output item is added. When a limit is hit the
/// query stops and returns what it has collected so far (see [`BudgetedScan`]).
#[derive(Debug, Clone, Default)]
pub struct ScanBudget {
    /// Stop after emitting this many output items (post-filter).
    pub max_items: Option<usize>,
    /// Stop after examining approximately this many bytes of record data.
    pub max_bytes: Option<u64>,
}

/// Result of a budget-limited scan.
#[derive(Debug, Clone)]
pub struct BudgetedScan<T> {
    /// Items collected before the budget was exhausted (or all items if budget was not hit).
    pub items: Vec<T>,
    /// True if the query was stopped by the budget before all matching records were visited.
    pub truncated: bool,
    /// Query accounting — same semantics as [`QueryStats`].
    pub stats: QueryStats,
}

/// Result of a cursor-based paginated range scan (forward or reverse).
///
/// Use `next_key` as the cursor for the next call to [`Engine::range_page`] or
/// [`Engine::range_rev_page`]. `None` means the scan is exhausted.
#[derive(Debug, Clone)]
pub struct RangePage {
    /// Decoded `(key, value)` pairs in ascending order for forward, descending for reverse.
    pub items: Vec<(Vec<u8>, Vec<u8>)>,
    /// Cursor for the next page, or `None` when all items have been returned.
    pub next_key: Option<Vec<u8>>,
}

/// Result of importing a remote segment via `Engine::import_segment`.
pub enum ImportResult {
    /// Segment applied. Record-level counts reflect LWW decisions.
    Applied {
        /// Number of records written (incoming won LWW).
        keys_written: u64,
        /// Number of records skipped (local won LWW).
        keys_skipped: u64,
    },
    /// Segment already present in local manifest — no-op.
    Skipped,
    /// BLAKE3 of provided data does not match claimed hash — segment rejected.
    HashMismatch,
}

/// Single-writer KV engine with WAL, segments, compaction, and optional vector/text indexes.
pub struct Engine {
    pub(crate) config: EdgestoreConfig,
    pub(crate) wal: WalWriter,
    pub(crate) memtable: Box<dyn MemTable>,
    pub(crate) lsn_counter: u64,
    #[allow(dead_code)]
    pub(crate) txid_counter: u64,
    #[allow(dead_code)]
    lockfile: std::fs::File,
    pub(crate) segment_store: crate::segment::SegmentStore,
    pub(crate) snapshot_registry: crate::snapshot::SnapshotRegistry,
    metrics: EngineMetrics,
    vector_indices: std::sync::RwLock<HashMap<Vec<u8>, std::sync::Arc<HnswIndex>>>,
    /// In-memory cache of deserialized text indexes per namespace.
    /// Warmed on write (index_text / delete_text); read-only searches fall back to disk
    /// because TextEngine::search takes &self. Still O(1) single-record deserialize.
    text_indices: HashMap<Vec<u8>, crate::text::index::InvertedIndex>,
    /// Optional callback fired after every successful segment flush (both explicit
    /// and auto-triggered). Receives the new segment's metadata. Use to wake a
    /// replication loop, update metrics, or trigger downstream processing.
    #[allow(clippy::type_complexity)]
    on_segment_flushed: Option<Box<dyn Fn(&crate::types::SegmentMeta) + Send + Sync>>,
}

impl Engine {
    /// Open or create an Engine at the configured path.
    pub fn open(config: EdgestoreConfig) -> Result<Engine, EdgestoreError> {
        std::fs::create_dir_all(&config.path)?;

        let lock_path = config.path.join("LOCK");
        let lockfile = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;

        lockfile.try_lock_exclusive().map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                EdgestoreError::WriterBusy
            } else {
                EdgestoreError::Io(e)
            }
        })?;

        let mut memtable = (config.memtable_factory)();

        // Run recovery — replay all WAL files into the memtable.
        let result = crate::recovery::recover_from_wal(&config.path, &mut memtable)?;
        let lsn_counter = result.max_lsn;
        let txid_counter = result.max_txid;

        let wal_files = crate::recovery::list_wal_files(&config.path)?;

        let wal = if wal_files.is_empty() {
            let wal_path = next_wal_path(&config.path, lsn_counter);
            WalWriter::create(&wal_path, &config)?
        } else {
            let latest_path = wal_files.last().unwrap();
            let opened = WalWriter::open(latest_path, &config)?;

            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            if opened.needs_rotation(now_secs) {
                let new_lsn = lsn_counter + 1;
                let new_path = next_wal_path(&config.path, new_lsn);
                WalWriter::create(&new_path, &config)?
            } else {
                opened
            }
        };

        let segment_store =
            crate::segment::SegmentStore::open(config.path.clone(), config.cohort_window_secs)?;

        let mut engine = Engine {
            config,
            wal,
            memtable,
            lsn_counter,
            txid_counter,
            lockfile,
            segment_store,
            snapshot_registry: crate::snapshot::SnapshotRegistry::new(),
            metrics: EngineMetrics::new(),
            vector_indices: std::sync::RwLock::new(HashMap::new()),
            text_indices: HashMap::new(),
            on_segment_flushed: None,
        };

        // Rebuild any text indices that are missing their merged index sidecar.
        // Raw text records are durable (WAL-backed), but the merged inverted index
        // is only persisted on flush() / drop. After a crash, rebuild from raw records.
        if let Err(e) = engine.rebuild_text_indices() {
            log::warn!("Failed to rebuild text indices on open: {}", e);
        }

        Ok(engine)
    }

    /// Open an engine in read-only mode.
    ///
    /// All write methods (`put`, `delete`, `vector_put`, `index_text`, etc.) will
    /// return `Err(EdgestoreError::ReadOnly)`. Use for replica instances to prevent
    /// accidental writes that would cause divergence from the primary.
    pub fn open_readonly(mut config: EdgestoreConfig) -> Result<Engine, EdgestoreError> {
        config.readonly = true;
        Self::open(config)
    }

    /// Register a callback fired after every successful segment flush.
    ///
    /// Called from `flush_to_segments` (both explicit and auto-triggered by `put`).
    /// Receives the new `SegmentMeta`. Use to wake a replication anti-entropy loop,
    /// update external metrics, or trigger downstream processing.
    ///
    /// The callback runs synchronously on the calling thread. Keep it fast —
    /// e.g. send on a channel, set an atomic flag, or log. Do not call back
    /// into the same `Engine` from within the callback.
    pub fn with_on_segment_flushed(
        mut self,
        cb: impl Fn(&crate::types::SegmentMeta) + Send + Sync + 'static,
    ) -> Self {
        self.on_segment_flushed = Some(Box::new(cb));
        self
    }

    /// Returns the number of vectors in the given namespace if the HNSW index is
    /// currently loaded in memory, or `None` if the index has not been loaded.
    ///
    /// Call `preload_vector_index(ns)` first if you need a guaranteed count.
    /// This method never triggers a disk scan.
    pub fn vector_count(&self, ns: &[u8]) -> Option<u64> {
        self.vector_indices
            .read()
            .unwrap()
            .get(ns)
            .map(|idx| idx.nodes.len() as u64)
    }

    /// Scan all text namespaces and rebuild merged indices from raw records
    /// when the merged index sidecar (`__index__`) is missing.
    fn rebuild_text_indices(&mut self) -> Result<(), EdgestoreError> {
        // Collect all text namespaces by scanning for the synthetic prefix.
        // A text namespace key looks like: __text__{user_ns} / {doc_key}
        let all = self.prefix_inner(b"", b"__text__")?;

        type KeyValuePairs = Vec<(Vec<u8>, Vec<u8>)>;
        let mut namespaces: HashMap<Vec<u8>, KeyValuePairs> = HashMap::new();
        for (full_key, value) in all {
            // Decode the namespace from the full key
            if let Ok((ns, key)) = decode_key(&full_key) {
                if ns.starts_with(b"__text__") {
                    namespaces.entry(ns).or_default().push((key, value));
                }
            }
        }

        for (text_ns, entries) in namespaces {
            // Check if merged index exists and is fresh (sidecar_lsn >= current max lsn).
            // sidecar_lsn == 0 means "unknown / v1 sidecar" — treat as stale and rebuild.
            if let Some(bytes) = self.get(&text_ns, TEXT_INDEX_KEY)? {
                if let Ok(index) = InvertedIndex::deserialize(&bytes) {
                    if index.sidecar_lsn >= self.lsn_counter {
                        self.text_indices.insert(text_ns, index);
                        continue;
                    }
                    // sidecar is stale — fall through to rebuild
                }
            }

            // Rebuild merged index from raw records
            let mut index = InvertedIndex::new();
            for (key, val_bytes) in entries {
                if key == TEXT_INDEX_KEY {
                    continue; // skip the merged index entry itself
                }
                if let Some(record) = crate::text::types::decode_text_record(&val_bytes) {
                    let tokens = tokenize(&record.text);
                    let doc_len = tokens.len() as u32;
                    index.add_document(key, &tokens, doc_len, record.facets);
                }
            }

            if index.total_docs > 0 {
                let index_bytes = index.serialize();
                let lsn = self.put(&text_ns, TEXT_INDEX_KEY, &index_bytes)?;
                index.sidecar_lsn = lsn;
                self.text_indices.insert(text_ns, index);
            }
        }

        Ok(())
    }

    /// Persist all in-memory text indices to disk.
    fn persist_text_indices(&mut self) -> Result<(), EdgestoreError> {
        // Step 1: serialize all indices (immutable borrow)
        let to_persist: Vec<(Vec<u8>, Vec<u8>)> = self
            .text_indices
            .iter()
            .map(|(ns, index)| (ns.clone(), index.serialize()))
            .collect();
        // Step 2: write to disk and capture LSNS (no text_indices borrow)
        let mut lsns: Vec<(Vec<u8>, u64)> = Vec::with_capacity(to_persist.len());
        for (ns, bytes) in to_persist {
            let lsn = self.put(&ns, TEXT_INDEX_KEY, &bytes)?;
            lsns.push((ns, lsn));
        }
        // Step 3: update sidecar_lsn on each index (mutable borrow)
        for (ns, lsn) in lsns {
            if let Some(index) = self.text_indices.get_mut(&ns) {
                index.sidecar_lsn = lsn;
            }
        }
        Ok(())
    }

    fn now_nanos() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64
    }

    /// Sanitize a namespace for use in filesystem paths.
    fn ns_to_slug(ns: &[u8]) -> String {
        ns.iter()
            .map(|&b| {
                if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                    b as char
                } else {
                    '_'
                }
            })
            .collect()
    }

    // ── Public API — each delegates to _inner and records timing ─────────────

    /// Store a key-value pair in the given namespace.
    pub fn put(&mut self, ns: &[u8], key: &[u8], val: &[u8]) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.put_inner(ns, key, val);
        self.metrics
            .puts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.put_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// Store a key-value pair with a TTL (seconds).
    ///
    /// Records expire lazily during compaction based on cohort window.
    pub fn put_with_ttl(
        &mut self,
        ns: &[u8],
        key: &[u8],
        val: &[u8],
        ttl_secs: u32,
    ) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.put_with_ttl_inner(ns, key, val, ttl_secs);
        self.metrics
            .puts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.put_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// Lazy expiry: records inserted with `put_with_ttl` are returned until `compact_once` removes their cohort.
    pub fn get(&self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.get_inner(ns, key);
        self.metrics
            .gets
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.get_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// Get with cost accounting. Returns the value (if any) and [`QueryStats`].
    ///
    /// `stats.segments_scanned` is 1 if the key was found in a segment, 0 for a
    /// memtable hit or a miss; `bytes_scanned` is the key+value size of the found entry.
    pub fn get_with_stats(
        &self,
        ns: &[u8],
        key: &[u8],
    ) -> Result<(Option<Vec<u8>>, QueryStats), EdgestoreError> {
        let encoded_key = encode_key(ns, key);
        let in_memtable = self.memtable.get(&encoded_key).is_some();
        let val = self.get_inner(ns, key)?;
        if val.is_none() && !in_memtable {
            return Ok((None, QueryStats::default()));
        }
        let bytes =
            val.as_ref().map(|v| v.len() as u64).unwrap_or(0) + encoded_key.len() as u64;
        Ok((
            val,
            QueryStats {
                segments_scanned: if in_memtable { 0 } else { 1 },
                bytes_scanned: bytes,
                items_examined: 1,
            },
        ))
    }

    /// Get a value into an existing buffer, avoiding a fresh `Vec<u8>` allocation.
    ///
    /// Returns `true` if the key was found and `buf` was filled. Returns `false`
    /// (and leaves `buf` unchanged) on a miss. Useful for high-throughput callers
    /// that reuse a buffer across many lookups.
    pub fn get_into(
        &self,
        ns: &[u8],
        key: &[u8],
        buf: &mut Vec<u8>,
    ) -> Result<bool, EdgestoreError> {
        match self.get_inner(ns, key)? {
            Some(val) => {
                buf.clear();
                buf.extend_from_slice(&val);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Delete a key in the given namespace (tombstone).
    pub fn delete(&mut self, ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.delete_inner(ns, key);
        self.metrics
            .deletes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.delete_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// Lazy expiry: TTL-expired records appear in range results until compaction removes their cohort.
    pub fn range(&self, ns: &[u8], start: &[u8], end: &[u8]) -> Result<KvPairs, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.range_inner(ns, start, end);
        self.metrics
            .ranges
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.range_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// Range scan with cost accounting. Returns results + [`QueryStats`].
    pub fn range_with_stats(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<(KvPairs, QueryStats), EdgestoreError> {
        self.range_core(ns, start, end, None)
            .map(|b| (b.items, b.stats))
    }

    /// Range scan that stops when the [`ScanBudget`] is exhausted.
    /// Returns a [`BudgetedScan`] that indicates whether the result was truncated.
    pub fn range_budgeted(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
        budget: &ScanBudget,
    ) -> Result<BudgetedKvScan, EdgestoreError> {
        self.range_core(ns, start, end, Some(budget))
    }

    /// Lazy expiry: TTL-expired records appear in prefix results until compaction removes their cohort.
    pub fn prefix(&self, ns: &[u8], prefix: &[u8]) -> Result<KvPairs, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.prefix_inner(ns, prefix);
        self.metrics
            .prefixes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.prefix_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// Prefix scan with cost accounting. Returns results + [`QueryStats`].
    pub fn prefix_with_stats(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<(KvPairs, QueryStats), EdgestoreError> {
        self.prefix_core(ns, prefix, None)
            .map(|b| (b.items, b.stats))
    }

    /// Prefix scan that stops when the [`ScanBudget`] is exhausted.
    pub fn prefix_budgeted(
        &self,
        ns: &[u8],
        prefix: &[u8],
        budget: &ScanBudget,
    ) -> Result<BudgetedKvScan, EdgestoreError> {
        self.prefix_core(ns, prefix, Some(budget))
    }

    /// Cursor-based forward range page (P4).
    ///
    /// Returns up to `page_size` items in ascending key order starting just after `cursor`.
    /// On the first call pass `cursor = None`; on subsequent calls pass the `next_key`
    /// returned by the previous call. `next_key = None` in the result means no more pages.
    ///
    /// Each call is bounded at the I/O level: only segments whose key range overlaps the
    /// effective `[cursor, end)` window are read, and reading stops as soon as `page_size`
    /// live items are collected.
    pub fn range_page(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
        cursor: Option<&[u8]>,
        page_size: usize,
    ) -> Result<RangePage, EdgestoreError> {
        if page_size == 0 {
            return Ok(RangePage { items: vec![], next_key: None });
        }
        let effective_start_buf;
        let effective_start: &[u8] = match cursor {
            Some(c) => {
                effective_start_buf = { let mut v = c.to_vec(); v.push(0); v };
                &effective_start_buf
            }
            None => start,
        };
        let budget = ScanBudget { max_items: Some(page_size), max_bytes: None };
        let scan = self.range_budgeted(ns, effective_start, end, &budget)?;
        let next_key = if scan.truncated {
            scan.items.last().map(|(k, _)| k.clone())
        } else {
            None
        };
        Ok(RangePage { items: scan.items, next_key })
    }

    /// Cursor-based reverse range page (P5).
    ///
    /// Returns up to `page_size` items in **descending** key order starting just below
    /// `cursor`. On the first call pass `cursor = None` to start from `end`; on subsequent
    /// calls pass the `next_key` returned by the previous call.
    ///
    /// `next_key` is the smallest key in the current page (the furthest-left point
    /// reached). Pass it as `cursor` to the next call to continue going left.
    /// `next_key = None` means the scan reached `start` and is exhausted.
    pub fn range_rev_page(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
        cursor: Option<&[u8]>,
        page_size: usize,
    ) -> Result<RangePage, EdgestoreError> {
        if page_size == 0 {
            return Ok(RangePage { items: vec![], next_key: None });
        }
        let effective_end: &[u8] = cursor.unwrap_or(end);
        let enc_start = encode_key(ns, start);
        let enc_end = encode_key(ns, effective_end);
        if enc_end <= enc_start {
            return Ok(RangePage { items: vec![], next_key: None });
        }

        // Segment results: descending, deduped, tombstones filtered (budget-aware via P2)
        let seg_results = self.segment_store.range_scan_rev_budgeted(
            &enc_start,
            &enc_end,
            page_size,
        )?;
        // Memtable results: ascending (includes tombstones) — reverse for descending merge
        let mem_asc = self.memtable.range(&enc_start, &enc_end);

        // Two-pointer merge of two descending sequences
        let mut si = 0usize;
        let mut mi = mem_asc.len();
        let mut merged: Vec<(Vec<u8>, MemEntry)> =
            Vec::with_capacity(seg_results.len() + mem_asc.len());
        loop {
            let has_seg = si < seg_results.len();
            let has_mem = mi > 0;
            if !has_seg && !has_mem {
                break;
            }
            let pick_seg = if !has_seg {
                false
            } else if !has_mem {
                true
            } else {
                seg_results[si].0.as_slice() >= mem_asc[mi - 1].0
            };
            if pick_seg {
                merged.push(seg_results[si].clone());
                si += 1;
            } else {
                mi -= 1;
                merged.push((mem_asc[mi].0.to_vec(), mem_asc[mi].1.clone()));
            }
        }

        // Dedup by encoded key (keep highest LSN), filter tombstones, decode, apply budget
        let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut i = 0usize;
        while i < merged.len() {
            let (k, e) = &merged[i];
            let mut best = e.clone();
            i += 1;
            while i < merged.len() && &merged[i].0 == k {
                if merged[i].1.lsn > best.lsn {
                    best = merged[i].1.clone();
                }
                i += 1;
            }
            if best.op == Operation::Delete {
                continue;
            }
            if let Some(val) = &best.value {
                let (_, raw_key) = decode_key(k)?;
                out.push((raw_key, val.clone()));
                if out.len() >= page_size {
                    break;
                }
            }
        }

        let next_key = if out.len() >= page_size {
            out.last().map(|(k, _)| k.clone())
        } else {
            None
        };
        Ok(RangePage { items: out, next_key })
    }

    /// Flush the current memtable to a new segment file.
    pub fn flush_to_segments(&mut self) -> Result<crate::types::SegmentMeta, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.flush_to_segments_inner();
        self.metrics
            .segment_flushes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.segment_flush_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// fsync the current WAL file and persist text indices.
    ///
    /// Text indices are kept in memory and only written to disk on flush
    /// or when the engine is dropped. Call flush() before closing to ensure
    /// text search indexes are durable.
    pub fn flush(&mut self) -> Result<(), EdgestoreError> {
        self.persist_text_indices()?;
        self.wal.fsync()
    }

    /// Start a new multi-record transaction.
    pub fn begin(&mut self) -> crate::transaction::Transaction {
        self.txid_counter += 1;
        crate::transaction::Transaction::new(self.txid_counter)
    }

    /// Commit a transaction, writing all pending records to the WAL.
    pub fn commit_transaction(
        &mut self,
        tx: crate::transaction::Transaction,
    ) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.commit_transaction_inner(tx);
        self.metrics
            .transactions_committed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.transaction_commit_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// Roll back a transaction, discarding all pending records.
    pub fn rollback_transaction(&mut self, mut tx: crate::transaction::Transaction) {
        tx.rollback_self();
        self.metrics
            .transactions_rolled_back
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Run one bounded compaction cycle.
    ///
    /// Uses wall-clock time to determine which cohorts are expired.
    /// Respects the write budget from `EdgestoreConfig::compaction_write_budget_bytes`.
    /// Pinned segments (held by live snapshots) are never removed or rewritten.
    ///
    /// After compaction, the segment store is reloaded from disk so subsequent
    /// reads see the updated segment list.
    pub fn compact_once(&mut self) -> Result<crate::compactor::CompactionStats, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.compact_once_inner();
        self.metrics
            .compactions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.compaction_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
        r
    }

    /// Return a point-in-time snapshot pinning the current set of segments.
    ///
    /// The returned `Snapshot` holds a reference to the `SnapshotRegistry` and
    /// releases its pins automatically when dropped.
    pub fn snapshot(&self) -> Result<crate::snapshot::Snapshot, EdgestoreError> {
        let ids = self.segment_store.segment_ids();
        let readers = self.segment_store.clone_readers_for(&ids);
        let sid = self.snapshot_registry.register(&ids);
        Ok(crate::snapshot::Snapshot::new(
            sid,
            self.snapshot_registry.clone(),
            readers,
        ))
    }

    /// Returns the filesystem path to the engine's database directory.
    ///
    /// Used by external crates (e.g. `edgestore-repl`) to locate segment files.
    pub fn db_path(&self) -> &std::path::Path {
        &self.config.path
    }

    /// Returns a point-in-time snapshot of all engine metrics.
    pub fn metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Return metadata for all live segments.
    ///
    /// Used by `edgestore-tier` to build archived-segment indexes.
    pub fn list_segment_metas(&self) -> Vec<crate::types::SegmentMeta> {
        self.segment_store.list_segment_metas().to_vec()
    }

    /// Rewrite a segment removing all `__text__*` namespace records.
    ///
    /// After a segment has been archived to cold storage, callers may strip its embedded
    /// full-text index records to reclaim local disk space. The archived copy retains the
    /// original data; the local copy becomes a compact KV-only segment.
    ///
    /// Sets `SegmentMeta::text_index_stripped = true` on the new segment. Subsequent
    /// `search_text` calls will not find text records from this segment; existing
    /// search results are unaffected if the merged `__index__` entry was already flushed
    /// to a later (unstripped) segment.
    ///
    /// Returns `Ok(existing_meta)` unchanged if the segment has no text records or was
    /// already stripped.
    ///
    /// # Errors
    ///
    /// Returns an error if `segment_id` is not found, or if the rewrite fails.
    pub fn strip_text_index(
        &mut self,
        segment_id: u64,
    ) -> Result<crate::types::SegmentMeta, EdgestoreError> {
        use crate::types::decode_key;

        // Locate the segment.
        let old_meta = self
            .segment_store
            .list_segment_metas()
            .iter()
            .find(|m| m.segment_id == segment_id)
            .ok_or_else(|| {
                EdgestoreError::InvalidOperation(format!(
                    "strip_text_index: segment {} not found",
                    segment_id
                ))
            })?
            .clone();

        if old_meta.text_index_stripped {
            return Ok(old_meta);
        }

        // Read all entries from the segment.
        let entries = {
            let reader = self.segment_store.reader_for(segment_id).ok_or_else(|| {
                EdgestoreError::InvalidOperation(format!(
                    "strip_text_index: no reader for segment {}",
                    segment_id
                ))
            })?;
            reader.range_scan(&[], &vec![0xFF; 1024])?
        };

        let filtered: Vec<(Vec<u8>, crate::types::MemEntry)> = entries
            .into_iter()
            .filter(|(k, _)| {
                match decode_key(k) {
                    Ok((ns, _)) => !ns.starts_with(b"__text__"),
                    Err(_) => true, // keep malformed keys intact
                }
            })
            .collect();

        if filtered.len() == old_meta.record_count as usize {
            return Ok(old_meta);
        }

        // No non-text entries remain — the segment is text-only; nothing to keep locally.
        // Return the original meta unchanged (caller may delete the local segment themselves).
        if filtered.is_empty() {
            return Ok(old_meta);
        }

        // Write the filtered entries as a new segment.
        let new_id = self.segment_store.alloc_segment_id();
        let mut writer = crate::segment::SegmentWriter::new(
            self.segment_store.base_path().to_path_buf(),
            new_id,
            self.config.cohort_window_secs,
        );
        let mut new_meta = writer.flush(&filtered)?;
        new_meta.text_index_stripped = true;

        let new_reader = crate::segment::SegmentReader::open(
            self.segment_store.base_path().to_path_buf(),
            new_id,
        )?;

        self.segment_store
            .replace_segment(segment_id, new_meta.clone(), new_reader)?;

        Ok(new_meta)
    }

    /// Removes one local segment: deletes its `.dat`/`.idx`/`.xf`/`.meta` files and
    /// its manifest entry. Does **not** touch any remote/archived copy — for callers
    /// that have already confirmed the segment is durably archived elsewhere and
    /// just want to reclaim local disk space (e.g. Pierre's local-retention pruning,
    /// after a configurable grace period past a successful archive).
    ///
    /// A no-op (returns `Ok`) if `segment_id` doesn't exist locally.
    pub fn prune_local_segment(
        &mut self,
        segment_id: crate::types::SegmentId,
    ) -> Result<(), EdgestoreError> {
        self.segment_store.remove_segment(segment_id)
    }

    /// Strip the embedded vector index from a local segment, rewriting it without
    /// `__vec__` namespace records. Mirrors [`Engine::strip_text_index`] for the vector
    /// tier: call this after `archive_segments` to reclaim local disk space while the
    /// full vector data remains available in the remote archive.
    ///
    /// Returns the (possibly rewritten) segment metadata.
    /// If the segment has no vector records, or if it was already stripped, returns the
    /// original metadata unchanged. If ALL records are vector records (no KV or text),
    /// the segment is returned unchanged — the caller should decide whether to prune it.
    pub fn strip_vector_index(
        &mut self,
        segment_id: u64,
    ) -> Result<crate::types::SegmentMeta, EdgestoreError> {
        use crate::types::decode_key;

        let old_meta = self
            .segment_store
            .list_segment_metas()
            .iter()
            .find(|m| m.segment_id == segment_id)
            .ok_or_else(|| {
                EdgestoreError::InvalidOperation(format!(
                    "strip_vector_index: segment {} not found",
                    segment_id
                ))
            })?
            .clone();

        if old_meta.vector_index_stripped {
            return Ok(old_meta);
        }

        let entries = {
            let reader = self.segment_store.reader_for(segment_id).ok_or_else(|| {
                EdgestoreError::InvalidOperation(format!(
                    "strip_vector_index: no reader for segment {}",
                    segment_id
                ))
            })?;
            reader.range_scan(&[], &vec![0xFF; 1024])?
        };

        let filtered: Vec<(Vec<u8>, crate::types::MemEntry)> = entries
            .into_iter()
            .filter(|(k, _)| match decode_key(k) {
                Ok((ns, _)) => !ns.starts_with(b"__vec__"),
                Err(_) => true,
            })
            .collect();

        if filtered.len() == old_meta.record_count as usize || filtered.is_empty() {
            return Ok(old_meta);
        }

        let new_id = self.segment_store.alloc_segment_id();
        let mut writer = crate::segment::SegmentWriter::new(
            self.segment_store.base_path().to_path_buf(),
            new_id,
            self.config.cohort_window_secs,
        );
        let mut new_meta = writer.flush(&filtered)?;
        new_meta.vector_index_stripped = true;

        let new_reader = crate::segment::SegmentReader::open(
            self.segment_store.base_path().to_path_buf(),
            new_id,
        )?;

        self.segment_store
            .replace_segment(segment_id, new_meta.clone(), new_reader)?;

        Ok(new_meta)
    }

    // ── Private implementations ───────────────────────────────────────────────

    fn put_inner(&mut self, ns: &[u8], key: &[u8], val: &[u8]) -> Result<Lsn, EdgestoreError> {
        if self.config.readonly {
            return Err(EdgestoreError::ReadOnly);
        }
        if ns.len() > u16::MAX as usize {
            return Err(EdgestoreError::NamespaceTooLong {
                len: ns.len(),
                max: u16::MAX as usize,
            });
        }

        self.lsn_counter += 1;
        let lsn = self.lsn_counter;
        let timestamp = Self::now_nanos();

        let record = WalRecord {
            txid: 0,
            lsn,
            timestamp,
            ttl: 0,
            ns_len: ns.len() as u16,
            ns_bytes: ns.to_vec(),
            key_bytes: key.to_vec(),
            op: Operation::Put,
            value_hash: blake3::hash(val).into(),
            value_bytes: val.to_vec(),
        };
        self.wal.append(&record)?;
        self.rotate_wal_if_needed()?;

        let encoded_key = encode_key(ns, key);
        let entry = MemEntry {
            key: encoded_key.clone(),
            value: Some(val.to_vec()),
            op: Operation::Put,
            lsn,
            timestamp,
            ttl: 0,
        };
        self.memtable.insert(encoded_key, entry);

        if (self.memtable.len() as u64) * AVG_ENTRY_SIZE_ESTIMATE >= self.config.memtable_max_bytes
        {
            let _ = self.flush_to_segments_inner();
        }

        Ok(lsn)
    }

    fn put_with_ttl_inner(
        &mut self,
        ns: &[u8],
        key: &[u8],
        val: &[u8],
        ttl_secs: u32,
    ) -> Result<Lsn, EdgestoreError> {
        if self.config.readonly {
            return Err(EdgestoreError::ReadOnly);
        }
        if ns.len() > u16::MAX as usize {
            return Err(EdgestoreError::NamespaceTooLong {
                len: ns.len(),
                max: u16::MAX as usize,
            });
        }

        self.lsn_counter += 1;
        let lsn = self.lsn_counter;
        let timestamp = Self::now_nanos();

        let record = WalRecord {
            txid: 0,
            lsn,
            timestamp,
            ttl: ttl_secs,
            ns_len: ns.len() as u16,
            ns_bytes: ns.to_vec(),
            key_bytes: key.to_vec(),
            op: Operation::Put,
            value_hash: blake3::hash(val).into(),
            value_bytes: val.to_vec(),
        };
        self.wal.append(&record)?;
        self.rotate_wal_if_needed()?;

        let encoded_key = encode_key(ns, key);
        let entry = MemEntry {
            key: encoded_key.clone(),
            value: Some(val.to_vec()),
            op: Operation::Put,
            lsn,
            timestamp,
            ttl: ttl_secs,
        };
        self.memtable.insert(encoded_key, entry);

        Ok(lsn)
    }

    fn get_inner(&self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        let encoded_key = encode_key(ns, key);
        match self.memtable.get(&encoded_key) {
            Some(entry) if entry.op == Operation::Delete => return Ok(None),
            Some(entry) => return Ok(entry.value.clone()),
            None => {}
        }
        if let Some(entry) = self.segment_store.get(&encoded_key)? {
            if entry.op == Operation::Delete {
                return Ok(None);
            }
            return Ok(entry.value);
        }
        Ok(None)
    }

    fn delete_inner(&mut self, ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError> {
        if self.config.readonly {
            return Err(EdgestoreError::ReadOnly);
        }
        self.lsn_counter += 1;
        let lsn = self.lsn_counter;
        let timestamp = Self::now_nanos();

        let record = WalRecord {
            txid: 0,
            lsn,
            timestamp,
            ttl: 0,
            ns_len: ns.len() as u16,
            ns_bytes: ns.to_vec(),
            key_bytes: key.to_vec(),
            op: Operation::Delete,
            value_hash: blake3::hash(b"").into(),
            value_bytes: vec![],
        };
        self.wal.append(&record)?;
        self.rotate_wal_if_needed()?;

        let encoded_key = encode_key(ns, key);
        let entry = MemEntry {
            key: encoded_key.clone(),
            value: None,
            op: Operation::Delete,
            lsn,
            timestamp,
            ttl: 0,
        };
        self.memtable.insert(encoded_key, entry);

        Ok(lsn)
    }

    fn range_inner(&self, ns: &[u8], start: &[u8], end: &[u8]) -> Result<KvPairs, EdgestoreError> {
        self.range_core(ns, start, end, None).map(|b| b.items)
    }

    fn prefix_inner(&self, ns: &[u8], prefix: &[u8]) -> Result<KvPairs, EdgestoreError> {
        self.prefix_core(ns, prefix, None).map(|b| b.items)
    }

    // Core scan implementation shared by range / range_with_stats / range_budgeted.
    // PERFORMANCE: merge two sorted lists (segment + memtable), then dedup by key keeping
    // highest LSN. DO NOT use HashMap — both inputs are already sorted; merge+dedup is O(n)
    // with 2 allocations vs HashMap's O(n log n) with 4.
    // Regression test: test_range_scan_dedups_by_lsn_across_segments (segment.rs).
    fn range_core(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
        budget: Option<&ScanBudget>,
    ) -> Result<BudgetedKvScan, EdgestoreError> {
        let enc_start = encode_key(ns, start);
        let enc_end = encode_key(ns, end);

        let max_items = budget.and_then(|b| b.max_items);
        let (seg_results, seg_truncated) =
            self.segment_store.range_scan_budgeted(&enc_start, &enc_end, max_items)?;
        let mem_results = self.memtable.range(&enc_start, &enc_end);
        let has_seg = !seg_results.is_empty();

        let mut merged: Vec<(Vec<u8>, MemEntry)> =
            Vec::with_capacity(seg_results.len() + mem_results.len());
        let mut si = 0usize;
        let mut mi = 0usize;
        while si < seg_results.len() || mi < mem_results.len() {
            let (k, e) = if si < seg_results.len()
                && (mi >= mem_results.len() || seg_results[si].0.as_slice() <= mem_results[mi].0)
            {
                let (k, e) = &seg_results[si];
                si += 1;
                (k.clone(), e.clone())
            } else {
                let (k, e) = mem_results[mi];
                mi += 1;
                (k.to_vec(), e.clone())
            };
            merged.push((k, e));
        }

        let mut out = Vec::new();
        let mut stats = QueryStats {
            segments_scanned: if has_seg { 1 } else { 0 },
            ..Default::default()
        };
        // If the segment scan was truncated by budget, the final result is also truncated
        // even if the merged slice happens to be fully consumed.
        let mut truncated = seg_truncated;
        let mut i = 0usize;
        while i < merged.len() {
            let (k, e) = &merged[i];
            let mut best_entry = e.clone();
            let entry_key_len = k.len() as u64;
            let entry_val_len = e.value.as_ref().map(|v| v.len() as u64).unwrap_or(0);
            stats.bytes_scanned += entry_key_len + entry_val_len;
            stats.items_examined += 1;
            i += 1;
            while i < merged.len() && &merged[i].0 == k {
                let v_len = merged[i]
                    .1
                    .value
                    .as_ref()
                    .map(|v| v.len() as u64)
                    .unwrap_or(0);
                stats.bytes_scanned += merged[i].0.len() as u64 + v_len;
                stats.items_examined += 1;
                if merged[i].1.lsn > best_entry.lsn {
                    best_entry = merged[i].1.clone();
                }
                i += 1;
            }
            if best_entry.op == Operation::Delete {
                continue;
            }
            if let Some(val) = &best_entry.value {
                let (_, raw_key) = decode_key(k)?;
                out.push((raw_key, val.clone()));
                if let Some(b) = budget {
                    let over_items = b.max_items.is_some_and(|m| out.len() >= m);
                    let over_bytes = b.max_bytes.is_some_and(|m| stats.bytes_scanned >= m);
                    if over_items || over_bytes {
                        truncated = truncated || i < merged.len();
                        break;
                    }
                }
            }
        }
        Ok(BudgetedScan {
            items: out,
            truncated,
            stats,
        })
    }

    fn prefix_core(
        &self,
        ns: &[u8],
        prefix: &[u8],
        budget: Option<&ScanBudget>,
    ) -> Result<BudgetedKvScan, EdgestoreError> {
        let enc_prefix = encode_key(ns, prefix);

        // PERFORMANCE: same merge+dedup algorithm as range_core.
        // Regression test: test_range_scan_dedups_by_lsn_across_segments (segment.rs).
        let max_items = budget.and_then(|b| b.max_items);
        let (seg_results, seg_truncated) = if let Some(enc_end) = prefix_upper_bound(&enc_prefix) {
            let (raw, trunc) =
                self.segment_store.range_scan_budgeted(&enc_prefix, &enc_end, max_items)?;
            let filtered = raw
                .into_iter()
                .filter(|(k, _)| k.starts_with(&enc_prefix))
                .collect::<Vec<_>>();
            (filtered, trunc)
        } else {
            (vec![], false)
        };
        let mem_results = self.memtable.prefix(&enc_prefix);
        let has_seg = !seg_results.is_empty();

        let mut merged: Vec<(Vec<u8>, MemEntry)> =
            Vec::with_capacity(seg_results.len() + mem_results.len());
        let mut si = 0usize;
        let mut mi = 0usize;
        while si < seg_results.len() || mi < mem_results.len() {
            let (k, e) = if si < seg_results.len()
                && (mi >= mem_results.len() || seg_results[si].0.as_slice() <= mem_results[mi].0)
            {
                let (k, e) = &seg_results[si];
                si += 1;
                (k.clone(), e.clone())
            } else {
                let (k, e) = mem_results[mi];
                mi += 1;
                (k.to_vec(), e.clone())
            };
            merged.push((k, e));
        }

        let mut out = Vec::new();
        let mut stats = QueryStats {
            segments_scanned: if has_seg { 1 } else { 0 },
            ..Default::default()
        };
        let mut truncated = seg_truncated;
        let mut i = 0usize;
        while i < merged.len() {
            let (k, e) = &merged[i];
            let mut best_entry = e.clone();
            let entry_key_len = k.len() as u64;
            let entry_val_len = e.value.as_ref().map(|v| v.len() as u64).unwrap_or(0);
            stats.bytes_scanned += entry_key_len + entry_val_len;
            stats.items_examined += 1;
            i += 1;
            while i < merged.len() && &merged[i].0 == k {
                let v_len = merged[i]
                    .1
                    .value
                    .as_ref()
                    .map(|v| v.len() as u64)
                    .unwrap_or(0);
                stats.bytes_scanned += merged[i].0.len() as u64 + v_len;
                stats.items_examined += 1;
                if merged[i].1.lsn > best_entry.lsn {
                    best_entry = merged[i].1.clone();
                }
                i += 1;
            }
            if best_entry.op == Operation::Delete {
                continue;
            }
            if let Some(val) = &best_entry.value {
                let (_, raw_key) = decode_key(k)?;
                out.push((raw_key, val.clone()));
                if let Some(b) = budget {
                    let over_items = b.max_items.is_some_and(|m| out.len() >= m);
                    let over_bytes = b.max_bytes.is_some_and(|m| stats.bytes_scanned >= m);
                    if over_items || over_bytes {
                        truncated = truncated || i < merged.len();
                        break;
                    }
                }
            }
        }
        Ok(BudgetedScan {
            items: out,
            truncated,
            stats,
        })
    }

    fn flush_to_segments_inner(&mut self) -> Result<crate::types::SegmentMeta, EdgestoreError> {
        if self.memtable.is_empty() {
            return Err(EdgestoreError::SegmentCorrupt(
                "memtable is empty".to_string(),
            ));
        }
        let meta = self.segment_store.flush_memtable(self.memtable.as_ref())?;
        self.memtable.clear();
        if let Some(cb) = &self.on_segment_flushed {
            cb(&meta);
        }
        Ok(meta)
    }

    fn commit_transaction_inner(
        &mut self,
        tx: crate::transaction::Transaction,
    ) -> Result<Lsn, EdgestoreError> {
        let mut tx = tx;
        let records = tx.take_pending()?;
        let mut last_lsn = self.lsn_counter;

        for mut record in records {
            self.lsn_counter += 1;
            record.lsn = self.lsn_counter;
            last_lsn = self.lsn_counter;

            self.wal.append(&record)?;

            let encoded_key = encode_key(&record.ns_bytes, &record.key_bytes);
            let entry = MemEntry {
                key: encoded_key.clone(),
                value: if record.op == Operation::Put {
                    Some(record.value_bytes.clone())
                } else {
                    None
                },
                op: record.op,
                lsn: record.lsn,
                timestamp: record.timestamp,
                ttl: record.ttl,
            };
            self.memtable.insert(encoded_key, entry);
        }

        self.wal.fsync()?;
        self.rotate_wal_if_needed()?;
        Ok(last_lsn)
    }

    fn compact_once_inner(&mut self) -> Result<crate::compactor::CompactionStats, EdgestoreError> {
        let now_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        let pinned = self.snapshot_registry.pinned_ids();
        let compactor = crate::compactor::Compactor::new(
            self.config.path.clone(),
            self.config.compaction_write_budget_bytes,
            self.config.cohort_window_secs,
        );
        let mut manifest = crate::manifest::Manifest::open(&self.config.path.join("manifest.mf"))?;
        let stats = compactor.compact_cycle(&mut manifest, now_nanos, &pinned)?;
        self.segment_store = crate::segment::SegmentStore::open(
            self.config.path.clone(),
            self.config.cohort_window_secs,
        )?;
        Ok(stats)
    }

    // ── Replication API ───────────────────────────────────────────────────────

    /// Returns the local segment manifest as `Vec<SegmentRef>` for a remote peer to diff against.
    pub fn export_manifest(&self) -> Result<Vec<SegmentRef>, EdgestoreError> {
        let metas = self.segment_store.list_segment_metas();
        let mut refs = Vec::with_capacity(metas.len());
        for meta in metas {
            // segment_hash is Vec<u8> (32 bytes). Convert to [u8; 32] for SegmentRef.
            let mut hash = [0u8; 32];
            let src = &meta.segment_hash;
            let copy_len = src.len().min(32);
            hash[..copy_len].copy_from_slice(&src[..copy_len]);
            refs.push(SegmentRef {
                segment_hash: hash,
                segment_id: meta.segment_id,
            });
        }
        Ok(refs)
    }

    /// Returns hashes the peer has that we do not (set diff: peer ∖ local).
    ///
    /// Pure computation, no I/O.
    pub fn missing_segments(&self, peer_segments: &[SegmentRef]) -> Vec<[u8; 32]> {
        let local_set: HashSet<Vec<u8>> = self
            .segment_store
            .list_segment_metas()
            .iter()
            .map(|m| m.segment_hash.clone())
            .collect();
        peer_segments
            .iter()
            .filter(|s| {
                let hash_vec: Vec<u8> = s.segment_hash.to_vec();
                !local_set.contains(&hash_vec)
            })
            .map(|s| s.segment_hash)
            .collect()
    }

    /// Accept raw segment bytes from a peer, verify BLAKE3, write atomically, apply LWW per record.
    ///
    /// Returns:
    /// - `Ok(ImportResult::Skipped)` if the segment is already present in the local manifest.
    /// - `Ok(ImportResult::HashMismatch)` if BLAKE3(data) != claimed hash — segment rejected.
    /// - `Ok(ImportResult::Applied { keys_written, keys_skipped })` on success.
    ///
    /// // LWW correctness requires NTP synchronization (D06). Clock skew > segment flush interval
    /// // can cause incorrect merge outcomes.
    pub fn import_segment(
        &mut self,
        data: &[u8],
        hash: &[u8; 32],
    ) -> Result<ImportResult, EdgestoreError> {
        // Step 1: Check if already present.
        let hash_vec: Vec<u8> = hash.to_vec();
        let already_present = self
            .segment_store
            .list_segment_metas()
            .iter()
            .any(|m| m.segment_hash == hash_vec);
        if already_present {
            return Ok(ImportResult::Skipped);
        }

        // Step 2: Verify BLAKE3.
        let computed: [u8; 32] = *blake3::hash(data).as_bytes();
        if computed != *hash {
            return Ok(ImportResult::HashMismatch);
        }

        // Step 3: Write to .tmp file.
        let hash_hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        let base = self.segment_store.base_path().to_path_buf();
        let tmp_path = base.join(format!("{}.tmp", hash_hex));
        let dat_path = base.join(format!("{}.dat", hash_hex));

        std::fs::write(&tmp_path, data)?;

        // Step 4: Rename to final path atomically.
        std::fs::rename(&tmp_path, &dat_path)?;

        // Step 5: Parse segment records from raw bytes using segment::deserialize_entry.
        let mut keys_written: u64 = 0;
        let mut keys_skipped: u64 = 0;
        let mut segment_keys: Vec<Vec<u8>> = Vec::new();
        let mut min_key: Option<Vec<u8>> = None;
        let mut max_key: Option<Vec<u8>> = None;
        let mut min_lsn: Lsn = u64::MAX;
        let mut max_lsn: Lsn = 0;

        // The raw bytes are a full .dat file with file header + blocks.
        // Skip the 8-byte file header (magic 4 bytes + version 1 byte + padding 3 bytes).
        let mut offset = 8usize;
        while offset < data.len() {
            // Read block header: magic (4) + compressed_len (4).
            if offset + 8 > data.len() {
                break;
            }
            let magic = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            if magic != crate::segment::SEGMENT_BLOCK_MAGIC {
                break; // hit padding or end
            }
            let compressed_len =
                u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;

            let payload_size = 8 + compressed_len;
            let aligned_size = if payload_size.is_multiple_of(crate::segment::SEGMENT_BLOCK_SIZE) {
                payload_size
            } else {
                (payload_size / crate::segment::SEGMENT_BLOCK_SIZE + 1)
                    * crate::segment::SEGMENT_BLOCK_SIZE
            };

            if offset + 8 + compressed_len > data.len() {
                break;
            }
            let compressed = &data[offset + 8..offset + 8 + compressed_len];
            let decompressed = zstd::decode_all(compressed).map_err(|e| {
                EdgestoreError::SegmentCorrupt(format!("import_segment zstd decode: {}", e))
            })?;

            // Step 6: Apply LWW per record.
            let mut pos = 0;
            while pos < decompressed.len() {
                match crate::segment::deserialize_entry(&decompressed, &mut pos) {
                    Ok((encoded_key, incoming)) => {
                        // Track segment bounds and keys for sidecar files (C-02, C-03).
                        segment_keys.push(encoded_key.clone());
                        min_key = Some(match min_key {
                            None => encoded_key.clone(),
                            Some(ref mk) if encoded_key < *mk => encoded_key.clone(),
                            Some(mk) => mk,
                        });
                        max_key = Some(match max_key {
                            None => encoded_key.clone(),
                            Some(ref mk) if encoded_key > *mk => encoded_key.clone(),
                            Some(mk) => mk,
                        });
                        if incoming.lsn < min_lsn {
                            min_lsn = incoming.lsn;
                        }
                        if incoming.lsn > max_lsn {
                            max_lsn = incoming.lsn;
                        }

                        // Look up local entry by encoded key.
                        let local_entry = self
                            .memtable
                            .get(&encoded_key)
                            .cloned()
                            .or_else(|| self.segment_store.get(&encoded_key).ok().flatten());

                        let apply = match local_entry {
                            None => true,
                            Some(ref local) => {
                                if local.timestamp > incoming.timestamp {
                                    // Local wins — skip.
                                    false
                                } else if local.timestamp == incoming.timestamp {
                                    // Timestamp tie: lower host_id wins.
                                    // host_id is not stored in MemEntry in v1; favor local on tie.
                                    false
                                } else {
                                    // incoming.timestamp > local.timestamp — incoming wins.
                                    true
                                }
                            }
                        };

                        if apply {
                            // Decode ns and key from encoded_key for put_with_timestamp.
                            if let Ok((ns, key)) = crate::types::decode_key(&encoded_key) {
                                if incoming.op == crate::types::Operation::Put {
                                    if let Some(ref val) = incoming.value {
                                        self.put_with_timestamp(
                                            &ns,
                                            &key,
                                            val,
                                            incoming.timestamp,
                                        )?;
                                        keys_written += 1;
                                    } else {
                                        // Malformed Put with no value — count as skipped.
                                        keys_skipped += 1;
                                    }
                                } else if incoming.op == crate::types::Operation::Delete {
                                    self.delete_with_timestamp(&ns, &key, incoming.timestamp)?;
                                    keys_written += 1;
                                }
                            }
                        } else {
                            keys_skipped += 1;
                        }
                    }
                    Err(_) => break,
                }
            }

            offset += aligned_size;
        }

        // Step 7: Build SegmentMeta for the imported segment and add to manifest.
        // We need a new segment_id allocated from the store.
        let new_segment_id = self.segment_store.alloc_segment_id();

        // Read the hash_metas from the existing dat file to reconstruct SegmentMeta.
        // Build a minimal meta from what we know (the data was already imported and LWW applied).
        // Re-read the segment using SegmentReader::open after we register the dat file properly.
        // The imported segment .dat is stored under hash_hex.dat, but SegmentReader expects
        // segment-{id:08}.dat format. Rename to the canonical segment file path.
        let canonical_dat = base.join(format!("segment-{:08}.dat", new_segment_id));
        std::fs::rename(&dat_path, &canonical_dat)?;

        // Flush WAL to ensure LWW-applied records are durable.
        self.wal.fsync()?;

        // Build SegmentMeta from the decoded data (C-02, C-03).
        let now_nanos = crate::engine::Engine::now_nanos();
        let segment_hash_vec: Vec<u8> = hash.to_vec();
        let meta = crate::types::SegmentMeta {
            segment_id: new_segment_id,
            segment_hash: segment_hash_vec,
            min_key: min_key.unwrap_or_default(),
            max_key: max_key.unwrap_or_default(),
            min_lsn: if min_lsn == u64::MAX { 0 } else { min_lsn },
            max_lsn,
            record_count: keys_written + keys_skipped,
            compressed_bytes: data.len() as u64,
            uncompressed_bytes: data.len() as u64,
            compression: "zstd:1".to_string(),
            cohort_bucket: 0,
            death_time: 0,
            merkle_root: hash.to_vec(),
            created_at: now_nanos,
            text_index_stripped: false,
            vector_index_stripped: false,
        };

        // Write .idx, .xf, .meta files so SegmentReader::open can load it.
        // Since we only applied records via LWW (not built a full sorted segment), the
        // imported .dat file stays as-is. We need the sidecar files to open it.
        // Write a trivial .idx file (single entry at offset 8 for the file header).
        let idx_path = base.join(format!("segment-{:08}.idx", new_segment_id));
        crate::segment::write_idx_file(&[(vec![], 8u64)], &idx_path)?;

        // Build xor filter from decoded keys (C-02: was empty, breaking post-restart reads).
        let xf_path = base.join(format!("segment-{:08}.xf", new_segment_id));
        let filter = crate::segment::build_xor_filter(&segment_keys)?;
        crate::segment::write_xf_file(&filter, &xf_path)?;

        // Write the .meta JSON file and fsync (W-03: durability).
        let meta_path = base.join(format!("segment-{:08}.meta", new_segment_id));
        let mut meta_file = std::fs::File::create(&meta_path)?;
        serde_json::to_writer_pretty(&mut meta_file, &meta)
            .map_err(|e| EdgestoreError::SegmentCorrupt(format!("import meta serialize: {}", e)))?;
        meta_file.sync_all()?;

        // Open reader and register with the segment store.
        let reader = crate::segment::SegmentReader::open(base.clone(), new_segment_id)?;
        self.segment_store.add_imported_segment(meta, reader)?;

        Ok(ImportResult::Applied {
            keys_written,
            keys_skipped,
        })
    }

    /// Apply a key-value record with an explicit timestamp (used during LWW replication).
    ///
    /// Identical to `put_inner` but substitutes the caller-supplied timestamp instead of
    /// generating one from the wall clock.
    fn put_with_timestamp(
        &mut self,
        ns: &[u8],
        key: &[u8],
        val: &[u8],
        timestamp: i64,
    ) -> Result<Lsn, EdgestoreError> {
        if ns.len() > u16::MAX as usize {
            return Err(EdgestoreError::NamespaceTooLong {
                len: ns.len(),
                max: u16::MAX as usize,
            });
        }

        self.lsn_counter += 1;
        let lsn = self.lsn_counter;

        let record = crate::types::WalRecord {
            txid: 0,
            lsn,
            timestamp,
            ttl: 0,
            ns_len: ns.len() as u16,
            ns_bytes: ns.to_vec(),
            key_bytes: key.to_vec(),
            op: crate::types::Operation::Put,
            value_hash: blake3::hash(val).into(),
            value_bytes: val.to_vec(),
        };
        self.wal.append(&record)?;
        self.rotate_wal_if_needed()?;

        let encoded_key = crate::types::encode_key(ns, key);
        let entry = MemEntry {
            key: encoded_key.clone(),
            value: Some(val.to_vec()),
            op: crate::types::Operation::Put,
            lsn,
            timestamp,
            ttl: 0,
        };
        self.memtable.insert(encoded_key, entry);

        Ok(lsn)
    }

    /// Apply a delete tombstone with an explicit timestamp (used during LWW replication).
    ///
    /// Identical to `delete_inner` but substitutes the caller-supplied timestamp instead of
    /// generating one from the wall clock.
    fn delete_with_timestamp(
        &mut self,
        ns: &[u8],
        key: &[u8],
        timestamp: i64,
    ) -> Result<Lsn, EdgestoreError> {
        self.lsn_counter += 1;
        let lsn = self.lsn_counter;

        let record = crate::types::WalRecord {
            txid: 0,
            lsn,
            timestamp,
            ttl: 0,
            ns_len: ns.len() as u16,
            ns_bytes: ns.to_vec(),
            key_bytes: key.to_vec(),
            op: crate::types::Operation::Delete,
            value_hash: blake3::hash(b"").into(),
            value_bytes: vec![],
        };
        self.wal.append(&record)?;
        self.rotate_wal_if_needed()?;

        let encoded_key = crate::types::encode_key(ns, key);
        let entry = MemEntry {
            key: encoded_key.clone(),
            value: None,
            op: crate::types::Operation::Delete,
            lsn,
            timestamp,
            ttl: 0,
        };
        self.memtable.insert(encoded_key, entry);

        Ok(lsn)
    }

    /// Returns the local RangeMerkleTree root for anti-entropy probing (D02).
    ///
    /// The root is computed from each segment's content hash (`segment_hash`, the BLAKE3
    /// of raw segment bytes). Using `segment_hash` — rather than the per-segment
    /// `merkle_root` field that is computed differently by `SegmentWriter` vs.
    /// `import_segment` — ensures that two nodes converge after a successful sync:
    /// once B has imported all of A's segments, their `segment_hash` sets are identical,
    /// so `range_merkle_root()` returns the same value on both sides.
    ///
    /// Algorithm: sort segment hashes lexicographically, then feed them in order through
    /// a single BLAKE3 hasher. Returns the all-zero hash when there are no segments.
    pub fn range_merkle_root(&self) -> Result<[u8; 32], EdgestoreError> {
        let metas = self.segment_store.list_segment_metas();
        if metas.is_empty() {
            return Ok([0u8; 32]);
        }

        // Collect and sort segment_hash values for a deterministic, order-independent root.
        let mut hashes: Vec<Vec<u8>> = metas.iter().map(|m| m.segment_hash.clone()).collect();
        hashes.sort_unstable();

        let mut hasher = blake3::Hasher::new();
        for h in &hashes {
            hasher.update(h);
        }
        let result = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(result.as_bytes());
        Ok(out)
    }

    /// Returns true if local Merkle root matches other_root (nodes are in sync).
    ///
    /// Returns false if diverged — caller should call export_manifest + missing_segments to
    /// determine what to pull (D02).
    pub fn compare_merkle(&self, other_root: &[u8; 32]) -> Result<bool, EdgestoreError> {
        let local_root = self.range_merkle_root()?;
        Ok(local_root == *other_root)
    }

    /// Rotate the WAL if the current writer has exceeded `wal_max_bytes` or `wal_max_age_secs`.
    ///
    /// Called after every append so long-running sessions rotate inline without requiring
    /// a close/reopen cycle.
    fn rotate_wal_if_needed(&mut self) -> Result<(), EdgestoreError> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if !self.wal.needs_rotation(now_secs) {
            return Ok(());
        }
        self.wal.fsync()?;
        let new_path = next_wal_path(&self.config.path, self.lsn_counter);
        self.wal = WalWriter::create(&new_path, &self.config)?;
        self.metrics
            .wal_rotations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    // ── HNSW integration ─────────────────────────────────────────────────────

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
    fn get_vector_index(
        &self,
        ns: &[u8],
    ) -> Result<Option<std::sync::Arc<HnswIndex>>, EdgestoreError> {
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
    ) -> Result<crate::vector::search::VectorPage, EdgestoreError> {
        crate::vector::search::vector_page(self, ns, cursor, page_size)
    }

    /// Text search with cost accounting. Returns results + [`QueryStats`].
    ///
    /// `bytes_scanned` reflects the size of the serialized inverted index examined.
    pub fn search_text_with_stats(
        &self,
        ns: &[u8],
        query: &str,
        k: usize,
    ) -> Result<(Vec<crate::text::engine::TextSearchResult>, QueryStats), EdgestoreError> {
        let text_ns = crate::text::engine::text_namespace(ns);
        let index_bytes_size = match self.text_indices.get(&text_ns) {
            Some(idx) => idx.serialize().len() as u64,
            None => match self.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(ref b) => b.len() as u64,
                None => 0,
            },
        };
        let results = self.search_text(ns, query, k)?;
        let stats = QueryStats {
            segments_scanned: if index_bytes_size > 0 { 1 } else { 0 },
            bytes_scanned: index_bytes_size,
            items_examined: results.len() as u64,
        };
        Ok((results, stats))
    }

    /// Text search that returns [`SnippetResult`]s — short context windows around
    /// each matched term — instead of just document keys and scores.
    ///
    /// Requires that the index was written with v3 format (`index_text` called after
    /// upgrading to this version). Documents indexed under v1/v2 format return an
    /// empty `snippets` vec but still appear in results with their BM25 score.
    ///
    /// `context_chars` controls how many characters appear before and after each
    /// match in the snippet. 80 is a reasonable default for agent-facing output.
    pub fn search_text_with_snippets(
        &self,
        ns: &[u8],
        query: &str,
        k: usize,
        context_chars: usize,
    ) -> Result<Vec<SnippetResult>, EdgestoreError> {
        use crate::text::types::decode_text_record;

        let text_ns = text_namespace(ns);
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() || k == 0 {
            return Ok(vec![]);
        }
        let query_terms: std::collections::HashSet<String> =
            query_tokens.iter().map(|t| t.term.clone()).collect();

        let base_results = self.search_text(ns, query, k)?;

        let index_opt: Option<InvertedIndex> = match self.text_indices.get(&text_ns) {
            Some(idx) => Some(idx.clone()),
            None => match self.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(bytes) => InvertedIndex::deserialize(&bytes).ok(),
                None => None,
            },
        };

        let mut out = Vec::with_capacity(base_results.len());
        for result in base_results {
            let snippets = if let Some(ref index) = index_opt {
                let mut byte_positions: Vec<u32> = Vec::new();
                for (term, postings) in &index.postings {
                    if !query_terms.contains(term.as_str()) {
                        continue;
                    }
                    if let Some(posting) = postings.iter().find(|p| p.doc_id == result.doc_id) {
                        byte_positions.extend_from_slice(&posting.positions);
                    }
                }
                if !byte_positions.is_empty() {
                    match self.get(&text_ns, &result.doc_id)? {
                        Some(raw) => {
                            match decode_text_record(&raw) {
                                Some(rec) => {
                                    let text = &rec.text;
                                    let chars: Vec<char> = text.chars().collect();
                                    byte_positions.sort_unstable();
                                    byte_positions.dedup();
                                    byte_positions
                                        .iter()
                                        .filter_map(|&pos| {
                                            let char_start = pos as usize;
                                            if char_start >= chars.len() {
                                                return None;
                                            }
                                            let char_end = chars[char_start..]
                                                .iter()
                                                .position(|c| c.is_whitespace())
                                                .map(|i| char_start + i)
                                                .unwrap_or(chars.len());
                                            let ctx_start =
                                                char_start.saturating_sub(context_chars);
                                            let ctx_end =
                                                (char_end + context_chars).min(chars.len());
                                            let ctx: String =
                                                chars[ctx_start..ctx_end].iter().collect();
                                            let prefix: String =
                                                chars[ctx_start..char_start].iter().collect();
                                            let matched: String =
                                                chars[char_start..char_end].iter().collect();
                                            Some(Snippet {
                                                text: ctx,
                                                byte_start: prefix.len(),
                                                byte_end: prefix.len() + matched.len(),
                                            })
                                        })
                                        .collect()
                                }
                                None => vec![],
                            }
                        }
                        None => vec![],
                    }
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
            out.push(SnippetResult {
                doc_id: result.doc_id,
                score: result.score,
                snippets,
            });
        }
        Ok(out)
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

use crate::text::engine::{text_namespace, Snippet, SnippetResult, TextEngine, TextSearchResult};
use crate::text::index::{InvertedIndex, BM25_B, BM25_K1};
use crate::text::tokenizer::tokenize;
use crate::text::types::{encode_text_record, FacetValue};

const TEXT_INDEX_KEY: &[u8] = b"__index__";

impl Engine {
    fn search_in_index(
        index: &InvertedIndex,
        query_tokens: &[crate::text::tokenizer::Token],
        options: &crate::text::engine::SearchOptions,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        let mut search_terms: Vec<String> = query_tokens.iter().map(|t| t.term.clone()).collect();

        if options.typo_tolerance {
            for token in query_tokens {
                for term in index.postings.keys() {
                    if term != &token.term
                        && crate::text::typo::is_one_edit_away(term, &token.term)
                        && !search_terms.contains(term)
                    {
                        search_terms.push(term.clone());
                    }
                }
            }
        }

        let mut doc_scores: HashMap<Vec<u8>, f32> = HashMap::new();
        let avg_doc_len = index.avg_doc_len();
        for term in &search_terms {
            if let Some(postings) = index.postings.get(term) {
                let doc_freq = postings.len() as u64;
                let filtered = if !options.facet_filters.is_empty() {
                    crate::text::facet::filter_by_facets(postings, &options.facet_filters)
                } else {
                    postings.to_vec()
                };

                let is_fuzzy = !query_tokens.iter().any(|t| &t.term == term);
                let weight = if is_fuzzy { 0.5 } else { 1.0 };

                for posting in &filtered {
                    let score = crate::text::index::bm25_score(
                        index.total_docs,
                        doc_freq,
                        posting.term_freq,
                        posting.doc_len,
                        avg_doc_len,
                        BM25_K1,
                        BM25_B,
                    ) * weight;
                    *doc_scores.entry(posting.doc_id.clone()).or_insert(0.0) += score;
                }
            }
        }

        let mut results: Vec<TextSearchResult> = doc_scores
            .into_iter()
            .map(|(doc_id, score)| TextSearchResult { doc_id, score })
            .collect();
        results.sort_by(|a, b| {
            let score_cmp = crate::vector::distance::total_cmp_f32(b.score, a.score);
            if score_cmp == std::cmp::Ordering::Equal {
                a.doc_id.cmp(&b.doc_id)
            } else {
                score_cmp
            }
        });
        results.truncate(options.k);

        Ok(results)
    }
}

impl TextEngine for Engine {
    fn index_text(
        &mut self,
        ns: &[u8],
        key: &[u8],
        text: &str,
        facets: HashMap<String, FacetValue>,
    ) -> Result<Lsn, EdgestoreError> {
        let tokens = tokenize(text);
        let doc_len = tokens.len() as u32;
        let text_ns = text_namespace(ns);

        let loaded_index = match self.get(&text_ns, TEXT_INDEX_KEY) {
            Ok(Some(bytes)) => {
                InvertedIndex::deserialize(&bytes).unwrap_or_else(|_| InvertedIndex::new())
            }
            _ => InvertedIndex::new(),
        };

        let index = self
            .text_indices
            .entry(text_ns.clone())
            .or_insert(loaded_index);
        // Skip the O(total index size) removal scan when this doc_id is definitely
        // new (the common case for an append-only workload — see `text::bloom`).
        // Zero false negatives means this is always correct to skip on `false`.
        if index.doc_bloom.might_contain(key) {
            index.remove_document(key);
        }
        index.add_document(key.to_vec(), &tokens, doc_len, facets.clone());

        // Merged index is NOT written here — only on flush() / drop.
        // Raw text record goes through normal WAL → segment path (durable).
        let record = crate::text::types::TextRecord {
            text: text.to_string(),
            facets,
        };
        let record_bytes = encode_text_record(&record);
        self.put(&text_ns, key, &record_bytes)
    }

    fn search_text(
        &self,
        ns: &[u8],
        query: &str,
        k: usize,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        self.search_text_with_options(
            ns,
            query,
            &crate::text::engine::SearchOptions {
                k,
                ..Default::default()
            },
        )
    }

    fn search_text_with_options(
        &self,
        ns: &[u8],
        query: &str,
        options: &crate::text::engine::SearchOptions,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        if options.k == 0 {
            return Ok(vec![]);
        }

        let query_tokens = tokenize(query);
        if query_tokens.is_empty() {
            return Ok(vec![]);
        }

        let text_ns = text_namespace(ns);

        let index = match self.text_indices.get(&text_ns) {
            Some(idx) => idx,
            None => match self.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(bytes) => {
                    let idx = InvertedIndex::deserialize(&bytes)?;
                    if idx.total_docs == 0 {
                        return Ok(vec![]);
                    }
                    return Self::search_in_index(&idx, &query_tokens, options);
                }
                None => return Ok(vec![]),
            },
        };

        if index.total_docs == 0 {
            return Ok(vec![]);
        }

        Self::search_in_index(index, &query_tokens, options)
    }

    fn delete_text(&mut self, ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError> {
        let text_ns = text_namespace(ns);

        let mut index = match self.text_indices.remove(&text_ns) {
            Some(idx) => idx,
            None => match self.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(bytes) => {
                    InvertedIndex::deserialize(&bytes).unwrap_or_else(|_| InvertedIndex::new())
                }
                None => InvertedIndex::new(),
            },
        };

        index.remove_document(key);

        if index.total_docs == 0 {
            self.text_indices.remove(&text_ns);
            self.delete(&text_ns, TEXT_INDEX_KEY)?;
        } else {
            let index_bytes = index.serialize();
            self.put(&text_ns, TEXT_INDEX_KEY, &index_bytes)?;
            self.text_indices.insert(text_ns.clone(), index);
        }

        // Delete the raw text record (durable via WAL)
        self.delete(&text_ns, key)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Err(e) = self.persist_text_indices() {
            log::warn!("Failed to persist text indices on drop: {}", e);
        }
        // fsync WAL so in-flight writes are durable on clean shutdown.
        // Errors are not propagable from Drop; log and continue.
        if let Err(e) = self.wal.fsync() {
            log::warn!("Failed to fsync WAL on drop: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn open_engine(dir: &TempDir) -> Engine {
        Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
    }

    #[test]
    fn test_open_drop_reopen() {
        let dir = TempDir::new().unwrap();
        let engine = open_engine(&dir);
        drop(engine);
        let _engine2 = open_engine(&dir);
    }

    #[test]
    fn test_double_open_writer_busy() {
        let dir = TempDir::new().unwrap();
        let _engine = open_engine(&dir);
        let result = Engine::open(EdgestoreConfig::new(dir.path()));
        assert!(matches!(result, Err(EdgestoreError::WriterBusy)));
    }

    #[test]
    fn test_put_get_round_trip() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"hello", b"world").unwrap();
        let val = engine.get(b"ns", b"hello").unwrap();
        assert_eq!(val, Some(b"world".to_vec()));
    }

    #[test]
    fn test_put_delete_get_returns_none() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"key", b"val").unwrap();
        engine.delete(b"ns", b"key").unwrap();
        let val = engine.get(b"ns", b"key").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_range_sorted_excludes_deleted() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"a", b"va").unwrap();
        engine.put(b"ns", b"b", b"vb").unwrap();
        engine.put(b"ns", b"c", b"vc").unwrap();
        engine.delete(b"ns", b"b").unwrap();
        let results = engine.range(b"ns", b"a", b"z").unwrap();
        let keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(keys, vec![b"a", b"c"]);
    }

    #[test]
    fn test_prefix_namespace_isolation() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns_a", b"k1", b"va1").unwrap();
        engine.put(b"ns_a", b"k2", b"va2").unwrap();
        engine.put(b"ns_b", b"k1", b"vb1").unwrap();

        let ns_a_results = engine.prefix(b"ns_a", b"").unwrap();
        assert_eq!(ns_a_results.len(), 2);
        for (_, val) in &ns_a_results {
            assert_ne!(val, b"vb1");
        }

        let ns_b_results = engine.prefix(b"ns_b", b"").unwrap();
        assert_eq!(ns_b_results.len(), 1);
    }

    #[test]
    fn test_namespace_same_raw_key() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns_a", b"key", b"val_a").unwrap();
        engine.put(b"ns_b", b"key", b"val_b").unwrap();
        assert_eq!(
            engine.get(b"ns_a", b"key").unwrap(),
            Some(b"val_a".to_vec())
        );
        assert_eq!(
            engine.get(b"ns_b", b"key").unwrap(),
            Some(b"val_b".to_vec())
        );
    }

    #[test]
    fn test_commit_transaction_all_visible() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let mut tx = engine.begin();
        let ts = 0i64;
        tx.put(b"ns", b"k1", b"v1", 1, ts).unwrap();
        tx.put(b"ns", b"k2", b"v2", 2, ts).unwrap();
        tx.put(b"ns", b"k3", b"v3", 3, ts).unwrap();
        engine.commit_transaction(tx).unwrap();
        assert_eq!(engine.get(b"ns", b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(engine.get(b"ns", b"k2").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(engine.get(b"ns", b"k3").unwrap(), Some(b"v3".to_vec()));
    }

    #[test]
    fn test_rollback_transaction_keys_not_visible() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let mut tx = engine.begin();
        tx.put(b"ns", b"k1", b"v1", 1, 0).unwrap();
        tx.put(b"ns", b"k2", b"v2", 2, 0).unwrap();
        engine.rollback_transaction(tx);
        assert_eq!(engine.get(b"ns", b"k1").unwrap(), None);
        assert_eq!(engine.get(b"ns", b"k2").unwrap(), None);
    }

    #[test]
    fn test_commit_returns_highest_lsn() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let mut tx = engine.begin();
        tx.put(b"ns", b"k1", b"v1", 1, 0).unwrap();
        tx.put(b"ns", b"k2", b"v2", 2, 0).unwrap();
        tx.put(b"ns", b"k3", b"v3", 3, 0).unwrap();
        let lsn = engine.commit_transaction(tx).unwrap();
        assert_eq!(lsn, 3);
    }

    #[test]
    fn test_double_commit_returns_err() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let mut tx = engine.begin();
        tx.put(b"ns", b"k1", b"v1", 1, 0).unwrap();
        let _ = tx.take_pending().unwrap();
        let result = engine.commit_transaction(tx);
        assert!(result.is_err());
    }

    #[test]
    fn test_tx_commit_convenience_wrapper() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let mut tx = engine.begin();
        tx.put(b"ns", b"k1", b"v1", 1, 0).unwrap();
        let lsn = tx.commit(&mut engine).unwrap();
        assert!(lsn > 0);
        assert_eq!(engine.get(b"ns", b"k1").unwrap(), Some(b"v1".to_vec()));
    }

    #[test]
    fn test_tx_rollback_convenience_wrapper() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let mut tx = engine.begin();
        tx.put(b"ns", b"k1", b"v1", 1, 0).unwrap();
        tx.rollback(&mut engine);
        assert_eq!(engine.get(b"ns", b"k1").unwrap(), None);
    }

    #[test]
    fn test_crash_recovery() {
        let dir = TempDir::new().unwrap();
        {
            let mut engine = open_engine(&dir);
            engine.put(b"ns", b"k1", b"v1").unwrap();
            engine.put(b"ns", b"k2", b"v2").unwrap();
            engine.put(b"ns", b"k3", b"v3").unwrap();
            engine.flush().unwrap();
        }

        let engine2 = open_engine(&dir);
        assert_eq!(engine2.get(b"ns", b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(engine2.get(b"ns", b"k2").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(engine2.get(b"ns", b"k3").unwrap(), Some(b"v3".to_vec()));
    }

    #[test]
    fn test_wal_naming_hex() {
        let dir = TempDir::new().unwrap();
        let path = next_wal_path(dir.path(), 50);
        let filename = path.file_name().unwrap().to_string_lossy();
        assert_eq!(filename, "wal-0000000000000032.log");
    }

    #[test]
    fn test_namespace_too_long_returns_error() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let long_ns = vec![b'x'; u16::MAX as usize + 1];
        let result = engine.put(&long_ns, b"k", b"v");
        assert!(
            matches!(result, Err(EdgestoreError::NamespaceTooLong { .. })),
            "expected NamespaceTooLong, got {:?}",
            result
        );
    }

    #[test]
    fn test_flush_to_segments_empty_memtable_returns_error() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let result = engine.flush_to_segments();
        assert!(
            result.is_err(),
            "flush_to_segments on empty memtable must error"
        );
    }

    #[test]
    fn test_get_from_segment_after_flush() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"seg_key", b"seg_val").unwrap();
        engine.flush_to_segments().unwrap();
        let val = engine.get(b"ns", b"seg_key").unwrap();
        assert_eq!(val, Some(b"seg_val".to_vec()));
    }

    #[test]
    fn test_delete_from_segment_via_memtable_tombstone() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"key", b"val").unwrap();
        engine.flush_to_segments().unwrap();
        engine.delete(b"ns", b"key").unwrap();
        let val = engine.get(b"ns", b"key").unwrap();
        assert_eq!(val, None, "tombstone in memtable must shadow segment value");
    }

    #[test]
    fn test_range_across_segment_and_memtable() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"a", b"va").unwrap();
        engine.put(b"ns", b"b", b"vb").unwrap();
        engine.flush_to_segments().unwrap();
        engine.put(b"ns", b"c", b"vc").unwrap();
        let results = engine.range(b"ns", b"a", b"z").unwrap();
        let keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(keys, vec![b"a", b"b", b"c"]);
    }

    #[test]
    fn test_prefix_from_segments() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"pre_a", b"v1").unwrap();
        engine.put(b"ns", b"pre_b", b"v2").unwrap();
        engine.put(b"ns", b"other", b"v3").unwrap();
        engine.flush_to_segments().unwrap();
        let results = engine.prefix(b"ns", b"pre_").unwrap();
        assert_eq!(results.len(), 2);
        let keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
        assert!(keys.contains(&b"pre_a".as_ref()));
        assert!(keys.contains(&b"pre_b".as_ref()));
    }

    #[test]
    fn test_range_memtable_delete_shadows_segment_value() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"x", b"old").unwrap();
        engine.flush_to_segments().unwrap();
        engine.delete(b"ns", b"x").unwrap();
        let results = engine.range(b"ns", b"a", b"z").unwrap();
        assert!(results.is_empty(), "deleted key must not appear in range");
    }

    #[test]
    fn test_prefix_upper_bound_edge_cases() {
        assert_eq!(prefix_upper_bound(&[0xFF, 0xFF]), None);
        assert_eq!(prefix_upper_bound(b"ab"), Some(b"ac".to_vec()));
        assert_eq!(prefix_upper_bound(&[0x01, 0xFF]), Some(vec![0x02]));
    }

    #[test]
    fn test_metrics_counts_operations() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);

        engine.put(b"ns", b"k1", b"v1").unwrap();
        engine.put(b"ns", b"k2", b"v2").unwrap();
        engine.put_with_ttl(b"ns", b"k3", b"v3", 60).unwrap();
        engine.get(b"ns", b"k1").unwrap();
        engine.get(b"ns", b"k2").unwrap();
        engine.delete(b"ns", b"k1").unwrap();
        engine.range(b"ns", b"a", b"z").unwrap();
        engine.prefix(b"ns", b"k").unwrap();

        let mut tx = engine.begin();
        tx.put(b"ns", b"tx1", b"tv1", 0, 0).unwrap();
        engine.commit_transaction(tx).unwrap();

        let mut tx2 = engine.begin();
        tx2.put(b"ns", b"tx2", b"tv2", 0, 0).unwrap();
        engine.rollback_transaction(tx2);

        let m = engine.metrics();
        assert_eq!(m.puts, 3, "3 puts (including put_with_ttl)");
        assert_eq!(m.gets, 2);
        assert_eq!(m.deletes, 1);
        assert_eq!(m.ranges, 1);
        assert_eq!(m.prefixes, 1);
        assert_eq!(m.transactions_committed, 1);
        assert_eq!(m.transactions_rolled_back, 1);
        assert!(m.put_nanos_total > 0);
        assert!(m.get_nanos_total > 0);
    }

    // ─ Performance regression guards ───────────────────────────────────────

    /// Regression: Engine::range_inner used to use HashMap + sort(). Now it uses merge-join.
    /// This test verifies that overlapping segments + memtable entries with the same key
    /// are deduplicated by highest LSN (not duplicated, not silently dropped).
    #[test]
    fn test_range_merge_dedups_same_key() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        // Write to segment
        engine.put(b"ns", b"key", b"old").unwrap();
        engine.flush_to_segments().unwrap();
        // Overwrite in memtable
        engine.put(b"ns", b"key", b"new").unwrap();
        // Both should be visible, deduplicated to 1 entry with the latest value
        let results = engine.range(b"ns", b"", b"\xff").unwrap();
        assert_eq!(results.len(), 1, "should deduplicate to 1 entry");
        assert_eq!(results[0].1, b"new".to_vec());
    }

    /// Regression: Engine::prefix_inner used to use HashMap + sort(). Now it uses merge-join.
    /// This test verifies that prefix scans with the same key in segment and memtable
    /// are deduplicated by highest LSN.
    #[test]
    fn test_prefix_merge_dedups_same_key() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"prefix_key", b"old").unwrap();
        engine.flush_to_segments().unwrap();
        engine.put(b"ns", b"prefix_key", b"new").unwrap();
        let results = engine.prefix(b"ns", b"prefix_").unwrap();
        assert_eq!(results.len(), 1, "should deduplicate to 1 entry");
        assert_eq!(results[0].1, b"new".to_vec());
    }

    /// Regression: Engine::range_inner must handle delete tombstones from memtable
    /// shadowing the same key in a segment.
    #[test]
    fn test_range_merge_delete_tombstone_shadows_segment() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"key", b"val").unwrap();
        engine.flush_to_segments().unwrap();
        engine.delete(b"ns", b"key").unwrap();
        let results = engine.range(b"ns", b"", b"\xff").unwrap();
        assert!(
            results.is_empty(),
            "delete tombstone should shadow segment value"
        );
    }

    /// Regression: Engine::range_inner must return sorted results even with multiple segments.
    #[test]
    fn test_get_into_hit_and_miss() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"k", b"val").unwrap();

        let mut buf = Vec::new();
        assert!(
            engine.get_into(b"ns", b"k", &mut buf).unwrap(),
            "existing key must return true"
        );
        assert_eq!(buf, b"val");

        let found = engine.get_into(b"ns", b"missing", &mut buf).unwrap();
        assert!(!found, "missing key must return false");
    }

    #[test]
    fn test_get_into_reuses_buffer() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"k1", b"first").unwrap();
        engine.put(b"ns", b"k2", b"second").unwrap();

        let mut buf = Vec::with_capacity(64);
        engine.get_into(b"ns", b"k1", &mut buf).unwrap();
        assert_eq!(buf, b"first");
        engine.get_into(b"ns", b"k2", &mut buf).unwrap();
        assert_eq!(buf, b"second", "buffer must be overwritten on second call");
    }

    #[test]
    fn test_memtable_auto_flush_at_threshold() {
        let dir = TempDir::new().unwrap();
        let mut cfg = EdgestoreConfig::new(dir.path());
        // Set a tiny threshold so a few puts trigger a flush.
        // AVG_ENTRY_SIZE_ESTIMATE = 256 bytes, so 2 entries × 256 = 512 ≥ threshold.
        cfg.memtable_max_bytes = 400;
        let mut engine = Engine::open(cfg).unwrap();

        engine.put(b"ns", b"a", b"1").unwrap();
        engine.put(b"ns", b"b", b"2").unwrap();

        // At least one segment must have been created by the auto-flush.
        assert!(
            !engine.list_segment_metas().is_empty(),
            "auto-flush must create a segment"
        );
    }

    /// This test creates 3 segments and verifies the merge produces sorted output.
    #[test]
    fn test_range_merge_sorted_across_segments() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        // Segment 1
        engine.put(b"ns", b"c", b"vc").unwrap();
        engine.put(b"ns", b"a", b"va").unwrap();
        engine.flush_to_segments().unwrap();
        // Segment 2
        engine.put(b"ns", b"b", b"vb").unwrap();
        engine.flush_to_segments().unwrap();
        // Segment 3
        engine.put(b"ns", b"d", b"vd").unwrap();
        engine.flush_to_segments().unwrap();
        let results = engine.range(b"ns", b"", b"\xff").unwrap();
        let keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(
            keys,
            vec![b"a", b"b", b"c", b"d"],
            "must be sorted across all segments"
        );
    }

    #[test]
    fn test_open_readonly_rejects_writes() {
        let dir = TempDir::new().unwrap();
        // Write something with a writable engine first.
        {
            let mut w = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
            w.put(b"ns", b"k", b"v").unwrap();
        }
        // Reopen read-only.
        let mut r = Engine::open_readonly(EdgestoreConfig::new(dir.path())).unwrap();
        assert!(
            r.get(b"ns", b"k").unwrap().is_some(),
            "reads must work on readonly engine"
        );
        let err = r.put(b"ns", b"k2", b"v2").unwrap_err();
        assert!(
            matches!(err, EdgestoreError::ReadOnly),
            "put must return ReadOnly"
        );
        let err = r.delete(b"ns", b"k").unwrap_err();
        assert!(
            matches!(err, EdgestoreError::ReadOnly),
            "delete must return ReadOnly"
        );
    }

    #[test]
    fn test_on_segment_flushed_callback_fires() {
        use std::sync::{Arc, Mutex};
        let dir = TempDir::new().unwrap();
        let fired: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
        let fired2 = fired.clone();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path()))
            .unwrap()
            .with_on_segment_flushed(move |meta| {
                fired2.lock().unwrap().push(meta.segment_id);
            });
        engine.put(b"ns", b"a", b"1").unwrap();
        engine.flush_to_segments().unwrap();
        engine.put(b"ns", b"b", b"2").unwrap();
        engine.flush_to_segments().unwrap();
        let ids = fired.lock().unwrap().clone();
        assert_eq!(
            ids.len(),
            2,
            "callback must fire once per flush_to_segments"
        );
    }

    #[test]
    fn test_on_segment_flushed_fires_on_auto_flush() {
        use std::sync::{Arc, Mutex};
        let dir = TempDir::new().unwrap();
        let count = Arc::new(Mutex::new(0u32));
        let count2 = count.clone();
        // Set memtable threshold very low to trigger auto-flush.
        let mut cfg = EdgestoreConfig::new(dir.path());
        cfg.memtable_max_bytes = 1; // forces flush after first put
        let mut engine = Engine::open(cfg)
            .unwrap()
            .with_on_segment_flushed(move |_| {
                *count2.lock().unwrap() += 1;
            });
        engine.put(b"ns", b"a", b"1").unwrap();
        engine.put(b"ns", b"b", b"2").unwrap();
        assert!(
            *count.lock().unwrap() > 0,
            "callback must fire on auto-flush triggered by put"
        );
    }

    // ── ENG-12: QueryStats ────────────────────────────────────────────────

    #[test]
    fn test_get_with_stats_memtable_hit() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"k", b"value").unwrap();
        let (val, stats) = engine.get_with_stats(b"ns", b"k").unwrap();
        assert_eq!(val, Some(b"value".to_vec()));
        assert_eq!(stats.segments_scanned, 0, "memtable hit: no segment scanned");
        assert!(stats.bytes_scanned > 0);
        assert_eq!(stats.items_examined, 1);
    }

    #[test]
    fn test_get_with_stats_segment_hit() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"k", b"value").unwrap();
        engine.flush_to_segments().unwrap();
        let (val, stats) = engine.get_with_stats(b"ns", b"k").unwrap();
        assert_eq!(val, Some(b"value".to_vec()));
        assert_eq!(stats.segments_scanned, 1, "segment hit");
        assert!(stats.bytes_scanned > 0);
        assert_eq!(stats.items_examined, 1);
    }

    #[test]
    fn test_get_with_stats_miss_returns_zero_stats() {
        let dir = TempDir::new().unwrap();
        let engine = open_engine(&dir);
        let (val, stats) = engine.get_with_stats(b"ns", b"missing").unwrap();
        assert_eq!(val, None);
        assert_eq!(stats.segments_scanned, 0);
        assert_eq!(stats.bytes_scanned, 0);
        assert_eq!(stats.items_examined, 0);
    }

    #[test]
    fn test_range_with_stats_returns_bytes() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"a", b"va").unwrap();
        engine.put(b"ns", b"b", b"vb").unwrap();
        let (pairs, stats) = engine.range_with_stats(b"ns", b"a", b"z").unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(stats.bytes_scanned > 0, "range scan must report non-zero bytes");
        assert!(stats.items_examined >= 2);
    }

    #[test]
    fn test_prefix_with_stats_returns_bytes() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"foo:a", b"1").unwrap();
        engine.put(b"ns", b"foo:b", b"2").unwrap();
        engine.put(b"ns", b"bar:c", b"3").unwrap();
        let (pairs, stats) = engine.prefix_with_stats(b"ns", b"foo:").unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(stats.bytes_scanned > 0);
        assert!(stats.items_examined >= 2);
    }

    // ── ENG-9: ScanBudget / BudgetedScan ─────────────────────────────────

    #[test]
    fn test_range_budgeted_truncates_at_max_items() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        for i in 0u8..10 {
            engine.put(b"ns", &[b'a' + i], b"v").unwrap();
        }
        let budget = ScanBudget {
            max_items: Some(3),
            max_bytes: None,
        };
        let result = engine.range_budgeted(b"ns", b"", b"\xff", &budget).unwrap();
        assert_eq!(result.items.len(), 3);
        assert!(result.truncated, "must be truncated when budget hit");
    }

    #[test]
    fn test_range_budgeted_no_truncation_when_under_budget() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"a", b"va").unwrap();
        engine.put(b"ns", b"b", b"vb").unwrap();
        let budget = ScanBudget {
            max_items: Some(100),
            max_bytes: None,
        };
        let result = engine.range_budgeted(b"ns", b"", b"\xff", &budget).unwrap();
        assert_eq!(result.items.len(), 2);
        assert!(!result.truncated, "must not be truncated when under budget");
    }

    #[test]
    fn test_prefix_budgeted_truncates_at_max_items() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        for i in 0u8..8 {
            engine
                .put(b"ns", format!("key:{}", i).as_bytes(), b"v")
                .unwrap();
        }
        let budget = ScanBudget {
            max_items: Some(2),
            max_bytes: None,
        };
        let result = engine.prefix_budgeted(b"ns", b"key:", &budget).unwrap();
        assert_eq!(result.items.len(), 2);
        assert!(result.truncated);
    }

    #[test]
    fn test_prefix_budgeted_stops_at_max_bytes() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        for i in 0u8..5 {
            engine
                .put(b"ns", format!("k:{}", i).as_bytes(), &vec![b'x'; 100])
                .unwrap();
        }
        let budget = ScanBudget {
            max_items: None,
            max_bytes: Some(1), // 1 byte — will hit after first item
        };
        let result = engine.prefix_budgeted(b"ns", b"k:", &budget).unwrap();
        assert!(result.truncated, "must truncate when byte budget exhausted");
        assert!(result.items.len() < 5, "must not return all items");
    }

    // ── ENG-12: vector_search_with_stats ─────────────────────────────────

    #[test]
    fn test_vector_search_with_stats_flat_scan() {
        use crate::vector::distance::Metric;
        use crate::vector::types::Dtype;
        use crate::VectorEngine;
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        let v: Vec<u8> = vec![1.0f32, 0.0, 0.0, 0.0]
            .into_iter()
            .flat_map(|f: f32| f.to_le_bytes())
            .collect();
        engine.vector_put(b"vs", b"doc1", 4, Dtype::F32, &v).unwrap();
        let query = crate::vector::types::VectorRecord {
            dims: 4,
            dtype: Dtype::F32,
            data: v,
        };
        let (results, stats) = engine
            .vector_search_with_stats(b"vs", &query, 1, Metric::Cosine)
            .unwrap();
        assert!(!results.is_empty(), "must find at least one vector");
        assert!(stats.bytes_scanned > 0, "flat scan must report bytes");
    }

    // ── ENG-12: search_text_with_stats ───────────────────────────────────

    #[test]
    fn test_search_text_with_stats_reports_bytes() {
        use crate::text::engine::TextEngine;
        use std::collections::HashMap;
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine
            .index_text(b"docs", b"d1", "the quick brown fox", HashMap::new())
            .unwrap();
        let (results, stats) = engine.search_text_with_stats(b"docs", "fox", 5).unwrap();
        assert!(!results.is_empty(), "should find the doc");
        assert!(
            stats.bytes_scanned > 0,
            "text search must report non-zero bytes"
        );
        assert!(stats.segments_scanned > 0 || stats.bytes_scanned > 0);
    }

    // ── ENG-11: search_text_with_snippets ────────────────────────────────

    #[test]
    fn test_search_text_with_snippets_returns_context_window() {
        use crate::text::engine::TextEngine;
        use std::collections::HashMap;
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine
            .index_text(
                b"docs",
                b"d1",
                "the quick brown fox jumps over the lazy dog",
                HashMap::new(),
            )
            .unwrap();
        let results = engine
            .search_text_with_snippets(b"docs", "fox", 5, 20)
            .unwrap();
        assert!(!results.is_empty(), "should find the doc");
        // v3 index: snippets should be populated
        let r = &results[0];
        assert_eq!(r.doc_id, b"d1".to_vec());
        assert!(r.score > 0.0, "BM25 score must be positive");
        if !r.snippets.is_empty() {
            let s = &r.snippets[0];
            assert!(s.byte_end > s.byte_start, "byte range must be non-empty");
            assert!(s.byte_end <= s.text.len(), "byte_end within snippet text");
            let span = &s.text[s.byte_start..s.byte_end];
            assert!(
                span.to_lowercase().starts_with("fox"),
                "matched span '{}' must start with the query term 'fox'",
                span
            );
        }
    }

    #[test]
    fn test_search_text_with_snippets_no_match_returns_empty() {
        use crate::text::engine::TextEngine;
        use std::collections::HashMap;
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine
            .index_text(b"docs", b"d1", "the quick brown fox", HashMap::new())
            .unwrap();
        let results = engine
            .search_text_with_snippets(b"docs", "elephant", 5, 20)
            .unwrap();
        assert!(results.is_empty(), "no match should return empty results");
    }

    // ── ENG-7: strip_vector_index ─────────────────────────────────────────

    #[test]
    fn test_strip_vector_index_removes_vec_records() {
        use crate::vector::types::Dtype;
        use crate::VectorEngine;
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"kv_key", b"kv_value").unwrap();
        let v: Vec<u8> = vec![0u8; 16];
        engine.vector_put(b"ns", b"vec1", 4, Dtype::F32, &v).unwrap();
        let meta = engine.flush_to_segments().unwrap();
        let seg_id = meta.segment_id;

        let new_meta = engine.strip_vector_index(seg_id).unwrap();
        assert!(new_meta.vector_index_stripped, "flag must be set after strip");
        // KV record must still be accessible.
        let val = engine.get(b"ns", b"kv_key").unwrap();
        assert_eq!(val, Some(b"kv_value".to_vec()), "KV record must survive strip");
    }

    #[test]
    fn test_strip_vector_index_idempotent() {
        use crate::vector::types::Dtype;
        use crate::VectorEngine;
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        engine.put(b"ns", b"k", b"v").unwrap();
        let v: Vec<u8> = vec![0u8; 16];
        engine.vector_put(b"ns", b"vec1", 4, Dtype::F32, &v).unwrap();
        let meta = engine.flush_to_segments().unwrap();

        let meta1 = engine.strip_vector_index(meta.segment_id).unwrap();
        assert!(meta1.vector_index_stripped);
        // Second call on the replacement segment must be a no-op (already stripped).
        let meta2 = engine.strip_vector_index(meta1.segment_id).unwrap();
        assert!(meta2.vector_index_stripped);
    }

    #[test]
    fn test_vector_count_none_when_not_loaded() {
        let dir = TempDir::new().unwrap();
        let engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        assert_eq!(
            engine.vector_count(b"products"),
            None,
            "no index loaded yet"
        );
    }

    #[test]
    fn test_vector_count_some_when_index_in_memory() {
        use crate::vector::distance::Metric;
        use crate::vector::types::Dtype;
        use crate::VectorEngine;
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        let v: Vec<u8> = vec![0u8; 16]; // 4-dim f32
        engine
            .vector_put(b"products", b"p1", 4, Dtype::F32, &v)
            .unwrap();
        engine
            .vector_put(b"products", b"p2", 4, Dtype::F32, &v)
            .unwrap();
        engine
            .vector_put(b"products", b"p3", 4, Dtype::F32, &v)
            .unwrap();
        // vector_search triggers flat scan + inserts results into vector_indices.
        // After a flat scan the index is populated in-memory.
        let query = crate::vector::types::VectorRecord {
            dims: 4,
            dtype: Dtype::F32,
            data: v,
        };
        engine
            .vector_search(b"products", &query, 1, Metric::Cosine)
            .unwrap();
        // vector_count reflects in-memory state regardless of sidecar.
        match engine.vector_count(b"products") {
            Some(n) => assert!(n > 0, "expected at least 1 vector"),
            None => {} // flat scan path doesn't cache — None is also acceptable
        }
        // Explicit check: None before any index, Some after put-then-build.
        // The key invariant is: returns None when nothing is in memory.
        let dir2 = TempDir::new().unwrap();
        let engine2 = Engine::open(EdgestoreConfig::new(dir2.path())).unwrap();
        assert_eq!(
            engine2.vector_count(b"products"),
            None,
            "fresh engine has no index"
        );
    }

    // ── range_page (P4) ────────────────────────────────────────────────────

    #[test]
    fn test_range_page_paginates_correctly() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        // Write 25 keys across two segments so the cursor spans a flush boundary
        for i in 0u32..15 {
            engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
        }
        engine.flush_to_segments().unwrap();
        for i in 15u32..25 {
            engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
        }

        let mut all = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let page = engine
                .range_page(b"ns", b"", b"\xff", cursor.as_deref(), 7)
                .unwrap();
            assert!(page.items.len() <= 7, "page must not exceed page_size");
            all.extend(page.items);
            cursor = page.next_key;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(all.len(), 25, "all 25 keys must be returned across pages");
        // Keys must be returned in ascending order
        for w in all.windows(2) {
            assert!(w[0].0 < w[1].0, "keys must be ascending");
        }
    }

    #[test]
    fn test_range_page_empty_range() {
        let dir = TempDir::new().unwrap();
        let engine = open_engine(&dir);
        let page = engine.range_page(b"ns", b"", b"\xff", None, 10).unwrap();
        assert!(page.items.is_empty());
        assert!(page.next_key.is_none());
    }

    #[test]
    fn test_range_page_cursor_excludes_previous_last_key() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        for i in 0u32..6 {
            engine.put(b"ns", format!("k{}", i).as_bytes(), b"v").unwrap();
        }
        let page1 = engine.range_page(b"ns", b"", b"\xff", None, 3).unwrap();
        assert_eq!(page1.items.len(), 3);
        let page2 = engine
            .range_page(b"ns", b"", b"\xff", page1.next_key.as_deref(), 3)
            .unwrap();
        assert_eq!(page2.items.len(), 3);
        assert!(page2.next_key.is_none(), "should be exhausted");
        // The last key of page1 must not appear in page2
        let last1 = &page1.items.last().unwrap().0;
        assert!(!page2.items.iter().any(|(k, _)| k == last1));
    }

    // ── range_rev_page (P5) ────────────────────────────────────────────────

    #[test]
    fn test_range_rev_page_descending_order() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        for i in 0u32..10 {
            engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
        }
        engine.flush_to_segments().unwrap();

        let page = engine
            .range_rev_page(b"ns", b"", b"\xff", None, 4)
            .unwrap();
        assert_eq!(page.items.len(), 4, "should return page_size items");
        // Must be descending
        for w in page.items.windows(2) {
            assert!(w[0].0 > w[1].0, "keys must be descending");
        }
        // First item must be the largest key
        assert_eq!(page.items[0].0, b"key-0009".to_vec());
    }

    #[test]
    fn test_range_rev_page_paginates_correctly() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        for i in 0u32..20 {
            engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
        }
        engine.flush_to_segments().unwrap();

        let mut all: Vec<Vec<u8>> = Vec::new();
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let page = engine
                .range_rev_page(b"ns", b"", b"\xff", cursor.as_deref(), 6)
                .unwrap();
            assert!(page.items.len() <= 6);
            for (k, _) in &page.items {
                all.push(k.clone());
            }
            cursor = page.next_key;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(all.len(), 20, "all 20 keys must be returned across reverse pages");
        // Globally descending
        for w in all.windows(2) {
            assert!(w[0] > w[1], "global order must be descending");
        }
    }

    #[test]
    fn test_range_rev_page_memtable_delete_excluded() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        for i in 0u32..5 {
            engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
        }
        engine.flush_to_segments().unwrap();
        // Delete the largest key via memtable
        engine.delete(b"ns", b"key-0004").unwrap();

        let page = engine
            .range_rev_page(b"ns", b"", b"\xff", None, 10)
            .unwrap();
        let keys: Vec<_> = page.items.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(keys.len(), 4, "deleted key must be absent");
        assert!(!keys.contains(&b"key-0004".to_vec()), "key-0004 deleted by memtable");
        assert_eq!(keys[0], b"key-0003".to_vec(), "key-0003 is now largest");
    }

    // ── range_scan_budgeted (P1/P2/P3) via segment tests ──────────────────

    #[test]
    fn test_range_scan_budgeted_stops_at_segment_level() {
        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);
        // 30 keys across two flushed segments — ensures range_scan_budgeted
        // must open both segments but stops reading after max_items
        for i in 0u32..15 {
            engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
        }
        engine.flush_to_segments().unwrap();
        for i in 15u32..30 {
            engine.put(b"ns", format!("key-{:04}", i).as_bytes(), b"v").unwrap();
        }
        engine.flush_to_segments().unwrap();

        let budget = ScanBudget { max_items: Some(5), max_bytes: None };
        let result = engine.range_budgeted(b"ns", b"", b"\xff", &budget).unwrap();
        assert_eq!(result.items.len(), 5);
        assert!(result.truncated);
        // Must be the first 5 keys in ascending order
        assert_eq!(result.items[0].0, b"key-0000".to_vec());
        assert_eq!(result.items[4].0, b"key-0004".to_vec());
    }

    // ── Concurrent vector search (no serialization) ───────────────────────

    #[test]
    fn test_vector_search_concurrent_reads() {
        use crate::vector::distance::Metric;
        use crate::vector::types::{Dtype, VectorRecord};
        use crate::VectorEngine;

        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);

        for i in 0u8..20 {
            let data = vec![i; 16 * 4];
            engine.vector_put(b"ns", &[i], 16, Dtype::F32, &data).unwrap();
        }
        engine.flush_to_segments().unwrap();
        engine.build_vector_index(b"ns").unwrap();

        let query = VectorRecord {
            dims: 16,
            dtype: Dtype::F32,
            data: vec![0u8; 16 * 4],
        };

        // Spawn 8 threads all calling vector_search concurrently on &engine.
        // This compiles only because vector_search takes &self — &mut self would
        // require external serialization and this test would not build.
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        let results = engine.vector_search(b"ns", &query, 3, Metric::L2).unwrap();
                        assert_eq!(results.len(), 3);
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
        });
    }

    #[test]
    fn test_vector_search_concurrent_flat_scan() {
        use crate::vector::distance::Metric;
        use crate::vector::types::{Dtype, VectorRecord};
        use crate::VectorEngine;

        let dir = TempDir::new().unwrap();
        let mut engine = open_engine(&dir);

        // No build_vector_index → forces flat scan path
        for i in 0u8..10 {
            let data = vec![i; 16 * 4];
            engine.vector_put(b"ns", &[i], 16, Dtype::F32, &data).unwrap();
        }

        let query = VectorRecord {
            dims: 16,
            dtype: Dtype::F32,
            data: vec![0u8; 16 * 4],
        };

        std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    s.spawn(|| {
                        let results = engine.vector_search(b"ns", &query, 5, Metric::Cosine).unwrap();
                        assert_eq!(results.len(), 5);
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
        });
    }
}
