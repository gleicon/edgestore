# Roadmap — EdgeStore

**8 phases** | **34 requirements mapped** | All v1 requirements covered ✓

| # | Phase | Goal | Requirements | Success Criteria |
|---|-------|------|--------------|-----------------|
| 1 | Core KV Engine | Durable, crash-safe local KV store | CORE-01–08 | 5 |
| 2 | Segment Store | Immutable sorted segments on disk | STORE-01–07 | 4 |
| 3 | Deathtime Compaction | Cohort-based GC, range scans, snapshots | COMPACT-01–07 | 5 |
| 4 | Replication + S3 | Merkle delta sync, S3 archive | REPL-01–06 | 5 |
| 4.1 | Engine Correctness & Edge Cases (INSERTED) | Fix API contract bugs and semantic divergences discovered during Phase 3 review | CORE-02, CORE-04, CORE-05 | 4 |
| 5 | Vector Search | Flat SIMD ANN search on top of KV | VECTOR-01–05 | 4 |
| 6 | SSD Optimization + HNSW | FDP hints, HNSW index, async wrapper | SSD-01–05 | 4 |
| 7 | Full-Text Search (v2) | Embedded Algolia-like search | SEARCH-01–04 | 3 |

---

### Phase 1: Core KV Engine

**Goal:** Ship a durable, crash-safe local KV store with WAL, memtable, single-writer transactions, and deterministic recovery. This is the foundation every other phase builds on — format decisions made here are permanent.

**Requirements:** CORE-01, CORE-02, CORE-03, CORE-04, CORE-05, CORE-06, CORE-07, CORE-08

**Plans:** 7 plans

Plans:
- [x] 01-01-PLAN.md — Cargo workspace scaffold + core types + error types
- [x] 01-02-PLAN.md — WAL append, LZ4 compression, CRC32C, versioned file header, rotation
- [x] 01-03-PLAN.md — MemTable trait + BTreeMap implementation
- [x] 01-04-PLAN.md — KV Engine single-writer lock, group commit, KV API
- [x] 01-05-PLAN.md — Transaction API begin/commit/rollback/LSN, group commit fsync
- [x] 01-06-PLAN.md — Crash recovery WAL replay, deterministic memtable rebuild
- [x] 01-07-PLAN.md — Integration tests: WAL format, crash recovery, namespace isolation, round-trip

**Success Criteria:**
1. `put` → `get` round-trips correctly across namespaces with prefix isolation
2. Process kill mid-write → recovery replays WAL → no acknowledged write lost
3. `tx.begin()` → multiple puts → `tx.commit()` → group commit batches fsync
4. WAL rotates at configured size/time thresholds, new file starts cleanly
5. Format tests pass: WAL encode/decode, corruption detection, truncation handling

**Key risks:**
- WAL format decisions are permanent — get checksums, versioning, and record schema right before any other phase
- Group commit implementation must handle concurrent reader access without blocking

---

### Phase 2: Segment Store

**Goal:** Flush memtable to immutable sorted segment files with xor filters, sparse indexes, BLAKE3 content addressing, and a live manifest. Point lookups and range scans work across in-memory and on-disk data.

**Requirements:** STORE-01, STORE-02, STORE-03, STORE-04, STORE-05, STORE-06, STORE-07

**Plans:** 6 plans

Plans:
- [x] 02-01-PLAN.md — Segment types, error variants, Cargo.toml dependencies
- [x] 02-02-PLAN.md — Segment writer: ZSTD blocks, sparse index, xor filter, BLAKE3, .meta
- [x] 02-03-PLAN.md — Segment reader: xor filter check, sparse index seek, block scan
- [x] 02-04-PLAN.md — Manifest: append-only, CRC32C checksummed, live segment tracking
- [x] 02-05-PLAN.md — SegmentStore + Engine integration: flush, read-through, segment-backed gets
- [x] 02-06-PLAN.md — Integration tests: all 5 Phase 2 success criteria

**Success Criteria:**
1. Memtable flush produces `.dat` + `.idx` + `.xf` + `.meta` files; BLAKE3 hash matches content
2. Point lookup hits xor filter → skips segment if key absent; no false negatives
3. Sparse index seek lands within N keys of target; no full segment scan needed
4. Segment metadata includes `cohort_bucket` and `death_time` fields (required by Phase 3)
5. Format tests: segment encode/decode, manifest parsing, backward compat, corruption detection

