---
phase: 3
plan: "03"
subsystem: snapshot
tags: [snapshot, pinning, compaction, read-view, drop]
dependency_graph:
  requires: [03-01]
  provides: [Snapshot, SnapshotRegistry]
  affects: [03-02, 03-04]
tech_stack:
  added: []
  patterns:
    - Arc<Mutex<Inner>> newtype registry
    - Drop-based RAII pin release
    - LWW merge by LSN across segment readers
key_files:
  created: []
  modified:
    - edgestore/src/snapshot.rs
decisions:
  - "SnapshotRegistryInner uses Vec<SegmentId> per snapshot_id (not HashSet) per plan spec; is_pinned does linear scan over values"
  - "pinned_ids() flattens all Vec<SegmentId> entries into a HashSet for O(1) lookup by compactor"
  - "Snapshot::get iterates all pinned segments and picks highest-LSN entry; filters Delete ops before returning"
  - "Snapshot::range uses HashMap LWW merge by LSN then decodes namespace prefix off raw_key before returning"
  - "Used is_none_or() per clippy lint instead of map_or(true, ...) to satisfy -D warnings"
metrics:
  duration_secs: 81
  completed_date: "2026-05-19"
  tasks_completed: 2
  files_modified: 1
---

# Phase 3 Plan 03: Snapshot Implementation Summary

## One-liner

SnapshotRegistry Arc/Mutex pin tracker with Drop-based RAII release and Snapshot read view (get/range) over pinned segments with LWW-by-LSN merge.

## What Was Built

### Task 1: SnapshotRegistry with pin/release/is_pinned/pinned_ids

`SnapshotRegistryInner` holds `next_id: u64` (starts at 1) and `pinned: HashMap<u64, Vec<SegmentId>>`.

`SnapshotRegistry` is a newtype over `Arc<Mutex<SnapshotRegistryInner>>` and derives `Clone` (Arc clone, not deep clone). Methods:

- `register(&[SegmentId]) -> u64`: locks inner, assigns `next_id`, increments, inserts id→segment list.
- `release(u64)`: locks inner, removes entry (idempotent).
- `is_pinned(SegmentId) -> bool`: scans all value Vecs for the segment.
- `pinned_ids() -> HashSet<SegmentId>`: flattens all pinned Vec entries into a set (used by Compactor).

Unit tests: `test_registry_register_pins_segments`, `test_registry_release_unpins`, `test_registry_two_snapshots_overlap`.

### Task 2: Snapshot::new, Drop, get, range

`Snapshot` fields: `snapshot_id`, `registry`, `segment_ids`, `base_path`.

- `Drop`: calls `registry.release(snapshot_id)`.
- `get(ns, key)`: encodes key via `encode_key`, opens each `SegmentReader`, calls `reader.get()`, picks highest-LSN entry, returns None for Delete ops.
- `range(ns, start, end)`: encodes bounds, opens each reader, calls `range_scan`, merges via `HashMap` LWW by LSN, decodes `decode_key` to strip namespace prefix, filters Delete ops, sorts.

Unit tests: `test_snapshot_drop_releases_pins`, `test_snapshot_get_reads_pinned_segment` (real SegmentWriter flush in TempDir).

## Verification Results

```
cargo build --workspace           # OK
cargo test -p edgestore snapshot::  # 5/5 passed
cargo clippy -p edgestore -- -D warnings  # clean
```

Test output:
```
running 5 tests
test snapshot::tests::test_registry_release_unpins ... ok
test snapshot::tests::test_registry_register_pins_segments ... ok
test snapshot::tests::test_registry_two_snapshots_overlap ... ok
test snapshot::tests::test_snapshot_drop_releases_pins ... ok
test snapshot::tests::test_snapshot_get_reads_pinned_segment ... ok
test result: ok. 5 passed; 0 failed
```

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Clippy lint: unnecessary_map_or**
- **Found during:** Task 2 clippy verification
- **Issue:** `best.as_ref().map_or(true, |b| entry.lsn > b.lsn)` triggers `-D clippy::unnecessary_map_or`
- **Fix:** Replaced with `best.as_ref().is_none_or(|b| entry.lsn > b.lsn)`
- **Files modified:** edgestore/src/snapshot.rs
- **Commit:** 0bdb1aa

## Known Stubs

None. `Snapshot::get` and `Snapshot::range` are fully implemented (segment reads wired). The stub comment in the scaffold has been replaced.

## Threat Flags

None. snapshot.rs is a read-only, in-process data structure. No new network endpoints, auth paths, or file access patterns beyond reading existing segment files via `SegmentReader::open`.

## Self-Check: PASSED

- edgestore/src/snapshot.rs: FOUND
- Commit 0bdb1aa: FOUND (git log confirms)
- 5 snapshot tests: all pass
- clippy -D warnings: clean
