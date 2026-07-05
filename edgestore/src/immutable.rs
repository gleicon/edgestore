//! ImmutableEngine — read-only engine for serverless / edge / WASM environments.
//!
//! Holds segments entirely in memory. No WAL, no memtable, no local filesystem.
//! Initialized from a manifest + downloaded segment bytes.
//!
//! Phase 9 deliverable. See `.planning/phases/09-readonly-edge/09-00-PLAN.md`.

use std::collections::BinaryHeap;

use crate::error::EdgestoreError;
use crate::segment::in_memory::InMemorySegmentReader;
use crate::types::{decode_key, encode_key, MemEntry, Operation, SegmentMeta};

/// Read-only engine that serves from in-memory segments.
///
/// No local path. No WAL. No lock. All segments are `InMemorySegmentReader`
/// instances backed by downloaded bytes. After initialization, reads are
/// purely in-memory with no I/O.
pub struct ImmutableEngine {
    readers: Vec<InMemorySegmentReader>,
}

impl ImmutableEngine {
    /// Create an `ImmutableEngine` from pre-built in-memory segment readers.
    ///
    /// The caller is responsible for downloading segments and converting them
    /// to `InMemorySegmentReader` via `InMemorySegmentReader::from_bytes`.
    pub fn from_readers(readers: Vec<InMemorySegmentReader>) -> Self {
        Self { readers }
    }

    /// Create an `ImmutableEngine` from a list of `(SegmentMeta, bytes)` pairs.
    ///
    /// Each pair is parsed into an `InMemorySegmentReader`. This is the
    /// eager-init path: all segments are parsed upfront.
    pub fn from_segment_bytes(
        segments: Vec<(SegmentMeta, Vec<u8>)>,
    ) -> Result<Self, EdgestoreError> {
        let mut readers = Vec::with_capacity(segments.len());
        for (meta, bytes) in segments {
            let hash: [u8; 32] = meta.segment_hash.as_slice().try_into().map_err(|_| {
                EdgestoreError::ReplicationError(format!(
                    "segment {} hash is not 32 bytes",
                    meta.segment_id
                ))
            })?;
            let reader = InMemorySegmentReader::from_bytes(meta.segment_id, hash, meta, &bytes)?;
            readers.push(reader);
        }
        Ok(Self { readers })
    }

    /// Look up a single key across all segments, highest-LSN wins.
    pub fn get(&self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        let encoded = encode_key(ns, key);
        let mut best: Option<MemEntry> = None;

        for reader in &self.readers {
            if let Some(entry) = reader.get(&encoded)? {
                let is_better = best.as_ref().is_none_or(|b| entry.lsn > b.lsn);
                if is_better {
                    best = Some(entry);
                }
            }
        }

        match best {
            Some(e) if e.op == Operation::Put => Ok(e.value),
            _ => Ok(None),
        }
    }