**Key risks:**
- 4 KiB block alignment + ZSTD compression must not create arbitrary-offset reads
- Xor filter construction (`xorf`) requires all keys known at build time — flush must be atomic

---

### Phase 3: Deathtime-Cohort Compaction ✓ COMPLETE

**Goal:** Implement TTL-aware deathtime-cohort compaction. Expired cohorts compact to zero live-data relocation. Range scans merge overlapping segments. Snapshots pin segments.

**Requirements:** COMPACT-01, COMPACT-02, COMPACT-03, COMPACT-04, COMPACT-05, COMPACT-06, COMPACT-07

**Plans:** 5 plans — all complete

Plans:
- [x] 03-01-PLAN.md — Compactor + Snapshot scaffold, config, error
- [x] 03-02-PLAN.md — Compactor core algorithm (identify/collect/compact/cycle)
- [x] 03-03-PLAN.md — Snapshot implementation (SnapshotRegistry, Snapshot RAII)
- [x] 03-04-PLAN.md — Engine integration (compact_once, snapshot wired to Engine)
- [x] 03-05-PLAN.md — Integration tests: all 5 Phase 3 success criteria

**Success Criteria:** All 5 verified by integration tests (2026-05-20)
1. `put_with_ttl(key, val, 1)` → sleep 2s → compact_once → live_records_relocated == 0 ✓
2. Range scan across 3+ overlapping segments returns correct latest-wins merged result ✓
3. Snapshot holds segment pins → compaction runs → snapshot data still readable ✓
4. Compaction is incremental: write_budget_bytes=1 stops after first partial cohort ✓
5. Merkle roots on output segments match recomputed values after compaction ✓

**Key risks resolved:**
- Deathtime-cohort correctness: no-TTL records cluster by write-time cohort (cohort_bucket_for)
- Snapshot pinning uses RAII Drop on Snapshot; SnapshotRegistry releases on drop

---

### Phase 4: Replication + S3

**Goal:** Implement Merkle-based delta sync and S3 integration. Two nodes can compare manifests, identify divergent ranges, and exchange only missing segments. S3 used as replication mailbox and cold archive.

**Requirements:** REPL-01, REPL-02, REPL-03, REPL-04, REPL-05, REPL-06

**Plans:** 5 plans

Plans:
- [ ] 04-01-PLAN.md — Range-level Merkle tree + replication protocol types and trait (Wave 1)
- [ ] 04-02-PLAN.md — Engine public API: export_manifest, import_segment, compare_merkle (Wave 2)
- [ ] 04-03-PLAN.md — edgestore-repl crate: HTTP transport client + server (Wave 3)
- [ ] 04-04-PLAN.md — edgestore-repl: S3 backend with SigV4 signing (Wave 4)
- [ ] 04-05-PLAN.md — Integration tests: all 5 success criteria (Wave 5)

**Cross-cutting constraints:**
- No async in edgestore crate; edgestore-repl owns HTTP/S3 transport
- LWW via WAL record timestamp (unix nanoseconds) — applied in import_segment (REPL-05)
- S3 endpoint overridable via EDGESTORE_S3_ENDPOINT_URL for localstack integration tests

**Success Criteria:**
1. Two nodes with identical data → `compare_merkle` returns no diff
2. Node A writes 100 keys → Node B syncs → only changed segments transferred (not full copy)
3. Node killed → segments restored from S3 → DB opens cleanly with full data
4. LWW: same key written on two nodes at different timestamps → higher timestamp wins on sync
5. Interrupted sync resumes from last `Ack(lsn)` without re-transferring acknowledged segments

**Key risks:**
- Range-level Merkle bucket granularity affects sync efficiency — too coarse = over-transfer, too fine = metadata overhead
- S3 eventual consistency: segment upload must complete before manifest update references it

---

### Phase 4.1: Engine Correctness & Edge Cases (INSERTED)

**Goal:** Fix four semantic bugs and contract violations discovered during Phase 3 test review. No new features — correctness only. All four items are load-bearing for Phases 5–7.

**Requirements:** CORE-02 (WAL durability), CORE-04 (single-writer KV API), CORE-05 (range scan semantics)

**Plans:** 4 plans

