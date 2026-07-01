use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::error::EdgestoreError;
use crate::segment::SegmentReader;
use crate::types::{decode_key, encode_key, MemEntry, Operation, SegmentId};

// ── SnapshotRegistryInner ──────────────────────────────────────────────────

/// Inner (unshared) state for `SnapshotRegistry`.
struct SnapshotRegistryInner {
    /// Monotonically increasing counter used to assign snapshot IDs. Starts at 1.
    next_id: u64,
    /// Map from snapshot_id to the list of segment IDs it pins.
    pinned: HashMap<u64, Vec<SegmentId>>,
}

impl SnapshotRegistryInner {
    fn new() -> Self {
        SnapshotRegistryInner {
            next_id: 1,
            pinned: HashMap::new(),
        }
    }
}

// ── SnapshotRegistry ───────────────────────────────────────────────────────

/// Shared registry that tracks which segments are pinned by live snapshots.
///
/// Cloning a `SnapshotRegistry` shares the same inner state (Arc clone),
/// so the compactor and the engine both see the same pin set.
#[derive(Clone)]
pub struct SnapshotRegistry(Arc<Mutex<SnapshotRegistryInner>>);

impl SnapshotRegistry {
    /// Create a new, empty registry.
    pub fn new() -> Self {
        SnapshotRegistry(Arc::new(Mutex::new(SnapshotRegistryInner::new())))
    }

    /// Register a snapshot over `segment_ids`, returning a unique snapshot ID.
    ///
    /// The segment IDs are pinned until `release` is called with the returned ID.
    pub fn register(&self, segment_ids: &[SegmentId]) -> u64 {
        let mut inner = self.0.lock().expect("SnapshotRegistry lock poisoned");
        let id = inner.next_id;
        inner.next_id += 1;
        inner.pinned.insert(id, segment_ids.to_vec());
        id
    }

    /// Release all segment pins held by `snapshot_id`.
    ///
    /// Subsequent calls with the same ID are no-ops.
    pub fn release(&self, snapshot_id: u64) {
        let mut inner = self.0.lock().expect("SnapshotRegistry lock poisoned");
        inner.pinned.remove(&snapshot_id);
    }

    /// Returns `true` if any live snapshot pins `segment_id`.
    pub fn is_pinned(&self, segment_id: SegmentId) -> bool {
        let inner = self.0.lock().expect("SnapshotRegistry lock poisoned");
        inner.pinned.values().any(|ids| ids.contains(&segment_id))
    }

    /// Returns the flat set of all currently-pinned segment IDs.
    ///
    /// Used by the Compactor to skip segments that are referenced by live snapshots.
    pub fn pinned_ids(&self) -> HashSet<SegmentId> {
        let inner = self.0.lock().expect("SnapshotRegistry lock poisoned");
        inner
            .pinned
            .values()
            .flat_map(|ids| ids.iter().copied())
            .collect()
    }
}

impl Default for SnapshotRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Snapshot ───────────────────────────────────────────────────────────────

/// A point-in-time read-only view over a set of segments.
///
/// The snapshot holds a reference to the `SnapshotRegistry` and automatically
/// releases its pins when dropped.
///
/// Holds cloned `SegmentReader` instances with cached indexes, so reads
/// reuse the Engine's already-opened segment metadata without re-parsing files.
pub struct Snapshot {
    /// Unique ID assigned by `SnapshotRegistry::register`.
    pub snapshot_id: u64,
    /// Shared registry used to release pins on drop.
    registry: SnapshotRegistry,
    /// The segment readers visible to this snapshot (cloned from Engine's store).
    readers: Vec<SegmentReader>,
}

impl Snapshot {
    /// Create a new Snapshot. Called by Engine::snapshot().
    pub fn new(
        snapshot_id: u64,
        registry: SnapshotRegistry,
        readers: Vec<SegmentReader>,
    ) -> Self {
        Snapshot { snapshot_id, registry, readers }
    }

