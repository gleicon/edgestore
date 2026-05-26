---
phase: 08
plan: 04b
subsystem: release

tags: [cargo, crates.io, benchmark, criterion, clippy, validation]

requires:
  - phase: 08-04a
    provides: Final integration tests and metadata finalization

provides:
  - crates.io publish dry-run passes for edgestore crate
  - BENCHMARKS.md updated with measured Criterion results
  - cargo clippy --workspace -D warnings clean
  - cargo test --workspace passes (250 tests)
  - cargo build --release --workspace succeeds
  - edgestore-cli installs and reports v1.0.0

affects:
  - release-announcement
  - crates.io-publish

tech-stack:
  added: []
  patterns:
    - "total_cmp_f32: NaN-safe f32 comparison for sort/BinaryHeap"

key-files:
  created: []
  modified:
    - BENCHMARKS.md
    - edgestore/src/vector/distance.rs
    - edgestore/src/vector/hnsw.rs
    - edgestore/src/vector/search.rs
    - edgestore/src/engine.rs
    - edgestore/src/lib.rs
    - edgestore/benches/vector_search.rs
    - edgestore/benches/hnsw_recall.rs
    - edgestore/benches/throughput.rs
    - edgestore-cli/src/main.rs
    - edgestore-tokio/Cargo.toml
    - edgestore-cli/Cargo.toml
    - edgestore/LICENSE-MIT
    - edgestore/LICENSE-APACHE
    - edgestore-tokio/LICENSE-MIT
    - edgestore-tokio/LICENSE-APACHE
    - edgestore-cli/LICENSE-MIT
    - edgestore-cli/LICENSE-APACHE

key-decisions:
  - "NaN-safe comparison: Added total_cmp_f32 helper to prevent sort panics when vector data contains NaN f32 values"
  - "LICENSE symlinks: Added LICENSE-MIT/LICENSE-APACHE symlinks in each crate directory for cargo publish inclusion"
  - "Versioned path deps: Added version='1.0.0' to edgestore-tokio and edgestore-cli path dependencies for publish readiness"

patterns-established:
  - "All f32 sorting uses total_cmp_f32 instead of partial_cmp + unwrap_or(Equal) to avoid NaN-induced total-order violations"

requirements-completed: [POLISH-07, POLISH-08]

duration: 35min
completed: 2026-05-25
---

# Phase 08 Plan 04b: Publish Dry-Run, Benchmarks, and Final Validation Summary

**Crates.io publish dry-run passes, benchmark suite runs successfully with measured results, and all validation gates (test, clippy, doc, release build, CLI install) are clean for v1.0.0.**

## Performance

- **Duration:** ~35 min
- **Started:** 2026-05-25
- **Completed:** 2026-05-25
- **Tasks:** 3
- **Files modified:** 15

## Accomplishments

- `cargo publish --dry-run -p edgestore` passes with 55 files packaged including LICENSE files
- All 4 Criterion benchmarks execute successfully on Apple M5 (hnsw_recall, text_search, throughput, vector_search)
- BENCHMARKS.md updated with measured throughput, latency, and QPS numbers
- cargo test --workspace passes with 250 tests across 16 suites
- cargo clippy --workspace -D warnings clean after fixing edgestore-cli issues
- cargo doc --workspace --no-deps generates without warnings
- cargo build --release --workspace succeeds
- edgestore-cli installs from path and reports `edgestore-cli 1.0.0`

## Task Commits

Each task was committed atomically:

1. **Task 1: Run crates.io publish dry-run for all crates** - `13a1792` (chore)
2. **Task 2: Execute benchmark suite and capture results** - `7628fa8` (fix) + `docs commit` for BENCHMARKS.md
3. **Task 3: Run final validation sequence** - `clippy fix commit` (fix)

**Plan metadata:** `final docs commit` (docs: complete plan)

## Files Created/Modified