Plans:
- [ ] 04.1-01-PLAN.md — WAL in-write rotation: check `needs_rotation` after each `append`; rotate inline without requiring Engine reopen
- [ ] 04.1-02-PLAN.md — TTL lazy-expiry contract: document and test that `get`/`range`/`prefix` return expired records until compaction; add assertion test pinning this behavior
- [ ] 04.1-03-PLAN.md — Fix `SegmentReader::range_scan` end-inclusive bug: change `k > end` → `k >= end`; audit `Engine::range`, `prefix_upper_bound`, and all range tests for correctness
- [ ] 04.1-04-PLAN.md — Fix `Snapshot::get` LWW ordering: replace first-found with highest-LSN across all pinned segments; add multi-segment divergence test

**Success Criteria:**
1. Long-running engine writes N batches without reopen → WAL rotates at `wal_max_bytes` boundary; recovery replays all files
2. `put_with_ttl(key, val, 1)` without compaction → `get` returns `Some(val)` immediately after TTL expires (lazy-expiry contract is explicit and tested)
3. `engine.range(ns, b"a", b"b")` excludes key `b` (exclusive end); all range/prefix tests updated and passing
4. `Snapshot::get` returns highest-LSN value when same key exists in multiple pinned segments regardless of segment ID ordering

**Key risks:**
- In-write WAL rotation must not lose records written between `needs_rotation` check and new file creation
- Changing `range_scan` end semantics is a breaking change in the internal API — all callers must be audited before merge

---

### Phase 5: Vector Search

**Goal:** Vector API on top of pure KV. Typed header encoding, flat SIMD ANN search for collections up to ~500K vectors, three distance metrics.

**Requirements:** VECTOR-01, VECTOR-02, VECTOR-03, VECTOR-04, VECTOR-05

**Success Criteria:**
1. `vector_put` with mismatched dims → error; correct dims → round-trips via `vector_get`
2. `vector_search(query, k=10, cosine)` returns same top-10 as brute-force reference implementation
3. SIMD path and scalar path return identical results on same dataset
4. 100K f32 vectors → search latency p99 < 50ms on modern hardware
5. KV layer unchanged: removing vector crate compiles and all KV tests still pass

**Key risks:**
- SIMD portability: x86 AVX2/AVX-512 vs ARM NEON — must compile on both without unsafe divergence
- f16/i8 dtype support requires careful widening/narrowing in distance computation

---

### Phase 6: SSD Optimization + HNSW

**Goal:** Add `StorageBackend` trait for FDP/ZNS hardware hints, HNSW index for large vector collections, and `edgestore-tokio` async wrapper. Validate device WAF with benchmark suite.

**Requirements:** SSD-01, SSD-02, SSD-03, SSD-04, SSD-05

**Success Criteria:**
1. `StorageBackend` trait compiles with both default (pread/pwrite) and FDP stub impl; compaction logic untouched
2. FDP placement hint emitted per segment write on supported hardware (verified via mock backend)
3. HNSW search on 1M vectors returns same top-10 as flat scan reference within ANN recall threshold (>0.95)
4. `edgestore-tokio::Db::put()` async resolves correctly; no deadlock under concurrent async callers
5. Benchmark: write amplification factor measured; device WAF approaches 1.0 on NVMe with deathtime-cohort enabled

**Key risks:**
- HNSW graph serialization must survive DB open/close without full graph rebuild
- FDP is NVMe 2.0 — needs conditional compilation guard for unsupported hardware

---

### Phase 7: Full-Text Search (v2)

**Goal:** Embedded Algolia-like search: tokenization, BM25 relevance, faceting. Inverted index stored in segments, merged during compaction. No server process.

**Requirements:** SEARCH-01, SEARCH-02, SEARCH-03, SEARCH-04

**Success Criteria:**
1. `index_text(ns, key, text)` → `search(ns, "query")` returns BM25-ranked results
2. Per-segment posting lists merge correctly during compaction; no stale entries
3. Typo-tolerant search returns expected results for 1-edit-distance queries
4. Search throughput: >1000 QPS on 100K indexed documents, single-threaded

**Key risks:**
- Posting list compaction semantics differ from KV tombstones — must not use same path
- Tokenizer/stemmer internationalization is a long tail — English-first, documented

---

## Milestone Structure

**Milestone 1 (v0.1):** Phases 1–3 — local durable KV with deathtime-cohort compaction
**Milestone 2 (v0.2):** Phase 4 — replication and S3 integration
**Milestone 3 (v0.3):** Phase 5 — vector search
**Milestone 4 (v1.0):** Phase 6 — SSD optimization, HNSW, production-ready
**Milestone 5 (v2.0):** Phase 7 — full-text search
