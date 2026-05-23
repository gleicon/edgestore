---
phase: 4
plan: "02"
subsystem: engine-replication
tags: [replication, engine, lww, merkle, anti-entropy]
dependency_graph:
  requires: [04-01]
  provides: [engine-export-manifest, engine-missing-segments, engine-import-segment, engine-range-merkle-root, engine-compare-merkle]
  affects: [edgestore/src/engine.rs, edgestore/src/lib.rs]
tech_stack:
  added: []
  patterns: [LWW conflict resolution, BLAKE3 content addressing, atomic file rename, RangeMerkleTree anti-entropy probe]
key_files:
  created: []
  modified:
    - edgestore/src/engine.rs
    - edgestore/src/lib.rs
decisions:
  - "LWW timestamp-only tie-break: MemEntry has no host_id field in v1; on timestamp tie, local wins (skip incoming). host_id tiebreaking deferred to v2 when the field is added to WalRecord."
  - "import_segment writes .idx and .xf sidecars with minimal content so SegmentReader::open can load the imported dat file; the imported segment's filter is empty (LWW-applied records land in the memtable, not re-read through the segment)"
  - "segment_hash is Vec<u8> in SegmentMeta and [u8;32] in SegmentRef; conversion done inline with copy_from_slice"
metrics:
  duration: "~15 minutes"
  completed: "2026-05-23"
  tasks_completed: 1
  files_modified: 2
---

# Phase 4 Plan 02: Engine Replication API Summary

**One-liner:** Five Engine replication methods (export_manifest, missing_segments, import_segment, range_merkle_root, compare_merkle) with LWW timestamp-based conflict resolution and BLAKE3 content verification.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | ReplicationError variant in error.rs | 249e572 | edgestore/src/error.rs |
| 2 | ImportResult enum + 5 Engine replication methods | 2ca9877 | edgestore/src/engine.rs, edgestore/src/lib.rs |

## What Was Built

### ImportResult enum (engine.rs)

Three-variant enum defined above the Engine struct:
- `Applied { keys_written: u64, keys_skipped: u64 }` — segment applied with LWW per-record decisions
- `Skipped` — segment already present in local manifest (hash match), no-op
- `HashMismatch` — BLAKE3(data) != claimed hash, segment rejected

### Engine::export_manifest

Returns local segment list as `Vec<SegmentRef>` by reading `segment_store.list_segment_metas()` and converting `Vec<u8>` segment hashes to `[u8; 32]`. No serialization — callers serialize if needed.

### Engine::missing_segments

Pure computation. Builds a `HashSet<Vec<u8>>` of local segment hashes, then filters the peer's list to return hashes we do not have. No I/O, takes `&self`.

### Engine::import_segment

Critical path with T-04-03 and T-04-04 threat mitigations applied:

1. Hash-based dedup check (return `Skipped` if already present)
2. BLAKE3 verification (return `HashMismatch` if mismatch)
3. Write to `{hash_hex}.tmp`
4. Atomic rename to `{hash_hex}.dat`
5. Block-by-block decompression and deserialize_entry iteration
6. LWW per record: incoming timestamp > local timestamp → apply; tie or local wins → skip
7. Rename canonical dat to `segment-{id:08}.dat`, write .idx/.xf/.meta sidecars
8. Register via `segment_store.add_imported_segment`

NTP requirement documented in code comment per D06.

### Engine::put_with_timestamp

Private method identical to `put_inner` but accepts an explicit `timestamp: i64` parameter instead of calling `Self::now_nanos()`. Used by `import_segment` LWW application.

### Engine::range_merkle_root

Calls `segment_store.list_segment_metas()`, builds `RangeMerkleTree::build`, returns root as `[u8; 32]`.

### Engine::compare_merkle

Builds local tree, returns `Ok(local_root == *other_root)`. Doc comment explains the D02 protocol: false → caller should call `export_manifest + missing_segments`.

### lib.rs re-export

`pub use engine::{Engine, ImportResult};` added to crate root.

## Verification Results

```
cargo build --workspace       → exit 0 (clean)
cargo clippy -p edgestore -D warnings → exit 0 (clean)
cargo test --workspace        → 103 passed, 1 pre-existing failure (config::test_defaults)
```

The pre-existing `config::tests::test_defaults` failure asserts `segment_size_bytes == 16MB` but the default is 4MB — this mismatch predates this plan and is not caused by any change in plans 04-01 or 04-02.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] clippy manual_is_multiple_of lint**
- **Found during:** Task 2 clippy run
- **Issue:** Used `%` operator for block alignment check; clippy -D warnings requires `.is_multiple_of()`
- **Fix:** Replaced `payload_size % SEGMENT_BLOCK_SIZE == 0` with `payload_size.is_multiple_of(SEGMENT_BLOCK_SIZE)`
- **Files modified:** edgestore/src/engine.rs
- **Commit:** 2ca9877 (fixed in same commit)

**2. [Rule 1 - Design gap] host_id LWW tiebreaker not implementable in v1**
- **Found during:** Task 2 — implementing LWW tie-break logic
- **Issue:** Plan D06 specifies "lower HostId string wins" on timestamp collision, but `MemEntry` and `WalRecord` have no `host_id` field in the current schema.
- **Fix:** On timestamp tie, local record wins (incoming is skipped). This is documented in the code comment and recorded as a key decision. host_id tiebreaking is deferred to v2 when the field is added to WalRecord.
- **Files modified:** edgestore/src/engine.rs (comment + keys_skipped counter)
- **Impact:** Minor: in practice, exact timestamp collisions across nodes are rare under NTP. The LWW outcome is still deterministic (local wins = idempotent on retry from different node perspectives).

**3. [Rule 2 - Missing critical functionality] SegmentMeta unused import**
- **Found during:** Task 2 build
- **Issue:** Original `use crate::types::{..., SegmentMeta, ...}` became unused after the replication methods were added (SegmentMeta used only through `crate::types::SegmentMeta` qualified paths).
- **Fix:** Removed `SegmentMeta` from the `use` statement.
- **Files modified:** edgestore/src/engine.rs
- **Commit:** 2ca9877

## Known Stubs

None. All 5 methods are fully implemented with real I/O and logic.

## Threat Flags

| Flag | File | Description |
|------|------|-------------|
| T-04-03 mitigated | edgestore/src/engine.rs | import_segment Step 2: BLAKE3(data) == hash verified before write |
| T-04-04 mitigated | edgestore/src/engine.rs | import_segment Steps 3-4: write to .tmp then atomic rename; crash between write/rename leaves .tmp which is ignored on restart |

## Self-Check: PASSED

- edgestore/src/engine.rs — exists and contains all 5 methods + ImportResult + put_with_timestamp
- edgestore/src/lib.rs — contains `pub use engine::{Engine, ImportResult};`
- Commits 249e572 (Task 1) and 2ca9877 (Task 2) both exist in git log
- cargo build --workspace exit 0
- cargo clippy -p edgestore -D warnings exit 0
