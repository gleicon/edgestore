---
phase: 4
plan: "05"
subsystem: replication
tags: [integration-tests, anti-entropy, lww, cursor, merkle, filesystem-remote-store, http]
dependency_graph:
  requires: ["04-01", "04-02", "04-03", "04-04"]
  provides:
    - "5 Phase 4 integration tests (SC1–SC5) all passing"
    - "range_merkle_root convergence after import_segment"
    - "AntiEntropyLoop.with_interval builder"
    - "HttpReplicationServer.start returns (JoinHandle, bound_port)"
    - "PeerCursor re-exported from edgestore-repl::lib"
  affects:
    - edgestore/src/engine.rs
    - edgestore/src/config.rs
    - edgestore-repl/src/anti_entropy.rs
    - edgestore-repl/src/http_server.rs
    - edgestore-repl/src/lib.rs
tech_stack:
  added:
    - "rmp-serde dev-dependency in edgestore/Cargo.toml (for SC2 cursor serialization)"
  patterns:
    - "Two-engine pull-only sync over loopback HTTP (SC1)"
    - "Cursor durability simulation via manual MessagePack write (SC2)"
    - "LWW timestamp-ordering assertions via import_segment (SC3)"
    - "Content-addressed segment BLAKE3 round-trip (SC4)"
    - "debug=json HTTP endpoint assertion via ureq+serde_json (SC5)"
key_files:
  created:
    - edgestore/tests/integration_replication.rs
    - edgestore-repl/tests/integration_replication.rs
  modified:
    - edgestore/src/engine.rs
    - edgestore/src/config.rs
    - edgestore-repl/src/anti_entropy.rs
    - edgestore-repl/src/http_server.rs
    - edgestore-repl/src/lib.rs
    - edgestore/Cargo.toml
decisions:
  - "range_merkle_root rewritten to use sorted segment_hash values (BLAKE3 of raw bytes) instead of per-segment merkle_root field — ensures A and B converge after sync (segment_hash is the same on both sides after import)"
  - "HttpReplicationServer::start() return type changed from Result<JoinHandle, E> to Result<(JoinHandle, u16), E> to expose the OS-assigned port for dynamic-port test servers"
  - "SC4 (FilesystemRemoteStore) placed in edgestore-repl/tests/ to avoid circular dependency (edgestore cannot depend on edgestore-repl)"
  - "SC1 uses AntiEntropyLoop.with_interval(1) to reduce probe wait from 30s to 1s in tests"
requirements: [REPL-01, REPL-02, REPL-03, REPL-04, REPL-05, REPL-06]
metrics:
  duration: "~35 minutes"
  completed: "2026-05-23"
  tasks_completed: 3
  tasks_total: 3
  files_created: 2
  files_modified: 6
---

# Phase 4 Plan 05: Integration Tests — 5 Phase 4 Success Criteria Summary

## One-liner

Five Phase 4 integration tests (SC1-SC5) all passing: two-engine HTTP anti-entropy sync, cursor durability, LWW conflict resolution, FilesystemRemoteStore BLAKE3 round-trip, and debug=json endpoints.

## What Was Built

### Task 1: edgestore/tests/integration_replication.rs — SC2, SC3

Three test functions covering cursor durability and LWW conflict resolution:

