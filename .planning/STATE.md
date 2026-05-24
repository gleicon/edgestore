---
gsd_state_version: 1.0
milestone: v0.1
milestone_name: milestone
  current_phase: 05
  status: Complete
  last_updated: "2026-05-23T03:30:00.000Z"
  progress:
    total_phases: 8
    completed_phases: 5
    total_plans: 32
    completed_plans: 27
    percent: 62
---

# Project State — EdgeStore

## Current Status

**Phase:** 5 — Vector Search
**Current Phase:** 05
**Milestone:** Milestone 3 (v0.3)

## Phase Progress

| Phase | Status | Started | Completed |
|-------|--------|---------|-----------|
| 1 — Core KV Engine | Complete | 2026-05-18 | 2026-05-18 |
| 2 — Segment Store | Complete | 2026-05-18 | 2026-05-18 |
| 3 — Deathtime Compaction | Complete | 2026-05-19 | 2026-05-20 |
| 4 — Replication + S3 | Complete | 2026-05-20 | 2026-05-23 |
| 4.1 — Engine Correctness & Edge Cases | Complete | 2026-05-21 | 2026-05-21 |
| 5 — Vector Search | Complete | 2026-05-23 | 2026-05-23 |
| 6 — SSD Optimization + HNSW | Not started | — | — |
| 7 — Full-Text Search (v2) | Not started | — | — |

## Requirement Status

34 requirements total — 0 validated, 34 active, 0 blocked

## Phase 3 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 03-01 | 1 | Compactor + Snapshot scaffold, config, error | COMPACT-03, COMPACT-04, COMPACT-06 |
| 03-02 | 2 | Compactor core algorithm | COMPACT-04, COMPACT-07 |
| 03-03 | 2 | Snapshot implementation | COMPACT-06 |
| 03-04 | 3 | Engine integration | COMPACT-04, COMPACT-06 |
| 03-05 | 4 | Integration tests | COMPACT-01–07 |

## Completed Plans: 03-01, 03-02, 03-03, 03-04, 03-05

Plan 03-05 (Integration tests) completed 2026-05-20.

- 5 integration tests in edgestore/tests/integration_compaction.rs; all pass
- SC1 (COMPACT-04): TTL expiry → compact_once → live_records_relocated == 0
- SC2 (COMPACT-05): range scan across 3+ overlapping segments returns correct LWW value
- SC3 (COMPACT-06): snapshot pins survive compaction; readable after compact; drop releases pins
- SC4 (COMPACT-04): write_budget_bytes=1 stops compaction after first partial cohort
- SC5 (COMPACT-07): output segment merkle_root matches recomputed blake3 hash of sorted key hashes
- 100 tests pass workspace-wide; cargo clippy -D warnings clean
- Commit: 888c15a

Plan 03-04 (Engine integration) completed 2026-05-20.

- Engine.snapshot_registry: SnapshotRegistry field added (COMPACT-06 wiring)
- SegmentStore.segment_ids() helper added (readers field is private)
- Engine::compact_once: wall-clock now_nanos, pinned_ids, Compactor, manifest.mf reload (COMPACT-04)
- Engine::snapshot: register current segments, return Snapshot with RAII pin release (COMPACT-06)
- 95 tests pass workspace-wide; cargo clippy -D warnings clean
- Commits: fe60746, 5a8011b, fc14138

Plan 03-01 (Compactor + Snapshot scaffold) completed 2026-05-19.

- EdgestoreConfig.compaction_write_budget_bytes = 256 MB default
- EdgestoreError.CompactionError(String) variant
- compactor.rs: Compactor, CohortInfo, CompactionStats
- snapshot.rs: Snapshot, SnapshotRegistry with Drop pin-release
- All modules declared and re-exported in lib.rs
- cargo build --workspace and cargo clippy -D warnings clean

Plan 03-02 (Compactor core algorithm) completed 2026-05-19.

- CohortInfo.is_fully_expired field added
- identify_cohorts: group by cohort_bucket, sort expired-first
- collect_expired_cohort: zero-live-relocation expired cohort removal (COMPACT-04)
- compact_partial_cohort: LWW merge, dead-record filter, SegmentWriter output (COMPACT-07)
- compact_cycle: budget-bounded, pinned-segment-aware, fully-expired-first dispatch (COMPACT-04)
- 8 unit tests; all pass; clippy -D warnings clean
- Commit: 31873ef

Plan 03-03 (Snapshot implementation) completed 2026-05-19.

- SnapshotRegistry: register/release/is_pinned/pinned_ids (COMPACT-06)
- Snapshot::new, Drop (RAII pin release), get (LWW by LSN), range (LWW merge + decode)
- 5 unit tests pass; clippy -D warnings clean
- Commit: 0bdb1aa

