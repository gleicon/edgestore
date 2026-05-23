---
phase: 04-replication-s3
verified: 2026-05-23T00:00:00Z
status: gaps_found
score: 8/12 must-haves verified
overrides_applied: 0
gaps:
  - truth: "Engine::import_segment applies LWW correctly for all record types including deletes"
    status: failed
    reason: "C-01: The apply block only handles Operation::Put. Delete tombstones from remote segments are silently dropped — never written to local WAL or memtable. Keys deleted on Node A reappear on Node B after sync."
    artifacts:
      - path: "edgestore/src/engine.rs"
        issue: "import_segment apply block checks `if incoming.op == Put` only; no Delete arm. Lines ~718-733."
    missing:
      - "Add delete_with_timestamp private method analogous to put_with_timestamp"
      - "Add Delete arm in apply block: `else if incoming.op == Delete { self.delete_with_timestamp(&ns, &key, incoming.timestamp)?; keys_written += 1; }`"

  - truth: "Imported segments are readable after engine restart (post-restart data durability)"
    status: failed
    reason: "C-02: import_segment builds the xor filter from an empty key list (vec![]). SegmentReader::get uses the filter as a mandatory positive-membership gate. After restart (cold memtable), every get on a key that arrived via import_segment returns None. SC1/SC2 tests pass only because assertion happens before restart while keys are still in memtable."
    artifacts:
      - path: "edgestore/src/engine.rs"
        issue: "Lines ~789-793: `let empty_keys: Vec<Vec<u8>> = vec![]; let filter = crate::segment::build_xor_filter(&empty_keys)?;`"
    missing:
      - "Collect decoded key bytes during the LWW scan loop into a `Vec<Vec<u8>>`"
      - "Pass that key list to build_xor_filter instead of empty_keys"

  - truth: "Range scans and prefix queries return data from imported segments"
    status: failed
    reason: "C-03: import_segment stores SegmentMeta with min_key: vec![] and max_key: vec![]. SegmentReader::range_scan uses these bounds for early exit: `start > self.meta.max_key` evaluates true for any non-empty start key, so range/prefix queries immediately return empty for all imported segments."
    artifacts:
      - path: "edgestore/src/engine.rs"
        issue: "Lines ~765-780: SegmentMeta constructed with `min_key: vec![]` and `max_key: vec![]`"
    missing:
      - "Track first and last encoded_key seen during LWW scan loop"
      - "Populate meta.min_key and meta.max_key before writing the .meta file"

  - truth: "REPL-03 full protocol spec satisfied (ListManifests, GetManifest, RequestDelta, SendSegments, Ack)"
    status: failed
    reason: "REPL-03 in REQUIREMENTS.md requires: ListManifests(), GetManifest(host_id), CompareMerkle(root), RequestDelta(range, since_lsn), SendSegments(hashes), Ack(lsn). The implemented protocol is a simplified pull-only design (3 methods: merkle_root, list_segments, fetch_segment) that omits RequestDelta(range, since_lsn), Ack(lsn), ListManifests(), and GetManifest(host_id). This is an intentional design deviation (D02, D08) chosen by the planning process, but constitutes a gap against the written REQUIREMENTS.md text."
    artifacts:
      - path: "edgestore/src/replication.rs"
        issue: "ReplicationProtocol trait has 3 methods; REPL-03 specifies 6 distinct operations"
    missing:
      - "Either update REQUIREMENTS.md to reflect the chosen pull-only design (D02 decision), or implement the missing protocol methods (RequestDelta, Ack, ListManifests, GetManifest)"
      - "Roadmap SC5 ('Interrupted sync resumes from last Ack(lsn)') is satisfied by cursor-based resumption, but the Ack protocol message itself is absent"
deferred: []
human_verification: []
---

# Phase 4: Replication + S3 Verification Report

**Phase Goal:** Implement pull-only peer replication with anti-entropy and remote segment archival
**Verified:** 2026-05-23
**Status:** gaps_found
**Re-verification:** No — initial verification