**`test_sc2_cursor_durability`**
- Engine A writes 10 keys across 2 segments (small segment_size forces multiple flushes)
- Engine B imports segment 0 via `import_segment` → `Applied`
- Cursor file written to `{b_path}/sync/peer-a.cursor` with segment 1 still pending (MessagePack format)
- Engine B dropped and reopened (simulates crash+restart)
- Segment 0 re-imported → `Skipped` (already present in B's manifest)
- Remaining segment imported → all 10 keys readable in B

**`test_sc3_lww_higher_timestamp_wins`**
- Engine A writes "shared" at `t_a`; sleeps 5 ms; Engine B writes "shared" at `t_b > t_a`
- A flushed to segment; segment imported into B
- Result: `Applied { keys_skipped >= 1 }` — B's higher-ts record wins
- B.get("shared") returns "from_b"

**`test_sc3_lww_collision_local_wins`**
- Engine A writes "collision" at `t_a`; sleeps 2ms; B writes "collision" at `t_b > t_a`
- A flushed and imported into B → B's record wins (local ts >= incoming ts)
- B.get("collision") returns "from_beta"

Supporting changes for Task 1:
- `edgestore-repl/src/anti_entropy.rs`: `with_interval(secs: u64)` builder added
- `edgestore-repl/src/http_server.rs`: `start()` now returns `(JoinHandle, u16)` with the OS-assigned bound port (via `tiny_http::Server::server_addr().to_ip().port()`)
- `edgestore-repl/src/lib.rs`: `pub use anti_entropy::PeerCursor` added
- `edgestore/Cargo.toml`: `rmp-serde = "1"` dev-dependency for cursor file serialization

### Task 2: edgestore-repl/tests/integration_replication.rs — SC1, SC4, SC5

Three test functions covering HTTP sync, storage round-trip, and debug endpoints:

**`test_sc1_two_engine_sync_via_http`**
- Engine A writes 10 keys, flushes to 10 segments (segment_size=256 → 1 key per segment)
- `HttpReplicationServer::new(engine_a).start("127.0.0.1:0")` binds to a random port
- Engine B starts fresh; `AntiEntropyLoop.with_interval(1).start()` probes A every 1 second
- After 4s sleep: B's `compare_merkle(&a_root)` returns true
- All 10 keys readable in B with correct values

**`test_sc4_filesystem_remote_store_roundtrip`** (T-04-14 mitigated)
- `FilesystemRemoteStore::new(tempdir)` created
- 1024 bytes of test data uploaded under `blake3::hash(data)` as the key
- Downloaded bytes compared byte-for-byte to original
- `blake3::hash(downloaded) == blake3::hash(original)` asserted
- After `delete()`, `download()` returns `Err`

**`test_sc5_debug_json_endpoint`**
- Engine A writes 3 keys, flushes to segment; server started on random port
- `GET /merkle?debug=json` → 200, `Content-Type: application/json`, body is JSON object with "root" key
- `GET /segments?debug=json` → 200, body is JSON array; each element has "segment_id" and "segment_hash" keys

**Root cause fix (Rule 1 — Bug):**
`Engine::range_merkle_root` was using `RangeMerkleTree::build` with `meta.merkle_root` (per-segment BLAKE3 of sorted key hashes, computed by `SegmentWriter`). However, `import_segment` sets `meta.merkle_root = hash.to_vec()` (BLAKE3 of raw segment bytes). These two computations produce different values, so A and B never converge after sync.

Fixed by rewriting `range_merkle_root` to compute the Merkle root directly from sorted `meta.segment_hash` values (BLAKE3 of raw bytes). Both `SegmentWriter::flush` and `import_segment` consistently set `meta.segment_hash` to the same content hash, so both nodes have identical sorted hash sets after a complete sync.

### Task 3: Full workspace test run and clippy clean

- All 144 workspace tests pass (0 failures)
- `cargo clippy --workspace -- -D warnings` clean (0 errors)
- **Pre-existing bug fixed (Rule 1):** `config.rs` had `segment_size_bytes = 4 MiB` but `config::tests::test_defaults` expected `16 MiB` per the CLAUDE.md spec. Restored to `16 * 1024 * 1024`.

## Verification Results

```
cargo test --workspace → 144 tests, 0 failed
cargo clippy --workspace -- -D warnings → 0 errors
cargo test --test integration_replication -p edgestore → 3 tests pass (SC2, SC3a, SC3b)
cargo test --test integration_replication -p edgestore-repl → 3 tests pass (SC1, SC4, SC5)
```

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] range_merkle_root did not converge after import_segment**
- **Found during:** Task 2 (SC1 test_sc1_two_engine_sync_via_http)
- **Issue:** `RangeMerkleTree::build` uses `meta.merkle_root` (BLAKE3 of sorted key hashes from SegmentWriter) but `import_segment` sets `meta.merkle_root = hash.to_vec()` (BLAKE3 of raw segment bytes). The two nodes use incompatible Merkle root inputs after sync, so `compare_merkle` always returns `false`.
- **Fix:** Rewrote `Engine::range_merkle_root` to use sorted `meta.segment_hash` (consistently set to BLAKE3 of raw bytes on both writer and importer). Removed unused `RangeMerkleTree` import from engine.rs.
- **Files modified:** `edgestore/src/engine.rs`
- **Commit:** 977e3ea