## Phase 4 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 04-01 | 1 | Range-level Merkle + Replication types | REPL-01, REPL-02, REPL-03 |
| 04-02 | 2 | Engine replication public API | REPL-05, REPL-06 |
| 04-03 | 3 | edgestore-repl: HTTP client + server | REPL-03 |
| 04-04 | 4 | edgestore-repl: S3 backend | REPL-04 |
| 04-05 | 5 | Integration tests: all 5 success criteria | REPL-01–06 |

## Phase 4.1 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 04.1-01 | 1 | WAL in-write rotation | CORE-02 |
| 04.1-02 | 2 | TTL lazy-expiry contract: document and test | CORE-05 |
| 04.1-03 | 1 | Fix SegmentReader::range_scan end-inclusive bug | CORE-05 |
| 04.1-04 | 2 | Snapshot::get LWW ordering — multi-segment divergence test | CORE-04 |

## Phase 5 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 05-01 | 1 | Vector types, encoding, dtype support | VECTOR-01, VECTOR-04 |
| 05-02 | 2 | Vector KV API (vector_put, vector_get, vector_delete) | VECTOR-02 |
| 05-03 | 3 | Distance metrics — SIMD + scalar, all three metrics | VECTOR-03, VECTOR-04 |
| 05-04 | 4 | Flat scan search with top-k heap | VECTOR-02, VECTOR-03 |
| 05-05 | 5 | Integration tests + benchmarks (all 5 SC) | VECTOR-01–05 |

## Next Step

Phase 5 complete. Phase 6 (SSD Optimization + HNSW) is the next unstarted phase.

Run `/gsd:plan-phase 6` to plan SSD Optimization + HNSW.

## Phase 2 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 02-01 | 1 | Segment types + deps | STORE-01–07 |
| 02-02 | 2 | Segment writer | STORE-01, STORE-02, STORE-03, STORE-04, STORE-05, STORE-07 |
| 02-03 | 3 | Segment reader | STORE-01, STORE-02, STORE-04, STORE-05 |
| 02-04 | 3 | Manifest | STORE-06, STORE-07 |
| 02-05 | 4 | SegmentStore + Engine | STORE-01–03, STORE-06–07 |
| 02-06 | 5 | Integration tests | STORE-01–07 |

## Phase 1 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 01-01 | 1 | Cargo workspace scaffold + core types | CORE-01–08 |
| 01-02 | 2 | WAL implementation | CORE-01, CORE-02, CORE-07 |
| 01-03 | 2 | MemTable trait + BTreeMap impl | CORE-03, CORE-08 |
| 01-04 | 3 | KV Engine — single-writer + KV API | CORE-04, CORE-05, CORE-08 |
| 01-05 | 4 | Transaction API | CORE-04, CORE-06 |
| 01-06 | 5 | Crash recovery | CORE-01, CORE-02, CORE-07 |
| 01-07 | 6 | Integration tests | CORE-01–08 |

## Roadmap Evolution

- Phase 4.1 inserted after Phase 4 (2026-05-21): Engine Correctness & Edge Cases — WAL in-write rotation, TTL lazy-expiry contract, range_scan end-inclusive fix, Snapshot::get LWW ordering fix. Discovered during Phase 3 test coverage review.

## Notes

- Project initialized 2026-05-18
- All 16 architectural decisions resolved via grill-me session before initialization
- prod.md contains full design spec with references
- No gsd-sdk installed — research subagents unavailable; roadmap generated inline

### Phase 4 Verification Gap Fixes (2026-05-23)

Three critical blockers in `import_segment` fixed post-execution:

- **C-01**: Delete tombstones now replicated — added `delete_with_timestamp()` and Delete arm in LWW apply block
- **C-02**: xor filter built from decoded keys — imported segments readable after engine restart
- **C-03**: min_key/max_key tracked during LWW scan — range/prefix queries work on imported segments after restart
- **W-03**: .meta file fsync added for durability

All fixes committed. 147 tests pass workspace-wide. cargo clippy -D warnings clean.

### Phase 5 Completion (2026-05-23)

All 5 plans executed successfully:

- **05-01**: Vector types (Dtype: F32/F16/I8), VectorRecord, encode/decode with 3-byte big-endian header
- **05-02**: VectorEngine trait with vector_put, vector_get, vector_delete on Engine; synthetic `__vec__{ns}` namespace isolation
- **05-03**: All 3 distance metrics (Cosine, L2, DotProduct) with SIMD (wide::f32x8) + scalar fallback; f16/i8 widening
- **05-04**: Brute-force flat scan with top-k BinaryHeap; results sorted ascending; deletes excluded
- **05-05**: 5 success criteria verified + Criterion benchmarks (10K, 100K collections)

**Key fixes during execution:**
- HeapItem Ord: fixed reverse ordering bug so BinaryHeap peek returns worst (largest distance) item
- SC2 tests use set-equality comparison to tolerate SIMD vs scalar near-tie differences

**Stats:**
- 187 tests pass workspace-wide (10 test suites)
- cargo clippy --workspace -D warnings clean
- Benchmarks compile successfully
