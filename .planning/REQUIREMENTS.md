# Requirements — EdgeStore

## v1 Requirements

### CORE — WAL + Memtable + KV Engine

- [ ] **CORE-01**: WAL is append-only, LZ4-compressed, CRC32C-checksummed, with a versioned format header on every file
- [ ] **CORE-02**: WAL rotates at 64 MB OR 60 seconds, whichever comes first (both configurable via `EdgestoreConfig`)
- [ ] **CORE-03**: MemTable implemented as `std::collections::BTreeMap` behind a `MemTable` trait; alternate implementations swappable at DB open via `Box<dyn MemTable>`
- [ ] **CORE-04**: Single writer enforced at engine level; multiple concurrent readers allowed; group commit batches multiple `tx.commit()` calls into one WAL fsync
- [ ] **CORE-05**: KV API exposes `put(ns, key, val)`, `put_with_ttl(ns, key, val, ttl_secs)`, `get(ns, key)`, `delete(ns, key)`, `range(ns, start, end)`, `prefix(ns, prefix)`
- [ ] **CORE-06**: Transaction API exposes `begin() -> Transaction`, `tx.put(...)`, `tx.put_with_ttl(...)`, `tx.delete(...)`, `tx.commit() -> Result<Lsn>`, `tx.rollback()`
- [ ] **CORE-07**: Crash recovery replays WAL to rebuild memtable and manifest; recovery is deterministic; partial writes ignored safely; no acknowledged writes lost
- [ ] **CORE-08**: All keys prefix-encoded as `{ns_len:u16}{ns_bytes}{key_bytes}`; namespace isolation enforced at API layer via prefix-bounded range scans

### STORE — Immutable Segment Store

- [ ] **STORE-01**: Segment files are immutable once written; structure: `.dat` (ZSTD data blocks), `.idx` (sparse index), `.xf` (xor filter), `.meta` (JSON metadata)
- [ ] **STORE-02**: Data blocks 4 KiB-aligned; ZSTD level 1 compression; compressed records packed into aligned blocks (no arbitrary-offset reads)
- [ ] **STORE-03**: Segment size target 16 MB (configurable); segments content-addressed by BLAKE3 hash of `.dat` file
- [ ] **STORE-04**: Sparse index stores offsets every N keys for fast range seeks and block lookups
- [ ] **STORE-05**: Xor filter built per segment at flush time using `xorf` crate; ~8 bits/key; FPR 1% default (configurable); stored in `.xf` file
- [ ] **STORE-06**: Manifest file tracks all live segments, LSN ranges, min/max keys, and cohort buckets; manifest is append-only and checksummed
- [ ] **STORE-07**: Segment metadata includes: `segment_id`, `segment_hash`, `min_key`, `max_key`, `min_lsn`, `max_lsn`, `record_count`, `cohort_bucket`, `death_time`, `merkle_root`, `created_at`

### COMPACT — Deathtime-Cohort Compaction

- [ ] **COMPACT-01**: WAL record includes optional `ttl: u32` field (seconds; 0 = no expiry); `put_with_ttl` populates this field
- [ ] **COMPACT-02**: Cohort bucket assigned at segment flush: `cohort_bucket = floor((write_time + ttl) / cohort_window)` for TTL records; `cohort_bucket = floor(write_time / cohort_window)` for no-TTL records
- [ ] **COMPACT-03**: Default cohort window is 1 hour (configurable via `EdgestoreConfig.cohort_window_secs`)
- [ ] **COMPACT-04**: Compaction is incremental and bounded; fully-expired cohorts (all records dead) collected first with zero live data relocation; partially-expired cohorts collected by dead-record ratio
- [ ] **COMPACT-05**: Range scans merge results across overlapping segments; latest version wins; tombstones respected
- [ ] **COMPACT-06**: `db.snapshot()` returns a point-in-time read view; pinned segments survive compaction until snapshot released
- [ ] **COMPACT-07**: Merkle roots recomputed on output segments after compaction; manifest updated atomically

### REPL — Merkle Trees + Replication + S3

