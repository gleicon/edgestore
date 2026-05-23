---
phase: 4
plan: "01"
subsystem: replication
tags: [merkle, replication, remote-store, anti-entropy, traits]
dependency_graph:
  requires: []
  provides:
    - edgestore::RangeMerkleTree
    - edgestore::ReplicationProtocol
    - edgestore::HostId
    - edgestore::SegmentRef
    - edgestore::RemoteStore
  affects:
    - edgestore/src/lib.rs
tech_stack:
  added: []
  patterns:
    - Range-level Merkle tree with 16 key-range buckets (leading nibble routing)
    - Pull-only replication protocol trait (object-safe, 3-method)
    - Content-addressed RemoteStore trait (4-method, Send + Sync)
key_files:
  created:
    - edgestore/src/merkle.rs
    - edgestore/src/replication.rs
    - edgestore/src/remote_store.rs
  modified:
    - edgestore/src/lib.rs
decisions:
  - "RangeMerkleTree is probe-only per D02; manifest-diff owns sync routing"
  - "ReplicationProtocol has exactly 3 methods matching pull-only model (D02, D08)"
  - "RemoteStore trait defined now; FilesystemRemoteStore is Phase 4 impl only (D04, D05)"
  - "No new external dependencies — blake3 already in Cargo.toml"
metrics:
  duration: "~10 minutes"
  completed: "2026-05-23"
  tasks_completed: 3
  tasks_total: 3
  files_created: 3
  files_modified: 1
---

# Phase 4 Plan 01: Range-level Merkle + Replication Types + RemoteStore Trait Summary

## One-liner

RangeMerkleTree probe (16-bucket BLAKE3), pull-only ReplicationProtocol trait, and content-addressed RemoteStore trait — type contracts for Plans 04-02 through 04-05.

## What Was Built

Three new modules in the `edgestore` crate establishing the replication type system:

**`edgestore/src/merkle.rs`** — `RangeMerkleTree` anti-entropy probe.
- 16-bucket structure; bucket assignment by `min_key[0] >> 4` (leading nibble)
- `build(segments)`: groups by bucket, sorts by segment_id, hashes each group with BLAKE3
- `root()`: BLAKE3 of all 16 bucket hashes concatenated (512-byte input)
- `differing_buckets(&other)`: returns indices where bucket hashes differ
- Role is strictly probe-only per D02; manifest-diff handles sync routing
- 3 unit tests pass

**`edgestore/src/replication.rs`** — Pull-only protocol types and trait.
- `HostId(String)` newtype with Display, From<String>, From<&str>, Serialize, Deserialize
- `SegmentRef { segment_hash: [u8;32], segment_id: u64 }` with `hash_hex()` (no external hex crate)
- `ReplicationProtocol` trait: 3 methods only — `merkle_root`, `list_segments`, `fetch_segment`
- Object-safe: no generics, no associated types

**`edgestore/src/remote_store.rs`** — Durable segment backend trait.
- `RemoteStore: Send + Sync` with 4 methods: `upload`, `download`, `list`, `delete`
- Content-addressed by BLAKE3 hash; upload is idempotent

**`edgestore/src/lib.rs`** updated:
- `pub mod merkle; pub mod replication; pub mod remote_store;` declared
- Re-exports: `RangeMerkleTree`, `HostId`, `ReplicationProtocol`, `SegmentRef`, `RemoteStore`

## Verification

```
cargo test -p edgestore merkle     → 3 tests pass
cargo build --workspace            → exits 0
cargo clippy -p edgestore -D warnings → clean
```

## Deviations from Plan

None — plan executed exactly as written.

## Threat Model Coverage

- **T-04-01 (Tampering/fetch_segment):** Documented in `fetch_segment` doc comment — caller MUST verify BLAKE3 before applying. Enforced at call site in Plan 04-02.
- **T-04-02 (Spoofing/HostId):** Accepted — HostId is advisory only in Phase 4, no auth.
- **T-04-SC (Cargo slopcheck):** No new dependencies added.

## Commits

| Task | Hash | Description |
|------|------|-------------|
| Task 1 | f03581e | feat(04-01): create RangeMerkleTree anti-entropy probe |
| Task 2 | 74e2182 | feat(04-01): create replication.rs pull-only protocol types and trait |
| Task 3 | 432f24b | feat(04-01): create remote_store.rs and wire all three modules in lib.rs |

## Self-Check: PASSED

- [x] `edgestore/src/merkle.rs` exists with RangeMerkleTree, build, root, differing_buckets, empty
- [x] `edgestore/src/replication.rs` exists with HostId, SegmentRef, ReplicationProtocol (3 methods)
- [x] `edgestore/src/remote_store.rs` exists with RemoteStore (4 methods, Send + Sync)
- [x] `edgestore/src/lib.rs` declares all 3 modules and re-exports all 5 types
- [x] 3 merkle unit tests pass
- [x] cargo build --workspace exits 0
- [x] cargo clippy -p edgestore -D warnings clean
- [x] Commits f03581e, 74e2182, 432f24b exist