## Goal Achievement

### Observable Truths

| # | Truth | Status | Evidence |
|---|-------|--------|----------|
| 1 | RangeMerkleTree with 16 key-range buckets exists and builds correctly | VERIFIED | `edgestore/src/merkle.rs` fully implements RangeMerkleTree::build, root, differing_buckets, empty; 3 unit tests pass |
| 2 | ReplicationProtocol trait with 3 pull-only methods compiles and is object-safe | VERIFIED | `edgestore/src/replication.rs` defines merkle_root, list_segments, fetch_segment; HostId and SegmentRef derive Serialize, Deserialize |
| 3 | RemoteStore trait (4 methods, Send + Sync) exists | VERIFIED | `edgestore/src/remote_store.rs` defines upload, download, list, delete |
| 4 | Engine replication API (export_manifest, missing_segments, import_segment, range_merkle_root, compare_merkle) exposed publicly | VERIFIED | All 5 methods present in engine.rs; ImportResult enum with 3 variants; re-exported via lib.rs |
| 5 | LWW conflict resolution applies higher-timestamp wins; local wins on tie | VERIFIED | engine.rs lines 701-715 implement timestamp comparison; tie goes to local (host_id tiebreaker deferred to v2 per decision) |
| 6 | HTTP server serves 3 endpoints with MessagePack and ?debug=json | VERIFIED | http_server.rs dispatches GET /merkle, GET /segments, GET /segments/{hash_hex}; ?debug=json re-serializes to JSON |
| 7 | HttpReplicationClient implements ReplicationProtocol using rmp-serde | VERIFIED | http_client.rs implements all 3 trait methods using ureq + rmp_serde |
| 8 | AntiEntropyLoop background thread with per-peer cursor (D08) | VERIFIED | anti_entropy.rs implements full pull-sync loop; PeerCursor with 4 required fields; atomic cursor flush via .tmp+rename |
| 9 | FilesystemRemoteStore implements RemoteStore with atomic upload and idempotency | VERIFIED | filesystem_remote_store.rs implements all 4 methods; 5 unit tests pass |
| 10 | import_segment applies LWW for all record types including deletes | FAILED | C-01: Delete tombstones silently dropped — only Put handled in apply block |
| 11 | Imported segments remain readable after engine restart | FAILED | C-02: Empty xor filter written for imported segments; post-restart reads return None |
| 12 | Range/prefix queries return data from imported segments | FAILED | C-03: min_key/max_key both empty in imported SegmentMeta; range_scan early-exits for all queries |

**Score:** 9/12 truths verified (C-01, C-02, C-03 are FAILED blockers)

Note: Truth #4 ("REPL-03 full protocol spec satisfied") is scored separately in Requirements Coverage. The 9/12 score counts the phase plan must-haves; REPL-03 gap is a requirements coverage issue.

### Required Artifacts

| Artifact | Expected | Status | Details |
|----------|----------|--------|---------|
| `edgestore/src/merkle.rs` | RangeMerkleTree — 16-bucket probe | VERIFIED | Exists; build, root, differing_buckets, empty implemented; 3 tests pass |
| `edgestore/src/replication.rs` | HostId, SegmentRef, ReplicationProtocol (3 methods) | VERIFIED | All types present; Serialize/Deserialize derived; hash_hex() without external crate |
| `edgestore/src/remote_store.rs` | RemoteStore trait (4 methods, Send+Sync) | VERIFIED | Exists; all 4 methods; Send+Sync bounds |
| `edgestore/src/engine.rs` | 5 replication methods + ImportResult | VERIFIED | All present; import_segment has C-01/C-02/C-03 bugs |
| `edgestore-repl/src/http_server.rs` | HttpReplicationServer with 3 endpoints | VERIFIED | Exists; endpoints implemented; ?debug=json works; start() returns (JoinHandle, u16) |
| `edgestore-repl/src/http_client.rs` | HttpReplicationClient implementing ReplicationProtocol | VERIFIED | Exists; all 3 methods; MessagePack via rmp-serde |
| `edgestore-repl/src/anti_entropy.rs` | AntiEntropyLoop with cursor | VERIFIED | Exists; with_interval, with_remote_store builders; cursor atomic flush |
| `edgestore-repl/src/filesystem_remote_store.rs` | FilesystemRemoteStore implementing RemoteStore | VERIFIED | Exists; 4 methods; 5 unit tests pass |
| `edgestore/tests/integration_replication.rs` | SC2, SC3a, SC3b tests | VERIFIED | 3 test functions present and passing |
| `edgestore-repl/tests/integration_replication.rs` | SC1, SC4, SC5 tests | VERIFIED | 3 test functions present and passing |

