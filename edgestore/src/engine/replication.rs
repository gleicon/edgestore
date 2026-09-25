use std::collections::HashSet;

use super::Engine;
use super::ImportResult;
use crate::error::EdgestoreError;
use crate::replication::SegmentRef;
use crate::types::{Lsn, MemEntry};

impl Engine {
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
    /// - `Ok(ImportResult::Rejected { reason })` if the segment passes BLAKE3 but is permanently
    ///   unprocessable (corrupt block, oversized decompressed payload). Do not retry.
    /// - `Ok(ImportResult::Applied { keys_written, keys_skipped })` on success.
    ///
    /// // LWW correctness requires NTP synchronization. Clock skew > segment flush interval
    /// // can cause incorrect merge outcomes.
    pub fn import_segment(
        &mut self,
        data: &[u8],
        hash: &[u8; 32],
    ) -> Result<ImportResult, EdgestoreError> {
        let hash_vec: Vec<u8> = hash.to_vec();
        let already_present = self
            .segment_store
            .list_segment_metas()
            .iter()
            .any(|m| m.segment_hash == hash_vec);
        if already_present {
            return Ok(ImportResult::Skipped);
        }

        let computed: [u8; 32] = *blake3::hash(data).as_bytes();
        if computed != *hash {
            return Ok(ImportResult::HashMismatch);
        }

        let hash_hex: String = {
            let mut s = String::with_capacity(64);
            for b in hash { s.push_str(&format!("{b:02x}")); }
            s
        };
        let base = self.segment_store.base_path().to_path_buf();
        let tmp_path = base.join(format!("{}.tmp", hash_hex));
        let dat_path = base.join(format!("{}.dat", hash_hex));

        std::fs::write(&tmp_path, data)?;

        std::fs::rename(&tmp_path, &dat_path)?;

        let mut keys_written: u64 = 0;
        let mut keys_skipped: u64 = 0;
        let mut segment_keys: Vec<Vec<u8>> = Vec::new();
        let mut min_key: Option<Vec<u8>> = None;
        let mut max_key: Option<Vec<u8>> = None;
        let mut min_lsn: Lsn = u64::MAX;
        let mut max_lsn: Lsn = 0;

        let parsed = match crate::segment::parse_dat_entries(data) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ImportResult::Rejected {
                    reason: e.to_string(),
                });
            }
        };
        for (encoded_key, incoming) in parsed {
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

            // LWW: newer wall-clock timestamp wins; favor local on tie.
            let local_entry = self
                .memtable
                .get(&encoded_key)
                .cloned()
                .or_else(|| self.segment_store.get(&encoded_key).ok().flatten());

            let apply = match local_entry {
                None => true,
                Some(ref local) => incoming.timestamp > local.timestamp,
            };

            if apply {
                if let Ok((ns, key)) = crate::types::decode_key(&encoded_key) {
                    if incoming.op == crate::types::Operation::Put {
                        if let Some(ref val) = incoming.value {
                            self.put_with_timestamp(&ns, &key, val, incoming.timestamp)?;
                            keys_written += 1;
                        } else {
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

        let new_segment_id = self.segment_store.alloc_segment_id();
        // Re-read the segment using SegmentReader::open after we register the dat file properly.
        // The imported segment .dat is stored under hash_hex.dat, but SegmentReader expects
        // segment-{id:08}.dat format. Rename to the canonical segment file path.
        let canonical_dat = base.join(format!("segment-{:08}.dat", new_segment_id));
        std::fs::rename(&dat_path, &canonical_dat)?;

        // Flush WAL to ensure LWW-applied records are durable.
        self.wal.fsync()?;

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

        // Build xor filter from decoded keys (empty filter broke post-restart reads).
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

    /// Returns the local RangeMerkleTree root for anti-entropy probing.
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
    /// determine what to pull.
    pub fn compare_merkle(&self, other_root: &[u8; 32]) -> Result<bool, EdgestoreError> {
        let local_root = self.range_merkle_root()?;
        Ok(local_root == *other_root)
    }
}
