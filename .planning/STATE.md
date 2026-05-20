# Project State — EdgeStore

## Current Status

**Phase:** 3 — Deathtime Compaction
**Current Phase:** In Progress (Wave 1)
**Milestone:** Milestone 1 (v0.1)

## Phase Progress

| Phase | Status | Started | Completed |
|-------|--------|---------|-----------|
| 1 — Core KV Engine | Complete | 2026-05-18 | 2026-05-18 |
| 2 — Segment Store | Complete | 2026-05-18 | 2026-05-18 |
| 3 — Deathtime Compaction | In Progress | 2026-05-19 | — |
| 4 — Replication + S3 | Not started | — | — |
| 5 — Vector Search | Not started | — | — |
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

## Completed Plans: 03-01, 03-02, 03-03

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

## Next Step

Execute Plan 03-04 (Engine integration) to wire Compactor and SnapshotRegistry into Engine.

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

## Notes

- Project initialized 2026-05-18
- All 16 architectural decisions resolved via grill-me session before initialization
- prod.md contains full design spec with references
- No gsd-sdk installed — research subagents unavailable; roadmap generated inline