### Key Link Verification

| From | To | Via | Status | Details |
|------|----|-----|--------|---------|
| `edgestore/src/lib.rs` | `edgestore/src/merkle.rs` | `pub mod merkle; pub use merkle::RangeMerkleTree;` | WIRED | Confirmed in lib.rs lines 4, 23 |
| `edgestore/src/lib.rs` | `edgestore/src/replication.rs` | `pub mod replication; pub use replication::{HostId, ReplicationProtocol, SegmentRef};` | WIRED | Confirmed |
| `edgestore/src/lib.rs` | `edgestore/src/remote_store.rs` | `pub mod remote_store; pub use remote_store::RemoteStore;` | WIRED | Confirmed |
| `edgestore/src/engine.rs` | `edgestore/src/replication.rs` | `use crate::replication::SegmentRef;` | WIRED | Confirmed |
| `edgestore-repl/src/http_server.rs` | `edgestore/src/engine.rs` | Calls engine.range_merkle_root, engine.export_manifest | WIRED | Verified in http_server.rs dispatch |
| `edgestore-repl/src/anti_entropy.rs` | `edgestore/src/engine.rs` | Calls engine.compare_merkle, engine.missing_segments, engine.import_segment | WIRED | Verified in run_once() |
| `edgestore-repl/src/filesystem_remote_store.rs` | `edgestore/src/remote_store.rs` | `impl RemoteStore for FilesystemRemoteStore` | WIRED | Confirmed |
| `edgestore-repl/src/anti_entropy.rs` | `edgestore-repl/src/filesystem_remote_store.rs` | `remote_store: Option<Arc<dyn RemoteStore>>` field + with_remote_store builder | WIRED | Confirmed |
| `engine::range_merkle_root` | `merkle::RangeMerkleTree` | **NOT USED** — engine.rs rewrote range_merkle_root to bypass RangeMerkleTree | PARTIAL | Plan 04-01 key_link states "Engine::range_merkle_root builds RangeMerkleTree"; instead it computes a sorted hash directly; RangeMerkleTree exists but is not used by engine at runtime |

### Data-Flow Trace (Level 4)

| Artifact | Data Variable | Source | Produces Real Data | Status |
|----------|--------------|--------|--------------------|--------|
| `http_server.rs GET /merkle` | engine.range_merkle_root() | engine.rs segment store | Yes — reads live segment_hash list | FLOWING |
| `http_server.rs GET /segments` | engine.export_manifest() | engine.rs segment_store.list_segment_metas() | Yes — live manifest | FLOWING |
| `http_server.rs GET /segments/{hash}` | std::fs::read(segment-{id}.dat) | canonical .dat file | Yes — real bytes | FLOWING |
| `anti_entropy.rs import loop` | client.fetch_segment() → engine.import_segment() | peer HTTP → disk | Yes — real bytes with BLAKE3 verify | FLOWING but C-01/C-02/C-03 bugs corrupt outcome |
| `filesystem_remote_store.rs download` | std::fs::read({hash}.seg) | local filesystem | Yes — real content | FLOWING |

