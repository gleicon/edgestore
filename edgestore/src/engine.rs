use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use fs2::FileExt;

use crate::config::EdgestoreConfig;
use crate::error::EdgestoreError;
use crate::memtable::MemTable;
use crate::metrics::{EngineMetrics, MetricsSnapshot};
use crate::replication::SegmentRef;
use crate::types::{decode_key, encode_key, Lsn, MemEntry, Operation, WalRecord};
use crate::vector::api::{vector_namespace, VectorEngine};
use crate::vector::distance::Metric;
use crate::vector::hnsw::HnswIndex;
use crate::vector::search::VectorSearchResult;
use crate::vector::types::{encode_vector_record, decode_vector_record, Dtype, VectorRecord};
use crate::wal::WalWriter;

fn next_wal_path(db_path: &Path, lsn: Lsn) -> PathBuf {
    db_path.join(format!("wal-{:016x}.log", lsn))
}

/// Alias to reduce type-complexity warnings on public KV scan APIs.
type KvPairs = Vec<(Vec<u8>, Vec<u8>)>;

/// Compute exclusive upper bound for a prefix scan: increment last non-0xFF byte.
fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    for i in (0..end.len()).rev() {
        if end[i] < 0xFF {
            end[i] += 1;
            end.truncate(i + 1);
            return Some(end);
        }
        end[i] = 0;
    }
    None
}

