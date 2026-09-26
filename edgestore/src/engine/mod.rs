use fs2::FileExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::config::EdgestoreConfig;
use crate::error::EdgestoreError;
use crate::memtable::MemTable;
use crate::metrics::{EngineMetrics, MetricsSnapshot};
use crate::types::{
    decode_key, encode_key, prefix_upper_bound, Lsn, MemEntry, Operation, WalRecord,
};
use crate::vector::hnsw::HnswIndex;
use crate::wal::WalWriter;

pub(crate) mod replication;
pub(crate) mod vector;

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
    /// Segment data is permanently unprocessable (e.g. corrupt block that passes BLAKE3
    /// but fails decompression or exceeds size caps). Do not retry.
    Rejected {
        /// Human-readable reason for permanent rejection.
        reason: String,
    },
}

/// Single-writer KV engine with WAL, segments, compaction, and optional vector/text indexes.
pub struct Engine {
    pub(crate) config: EdgestoreConfig,
    pub(crate) wal: WalWriter,
    pub(crate) memtable: Box<dyn MemTable>,
    pub(crate) lsn_counter: u64,
    pub(crate) txid_counter: u64,
    #[allow(dead_code)]
    lockfile: std::fs::File,
    pub(crate) segment_store: crate::segment::SegmentStore,
    pub(crate) snapshot_registry: crate::snapshot::SnapshotRegistry,
    /// Monotonically increasing write token for primary fencing.
    ///
    /// Persisted to `{db_path}/WTOKEN`. A replica being promoted to primary increments
    /// this token so the old primary's segments are rejected by anti-entropy loops that
    /// have already observed the higher token. Inspired by BtrLog §4.2.
    write_token: u64,
    pub(crate) metrics: EngineMetrics,
    pub(crate) vector_indices: std::sync::RwLock<HashMap<Vec<u8>, std::sync::Arc<HnswIndex>>>,
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

        let write_token = Self::load_write_token_from_path(&config.path)?;

        let engine = Engine {
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
            on_segment_flushed: None,
            write_token,
        };

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

    // -----------------------------------------------------------------------
    // Commit watermark & write token  (BtrLog §4.2 — arXiv:2606.27051)
    // -----------------------------------------------------------------------

    /// Highest LSN whose segment is durably flushed and visible to replicas.
    ///
    /// Computed as `max(meta.max_lsn)` across all segment metas in the manifest.
    /// In-flight WAL records that have not yet been flushed to a segment are NOT
    /// counted — replicas use this value to decide what is safe to serve.
    ///
    /// Returns 0 when no segments have been written yet.
    pub fn confirmed_lsn(&self) -> u64 {
        self.list_segment_metas()
            .iter()
            .map(|m| m.max_lsn)
            .max()
            .unwrap_or(0)
    }

    /// Current LSN counter — the LSN that will be assigned to the next write.
    ///
    /// Monotonically increasing. Corresponds to `lsn` in `edgestore.tla`. Use
    /// `confirmed_lsn` for the highest LSN durably stored in a segment.
    pub fn current_lsn(&self) -> u64 {
        self.lsn_counter
    }

    /// Current write token.
    ///
    /// A monotonically increasing u64 that survives restarts.  When a replica is
    /// promoted to primary it increments this token so anti-entropy loops on peers
    /// that already observed the old token will recognise the topology change and
    /// can refuse or warn on stale segments.
    pub fn write_token(&self) -> u64 {
        self.write_token
    }

    /// Persist a new write token to disk and update the in-memory value.
    ///
    /// Callers must guarantee `token > self.write_token()` for fencing to hold —
    /// `ReplicatedEngine::promote_to_primary` enforces this invariant.
    pub fn set_write_token(&mut self, token: u64) -> Result<(), EdgestoreError> {
        let path = self.config.path.join("WTOKEN");
        std::fs::write(&path, token.to_le_bytes())?;
        self.write_token = token;
        Ok(())
    }

    /// Load the write token from `{db_path}/WTOKEN`.  Returns 0 for a new database.
    fn load_write_token_from_path(db_path: &std::path::Path) -> Result<u64, EdgestoreError> {
        let path = db_path.join("WTOKEN");
        match std::fs::read(&path) {
            Ok(bytes) if bytes.len() == 8 => {
                Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
            }
            Ok(_) => {
                log::warn!("WTOKEN file is corrupt (wrong length); resetting to 0");
                Ok(0)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(EdgestoreError::Io(e)),
        }
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

    pub(crate) fn now_nanos() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64
    }

    /// Sanitize a namespace for use in filesystem paths.
    pub(crate) fn ns_to_slug(ns: &[u8]) -> String {
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

    /// Flush the current memtable to a new on-disk segment.
    ///
    /// After a successful flush:
    /// - The memtable is cleared.
    /// - WAL files older than the current one are deleted (they are now redundant
    ///   for crash recovery — the segment is the durable record).
    /// - The `on_segment_flushed` callback fires if configured.
    ///
    /// Returns `Err` if the memtable is empty (nothing to flush).
    ///
    /// This is also called automatically when the memtable exceeds
    /// `EdgestoreConfig::memtable_max_bytes`.
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

    /// fsync the current WAL file.
    pub fn flush(&mut self) -> Result<(), EdgestoreError> {
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
    /// just want to reclaim local disk space after a successful archive.
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
        // Retire WAL files whose data is now durably stored in the segment.
        // Keep only the current WAL file — it may hold entries written after this flush.
        let current_wal_name = self.wal.path().file_name().map(|n| n.to_os_string());
        if let Ok(wal_files) = crate::recovery::list_wal_files(&self.config.path) {
            for path in &wal_files {
                if path.file_name().map(|n| n.to_os_string()) != current_wal_name {
                    let _ = std::fs::remove_file(path);
                }
            }
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
}

impl Engine {
    /// Rotate the WAL if the current writer has exceeded `wal_max_bytes` or `wal_max_age_secs`.
    ///
    /// Called after every append so long-running sessions rotate inline without requiring
    /// a close/reopen cycle.
    pub(crate) fn rotate_wal_if_needed(&mut self) -> Result<(), EdgestoreError> {
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
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Err(e) = self.wal.fsync() {
            log::warn!("Failed to fsync WAL on drop: {}", e);
        }
    }
}

#[cfg(test)]
mod tests;