### Behavioral Spot-Checks

| Behavior | Command | Result | Status |
|----------|---------|--------|--------|
| cargo build --workspace | `cargo build --workspace` | Finished dev profile | PASS |
| cargo clippy workspace | `cargo clippy --workspace -- -D warnings` | 0 errors | PASS |
| edgestore unit tests | `cargo test -p edgestore` | 104 passed, 0 failed | PASS |
| edgestore-repl unit tests | `cargo test -p edgestore-repl` | 5 passed (filesystem unit) + 14 passed (unit) | PASS |
| edgestore integration tests | `cargo test -p edgestore --test integration_replication` | 3 passed (SC2, SC3a, SC3b) | PASS |
| edgestore-repl integration tests | `cargo test -p edgestore-repl --test integration_replication` | 3 passed (SC1, SC4, SC5) | PASS |
| Full workspace test run | `cargo test --workspace` | 144 passed across all crates, 0 failed | PASS |

### Probe Execution

No probe scripts declared or found for this phase. Step 7c: SKIPPED (no probe-*.sh files).

### Requirements Coverage

| Requirement | Source Plan | Description | Status | Evidence |
|-------------|------------|-------------|--------|----------|
| REPL-01 | 04-01, 04-05 | Segment-level Merkle: each segment has a merkle_root (BLAKE3) in metadata | SATISFIED | SegmentMeta.merkle_root present; segment_hash also stored; compare works in tests |
| REPL-02 | 04-01, 04-05 | Range-level Merkle: tree over key range buckets; two hosts compare and exchange only divergent ranges | PARTIALLY SATISFIED | RangeMerkleTree with 16 buckets (leading nibble) exists and tested. However, engine::range_merkle_root bypasses RangeMerkleTree entirely and uses a flat sorted hash instead. The anti-entropy loop does not use differing_buckets for fine-grained routing; it always full-syncs on any root diff. Bucket-level routing is built but unused at runtime. |
| REPL-03 | 04-03, 04-05 | Transport-agnostic protocol: ListManifests, GetManifest(host_id), CompareMerkle(root), RequestDelta(range, since_lsn), SendSegments(hashes), Ack(lsn); HTTP transport first | PARTIALLY SATISFIED | HTTP transport implemented with 3-method subset. CompareMerkle equivalent (compare_merkle/range_merkle_root) and segment transfer exist. ListManifests, GetManifest(host_id), RequestDelta(range, since_lsn), Ack(lsn) are absent — deliberately replaced by simplified pull-only design (D02). This is a requirements deviation without a formal update to REQUIREMENTS.md. |
| REPL-04 | 04-04, 04-05 | S3 layout: segments/{hash}.dat, hosts/{id}/manifests/latest.json, wal/{id}/{lsn}.log, snapshots/{id}/manifest.json | NOT SATISFIED | FilesystemRemoteStore uses {hash_hex}.seg flat layout, not the S3 layout path scheme from REPL-04. No hosts/, wal/, snapshots/ directories. Phase 4 explicitly deferred S3 to a future phase (D04), but REQUIREMENTS.md marks REPL-04 as Phase 4 scope with no deferral annotation. |
| REPL-05 | 04-02, 04-05 | LWW by WAL timestamp; documented clock skew limitation; WAL record has reserved extension field for vector clock | PARTIALLY SATISFIED | LWW by timestamp implemented and tested (SC3). Clock skew documented in code comment. WalRecord has txid field for future use but no explicitly reserved extension field for vector clock per the requirement text. |
| REPL-06 | 04-02, 04-05 | db.export_manifest(), db.import_segment(path), db.compare_merkle(root) in public API | SATISFIED | All 3 methods public; import_segment accepts data: &[u8] + hash not path (minor API signature deviation from spec, but functionally equivalent and tested) |

### Anti-Patterns Found

