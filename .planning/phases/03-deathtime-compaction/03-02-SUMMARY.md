---
phase: 3
plan: "02"
subsystem: compaction
tags: [compactor, cohort, expired, deathtime, wal, segment, rust]
dependency_graph:
  requires: ["03-01"]
  provides: ["compact_cycle", "identify_cohorts", "collect_expired_cohort", "compact_partial_cohort"]
  affects: ["03-04"]
tech_stack:
  added: []
  patterns:
    - "Deathtime-cohort compaction — group by bucket, expire first, zero live relocation"
    - "LWW (last-write-wins) by LSN during partial cohort merge"
    - "Budget-bounded compaction loop stops when bytes_written >= write_budget_bytes"
    - "Conservative pinned-segment handling — skip entire cohort if any segment is pinned"
key_files:
  created: []
  modified:
    - edgestore/src/compactor.rs
decisions:
  - "Budget check occurs before processing each cohort — budget=0 means zero cohorts processed (consistent with 'stop when bytes_written >= budget' invariant)"
  - "compact_partial_cohort with zero survivors delegates to inline expired-file-removal (no output segment written), keeping cohorts_collected incremented"
  - "next_segment_id increments after each compact_partial_cohort call so parallel output segments never collide"
  - "Conservative pinning: if any segment in a cohort is pinned, entire cohort is skipped"
metrics:
  duration: "~20 minutes"
  completed: "2026-05-19"
  tasks_completed: 4
  files_modified: 1
---

# Phase 3 Plan 02: Compactor Core Algorithm Summary

One-liner: Deathtime-cohort compaction core — identify_cohorts, collect_expired_cohort (zero live relocation), compact_partial_cohort (LWW merge + dead-record filter), compact_cycle (budget-bounded, pinned-aware), with 8 unit tests.

## What Was Built

Implemented the full compactor algorithm in `edgestore/src/compactor.rs`:

### CohortInfo — added `is_fully_expired: bool`
Added the `is_fully_expired` field (computed as `now_nanos > max_death_time_nanos`) to `CohortInfo` so the cycle loop can dispatch without re-comparing timestamps.

### `Compactor::identify_cohorts`
Groups `&[SegmentMeta]` by `cohort_bucket` using a `HashMap<i64, Vec<&SegmentMeta>>`, computes `max_death_time_nanos` and `total_records` per group, sets `dead_record_estimate = 0`, then sorts: fully-expired cohorts first (lowest `max_death_time_nanos` first), partially-expired after.

### `Compactor::collect_expired_cohort`
Removes all four files per segment ID (`.dat`, `.idx`, `.xf`, `.meta`) using `std::fs::remove_file`. Missing files are logged with `eprintln!` and skipped — not an error. Calls `manifest.remove_segments()`, increments `stats.segments_removed` and `stats.cohorts_collected`. `live_records_relocated` is never incremented (COMPACT-04 invariant).

### `Compactor::compact_partial_cohort`
- Opens a `SegmentReader` for each segment in the cohort
- Full-range scan via `reader.range_scan(&[], &[0xFFu8; 256])`
- Merges into a `HashMap<Vec<u8>, MemEntry>` keyed by raw key, keeping highest-LSN entry (LWW)
- Filters: removes `Operation::Delete` tombstones and records where `death_time_for(timestamp, ttl, cohort_window_secs) <= now_nanos`
- Zero survivors → inline expired-removal path (no output segment, `cohorts_collected += 1`)
- Survivors exist → sort by key, flush with `SegmentWriter`, call `manifest.add_segment` + `manifest.remove_segments`, remove old files, update all stats

### `Compactor::compact_cycle`
- Filters pinned segments before `identify_cohorts`
- Checks `stats.bytes_written >= self.write_budget_bytes` before each cohort
- Skips any cohort where at least one segment ID is in `pinned_segment_ids`
- Routes to `collect_expired_cohort` or `compact_partial_cohort` based on `cohort.is_fully_expired`
- Tracks `next_segment_id` starting from max(manifest segment IDs) + 1

## Test Coverage

All 8 tests pass:
- `test_identify_cohorts_groups_and_sorts` — groups + sort ordering verified
- `test_identify_cohorts_empty` — empty input safe
- `test_collect_expired_cohort_removes_files` — all 4 files deleted, manifest empty, stats correct
- `test_collect_expired_cohort_missing_file_ok` — pre-deleted file does not error
- `test_compact_partial_cohort_removes_dead_entries` — ttl=1 records die, ttl=0 survive; new segment written
- `test_compact_cycle_respects_budget` — budget=0 means at most 1 cohort processed
- `test_compact_cycle_pinned_segments_never_removed` — pinned segment stays in manifest and on disk
- `test_compact_cycle_full_expiry_removes_all` — two segments in same cohort, all removed

## Commits

| Task | Commit | Description |
|------|--------|-------------|
| 1-4 (all) | 31873ef | feat(03-02): implement Compactor core algorithm with full test suite |

## Deviations from Plan

None — plan executed exactly as written.

The budget check position (before first cohort, not after) results in `cohorts_collected == 0` when `write_budget_bytes == 0`. The plan's test assertion `stats.cohorts_collected <= 1` accommodates both behaviors (0 or 1), so the test passes and the invariant is preserved.

## Self-Check

### Files
- `/Users/gleicon/code/markdown/edgestore/edgestore/src/compactor.rs` — FOUND (712 line implementation)

### Commits
- `31873ef` — FOUND

### Verification Commands
- `cargo build --workspace` — PASSED (0 errors)
- `cargo test -p edgestore compactor::` — PASSED (8/8 tests)
- `cargo clippy -p edgestore -- -D warnings` — PASSED (0 warnings)

## Self-Check: PASSED