    /// Iterate over key-value pairs in `[start, end)` within a namespace.
    ///
    /// Reads from all segments, merges using LWW (highest LSN per key), and
    /// returns a sorted vec of `(raw_key, value)` pairs with deletes filtered out.
    #[allow(clippy::type_complexity)]
    pub fn range(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let enc_start = encode_key(ns, start);
        let enc_end = encode_key(ns, end);

        let mut per_reader: Vec<Vec<(Vec<u8>, MemEntry)>> = Vec::with_capacity(self.readers.len());
        let mut total_len = 0usize;
        for reader in &self.readers {
            let mut seg = reader.range_scan(&enc_start, &enc_end)?;
            seg.sort_by(|(a, _), (b, _)| a.cmp(b));
            total_len += seg.len();
            per_reader.push(seg);
        }

        #[derive(Eq, PartialEq)]
        struct Item<'a> {
            key: &'a [u8],
            entry: &'a MemEntry,
            reader_idx: usize,
            elem_idx: usize,
        }
        impl<'a> Ord for Item<'a> {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                let key_cmp = other.key.cmp(self.key);
                if key_cmp != std::cmp::Ordering::Equal {
                    return key_cmp;
                }
                other.entry.lsn.cmp(&self.entry.lsn)
            }
        }
        impl<'a> PartialOrd for Item<'a> {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        let mut heap = BinaryHeap::new();
        for (ri, seg) in per_reader.iter().enumerate() {
            if let Some((k, e)) = seg.first() {
                heap.push(Item { key: k, entry: e, reader_idx: ri, elem_idx: 0 });
            }
        }

        let mut results: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(total_len);
        let mut last_key: Option<Vec<u8>> = None;
        let mut last_entry: Option<MemEntry> = None;
        while let Some(item) = heap.pop() {
            let seg = &per_reader[item.reader_idx];
            let next_idx = item.elem_idx + 1;
            if next_idx < seg.len() {
                let (k, e) = &seg[next_idx];
                heap.push(Item { key: k, entry: e, reader_idx: item.reader_idx, elem_idx: next_idx });
            }
            match last_key {
                Some(ref lk) if lk == item.key => {
                    if let Some(ref le) = last_entry {
                        if item.entry.lsn > le.lsn {
                            last_entry = Some(item.entry.clone());
                        }
                    }
                }
                _ => {
                    if let Some(e) = last_entry.take() {
                        if e.op != Operation::Delete {
                            if let Some(lk) = last_key {
                                let (_, user_key) = decode_key(&lk)?;
                                if let Some(val) = e.value {
                                    results.push((user_key, val));
                                }
                            }
                        }
                    }
                    last_key = Some(item.key.to_vec());
                    last_entry = Some(item.entry.clone());
                }
            }
        }
        if let Some(e) = last_entry {
            if e.op != Operation::Delete {
                if let Some(lk) = last_key {
                    let (_, user_key) = decode_key(&lk)?;
                    if let Some(val) = e.value {
                        results.push((user_key, val));
                    }
                }
            }
        }
        Ok(results)
    }

    /// Prefix scan within a namespace.
    #[allow(clippy::type_complexity)]
    pub fn prefix(
        &self,
        ns: &[u8],
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let enc_prefix = encode_key(ns, prefix);
        let mut end = enc_prefix.clone();
        if let Some(last) = end.last_mut() {
            *last = last.wrapping_add(1);
        } else {
            end.push(1);
        }
        self.range_encoded(&enc_prefix, &end)
    }

    #[allow(clippy::type_complexity)]
    fn range_encoded(
        &self,
        enc_start: &[u8],
        enc_end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let mut per_reader: Vec<Vec<(Vec<u8>, MemEntry)>> = Vec::with_capacity(self.readers.len());
        let mut total_len = 0usize;
        for reader in &self.readers {
            let mut seg = reader.range_scan(enc_start, enc_end)?;
            seg.sort_by(|(a, _), (b, _)| a.cmp(b));
            total_len += seg.len();
            per_reader.push(seg);
        }

        #[derive(Eq, PartialEq)]
        struct Item<'a> {
            key: &'a [u8],
            entry: &'a MemEntry,
            reader_idx: usize,
            elem_idx: usize,
        }
        impl<'a> Ord for Item<'a> {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                let key_cmp = other.key.cmp(self.key);
                if key_cmp != std::cmp::Ordering::Equal {
                    return key_cmp;
                }
                other.entry.lsn.cmp(&self.entry.lsn)
            }
        }
        impl<'a> PartialOrd for Item<'a> {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        let mut heap = BinaryHeap::new();
        for (ri, seg) in per_reader.iter().enumerate() {
            if let Some((k, e)) = seg.first() {
                heap.push(Item { key: k, entry: e, reader_idx: ri, elem_idx: 0 });
            }
        }

        let mut results: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(total_len);
        let mut last_key: Option<Vec<u8>> = None;
        let mut last_entry: Option<MemEntry> = None;
        while let Some(item) = heap.pop() {
            let seg = &per_reader[item.reader_idx];
            let next_idx = item.elem_idx + 1;
            if next_idx < seg.len() {
                let (k, e) = &seg[next_idx];
                heap.push(Item { key: k, entry: e, reader_idx: item.reader_idx, elem_idx: next_idx });
            }
            match last_key {
                Some(ref lk) if lk == item.key => {
                    if let Some(ref le) = last_entry {
                        if item.entry.lsn > le.lsn {
                            last_entry = Some(item.entry.clone());
                        }
                    }
                }
                _ => {
                    if let Some(e) = last_entry.take() {
                        if e.op != Operation::Delete {
                            if let Some(lk) = last_key {
                                let (_, user_key) = decode_key(&lk)?;
                                if let Some(val) = e.value {
                                    results.push((user_key, val));
                                }
                            }
                        }
                    }
                    last_key = Some(item.key.to_vec());
                    last_entry = Some(item.entry.clone());
                }
            }
        }
        if let Some(e) = last_entry {
            if e.op != Operation::Delete {
                if let Some(lk) = last_key {
                    let (_, user_key) = decode_key(&lk)?;
                    if let Some(val) = e.value {
                        results.push((user_key, val));
                    }
                }
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::SegmentWriter;
    use crate::types::{encode_key, MemEntry, Operation};
    use tempfile::TempDir;

    fn make_put_entry(key: &[u8], value: &[u8], lsn: u64) -> MemEntry {
        MemEntry {
            key: key.to_vec(),
            value: Some(value.to_vec()),
            op: Operation::Put,
            lsn,
            timestamp: 3_600_000_000_000,
            ttl: 0,
        }
    }

    fn build_engine_with_segments(segments_data: Vec<Vec<(Vec<u8>, MemEntry)>>) -> ImmutableEngine {
        let dir = TempDir::new().unwrap();
        let mut readers = Vec::new();
        for (seg_id, entries) in segments_data.into_iter().enumerate() {
            let mut sorted = entries;
            sorted.sort_by(|(a, _), (b, _)| a.cmp(b));
            let mut writer = SegmentWriter::new(dir.path().to_path_buf(), seg_id as u64, 3600);
            let meta = writer.flush(&sorted).unwrap();
            let dat_bytes = std::fs::read(dir.path().join(format!("segment-{:08}.dat", seg_id))).unwrap();
            let hash: [u8; 32] = meta.segment_hash.as_slice().try_into().unwrap();
            let reader = InMemorySegmentReader::from_bytes(seg_id as u64, hash, meta, &dat_bytes).unwrap();
            readers.push(reader);
        }
        ImmutableEngine::from_readers(readers)
    }

    #[test]
    fn test_immutable_get_single_segment() {
        let ns = b"ns";
        let key = encode_key(ns, b"key1");
        let entry = make_put_entry(&key, b"val1", 1);

        let engine = build_engine_with_segments(vec![vec![(key.clone(), entry)]]);
        let got = engine.get(ns, b"key1").unwrap();
        assert_eq!(got, Some(b"val1".to_vec()));
    }

    #[test]
    fn test_immutable_get_absent() {
        let ns = b"ns";
        let key = encode_key(ns, b"key1");
        let entry = make_put_entry(&key, b"val1", 1);

        let engine = build_engine_with_segments(vec![vec![(key, entry)]]);
        let got = engine.get(ns, b"missing").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn test_immutable_lww_higher_lsn_wins() {
        let ns = b"ns";
        let key = encode_key(ns, b"shared");

        // Segment 0: old value, lsn=1.
        let old_entry = make_put_entry(&key, b"old", 1);
        // Segment 1: new value, lsn=5.
        let new_entry = make_put_entry(&key, b"new", 5);

        let engine = build_engine_with_segments(vec![
            vec![(key.clone(), old_entry)],
            vec![(key.clone(), new_entry)],
        ]);

        let got = engine.get(ns, b"shared").unwrap();
        assert_eq!(got, Some(b"new".to_vec()), "higher LSN wins");
    }

    #[test]
    fn test_immutable_range_sorted_deduped() {
        let ns = b"ns";
        let mut seg1 = Vec::new();
        let mut seg2 = Vec::new();
        for i in 0..5u64 {
            let key = encode_key(ns, format!("key-{}", i).as_bytes());
            seg1.push((key.clone(), make_put_entry(&key, b"seg1", i + 1)));
        }
        for i in 3..8u64 {
            let key = encode_key(ns, format!("key-{}", i).as_bytes());
            seg2.push((key.clone(), make_put_entry(&key, b"seg2", i + 10))); // higher LSN
        }

        let engine = build_engine_with_segments(vec![seg1, seg2]);
        let results = engine.range(ns, b"key-2", b"key-6").unwrap();

        // key-2 (only seg1), key-3 (seg2 wins), key-4 (seg2 wins), key-5 (seg2 wins)
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].0, b"key-2");
        assert_eq!(results[0].1, b"seg1");
        assert_eq!(results[1].1, b"seg2", "seg2 has higher LSN for key-3");
    }

    #[test]
    fn test_immutable_prefix() {
        let ns = b"ns";
        let mut seg = Vec::new();
        for i in 0..10u64 {
            let key = encode_key(ns, format!("pref-{}", i).as_bytes());
            seg.push((key, make_put_entry(&encode_key(ns, format!("pref-{}", i).as_bytes()), b"val", i + 1)));
        }

        let engine = build_engine_with_segments(vec![seg]);
        let results = engine.prefix(ns, b"pref-").unwrap();
        assert_eq!(results.len(), 10);
    }

    #[test]
    fn test_immutable_delete_tombstone_filtered() {
        let ns = b"ns";
        let key = encode_key(ns, b"deleted");
        let put_entry = make_put_entry(&key, b"val", 1);
        let del_entry = MemEntry {
            key: key.clone(),
            value: None,
            op: Operation::Delete,
            lsn: 2,
            timestamp: 3_600_000_000_001,
            ttl: 0,
        };

        let engine = build_engine_with_segments(vec![vec![
            (key.clone(), put_entry),
            (key, del_entry),
        ]]);

        let got = engine.get(ns, b"deleted").unwrap();
        assert!(got.is_none(), "delete tombstone must filter out the key");

        let range = engine.range(ns, b"", b"\xff").unwrap();
        assert!(range.is_empty(), "range must exclude deleted keys");
    }

    #[test]
    fn test_immutable_from_segment_bytes() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";
        let key = encode_key(ns, b"key1");
        let entry = make_put_entry(&key, b"val1", 1);
        let mut entries = vec![(key, entry)];
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let meta = writer.flush(&entries).unwrap();
        let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();

        let engine = ImmutableEngine::from_segment_bytes(vec![(meta, dat_bytes)]).unwrap();
        let got = engine.get(ns, b"key1").unwrap();
        assert_eq!(got, Some(b"val1".to_vec()));
    }

    #[test]
    fn test_immutable_large_segment_10k() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";

        let mut entries: Vec<(Vec<u8>, MemEntry)> = (0..10_000u64).map(|i| {
            let enc = encode_key(ns, &i.to_be_bytes());
            let e = make_put_entry(&enc, b"value", i + 1);
            (enc, e)
        }).collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let meta = writer.flush(&entries).unwrap();
        let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();

        let engine = ImmutableEngine::from_segment_bytes(vec![(meta, dat_bytes)]).unwrap();

        for i in [0u64, 5000, 9999] {
            let got = engine.get(ns, &i.to_be_bytes()).unwrap();
            assert_eq!(got, Some(b"value".to_vec()), "key {} must be found", i);
        }

        let results = engine.range(ns, &0u64.to_be_bytes(), &10_000u64.to_be_bytes()).unwrap();
        assert_eq!(results.len(), 10_000, "range must include all 10K keys");
    }
}