    /// Look up a single key in the snapshot.
    ///
    /// Reads from all pinned segments and returns the value from the entry with
    /// the highest LSN. Returns `Ok(None)` if not found or if the latest entry
    /// is a Delete.
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
    /// Reads from all pinned segments, merges using LWW (last write wins by LSN),
    /// and returns a sorted vec of `(raw_key, value)` pairs with deletes filtered out.
    #[allow(clippy::type_complexity)]
    pub fn range(
        &self,
        ns: &[u8],
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        let enc_start = encode_key(ns, start);
        let enc_end = encode_key(ns, end);

        // K-way merge using a binary heap (same algorithm as SegmentStore::range_scan)
        let mut per_reader: Vec<Vec<(Vec<u8>, MemEntry)>> = Vec::with_capacity(self.readers.len());
        let mut total_len = 0usize;
        for reader in &self.readers {
            let mut seg = reader.range_scan(&enc_start, &enc_end)?;
            seg.sort_by(|(a, _), (b, _)| a.cmp(b));
            total_len += seg.len();
            per_reader.push(seg);
        }

        use std::collections::BinaryHeap;
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

impl Drop for Snapshot {
    fn drop(&mut self) {
        self.registry.release(self.snapshot_id);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::SegmentWriter;
    use crate::types::{encode_key, MemEntry, Operation};
    use tempfile::TempDir;

    // ─ SnapshotRegistry tests ─────────────────────────────────────────────

    #[test]
    fn test_registry_register_pins_segments() {
        let reg = SnapshotRegistry::new();
        reg.register(&[1, 2, 3]);
        assert!(reg.is_pinned(1));
        assert!(reg.is_pinned(2));
        assert!(reg.is_pinned(3));
        assert!(!reg.is_pinned(4));
    }

    #[test]
    fn test_registry_release_unpins() {
        let reg = SnapshotRegistry::new();
        let snap_id = reg.register(&[10, 20, 30]);
        assert!(reg.is_pinned(10));
        reg.release(snap_id);
        assert!(!reg.is_pinned(10));
        assert!(!reg.is_pinned(20));
        assert!(!reg.is_pinned(30));
    }

    #[test]
    fn test_registry_two_snapshots_overlap() {
        let reg = SnapshotRegistry::new();
        let snap1 = reg.register(&[1, 2]);
        let _snap2 = reg.register(&[2, 3]);

        // Both pinned before any release.
        assert!(reg.is_pinned(1));
        assert!(reg.is_pinned(2));
        assert!(reg.is_pinned(3));

        // Release snap1 — segment 1 is freed, but 2 and 3 must remain pinned.
        reg.release(snap1);
        assert!(!reg.is_pinned(1));
        assert!(reg.is_pinned(2)); // still pinned by snap2
        assert!(reg.is_pinned(3)); // still pinned by snap2
    }

    // ─ Snapshot Drop test ─────────────────────────────────────────────────

    #[test]
    fn test_snapshot_drop_releases_pins() {
        let reg = SnapshotRegistry::new();
        let snap_id = reg.register(&[42]);

        let snapshot = Snapshot::new(snap_id, reg.clone(), vec![]);

        assert!(reg.is_pinned(42));
        drop(snapshot);
        assert!(!reg.is_pinned(42));
    }

    // ─ Snapshot::get reads from a real segment ────────────────────────────

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

    #[test]
    fn test_snapshot_get_reads_pinned_segment() {
        let dir = TempDir::new().unwrap();
        let segment_id: SegmentId = 0;

        // Encode key with the namespace — must match how the segment was written.
        let ns = b"ns";
        let user_key = b"key1";
        let encoded_key = encode_key(ns, user_key);
        let value = b"hello-world";

        // Write a real segment using SegmentWriter.
        let entry = make_put_entry(&encoded_key, value, 1);
        let mut entries = vec![(encoded_key.clone(), entry)];
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), segment_id, 3600);
        writer.flush(&entries).unwrap();

        // Register segment and create snapshot.
        let reg = SnapshotRegistry::new();
        let snap_id = reg.register(&[segment_id]);
        let reader = SegmentReader::open(dir.path().to_path_buf(), segment_id).unwrap();
        let snapshot = Snapshot::new(snap_id, reg.clone(), vec![reader]);

        // get() should return the stored value.
        let result = snapshot.get(ns, user_key).unwrap();
        assert_eq!(result, Some(value.to_vec()));
    }

    #[test]
    fn test_snapshot_range_returns_sorted_pairs() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";

        // Write 10 entries into a single segment.
        let mut entries: Vec<(Vec<u8>, MemEntry)> = (0..10u64).map(|i| {
            let enc = encode_key(ns, format!("key-{:04}", i).as_bytes());
            let e = make_put_entry(&enc, format!("val-{}", i).as_bytes(), i + 1);
            (enc, e)
        }).collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&entries).unwrap();

        let reg = SnapshotRegistry::new();
        let snap_id = reg.register(&[0]);
        let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        let snapshot = Snapshot::new(snap_id, reg, vec![reader]);

        // SegmentReader::range_scan is [start, end) exclusive on the end key.
        let results = snapshot.range(ns, b"key-0002", b"key-0007").unwrap();

