use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::error::EdgestoreError;
use crate::types::SegmentId;

/// Inner (unshared) state for `SnapshotRegistry`.
struct SnapshotRegistryInner {
    /// Monotonically increasing counter used to assign snapshot IDs.
    next_id: u64,
    /// Map from snapshot_id to the set of segment IDs it pins.
    pinned: HashMap<u64, HashSet<SegmentId>>,
}

impl SnapshotRegistryInner {
    fn new() -> Self {
        SnapshotRegistryInner {
            next_id: 0,
            pinned: HashMap::new(),
        }
    }
}

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
        inner
            .pinned
            .insert(id, segment_ids.iter().copied().collect());
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
        inner.pinned.values().any(|set| set.contains(&segment_id))
    }
}

impl Default for SnapshotRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// A point-in-time read-only view over a set of segments.
///
/// The snapshot holds a reference to the `SnapshotRegistry` and automatically
/// releases its pins when dropped.
pub struct Snapshot {
    /// Unique ID assigned by `SnapshotRegistry::register`.
    pub snapshot_id: u64,
    /// Shared registry used to release pins on drop.
    registry: SnapshotRegistry,
    /// The segment IDs visible to this snapshot.
    pub segment_ids: Vec<SegmentId>,
    /// Base path of the database (used to open segment files for reads).
    pub base_path: PathBuf,
}

impl Snapshot {
    /// Look up a single key in the snapshot.
    ///
    /// Returns `Ok(Some(value))` if found, `Ok(None)` if not present.
    ///
    /// Stub: always returns `Ok(None)` until Plan 03-04 wires segment reads.
    pub fn get(&self, _ns: &[u8], _key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        Ok(None)
    }

    /// Iterate over key-value pairs in `[start, end)` within a namespace.
    ///
    /// Returns a sorted vec of `(key, value)` pairs.
    ///
    /// Stub: always returns `Ok(vec![])` until Plan 03-04 wires segment reads.
    pub fn range(
        &self,
        _ns: &[u8],
        _start: &[u8],
        _end: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError> {
        Ok(vec![])
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        self.registry.release(self.snapshot_id);
    }
}
