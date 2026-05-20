---
phase: 3
plan: "01"
subsystem: compaction
tags: [compactor, snapshot, config, error, scaffold]
dependency_graph:
  requires: []
  provides: [compactor.rs, snapshot.rs, EdgestoreConfig.compaction_write_budget_bytes, EdgestoreError.CompactionError]
  affects: [edgestore/src/lib.rs, edgestore/src/config.rs, edgestore/src/error.rs]
tech_stack:
  added: []
  patterns: [newtype-Arc-Mutex, deathtime-cohort, append-only]
key_files:
  created:
    - edgestore/src/compactor.rs
    - edgestore/src/snapshot.rs
  modified:
    - edgestore/src/config.rs
    - edgestore/src/error.rs
    - edgestore/src/lib.rs
decisions:
  - "CohortInfo.dead_record_estimate is u64 (not usize) to match CompactionStats byte counters for consistent arithmetic"
  - "SnapshotRegistry is a Clone-able Arc<Mutex<Inner>> newtype so engine and compactor share pin state without lifetime coupling"
  - "#[allow(clippy::type_complexity)] applied to Snapshot::range because the plan API spec mandates Vec<(Vec<u8>, Vec<u8>)> return type"
metrics:
  duration_secs: 145
  completed_date: "2026-05-19"
  tasks_total: 5
  tasks_completed: 5
  files_created: 2
  files_modified: 3
requirements: [COMPACT-03, COMPACT-04, COMPACT-06]
---

# Phase 3 Plan 01: Compactor + Snapshot Module Scaffold Summary

Scaffolded `compactor.rs` and `snapshot.rs` with all struct definitions needed by Plans 03-02 through 03-04, added `compaction_write_budget_bytes` to `EdgestoreConfig`, and added `CompactionError(String)` to `EdgestoreError` — all compiling clean with zero clippy warnings.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Add compaction_write_budget_bytes to EdgestoreConfig | a0fff5c | edgestore/src/config.rs |
| 2 | Add CompactionError variant to EdgestoreError | 1ea5d95 | edgestore/src/error.rs |
| 3 | Create compactor.rs scaffold | c8a34d6 | edgestore/src/compactor.rs, edgestore/src/lib.rs |
| 4 | Create snapshot.rs scaffold | 16c4c28 | edgestore/src/snapshot.rs, edgestore/src/lib.rs |
| 5 | Declare modules and re-exports in lib.rs | 31cf409 | edgestore/src/lib.rs, edgestore/src/snapshot.rs |

## What Was Built

### `edgestore/src/config.rs`
- `pub compaction_write_budget_bytes: u64` field added to `EdgestoreConfig`
- Default: `256 * 1024 * 1024` (268 435 456 bytes / 256 MB) in `EdgestoreConfig::new()`
- Debug impl updated to include the field

### `edgestore/src/error.rs`
- `CompactionError(String)` variant added to `EdgestoreError` enum
- Display impl: `"compaction error: {msg}"`

### `edgestore/src/compactor.rs` (new file)
- `CohortInfo { cohort_bucket: i64, segment_ids: Vec<SegmentId>, max_death_time_nanos: i64, total_records: u64, dead_record_estimate: u64 }` — derives `Debug, Default`
- `CompactionStats { cohorts_collected, segments_removed, segments_written, bytes_written, live_records_relocated: all u64 }` — derives `Debug, Default`
- `Compactor { base_path: PathBuf, write_budget_bytes: u64, cohort_window_secs: u64 }` — derives `Debug`
- `Compactor::new(base_path, write_budget_bytes, cohort_window_secs) -> Self`

### `edgestore/src/snapshot.rs` (new file)
- `SnapshotRegistryInner { next_id: u64, pinned: HashMap<u64, HashSet<SegmentId>> }` — private
- `SnapshotRegistry(Arc<Mutex<SnapshotRegistryInner>>)` — `Clone` shares Arc, implements `Default`
  - `new() -> Self`
  - `register(segment_ids: &[SegmentId]) -> u64`
  - `release(snapshot_id: u64)`
  - `is_pinned(segment_id: SegmentId) -> bool`
- `Snapshot { snapshot_id: u64, registry: SnapshotRegistry, segment_ids: Vec<SegmentId>, base_path: PathBuf }`
  - `Drop` calls `registry.release(self.snapshot_id)`
  - `get(_ns, _key) -> Ok(None)` — stub
  - `range(_ns, _start, _end) -> Ok(vec![])` — stub

### `edgestore/src/lib.rs`
- `pub mod compactor` and `pub mod snapshot` declared
- Re-exports: `Compactor`, `CompactionStats`, `Snapshot`, `SnapshotRegistry`

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Clippy type_complexity lint on Snapshot::range**
- **Found during:** Task 5 (`cargo clippy -D warnings`)
- **Issue:** The `Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError>` return type triggered `-D clippy::type_complexity`
- **Fix:** Added `#[allow(clippy::type_complexity)]` on `Snapshot::range` — the return type is mandated by the plan API spec and intentional
- **Files modified:** `edgestore/src/snapshot.rs`
- **Commit:** 31cf409

**2. [Rule 3 - Blocking] Module declarations needed before Task 3 verify**
- **Found during:** Task 3 implementation
- **Issue:** `cargo build -p edgestore` would compile without errors even without the module declared (file just ignored), but the struct types would be invisible. To ensure compactor.rs actually compiled and any errors surfaced, `pub mod compactor` was added to lib.rs during Task 3 rather than waiting until Task 5.
- **Fix:** Added `pub mod compactor` in Task 3 commit; `pub mod snapshot` in Task 4 commit. Task 5 then added re-exports only.
- **Files modified:** `edgestore/src/lib.rs`

## Known Stubs

| File | Location | Description | Resolved by |
|------|----------|-------------|-------------|
| edgestore/src/snapshot.rs | `Snapshot::get` | Returns `Ok(None)` always | Plan 03-04 |
| edgestore/src/snapshot.rs | `Snapshot::range` | Returns `Ok(vec![])` always | Plan 03-04 |

These stubs are intentional per the plan objective ("No implementation logic yet").

## Threat Flags

None. No new network endpoints, auth paths, file access patterns, or schema changes introduced. All new code is in-process struct definitions with no I/O.

## Self-Check: PASSED

- `/Users/gleicon/code/markdown/edgestore/edgestore/src/compactor.rs` — FOUND
- `/Users/gleicon/code/markdown/edgestore/edgestore/src/snapshot.rs` — FOUND
- Commit a0fff5c (config) — FOUND
- Commit 1ea5d95 (error) — FOUND
- Commit c8a34d6 (compactor) — FOUND
- Commit 16c4c28 (snapshot) — FOUND
- Commit 31cf409 (lib.rs re-exports) — FOUND
- `cargo build --workspace` exits 0 — VERIFIED
- `cargo clippy -p edgestore -- -D warnings` exits 0 — VERIFIED