- [ ] **REPL-01**: Segment-level Merkle: each segment has a `merkle_root` (BLAKE3) in its metadata; fast answer to "do two hosts have the same segment?"
- [ ] **REPL-02**: Range-level Merkle: tree over key range buckets `[a-f]→hash`, `[g-m]→hash`; lets two hosts compare trees and exchange only divergent ranges
- [ ] **REPL-03**: Replication protocol is transport-agnostic: `ListManifests()`, `GetManifest(host_id)`, `CompareMerkle(root)`, `RequestDelta(range, since_lsn)`, `SendSegments(hashes)`, `Ack(lsn)`; HTTP transport implemented first
- [ ] **REPL-04**: S3 layout: `segments/{hash}.dat`, `hosts/{id}/manifests/latest.json`, `wal/{id}/{lsn}.log`, `snapshots/{id}/manifest.json`; used for cold storage, replication mailbox, disaster recovery
- [ ] **REPL-05**: Conflict resolution is LWW by WAL `timestamp` (wall clock, unix nanoseconds); documented limitation on clock skew; WAL record has reserved extension field for future vector clock support
- [ ] **REPL-06**: `db.export_manifest()`, `db.import_segment(path)`, `db.compare_merkle(root)` exposed in public API

### VECTOR — Vector Search

- [ ] **VECTOR-01**: Vector records encoded as `{dims:u16}{dtype:u8}{data_bytes}` at vector API layer; KV layer stores opaque bytes
- [ ] **VECTOR-02**: Vector API: `vector_put(ns, key, dims, dtype, data)`, `vector_get(ns, key) -> VectorRecord`, `vector_search(ns, query, k, metric) -> Vec<(Key, f32)>`
- [ ] **VECTOR-03**: `vector_search` uses flat SIMD scan for v1; cosine, dot product, and euclidean metrics supported
- [ ] **VECTOR-04**: `dtype` field supports: `0=f32`, `1=f16`, `2=i8`; dimension validated on `vector_put`
- [ ] **VECTOR-05**: Vector benchmark suite: correctness vs brute-force reference, SIMD vs scalar parity, 10K / 100K / 500K vector collections

### SSD — Storage Backend Abstraction + HNSW

- [ ] **SSD-01**: `StorageBackend` trait abstracts I/O; default impl uses standard `pread`/`pwrite`; FDP placement hint impl plugs in without touching compaction logic
- [ ] **SSD-02**: FDP placement hints (NVMe 2.0): deathtime cohort ID maps to FDP placement handle; emit on segment writes where hardware supports it
- [ ] **SSD-03**: HNSW index for vector search replaces flat scan for large collections (>500K vectors); flat scan retained as fallback
- [ ] **SSD-04**: `edgestore-tokio` crate: thin async wrapper over sync core using `spawn_blocking`; no changes to core library
- [ ] **SSD-05**: Benchmark suite: write amplification factor (logical/physical), device WAF measurement, steady-state SSD behavior over multiple device-capacity writes

## v2 Requirements (Deferred)

- Full-text search: tokenizer, stemmer, inverted index (BM25), postings compression, `index_text`/`search` API
- SQL layer or SQLite virtual table over segment store
- Vector clocks for replication conflict resolution
- ZNS explicit zone management
- QUIC/gRPC replication transports

## Out of Scope

- Distributed consensus — local-first; no Raft/Paxos
- Multi-primary replication — single writer per node by design
- Automatic CRDT semantics — application handles merge
- Cluster orchestration — not a library concern
- Mandatory server process — library-first always
- Query planner / joins — v2+ SQL layer concern
- Full-text search in v1 — separate indexing pipeline, own milestone

## Traceability

| Phase | Requirements |
|-------|-------------|
| 1 — Core KV Engine | CORE-01 through CORE-08 |
| 2 — Segment Store | STORE-01 through STORE-07 |
| 3 — Deathtime Compaction | COMPACT-01 through COMPACT-07 |
| 4 — Replication + S3 | REPL-01 through REPL-06 |
| 5 — Vector Search | VECTOR-01 through VECTOR-05 |
| 6 — SSD Optimization + HNSW | SSD-01 through SSD-05 |
| 7 — Full-Text Search (v2) | SEARCH-01 through SEARCH-04 |

## Definition of Done (v1)

A phase is done when:
1. All requirements for that phase pass their test cases
2. Crash recovery tests pass for any new durability surface
3. No regression in write amplification metrics vs prior phase baseline
4. Format tests pass (encode/decode round-trip, backward compat, corruption detection)
