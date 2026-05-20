---
phase: 03-deathtime-compaction
plan: "05"
subsystem: testing
tags: [rust, integration-tests, compaction, ttl, merkle-root, blake3, snapshot, lsw-merge]

# Dependency graph
requires:
  - phase: 03-01
    provides: Compactor::identify_cohorts, CohortInfo structs
  - phase: 03-02
    provides: Compactor::collect_expired_cohort, compact_partial_cohort
  - phase: 03-03
    provides: Compactor::compact_cycle with budget and pinning
  - phase: 03-04
    provides: Engine::compact_once, Engine::snapshot

provides:
  - "5 integration tests covering all Phase 3 success criteria"
  - "edgestore/tests/integration_compaction.rs with SC1–SC5 tests"

affects: [04-vector, 05-hnsw]

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Use Engine::compact_once with small segment_size_bytes + TTL sleep for expiry tests"
    - "Use Compactor::compact_cycle directly with custom now_nanos for precise time-control tests"
    - "Use Manifest::open (manifest.mf) separately from Engine for direct Compactor tests"
    - "Merkle root recomputation: collect keys → blake3 hash each → sort hashes → chain through Hasher"

key-files:
  created:
    - edgestore/tests/integration_compaction.rs
  modified: []

key-decisions:
  - "Use Engine::compact_once (wall-clock) for SC1/SC3 tests with real TTL sleep; avoids clock injection complexity"
  - "Use Compactor directly with custom now_nanos for SC4/SC5 tests to control time without sleeping"
  - "Engine::memtable is pub(crate) — integration tests call flush_to_segments() and ignore empty-memtable errors"
  - "Manifest filename is manifest.mf — tests that use Compactor directly open Manifest::open(&path.join(manifest.mf))"
  - "Write budget test uses budget=1 with partially-expired cohorts so first cohort writes bytes, second trips budget check"

patterns-established:
  - "Integration tests that need expired records: set segment_size_bytes=512, write with put_with_ttl(ttl=1), sleep(2s)"
  - "Integration tests that need time control: use Compactor::compact_cycle directly with far-future now_nanos"
  - "Snapshot pin tests: take snapshot BEFORE compact, verify readable AFTER compact, drop + compact again"

requirements-completed: [COMPACT-01, COMPACT-02, COMPACT-03, COMPACT-04, COMPACT-05, COMPACT-06, COMPACT-07]

# Metrics
duration: 15min
completed: 2026-05-19
---

# Phase 3 Plan 05: Integration Tests — All Phase 3 Success Criteria Summary

**5 integration tests verifying deathtime-cohort compaction correctness: TTL expiry zero-relocation, LWW range merge across overlapping segments, snapshot pin survival, write-budget enforcement, and merkle_root recomputation match**

## Performance

- **Duration:** ~15 min
- **Started:** 2026-05-19T00:34:00Z
- **Completed:** 2026-05-20T00:49:00Z
- **Tasks:** 6 (including full workspace test + clippy)
- **Files modified:** 1

## Accomplishments
- Wrote 5 integration tests in `edgestore/tests/integration_compaction.rs` covering all Phase 3 success criteria
- All 5 tests pass: zero live relocation on TTL expiry, correct LWW range merge, snapshot pin survival, budget enforcement, merkle root verification
- Full workspace: 100 tests pass (75 unit + 5 new integration + 11 core + 9 segment integration)
- `cargo clippy --workspace -- -D warnings` clean with zero warnings

## Task Commits

Each task was committed atomically:

1. **Tasks 1-5: All 5 integration tests** - `888c15a` (feat) — all 5 SC tests implemented and passing
2. **Task 6: Full workspace test + clippy** — verified via `888c15a` (no separate commit needed, already passing)

**Plan metadata commit:** to be added by final docs commit

## Files Created/Modified
- `/Users/gleicon/code/markdown/edgestore/edgestore/tests/integration_compaction.rs` — 5 integration tests: test_ttl_expiry_zero_live_relocation, test_range_scan_overlapping_segments, test_snapshot_survives_compaction, test_compaction_write_budget_enforced, test_merkle_root_correct_after_compaction

## Decisions Made
- Used `Engine::compact_once` (wall-clock) + `sleep(Duration::from_secs(2))` for SC1 and SC3 tests rather than direct Compactor injection, since TTL=1s records naturally expire in real time and the test is simpler.
- Used `Compactor::compact_cycle` directly for SC4 (budget) and SC5 (merkle_root) to avoid any time dependency.
- `Engine::memtable` is `pub(crate)` so integration tests can't call `memtable.is_empty()` directly. Workaround: call `flush_to_segments()` and ignore the "empty memtable" error; the `put` loop with `segment_size_bytes=512` triggers auto-flushes anyway.
- For SC2 (range scan), used `segment_size_bytes=512` to force multiple segment flushes, then wrote 60 unique keys + 1 overlap key before and after flush. Range query uses `b""` to `b"\xFF\xFF\xFF\xFF"` to capture all keys.
- For SC4 (budget test), used future write_time_nanos to make cohorts partially expired (live records that require relocation). With `write_budget_bytes=1`, the first cohort writes N>0 bytes; the second cohort's pre-loop check fires.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Engine::memtable is pub(crate), not accessible from integration tests**
- **Found during:** Task 1 (scaffold compilation)
- **Issue:** Plan suggested `if !engine.memtable.is_empty() { engine.flush_to_segments() }` — memtable field is pub(crate), not accessible in integration tests
- **Fix:** Replace with `let _ = engine.flush_to_segments();` — ignores the SegmentCorrupt("memtable is empty") error; the auto-flush mechanism via small segment_size_bytes handles most flushes anyway
- **Files modified:** edgestore/tests/integration_compaction.rs
- **Verification:** Compile-time: no E0616 errors. Runtime: tests pass with correct flush semantics
- **Committed in:** 888c15a

---

**Total deviations:** 1 auto-fixed (1 blocking — private field access)
**Impact on plan:** Minimal. The flush strategy produces identical observable behavior since auto-flush triggers during the write loops with segment_size_bytes=512.

## Issues Encountered
- `open_engine` helper was initially written but unused (only `open_engine_small_segments` was needed). Removed to keep clippy clean.

## Known Stubs
None — all 5 tests verify real I/O behavior against actual segment files.

## Threat Flags
None — this plan adds only test code; no new network endpoints, auth paths, or schema changes.

## Self-Check: PASSED
- `edgestore/tests/integration_compaction.rs` exists and contains 5 test functions
- Commit `888c15a` exists with all 5 integration tests
- `cargo test --workspace` exits 0 with 100 tests passing
- `cargo clippy --workspace -- -D warnings` exits 0

## Next Phase Readiness
- Phase 3 complete: all 7 compaction requirements (COMPACT-01 through COMPACT-07) verified
- Phase 4 (vector indexing) can begin: Engine KV layer is clean, SegmentStore is stable, compaction proven correct
- No blockers or concerns

---
*Phase: 03-deathtime-compaction*
*Completed: 2026-05-19*
