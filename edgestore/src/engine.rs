use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use fs2::FileExt;

use crate::config::EdgestoreConfig;
use crate::error::EdgestoreError;
use crate::memtable::MemTable;
use crate::merkle::RangeMerkleTree;
use crate::metrics::{EngineMetrics, MetricsSnapshot};
use crate::replication::SegmentRef;
use crate::types::{decode_key, encode_key, Lsn, MemEntry, Operation, WalRecord};
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
    Applied { keys_written: u64, keys_skipped: u64 },
    /// Segment already present in local manifest — no-op.
    Skipped,
    /// BLAKE3 of provided data does not match claimed hash — segment rejected.
    HashMismatch,
}

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
}

impl Engine {
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
        })
    }

    fn now_nanos() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64
    }

    // ── Public API — each delegates to _inner and records timing ─────────────

    pub fn put(&mut self, ns: &[u8], key: &[u8], val: &[u8]) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.put_inner(ns, key, val);
        self.metrics.puts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.put_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

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

    pub fn flush_to_segments(&mut self) -> Result<crate::types::SegmentMeta, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.flush_to_segments_inner();
        self.metrics.segment_flushes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.segment_flush_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

    pub fn flush(&mut self) -> Result<(), EdgestoreError> {
        self.wal.fsync()
    }

    pub fn begin(&mut self) -> crate::transaction::Transaction {
        self.txid_counter += 1;
        crate::transaction::Transaction::new(self.txid_counter)
    }

    pub fn commit_transaction(&mut self, tx: crate::transaction::Transaction) -> Result<Lsn, EdgestoreError> {
        let t0 = Instant::now();
        let r = self.commit_transaction_inner(tx);
        self.metrics.transactions_committed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.metrics.transaction_commit_nanos.fetch_add(t0.elapsed().as_nanos() as u64, std::sync::atomic::Ordering::Relaxed);
        r
    }

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
                                    }
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

        // Build SegmentMeta from the written data.
        let now_nanos = crate::engine::Engine::now_nanos();
        let segment_hash_vec: Vec<u8> = hash.to_vec();
        // Use minimal meta — the reader will be rebuilt from the canonical path.
        let meta = crate::types::SegmentMeta {
            segment_id: new_segment_id,
            segment_hash: segment_hash_vec,
            min_key: vec![],
            max_key: vec![],
            min_lsn: 0,
            max_lsn: 0,
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

        // Build an xf filter from nothing (empty — keys already in memtable).
        let xf_path = base.join(format!("segment-{:08}.xf", new_segment_id));
        let empty_keys: Vec<Vec<u8>> = vec![];
        let filter = crate::segment::build_xor_filter(&empty_keys)?;
        crate::segment::write_xf_file(&filter, &xf_path)?;

        // Write the .meta JSON file.
        let meta_path = base.join(format!("segment-{:08}.meta", new_segment_id));
        let meta_file = std::fs::File::create(&meta_path)?;
        serde_json::to_writer_pretty(meta_file, &meta)
            .map_err(|e| EdgestoreError::SegmentCorrupt(format!("import meta serialize: {}", e)))?;

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

    /// Returns the local RangeMerkleTree root for anti-entropy probing (D02).
    pub fn range_merkle_root(&self) -> Result<[u8; 32], EdgestoreError> {
        let metas = self.segment_store.list_segment_metas();
        let refs: Vec<&crate::types::SegmentMeta> = metas.iter().collect();
        let tree = RangeMerkleTree::build(&refs);
        Ok(tree.root())
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