**2. [Rule 1 - Bug] segment_size_bytes default was 4 MiB, test expected 16 MiB**
- **Found during:** Task 3 (cargo test --workspace)
- **Issue:** `config.rs` had `segment_size_bytes = 4 * 1024 * 1024` but the CLAUDE.md spec says 16 MiB and the unit test `config::tests::test_defaults` asserts `16 * 1024 * 1024`.
- **Fix:** Changed to `16 * 1024 * 1024`.
- **Files modified:** `edgestore/src/config.rs`
- **Commit:** 6e197a2

**3. [Rule 2 - Missing critical functionality] SC4 moved to edgestore-repl/tests/**
- **Found during:** Task 1 planning
- **Issue:** Plan suggested SC4 could be in edgestore/tests/ with edgestore-repl as dev-dep, but that creates a circular dependency (edgestore-repl already depends on edgestore).
- **Fix:** SC4 test placed in edgestore-repl/tests/integration_replication.rs alongside SC1 and SC5.
- **Files modified:** None (design decision before writing)

**4. [Rule 2 - Missing critical API] HttpReplicationServer::start return type extended**
- **Found during:** Task 2 (SC1 needs dynamic port after "127.0.0.1:0" bind)
- **Issue:** `start()` returned only `JoinHandle`; no way to get the OS-assigned port for building peer URL.
- **Fix:** Changed return type to `Result<(JoinHandle, u16), E>` exposing `bound_port` via `tiny_http::Server::server_addr().to_ip().port()`.
- **Files modified:** `edgestore-repl/src/http_server.rs`
- **Commit:** 30cbf8f

## Known Stubs

None — all 5 success criteria are implemented and passing with real data.

## Threat Flags

None. No new network endpoints, auth paths, file access patterns, or schema changes beyond the planned scope.

## Commits

| Task | Hash | Description |
|------|------|-------------|
| Task 1 | 30cbf8f | test(04-05): SC2 and SC3 tests + with_interval + server port accessor |
| Task 2 | 977e3ea | test(04-05): SC1, SC4, SC5 tests + fix range_merkle_root convergence |
| Task 3 | 6e197a2 | fix(04-05): restore segment_size_bytes default to 16 MiB |

## Self-Check: PASSED

- [x] `edgestore/tests/integration_replication.rs` exists with test_sc2, test_sc3a, test_sc3b
- [x] `edgestore-repl/tests/integration_replication.rs` exists with test_sc1, test_sc4, test_sc5
- [x] `edgestore/src/engine.rs` range_merkle_root uses sorted segment_hash values
- [x] `edgestore-repl/src/anti_entropy.rs` has with_interval builder
- [x] `edgestore-repl/src/http_server.rs` start() returns (JoinHandle, u16)
- [x] `edgestore-repl/src/lib.rs` exports PeerCursor
- [x] Commits 30cbf8f, 977e3ea, 6e197a2 exist
- [x] cargo test --workspace exits 0 (144 passed, 0 failed)
- [x] cargo clippy --workspace -- -D warnings exits 0
