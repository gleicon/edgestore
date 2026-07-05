# Consistency Coverage — EdgeStore

This document tracks which consistency, fault, and performance scenarios are covered by tests across the codebase. It is a living document: after every significant implementation change, re-run the relevant tests and update the status column.

**Last updated:** 2026-07-05 (v1.1.2)

---

## Tiered Storage (`edgestore-tier`)

| # | Scenario | Test Name | Status | Notes |
|---|----------|-----------|--------|-------|
| 1 | Local hit | `test_put_get_local` | ✅ | |
| 2 | Read-through from remote | `test_archive_and_read_through` | ✅ | |
| 3 | Idempotent re-fetch | `test_idempotent_fetch` | ✅ | |
| 4 | LWW: local wins over archived | `test_lww_local_wins_over_archived` | ✅ | |
| 5 | Partial archive (mixed local/remote) | `test_partial_archive_mixed_local_remote` | ✅ | |
| 6 | Range after warming | `test_range_after_warming` | ✅ | |
| 7 | Remote segment missing | `test_archived_not_found_in_remote` | ✅ | |
| 8 | Selective overlapping fetch | `test_fetch_archived_overlapping_selective` | ✅ | |
| 9 | Read-through latency budget | `test_readthrough_latency_within_budget` | ✅ | <500ms debug, <100ms release |
| 10 | Large segment 10K keys | `test_large_segment_archive_and_readthrough_10k` | ✅ | |
| 11 | Concurrent archive + read | `test_concurrent_archive_and_read` | ✅ | Separate local dirs to avoid WriterBusy |
| 12 | Corrupt segment rejected | `test_corrupt_segment_hash_mismatch_rejected` | ✅ | Uses `fetch_segment` (error not swallowed) |
| 13 | Upload fault propagation | `test_upload_fault_propagates` | ✅ | Mock `FaultyRemoteStore` |
| 14 | Download fault propagation | `test_download_fault_propagates` | ✅ | Mock `FaultyRemoteStore` |
| 15 | Concurrent fetch of same segment | `test_concurrent_fetch_same_segment_idempotent` | ✅ | |
| 16 | Network timeout / retry | `test_network_delay_does_not_panic` | ✅ | 50ms delay injected |
| 17 | S3 throttling (503 Slow Down) | `test_throttling_retries_eventually_succeed` | ✅ | Upload + download retry with backoff |
| 18 | Memory pressure / OOM | — | ⚠️ MISSING | Not testable in unit tests |
| 19 | WAL rotation during fetch | — | ⚠️ MISSING | Would require stress harness |

### Reevaluation Trigger

Run these tests after ANY change to:
- `edgestore-tier/src/lib.rs` (`TieredEngine`, `fetch_and_import`, `archive_segments`)
- `edgestore/src/engine.rs` (`import_segment`, `get`, `range`)
- `edgestore/src/segment.rs` (`SegmentReader`, `SegmentStore`)
- `edgestore-repl/src/s3_remote_store.rs` (`S3RemoteStore`)
- `edgestore-repl/src/filesystem_remote_store.rs` (`FilesystemRemoteStore`)

Command:
```bash
cargo test --package edgestore-tier
cargo bench --package edgestore-tier --bench tiered_get
```

---

## ImmutableEngine (`edgestore::immutable`)

| # | Scenario | Test Name | Status | Notes |
|---|----------|-----------|--------|-------|
| 1 | Single segment get | `test_immutable_get_single_segment` | ✅ | |
| 2 | Absent key | `test_immutable_get_absent` | ✅ | |
| 3 | LWW: higher LSN wins across segments | `test_immutable_lww_higher_lsn_wins` | ✅ | |
| 4 | Range scan sorted + deduped | `test_immutable_range_sorted_deduped` | ✅ | |
| 5 | Prefix scan | `test_immutable_prefix` | ✅ | |
| 6 | Delete tombstone filtered | `test_immutable_delete_tombstone_filtered` | ✅ | |
| 7 | From segment bytes | `test_immutable_from_segment_bytes` | ✅ | |
| 8 | Large segment 10K keys | `test_immutable_large_segment_10k` | ✅ | |
| 9 | Multi-segment K-way merge | `test_immutable_lww_higher_lsn_wins` | ✅ | |
| 10 | Lazy fetch on first access | — | ⚠️ MISSING | Requires RemoteStore integration |
| 11 | Eager fetch all at init | — | ⚠️ MISSING | Requires RemoteStore integration |
| 12 | Cache eviction (LRU) | — | ⚠️ MISSING | Planned for Phase 9 Wave 3 |
| 13 | Manifest integrity (Merkle) | — | ⚠️ MISSING | Planned for Phase 9 Wave 3 |
| 14 | Sidecar upload/download | — | ⚠️ MISSING | Planned for Phase 9 Wave 3 |
| 15 | Serverless cold start <100ms | — | ⚠️ MISSING | Requires real deployment (D22: platform owners measure) |