- `BENCHMARKS.md` — Updated with actual measured results from Criterion suite
- `edgestore/src/vector/distance.rs` — Added `total_cmp_f32` helper for NaN-safe f32 ordering
- `edgestore/src/vector/hnsw.rs` — Replaced partial_cmp sorts with total_cmp_f32; removed unused import/variable
- `edgestore/src/vector/search.rs` — Replaced HeapItem::cmp and results sort with total_cmp_f32
- `edgestore/src/engine.rs` — Text search BM25 sort uses total_cmp_f32
- `edgestore/src/lib.rs` — Re-exports total_cmp_f32
- `edgestore/benches/vector_search.rs` — Removed unused import, fixed unnecessary parentheses
- `edgestore/benches/hnsw_recall.rs` — Replaced partial_cmp sort with total_cmp_f32, fixed parentheses
- `edgestore/benches/throughput.rs` — Removed unused BenchmarkId import
- `edgestore-cli/src/main.rs` — Fixed clippy warnings: single_match, redundant_pattern_matching, manual_is_multiple_of
- `edgestore-tokio/Cargo.toml` — Added version to path dependency
- `edgestore-cli/Cargo.toml` — Added version to path dependency
- `edgestore/{LICENSE-MIT,LICENSE-APACHE}` — Symlinks to root license files
- `edgestore-tokio/{LICENSE-MIT,LICENSE-APACHE}` — Symlinks
- `edgestore-cli/{LICENSE-MIT,LICENSE-APACHE}` — Symlinks

## Decisions Made

- **NaN-safe comparison:** The benchmark panic revealed that `partial_cmp` + `unwrap_or(Equal)` creates inconsistent total ordering when NaN is present. A dedicated `total_cmp_f32` helper treats NaN as Greater than all non-NaN values, providing a strict weak ordering suitable for `sort_by` and `BinaryHeap`.
- **LICENSE symlinks in crates:** `cargo publish` packages files within each crate directory. Symlinking LICENSE files from the repo root into each crate ensures they're included in the published tarball without duplication.
- **Versioned path dependencies:** Adding `version = "1.0.0"` alongside `path = "../edgestore"` in dependent crates' Cargo.toml is required for crates.io publish readiness while maintaining workspace development convenience.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Fixed NaN-induced total order violation in HNSW search**
- **Found during:** Task 2 (benchmark execution)
- **Issue:** `throughput/vector_search_1000_hnsw` benchmark panicked with "user-provided comparison function does not correctly implement a total order" because `partial_cmp` returns `None` for NaN distances, and `unwrap_or(Equal)` produces inconsistent ordering
- **Fix:** Added `total_cmp_f32` helper in `vector/distance.rs` that treats NaN as Greater than non-NaN. Replaced all 8 occurrences of `partial_cmp(...).unwrap_or(Equal)` across `hnsw.rs`, `search.rs`, `engine.rs`, and benchmarks
- **Files modified:** `edgestore/src/vector/distance.rs`, `edgestore/src/vector/hnsw.rs`, `edgestore/src/vector/search.rs`, `edgestore/src/engine.rs`, `edgestore/benches/hnsw_recall.rs`
- **Verification:** `cargo bench -p edgestore` completes all 4 benchmarks without panic
- **Committed in:** `7628fa8`

**2. [Rule 3 - Blocking] Fixed clippy warnings in edgestore-cli blocking `-D warnings`**
- **Found during:** Task 3 (clippy validation)
- **Issue:** `cargo clippy --workspace -D warnings` failed with 7 errors in `edgestore-cli/src/main.rs` (single_match, redundant_pattern_matching, manual_is_multiple_of, dead_code)
- **Fix:** Applied clippy-suggested rewrites: match→if let, if let Err(_)→.is_err(), count % 1000 == 0→count.is_multiple_of(1000), added #[allow(dead_code)] to deserialized ttl field
- **Files modified:** `edgestore-cli/src/main.rs`
- **Verification:** `cargo clippy --workspace -D warnings` passes clean
- **Committed in:** `clippy fix commit`

---

**Total deviations:** 2 auto-fixed (1 bug, 1 blocking)
**Impact on plan:** Both fixes were necessary for correctness and validation gate passage. No scope creep.

## Issues Encountered

- **Benchmark panic on HNSW search:** The `vector_search_1000_hnsw` throughput benchmark panicked due to NaN values in f32 vectors (deterministic byte patterns in the benchmark produce occasional NaN f32 values). This was fixed with the `total_cmp_f32` helper.
- **Clippy errors in edgestore-cli:** Previously uncaught clippy warnings in the CLI crate blocked the `-D warnings` validation. Fixed inline.
- **No LICENSE files in package:** Initial dry-run packaged README.md but not LICENSE files because they were only at repo root. Fixed by adding symlinks in each crate directory.

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness

- **Ready for v1.0.0 release:** All quality gates pass. The crate can be published to crates.io.
- **Recommended next steps:**
  1. Create git tag `v1.0.0`
  2. Publish `edgestore` to crates.io
  3. Publish `edgestore-tokio` and `edgestore-cli` (after edgestore is indexed)
  4. Create GitHub release with notes from RELEASE_CHECKLIST.md
- **No blockers remaining.**

---
*Phase: 08*
*Plan: 04b*
*Completed: 2026-05-25*