| File | Line | Pattern | Severity | Impact |
|------|------|---------|----------|--------|
| `edgestore/src/engine.rs` | ~789 | `let empty_keys: Vec<Vec<u8>> = vec![];` | BLOCKER | C-02: Empty xor filter — imported data invisible after restart |
| `edgestore/src/engine.rs` | ~765-780 | `min_key: vec![], max_key: vec![]` in SegmentMeta | BLOCKER | C-03: Range scans skip all imported segments |
| `edgestore/src/engine.rs` | ~718-733 | apply block handles only Operation::Put | BLOCKER | C-01: Delete tombstones silently dropped |
| `edgestore-repl/src/anti_entropy.rs` | 281 | `db_path.join("sync").join(format!("{}.cursor", peer_id))` | WARNING | C-04: Path traversal if peer_id contains `../` — no validation |
| `edgestore/src/engine.rs` | ~650-756 | Two-stage rename: {hash}.tmp → {hash}.dat → segment-{id}.dat | WARNING | C-05: On crash between renames, orphan .dat remains; permanent failure on Windows |
| `edgestore-repl/src/anti_entropy.rs` | 97-109 | `std::thread::sleep` at top of loop before first run_once | WARNING | W-01: First sync delayed by full interval_secs after node start |
| `edgestore-repl/src/http_server.rs` | ~164-166 | `request.as_reader().read_to_end(&mut _body)` unbounded | WARNING | W-02: No size cap; single large request can exhaust memory |
| `edgestore/src/engine.rs` | ~796-798 | `.meta` file written without fsync | WARNING | W-03: Power loss after .dat sync but before meta flush can corrupt segment on restart |

### Human Verification Required

None identified. All behavioral checks are programmatic.

---

## Gaps Summary

### Three functional correctness blockers from code review

**Root cause: import_segment was implemented to pass integration tests but not to handle edge cases correctly.**

The three critical bugs (C-01, C-02, C-03) share a common root cause: `import_segment` applies LWW records into the memtable (which keeps tests green while the engine is live) but registers the imported segment with incomplete metadata that breaks the read path after restart.

**C-01 — Delete tombstones never applied (data divergence):**
Keys deleted on Node A silently reappear on Node B after sync. The apply block checks `incoming.op == Put` only. No Delete arm exists.

**C-02 — Empty xor filter (post-restart data loss):**
Integration tests (SC1, SC2) pass because keys are in the memtable at assertion time. After engine restart, all imported keys are invisible — the xor filter built from `vec![]` rejects every lookup before the segment is even opened.

**C-03 — Empty min_key/max_key (range scans broken):**
`SegmentMeta` stored with `min_key: vec![]` and `max_key: vec![]`. `range_scan` early-exits when `start > max_key`, which is always true for any non-empty key against an empty max_key. Range and prefix queries return no results from imported segments.

### Requirements coverage gaps

**REPL-03 protocol specification:** The implemented protocol omits `RequestDelta(range, since_lsn)`, `Ack(lsn)`, `ListManifests()`, and `GetManifest(host_id)`. The planning decisions (D02, D08) deliberately chose a simpler pull-only 3-method design. This is coherent and the integration tests pass. However, REQUIREMENTS.md still describes the full 6-operation protocol. Either REQUIREMENTS.md needs an update, or this remains an open gap.

**REPL-04 S3 layout:** FilesystemRemoteStore was accepted as a stand-in for the S3 layout, but the path structure (`{hash}.seg` flat) does not match the required `segments/{hash}.dat`, `hosts/{id}/manifests/latest.json`, `wal/{id}/{lsn}.log`, `snapshots/{id}/manifest.json` hierarchy. The requirement was scoped to Phase 4 in REQUIREMENTS.md without a deferral annotation.

### Security gap

**C-04 — Path traversal via peer_id:** `peer_id` is used directly in filesystem path construction without validation. A malicious or misconfigured caller could write cursor files outside the intended sync/ directory.

---

_Verified: 2026-05-23_
_Verifier: Claude (gsd-verifier)_