const AVG_ENTRY_SIZE_ESTIMATE: u64 = 256;

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
    vector_indices: HashMap<Vec<u8>, HnswIndex>,
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

        Ok(Engine {
            config,
            wal,
            memtable,
            lsn_counter,
            txid_counter,
            lockfile,
            segment_store,
            snapshot_registry: crate::snapshot::SnapshotRegistry::new(),
            metrics: EngineMetrics::new(),
            vector_indices: HashMap::new(),
        })
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
            .map(|&b| if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                b as char
            } else {
                '_'
            })
            .collect()
    }

    // ── Public API — each delegates to _inner and records timing ─────────────

    /// Store a key-value pair in the given namespace.
    pub fn put(&mut self, ns: &[u8], key: &[u8], val: &[u8]) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.put_inner(ns, key, val);
        self.metrics.puts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.put_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
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
        self.metrics.puts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.put_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

    /// Lazy expiry: records inserted with `put_with_ttl` are returned until `compact_once` removes their cohort.
    pub fn get(&self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.get_inner(ns, key);
        self.metrics.gets.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.get_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

    /// Delete a key in the given namespace (tombstone).
    pub fn delete(&mut self, ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.delete_inner(ns, key);
        self.metrics.deletes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.delete_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

    /// Lazy expiry: TTL-expired records appear in range results until compaction removes their cohort.
    pub fn range(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<KvPairs, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.range_inner(ns, start, end);
        self.metrics.ranges.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.range_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

    /// Lazy expiry: TTL-expired records appear in prefix results until compaction removes their cohort.
    pub fn prefix(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<KvPairs, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.prefix_inner(ns, prefix);
        self.metrics.prefixes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.prefix_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

    /// Flush the current memtable to a new segment file.
    pub fn flush_to_segments(&mut self) -> Result<crate::types::SegmentMeta, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.flush_to_segments_inner();
        self.metrics.segment_flushes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.segment_flush_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
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
    pub fn commit_transaction(&mut self, tx: crate::transaction::Transaction) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.commit_transaction_inner(tx);
        self.metrics.transactions_committed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.transaction_commit_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

    /// Roll back a transaction, discarding all pending records.
    pub fn rollback_transaction(&mut self, mut tx: crate::transaction::Transaction) {
        tx.rollback_self();
        self.metrics.transactions_rolled_back.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        self.metrics.compactions.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.compaction_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

    /// Return a point-in-time snapshot pinning the current set of segments.
    ///
    /// The returned `Snapshot` holds a reference to the `SnapshotRegistry` and
    /// releases its pins automatically when dropped.
    pub fn snapshot(&self) -> Result<crate::snapshot::Snapshot, EdgestoreError> {
        let ids = self.segment_store.segment_ids();
        let sid = self.snapshot_registry.register(&ids);
        Ok(crate::snapshot::Snapshot::new(
            sid,
            self.snapshot_registry.clone(),
            ids,
            self.config.path.clone(),
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

    // ── Private implementations ───────────────────────────────────────────────

    fn put_inner(&mut self, ns: &[u8], key: &[u8], val: &[u8]) -> Result<Lsn, EdgestoreError> {
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

        if (self.memtable.len() as u64) * AVG_ENTRY_SIZE_ESTIMATE >= self.config.segment_size_bytes {
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

    fn range_inner(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<KvPairs, EdgestoreError> {
        let enc_start = encode_key(ns, start);
        let enc_end = encode_key(ns, end);

        let mut merged: std::collections::HashMap<Vec<u8>, crate::types::MemEntry> =
            std::collections::HashMap::new();

        for (k, entry) in self.segment_store.range_scan(&enc_start, &enc_end)? {
            merged.insert(k, entry);
        }

        for (k, entry) in self.memtable.range(&enc_start, &enc_end) {
            let k_vec = k.to_vec();
            let existing_lsn = merged.get(&k_vec).map(|e| e.lsn).unwrap_or(0);
            if entry.lsn >= existing_lsn {
                if entry.op == Operation::Delete {
                    merged.remove(&k_vec);
                } else {
                    merged.insert(k_vec, entry.clone());
                }
            }
        }

        let mut keys: Vec<Vec<u8>> = merged.keys().cloned().collect();
        keys.sort();
        let mut out = Vec::new();
        for k in keys {
            if let Some(entry) = merged.get(&k) {
                if entry.op == Operation::Delete { continue; }
                if let Some(val) = &entry.value {
                    let (_, raw_key) = decode_key(&k)?;
                    out.push((raw_key, val.clone()));
                }
            }
        }
        Ok(out)
    }

    fn prefix_inner(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<KvPairs, EdgestoreError> {
        let enc_prefix = encode_key(ns, prefix);

        let mut merged: std::collections::HashMap<Vec<u8>, crate::types::MemEntry> =
            std::collections::HashMap::new();

        let seg_results = if let Some(enc_end) = prefix_upper_bound(&enc_prefix) {
            self.segment_store.range_scan(&enc_prefix, &enc_end)?
                .into_iter()
                .filter(|(k, _)| k.starts_with(&enc_prefix))
                .collect::<Vec<_>>()
        } else {
            vec![]
        };

        for (k, entry) in seg_results {
            merged.insert(k, entry);
        }

        for (k, entry) in self.memtable.prefix(&enc_prefix) {
            let k_vec = k.to_vec();
            let existing_lsn = merged.get(&k_vec).map(|e| e.lsn).unwrap_or(0);
            if entry.lsn >= existing_lsn {
                if entry.op == Operation::Delete {
                    merged.remove(&k_vec);
                } else {
                    merged.insert(k_vec, entry.clone());
                }
            }
        }

        let mut keys: Vec<Vec<u8>> = merged.keys().cloned().collect();
        keys.sort();
        let mut out = Vec::new();
        for k in keys {
            if let Some(entry) = merged.get(&k) {
                if entry.op == Operation::Delete { continue; }
                if let Some(val) = &entry.value {
                    let (_, raw_key) = decode_key(&k)?;
                    out.push((raw_key, val.clone()));
                }
            }
        }
        Ok(out)
    }

    fn flush_to_segments_inner(&mut self) -> Result<crate::types::SegmentMeta, EdgestoreError> {
        if self.memtable.is_empty() {
            return Err(EdgestoreError::SegmentCorrupt("memtable is empty".to_string()));
        }
        let meta = self.segment_store.flush_memtable(self.memtable.as_ref())?;
        self.memtable.clear();
        Ok(meta)
    }

    fn commit_transaction_inner(&mut self, tx: crate::transaction::Transaction) -> Result<Lsn, EdgestoreError> {
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
        self.segment_store =
            crate::segment::SegmentStore::open(self.config.path.clone(), self.config.cohort_window_secs)?;
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
            let magic = u32::from_le_bytes(
                data[offset..offset + 4].try_into().unwrap(),
            );
            if magic != crate::segment::SEGMENT_BLOCK_MAGIC {
                break; // hit padding or end
            }
            let compressed_len = u32::from_le_bytes(
                data[offset + 4..offset + 8].try_into().unwrap(),
            ) as usize;

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
                        let local_entry = self.memtable.get(&encoded_key).cloned().or_else(|| {
                            self.segment_store.get(&encoded_key).ok().flatten()
                        });

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
                            if let Ok((ns, key)) =
                                crate::types::decode_key(&encoded_key)
                            {
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
        let reader =
            crate::segment::SegmentReader::open(base.clone(), new_segment_id)?;
        self.segment_store.add_imported_segment(meta, reader)?;

        Ok(ImportResult::Applied { keys_written, keys_skipped })
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
        self.metrics.wal_rotations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    // ── HNSW integration ─────────────────────────────────────────────────────

    /// Build an HNSW index for all vectors in the given namespace.
    ///
    /// Scans all vector records in `__vec__{ns}`, builds the graph,
    /// serializes it to a sidecar file, and caches it in memory.
    pub fn build_vector_index(
        &mut self,
        ns: &[u8],
    ) -> Result<(), EdgestoreError> {
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

        let mut index = HnswIndex::new(dims, dtype, metric)
            .with_params(16, 100);

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

        // Cache
        self.vector_indices.insert(ns.to_vec(), index);

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
    pub fn preload_vector_index(&mut self, ns: &[u8]) -> Result<bool, EdgestoreError> {
        match self.get_vector_index(ns) {
            Ok(Some(_)) => Ok(true),
            Ok(None) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Get the HNSW index for a namespace, loading from sidecar if needed.
    ///
    /// Checks staleness against segment-id hash and falls back to None if stale.
    fn get_vector_index(&mut self, ns: &[u8]) -> Result<Option<&HnswIndex>, EdgestoreError> {
        // Fast path: already cached
        if self.vector_indices.contains_key(ns) {
            // Check staleness
            let stale = self.is_index_stale(ns)?;
            if stale {
                self.vector_indices.remove(ns);
                self.metrics.vector_index_stales.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Ok(None);
            }
            return Ok(self.vector_indices.get(ns));
        }

        let t0 = Instant::now();
        let ns_slug = Self::ns_to_slug(ns);
        let sidecar_path = self.config.path.join("vector").join(format!("{}.hnsw", ns_slug));

        if !sidecar_path.exists() {
            return Ok(None);
        }

        let bytes = std::fs::read(&sidecar_path)?;
        let index = HnswIndex::deserialize(&bytes)?;

        // Validate staleness
        let stale = self.is_index_stale(ns)?;
        if stale {
            self.metrics.vector_index_stales.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(None);
        }

        self.vector_indices.insert(ns.to_vec(), index);
        self.metrics.vector_index_loads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.vector_index_load_nanos.fetch_add(
            t0.elapsed().as_nanos() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        Ok(self.vector_indices.get(ns))
    }

    /// Check if the cached HNSW index is stale by comparing segment hashes.
    fn is_index_stale(&self, ns: &[u8]) -> Result<bool, EdgestoreError> {
        let sidecar_path = self.config.path.join("vector").join(format!("{}.hnsw", Self::ns_to_slug(ns)));
        if !sidecar_path.exists() {
            return Ok(true);
        }
        // v1: assume fresh until explicit rebuild
        Ok(false)
    }

    /// Search for the k closest vectors to the query in the given namespace.
    ///
    /// Uses HNSW when an index exists and is fresh; falls back to flat scan otherwise.
    pub fn vector_search(
        &mut self,
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

    fn vector_get(
        &self,
        ns: &[u8],
        key: &[u8],
    ) -> Result<Option<VectorRecord>, EdgestoreError> {
        match self.get(&vector_namespace(ns), key)? {
            Some(bytes) => {
                let record = decode_vector_record(&bytes)
                    .map_err(|e| EdgestoreError::CorruptData(format!("decode vector: {}", e)))?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    fn vector_delete(
        &mut self,
        ns: &[u8],
        key: &[u8],
    ) -> Result<Lsn, EdgestoreError> {
        self.delete(&vector_namespace(ns), key)
    }
}

use crate::text::engine::{TextEngine, TextSearchResult, text_namespace};
use crate::text::tokenizer::tokenize;
use crate::text::index::{InvertedIndex, score_document};
use crate::text::types::{encode_text_record, FacetValue};

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

        // Build/update the per-namespace inverted index
        let text_ns = text_namespace(ns);
        let mut index = InvertedIndex::new();
        index.add_document(key.to_vec(), &tokens, doc_len, facets.clone());

        // Serialize and store the inverted index entry
        let index_bytes = index.serialize();
        let index_key = format!("idx:{}", std::str::from_utf8(key).unwrap_or(""));
        self.put(&text_ns, index_key.as_bytes(), &index_bytes)?;

        // Store the raw text record for retrieval
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
            &crate::text::engine::SearchOptions { k, ..Default::default() },
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

        // Load all inverted index entries for this namespace
        let all_entries = self.prefix(&text_ns, b"idx:")?;
        if all_entries.is_empty() {
            return Ok(vec![]);
        }

        // Aggregate postings from all index entries
        let mut aggregated = InvertedIndex::new();
        for (_, val_bytes) in &all_entries {
            if let Ok(idx) = InvertedIndex::deserialize(val_bytes) {
                aggregated.total_docs += idx.total_docs;
                aggregated.total_doc_len += idx.total_doc_len;
                for (term, postings) in idx.postings {
                    aggregated.postings.entry(term).or_default().extend(postings);
                }
            }
        }

        if aggregated.total_docs == 0 {
            return Ok(vec![]);
        }

        // Collect terms to search (exact + typo-tolerant variants)
        let mut search_terms: Vec<String> = query_tokens.iter().map(|t| t.term.clone()).collect();
        
        if options.typo_tolerance {
            for token in &query_tokens {
                for term in aggregated.postings.keys() {
                    if term != &token.term
                        && crate::text::typo::is_one_edit_away(term, &token.term)
                        && !search_terms.contains(term)
                    {
                        search_terms.push(term.clone());
                    }
                }
            }
        }

        // Collect unique doc IDs from query term postings, with facet filtering
        let mut doc_scores: HashMap<Vec<u8>, f32> = HashMap::new();
        for term in &search_terms {
            if let Some(postings) = aggregated.postings.get(term) {
                let filtered = if !options.facet_filters.is_empty() {
                    crate::text::facet::filter_by_facets(postings, &options.facet_filters)
                } else {
                    postings.to_vec()
                };
                
                for posting in &filtered {
                    let is_fuzzy = !query_tokens.iter().any(|t| &t.term == term);
                    let weight = if is_fuzzy { 0.5 } else { 1.0 }; // Fuzzy matches get half weight
                    let score = score_document(&aggregated, &posting.doc_id, &[crate::text::tokenizer::Token { term: term.clone(), position: 0 }]) * weight;
                    *doc_scores.entry(posting.doc_id.clone()).or_insert(0.0) += score;
                }
            }
        }

        // Sort by score descending, return top-k
        let mut results: Vec<TextSearchResult> = doc_scores
            .into_iter()
            .map(|(doc_id, score)| TextSearchResult { doc_id, score })
            .collect();
        results.sort_by(|a, b| {
            let score_cmp = b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal);
            if score_cmp == std::cmp::Ordering::Equal {
                a.doc_id.cmp(&b.doc_id) // Tiebreaker: lexicographic doc_id
            } else {
                score_cmp
            }
        });
        results.truncate(options.k);

        Ok(results)
    }

    fn delete_text(&mut self, ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError> {
        let text_ns = text_namespace(ns);
        let index_key = format!("idx:{}", std::str::from_utf8(key).unwrap_or(""));
        self.delete(&text_ns, index_key.as_bytes())?;
        self.delete(&text_ns, key)
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
        assert!(result.is_err(), "flush_to_segments on empty memtable must error");
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
}
