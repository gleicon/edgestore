---
phase: 3
plan: "04"
subsystem: engine
tags: [engine, compaction, snapshot, integration]
dependency_graph:
  requires: ["03-02", "03-03"]
  provides: ["Engine::compact_once", "Engine::snapshot", "Engine.snapshot_registry"]
  affects: ["edgestore/src/engine.rs", "edgestore/src/segment.rs"]
tech_stack:
  added: []
  patterns: ["caller-driven compaction", "Arc-shared snapshot registry", "segment reload after compaction"]
key_files:
  created: []
  modified:
    - "edgestore/src/engine.rs"
    - "edgestore/src/segment.rs"
decisions:
  - "Manifest path for compact_once uses manifest.mf (matches SegmentStore, not the MANIFEST path used in compactor tests)"
  - "segment_ids() helper added to SegmentStore since readers field is private"
  - "compact_once reloads segment_store after compaction so subsequent reads are consistent"
metrics:
  duration_secs: 115
  completed_date: "2026-05-20T00:44:08Z"
  tasks_completed: 4
  files_modified: 2
---

# Phase 3 Plan 04: Engine Integration — Engine::compact_once and Engine::snapshot Summary

Engine now exposes two public methods wiring the Compactor and SnapshotRegistry into the write path: `compact_once()` for caller-driven bounded compaction and `snapshot()` for point-in-time reads over pinned segments.

## Tasks Completed

| # | Task | Commit | Files |
|---|------|--------|-------|
| 1 | Add SnapshotRegistry to Engine struct | fe60746 | edgestore/src/engine.rs |
| 2 | Implement Engine::snapshot | 5a8011b | edgestore/src/engine.rs, edgestore/src/segment.rs |
| 3 | Implement Engine::compact_once | fc14138 | edgestore/src/engine.rs |
| 4 | Final clippy pass | (no-op — already clean) | — |

## What Was Built

**Engine::compact_once** (`edgestore/src/engine.rs:413`):
- Gets wall-clock `now_nanos` via `SystemTime::now()`
- Reads `pinned_ids()` from `snapshot_registry` to avoid evicting segments referenced by live snapshots
- Constructs a `Compactor` with config's `compaction_write_budget_bytes` and `cohort_window_secs`
- Opens `manifest.mf` (same path used by `SegmentStore::open`)
- Calls `compactor.compact_cycle()` with manifest, now_nanos, and pinned set
- Reloads `self.segment_store` from disk so subsequent point lookups and range scans see the updated segment list
- Returns `CompactionStats`

**Engine::snapshot** (`edgestore/src/engine.rs:436`):
- Calls `self.segment_store.segment_ids()` to collect current segment IDs
- Calls `self.snapshot_registry.register(&ids)` to get a unique snapshot ID (pins those segments)
- Constructs and returns `Snapshot::new(sid, registry.clone(), ids, config.path.clone())`
- Snapshot auto-releases pins on drop via its `Drop` impl

**SegmentStore::segment_ids** (`edgestore/src/segment.rs:525`):
- New helper returning `Vec<SegmentId>` from the private `readers` field
- Required because `readers` is not `pub` — cannot be accessed from engine.rs directly

## Decisions Made

1. **Manifest path**: `compact_once` uses `self.config.path.join("manifest.mf")` — must match the path that `SegmentStore::open` uses, not the `"MANIFEST"` name used in compactor unit tests (those tests create their own isolated manifests).

2. **No background thread**: Compaction is caller-driven in v1. `compact_once` is `&mut self`, so the caller controls scheduling (e.g., on a background thread that holds a `Mutex<Engine>`).

3. **Segment store reload**: After `compact_cycle` modifies the manifest (deleting/rewriting segments), the in-memory `segment_store` is stale. Reloading via `SegmentStore::open` ensures the Engine's readers list matches disk state. This is a full reload (no incremental diff) — acceptable for v1 since compaction is infrequent.

4. **`SystemTime` import**: Already present in engine.rs (`use std::time::{SystemTime, UNIX_EPOCH}`). No duplicate import added.

## Verification Results

```
cargo build --workspace        # Finished dev profile — 0 errors
cargo test --workspace         # 95 tests: 75 + 11 + 9 + 0 = 95 passed, 0 failed
cargo clippy -p edgestore -- -D warnings  # Finished dev profile — 0 warnings
grep -n "pub fn compact_once\|pub fn snapshot" edgestore/src/engine.rs
# 413: pub fn compact_once(&mut self) -> Result<crate::compactor::CompactionStats, EdgestoreError>
# 436: pub fn snapshot(&self) -> Result<crate::snapshot::Snapshot, EdgestoreError>
```

## Deviations from Plan

### Auto-fixed Issues

None — plan executed exactly as written, with one clarification:

**Manifest path clarification** (not a deviation, but a plan ambiguity resolved):
The plan says `self.config.path.join("MANIFEST")`. The actual file used by `SegmentStore` is `manifest.mf` (set in `SegmentStore::open`). Using `"MANIFEST"` would have opened a new, empty manifest and compaction would operate on no segments. Fixed to use `"manifest.mf"` matching `SegmentStore`.

## Known Stubs

None. Both methods are fully wired with real implementations.

## Threat Flags

None. `compact_once` and `snapshot` operate on local files only; no new network endpoints, auth paths, or trust boundary crossings introduced.

## Self-Check: PASSED

- edgestore/src/engine.rs modified: FOUND
- edgestore/src/segment.rs modified: FOUND
- Commit fe60746: FOUND
- Commit 5a8011b: FOUND
- Commit fc14138: FOUND
- cargo test --workspace: 95 passed, 0 failed
- cargo clippy -p edgestore -- -D warnings: 0 errors