### Reevaluation Trigger

Run these tests after ANY change to:
- `edgestore/src/immutable.rs`
- `edgestore/src/segment/in_memory.rs`
- `edgestore/src/segment.rs` (block format, deserialize_entry, xor filter)

Command:
```bash
cargo test --package edgestore --lib immutable
cargo test --package edgestore --lib segment::in_memory
```

---

## Core Engine (`edgestore`)

| # | Scenario | Test Name | Status | Notes |
|---|----------|-----------|--------|-------|
| 1 | Put → get round-trip | `test_put_get_round_trip` | ✅ | |
| 2 | Crash recovery | `test_crash_recovery` | ✅ | |
| 3 | Namespace isolation | `test_namespace_isolation` | ✅ | |
| 4 | Range scan exclusive end | `test_range_across_segment_and_memtable` | ✅ | |
| 5 | Prefix scan | `test_prefix_from_segments` | ✅ | |
| 6 | Delete tombstone | `test_put_delete_get_returns_none` | ✅ | |
| 7 | Transaction commit | `test_commit_transaction_all_visible` | ✅ | |
| 8 | Transaction rollback | `test_rollback_transaction_keys_not_visible` | ✅ | |
| 9 | Snapshot pinned segments | `test_snapshot_survives_compaction` | ✅ | |
| 10 | Compaction zero live relocation | `test_ttl_expiry_zero_live_relocation` | ✅ | |
| 11 | WAL rotation inline | `test_wal_rotates_inline_without_reopen` | ✅ | |
| 12 | Double open WriterBusy | `test_double_open_writer_busy` | ✅ | |

---

## Test Count Summary

| Crate | Unit Tests | Integration Tests | Doc Tests | Total |
|-------|-----------|--------------------|-----------|-------|
| `edgestore` | 191 + 12 (immutable/in_memory) | 14 | 0 | 217 |
| `edgestore-repl` | 11 | 3 | 1 | 15 |
| `edgestore-tier` | 20 | 2 | 1 | 23 |
| `edgestore-tokio` | 8 + 3 (tiered) | 0 | 0 | 11 |
| `edgestore-wasm` | 1 | 0 | 0 | 1 |
| `edgestore-cli` | 0 | 0 | 0 | 0 |
| **Workspace** | **246** | **19** | **2** | **267** |

---

## Benchmark Coverage

| Benchmark | File | What it measures | Baseline Captured? |
|-----------|------|------------------|-------------------|
| `throughput` | `edgestore/benches/throughput.rs` | KV put/get throughput | ✅ `BENCHMARKS.md` |
| `vector_search` | `edgestore/benches/vector_search.rs` | Flat vs HNSW latency | ✅ `BENCHMARKS.md` |
| `hnsw_recall` | `edgestore/benches/hnsw_recall.rs` | HNSW recall@10 | ✅ `BENCHMARKS.md` |
| `text_search` | `edgestore/benches/text_search.rs` | BM25 QPS | ✅ `BENCHMARKS.md` |
| `tiered_get` | `edgestore-tier/benches/tiered_get.rs` | Local hit, read-through, archive, bulk fetch | ✅ `BENCHMARKS.md` |
| `immutable` | `edgestore/benches/immutable.rs` | Cold start 1K/10K, hot get, range 1K | ✅ `BENCHMARKS.md` |

---

## Known Gaps (Intentionally Deferred)

| Gap | Rationale | Planned Resolution |
|-----|-----------|-------------------|
| Device-level WAF measurement | Requires `nvme smart-log` or hardware counters | Phase 6 (SSD optimization) |
| S3 integration tests in CI | Requires LocalStack container; currently manual | Add to CI with `docker compose` |
| Network timeout / retry tests | Requires async fault injection framework | Phase 9 Wave 3 |
| WASM bundle size optimization | Requires `wasm-pack` toolchain | Phase 9 Wave 4 |
| Serverless cold-start benchmark | Requires real deployment (Cloudflare, Vercel) | Phase 9 Wave 5 |

---

## How to Reevaluate

After any code change that touches:
1. **Tiered storage**: Run `cargo test --package edgestore-tier` + `cargo bench --package edgestore-tier`
2. **ImmutableEngine**: Run `cargo test --package edgestore --lib immutable` + `cargo test --package edgestore --lib segment::in_memory`
3. **Core engine**: Run `cargo test --package edgestore --lib`
4. **Full validation**: Run `cargo test --workspace` + `cargo clippy --workspace -- -D warnings`

If a test fails, update this document with the new status. If a new test is added, add a row to the relevant table.