        // keys key-0002 through key-0006 (exclusive end key-0007) → 5 entries
        assert_eq!(results.len(), 5, "range should return 5 entries");
        let raw_keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
        let mut sorted = raw_keys.clone();
        sorted.sort();
        assert_eq!(raw_keys, sorted, "range results must be sorted");
        assert_eq!(&raw_keys[0], b"key-0002");
        assert_eq!(&raw_keys[4], b"key-0006");
    }

    #[test]
    fn test_snapshot_get_absent_key_returns_none() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";
        let enc = encode_key(ns, b"only-key");
        let entry = make_put_entry(&enc, b"v", 1);
        let mut entries = vec![(enc, entry)];
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&entries).unwrap();

        let reg = SnapshotRegistry::new();
        let snap_id = reg.register(&[0]);
        let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        let snapshot = Snapshot::new(snap_id, reg, vec![reader]);
        let result = snapshot.get(ns, b"not-present").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_registry_pinned_ids_flat_set() {
        let reg = SnapshotRegistry::new();
        reg.register(&[1, 2]);
        reg.register(&[2, 3, 4]);
        let ids = reg.pinned_ids();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(ids.contains(&3));
        assert!(ids.contains(&4));
        assert!(!ids.contains(&5));
    }

    /// Regression test: `Snapshot::get` must return the highest-LSN value when
    /// the same key exists in multiple pinned segments, regardless of the order
    /// in which segment IDs are listed.
    #[test]
    fn test_snapshot_get_highest_lsn_across_segments() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";
        let enc = encode_key(ns, b"shared_key");

        // Segment 0 — old value, lsn=1.
        let old_entry = MemEntry {
            key: enc.clone(),
            value: Some(b"old_value".to_vec()),
            op: Operation::Put,
            lsn: 1,
            timestamp: 3_600_000_000_000,
            ttl: 0,
        };
        let mut entries0 = vec![(enc.clone(), old_entry)];
        entries0.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut writer0 = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer0.flush(&entries0).unwrap();

        // Segment 1 — new value, lsn=5 (higher LSN wins).
        let new_entry = MemEntry {
            key: enc.clone(),
            value: Some(b"new_value".to_vec()),
            op: Operation::Put,
            lsn: 5,
            timestamp: 3_600_000_000_001,
            ttl: 0,
        };
        let mut entries1 = vec![(enc.clone(), new_entry)];
        entries1.sort_by(|(a, _), (b, _)| a.cmp(b));
        let mut writer1 = SegmentWriter::new(dir.path().to_path_buf(), 1, 3600);
        writer1.flush(&entries1).unwrap();

        // Case A — segment IDs listed in ascending order [0, 1].
        {
            let reg = SnapshotRegistry::new();
            let snap_id = reg.register(&[0, 1]);
            let reader0 = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
            let reader1 = SegmentReader::open(dir.path().to_path_buf(), 1).unwrap();
            let snap = Snapshot::new(snap_id, reg, vec![reader0, reader1]);
            let result = snap.get(ns, b"shared_key").unwrap();
            assert_eq!(
                result,
                Some(b"new_value".to_vec()),
                "Case A [0,1]: expected new_value (lsn=5)"
            );
        }

        // Case B — segment IDs listed in descending order [1, 0].
        {
            let reg = SnapshotRegistry::new();
            let snap_id = reg.register(&[1, 0]);
            let reader1 = SegmentReader::open(dir.path().to_path_buf(), 1).unwrap();
            let reader0 = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
            let snap2 = Snapshot::new(snap_id, reg, vec![reader1, reader0]);
            let result = snap2.get(ns, b"shared_key").unwrap();
            assert_eq!(
                result,
                Some(b"new_value".to_vec()),
                "Case B [1,0]: expected new_value (lsn=5)"
            );
        }
    }

    /// Regression: Snapshot used to re-open all segment files on every get() and range(),
    /// re-parsing JSON metadata and sparse indexes each time. Now it holds cloned SegmentReader
    /// instances with cached indexes. This test verifies repeated reads work without re-opening.
    #[test]
    fn test_snapshot_reuses_cached_readers() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";
        let key = encode_key(ns, b"key");

        let entry = MemEntry {
            key: key.clone(),
            value: Some(b"val".to_vec()),
            op: Operation::Put,
            lsn: 1,
            timestamp: 3_600_000_000_000,
            ttl: 0,
        };
        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        writer.flush(&vec![(key.clone(), entry)]).unwrap();

        let reader = SegmentReader::open(dir.path().to_path_buf(), 0).unwrap();
        let reg = SnapshotRegistry::new();
        let snap_id = reg.register(&[0]);
        let snap = Snapshot::new(snap_id, reg, vec![reader]);

        // Call get() multiple times — each should succeed without re-opening files
        for _ in 0..10 {
            let result = snap.get(ns, b"key").unwrap();
            assert_eq!(result, Some(b"val".to_vec()));
        }

        // Call range() multiple times — each should succeed without re-opening files
        for _ in 0..10 {
            let results = snap.range(ns, b"", b"\xff").unwrap();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].1, b"val".to_vec());
        }
    }
}
