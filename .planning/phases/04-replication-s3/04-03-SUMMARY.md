---
phase: 4
plan: "03"
subsystem: replication
tags: [http, replication, anti-entropy, msgpack, pull-only]
dependency_graph:
  requires: ["04-01", "04-02"]
  provides: ["04-04", "04-05"]
  affects: ["edgestore-repl"]
tech_stack:
  added:
    - "ureq 2.x — HTTP client for ReplicationProtocol impl"
    - "tiny_http 0.12 — embedded HTTP server for pull-only endpoints"
    - "rmp-serde 1.x — MessagePack serialization for control messages (D07)"
  patterns:
    - "Arc<Mutex<Engine>> for serialized server access (single-writer)"
    - "Per-peer cursor file with atomic .tmp+rename flush (D08, T-04-09)"
    - "Pull-only anti-entropy: Merkle probe → manifest diff → segment pull"
key_files:
  created:
    - edgestore-repl/Cargo.toml
    - edgestore-repl/src/lib.rs
    - edgestore-repl/src/http_server.rs
    - edgestore-repl/src/http_client.rs
    - edgestore-repl/src/anti_entropy.rs
  modified:
    - Cargo.toml (workspace.members += edgestore-repl)
    - edgestore/src/engine.rs (Engine::db_path() added)
decisions:
  - "MessagePack for all control messages; raw bytes for segment data (D07)"
  - "Pull-only anti-entropy; no push endpoint (D08)"
  - "Per-peer cursor at {db_path}/sync/{peer_id}.cursor — atomic flush via .tmp+rename (T-04-09)"
  - "Engine::db_path() added to edgestore crate for server segment file lookup"
metrics:
  duration: "~30 min"
  completed: "2026-05-23"
  tasks_completed: 2
  tasks_total: 2
  files_created: 5
  files_modified: 2
---

# Phase 4 Plan 03: edgestore-repl HTTP server + pull-only anti-entropy client Summary

## One-liner

HTTP transport layer for pull-only replication: tiny_http server with 3 MessagePack endpoints, ureq client implementing ReplicationProtocol, and cursor-backed AntiEntropyLoop background thread.

## Tasks Completed

| Task | Name | Commit | Key Files |
|------|------|--------|-----------|
| 1 | edgestore-repl crate, Cargo.toml, HTTP server | 1330691 | Cargo.toml, edgestore-repl/Cargo.toml, src/lib.rs, src/http_server.rs |
| 2 | HttpReplicationClient, AntiEntropyLoop, wire lib.rs | 06ae416 | src/http_client.rs, src/anti_entropy.rs, src/lib.rs |

## What Was Built

### edgestore-repl workspace crate

New Rust crate added to workspace with dependencies: `ureq 2.x`, `tiny_http 0.12`, `rmp-serde 1.x`, `blake3 1.x`, `serde 1.x`, `serde_json 1.x`.

### HttpReplicationServer (`http_server.rs`)

Wraps `Arc<Mutex<Engine>>`. Call `start(bind_addr)` to spawn a background server thread.

Three pull-only endpoints:
- `GET /merkle` → MessagePack `{root: Vec<u8>}` — anti-entropy probe value
- `GET /segments` → MessagePack `[{segment_id, segment_hash}]` — full manifest
- `GET /segments/{hash_hex}` → raw bytes, `Content-Type: application/octet-stream`

All endpoints support `?debug=json` query parameter that re-serializes to JSON for human inspection (D07). URL parsing is done inline (no routing library). Engine lock held only during computation — released before file I/O for segment data.

### HttpReplicationClient (`http_client.rs`)

Implements `edgestore::replication::ReplicationProtocol` using `ureq` and `rmp-serde`:
- `merkle_root()` → GET /merkle, decode MessagePack `{root}`, convert Vec<u8> to `[u8; 32]`
- `list_segments()` → GET /segments, decode MessagePack list, convert to `Vec<SegmentRef>`
- `fetch_segment()` → GET /segments/{hash_hex}, read raw bytes body

All network errors mapped to `EdgestoreError::ReplicationError`.

### AntiEntropyLoop (`anti_entropy.rs`)

Background thread for pull-only sync (D08):
- Spawned via `AntiEntropyLoop::start()` which returns `JoinHandle<()>`
- Default probe interval: 30 seconds (configurable via `interval_secs` field)
- Per-peer cursor at `{db_path}/sync/{peer_id}.cursor` in MessagePack format

Cursor fields (`PeerCursor`):
- `last_known_merkle_root: Vec<u8>` — peer root from last sync
- `segments_pending: Vec<Vec<u8>>` — hashes pending application (durable resume after crash)
- `last_attempt_secs: u64` — unix timestamp of last probe
- `segments_applied_total: u64` — running total

Loop cycle:
1. Sleep `interval_secs`
2. Load cursor (corrupt cursor defaults to empty — T-04-09)
3. Probe peer Merkle root → if equal, update cursor and skip
4. Fetch peer manifest → compute missing via `engine.missing_segments()`
5. Update `segments_pending` in cursor, flush
6. For each pending hash: `client.fetch_segment()` → `engine.import_segment()` (BLAKE3 verify + LWW) → remove from pending, flush cursor
7. `HashMismatch` leaves hash in pending for retry next cycle

Atomic cursor flush: serialize with `rmp_serde::to_vec`, write to `{path}.cursor.tmp`, rename to `{path}.cursor` (T-04-09).

## Deviations from Plan

### Auto-added: `Engine::db_path()` public method

**Found during:** Task 1
**Issue:** `HttpReplicationServer` needs the engine's database path to locate segment `.dat` files by hash. The engine struct had `config.path` but no public accessor, and `segment_store` is `pub(crate)`.
**Fix:** Added `pub fn db_path(&self) -> &std::path::Path { &self.config.path }` to `Engine` in `edgestore/src/engine.rs`.
**Files modified:** `edgestore/src/engine.rs`
**Rule:** Rule 2 — missing critical functionality required for correct operation of external crate.

## Known Stubs

None. All three types (`HttpReplicationClient`, `HttpReplicationServer`, `AntiEntropyLoop`) are fully implemented. No hardcoded empty data or placeholder text.

## Threat Flags

No new threat surface beyond what the plan's threat model covers. Mitigations implemented:
- **T-04-07**: Segment data verified via `engine.import_segment()` (BLAKE3 check before applying)
- **T-04-09**: Cursor file uses atomic .tmp+rename write; corrupt cursor treated as empty default

## Self-Check: PASSED

All created files verified on disk:
- edgestore-repl/Cargo.toml: FOUND
- edgestore-repl/src/lib.rs: FOUND
- edgestore-repl/src/http_server.rs: FOUND
- edgestore-repl/src/http_client.rs: FOUND
- edgestore-repl/src/anti_entropy.rs: FOUND

All commits verified in git log:
- 1330691 (Task 1): FOUND
- 06ae416 (Task 2): FOUND
