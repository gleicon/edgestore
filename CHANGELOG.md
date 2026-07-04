# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.8] - 2026-07-04

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security

## [1.0.7] - 2026-07-03

### Fixed

- **Text search: per-document micro-index bug causing O(N) search deserialize+merge** (`engine.rs`, `text/index.rs`).
  - **Bug:** `index_text` built a brand-new one-document `InvertedIndex` per write and stored it under `idx:{doc_key}`. `search_text_with_options` did `self.prefix(&text_ns, b"idx:")` — pulled and deserialized every single per-document micro-index on every query, then merged them all into one aggregated index in memory before scoring. This was O(total_docs) deserialize+merge work per search, collapsing to ~6 QPS at 10K docs.
  - **Fix:** `index_text` now reads the existing merged inverted index from key `__index__`, incrementally adds the new document's postings, and writes back. `search_text` reads the single merged index directly — O(1) single-record deserialize instead of O(N) per-document merging. `delete_text` removes the document from the merged index.
  - **New API:** `InvertedIndex::remove_document()` removes all postings for a doc_id and updates total_docs/total_doc_len.
  - **Tests:** Added `test_reindex_updates_merged_index`, `test_incremental_index_many_docs`, `test_namespace_isolation`, `test_delete_all_docs_removes_index`, `test_search_performance_at_scale` (asserts < 1ms/search at 10K docs).
  - **Impact:** Text search QPS improves from ~6 to thousands at 10K docs. No API change.

### Changed

- **Makefile bump targets** now explicitly `git add` only known version files instead of `git add -A`, preventing accidental commits of build artifacts.

### Security

## [1.0.6] - 2026-07-01

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security

## [1.0.5] - 2026-06-29

### Changed

- **MSRV raised to Rust 1.95.0** (all crates).
  - `rust-version = "1.95.0"` added to workspace `Cargo.toml` and inherited by all member crates.
  - This ensures the `fdp_backend.rs` fix and other modern Rust APIs are available.

### Fixed

- **`fdp_backend.rs:43` — `as_raw_fd()` type mismatch on Rust 1.88+ + cross-platform failure** (`fdp_backend.rs`).
  - **Bug:** `if let Ok(_fd) = std::os::fd::AsRawFd::as_raw_fd(...)` — `as_raw_fd()` returns `RawFd` (`i32`), not `Result`. This pattern compiled as a soft warning in Rust ≤1.87 but became a hard error in Rust 1.88+.
  - **Fix v1:** Removed the `if let Ok(...)` pattern and called `as_raw_fd()` directly. But this left `open(path)?` which failed on Linux when the file didn't exist (e.g. `MemoryStorageBackend` tests).
  - **Fix v2 (final):** Best-effort `if let Ok(file) = open(path) { as_raw_fd(&file); log!(...); }`. Silently skips hint if file not on disk. Verified on macOS ARM, Linux x86_64, Linux ARM64.
  - **Impact:** EdgeStore compiles and tests pass on Rust 1.88+ on all platforms. No API change.

## [1.0.4] - 2026-06-17

### Fixed

- **`fdp_backend.rs:43` — `as_raw_fd()` type mismatch on Rust 1.88+** (`fdp_backend.rs`).
  - **Bug:** `if let Ok(_fd) = std::os::fd::AsRawFd::as_raw_fd(...)` — `as_raw_fd()` returns `RawFd` (`i32`), not `Result`. This pattern compiled as a soft warning in Rust ≤1.87 but became a hard error in Rust 1.88+.
  - **Fix:** Removed the `if let Ok(...)` pattern. The `?` on `open(path)?` already handles the error case; `as_raw_fd()` is called directly on the resulting `File`.
  - **Impact:** EdgeStore now compiles on Rust 1.88+ on Linux. No API change.

## [1.0.3] - 2026-06-17

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security

## [1.0.2] - 2026-06-16

### Performance

- **Major range scan performance improvements** (40x+ throughput improvement).
  - `SegmentReader` now caches the sparse index at `open()` time (previously re-read `.idx` file on *every* `get()`/`range_scan()` call).
  - `find_block_offset` uses `partition_point` binary search — O(log n) instead of O(n) linear scan.
  - `SegmentStore::range_scan` and `Engine::range_inner` use K-way merge via `BinaryHeap` instead of `HashMap` + `sort` — O(n) instead of O(n log n) + 4 allocations.
  - `Snapshot` holds cloned `SegmentReader` instances (previously re-opened all segment files on every snapshot read, re-parsing JSON metadata and sparse indexes each time).
  - `BinaryHeap` tie-breaking on LSN ensures `Ord` and `Eq` are consistent (prevents heap corruption and race conditions).
  - `MemEntry` and `Operation` derive `Eq` for K-way merge correctness.
  - All changes verified: `cargo test --workspace` (250/250), `cargo clippy --workspace -D warnings` (clean).

### Fixed

- **`is_index_stale()` always returned false after HNSW sidecar creation** (vector_search, `engine.rs`).
  - **Bug:** After `build_vector_index`, subsequent `vector_put` calls added new vectors to the KV store, but `vector_search` continued to use the cached HNSW sidecar. The new vectors were present in `__vec__{ns}` records but absent from the HNSW graph. Searches silently missed them.
  - **Fix:** `build_vector_index` now persists the `range_merkle_root` (32-byte BLAKE3 hash of the current segment set) alongside the `.hnsw` sidecar as a `.stamp` file. `is_index_stale` reads the stamp and compares it against the live `range_merkle_root`. If the stamp is missing or mismatched, the index is considered stale and rebuilt. This is deterministic across replicas and requires zero new dependencies.
  - **Impact:** Correctness of incremental vector search after mutations. No API change.

## [1.0.0] - 2026-05-25

### Added

#### Phase 1 — Core KV Engine
- WAL append-only with LZ4 compression, CRC32C checksumming, and versioned file header
- WAL rotation at configurable size (64 MB) or time (60 s) thresholds
- MemTable trait with `BTreeMap` implementation (swappable at DB open)
- Single-writer engine with group commit and concurrent readers
- KV API: `put`, `put_with_ttl`, `get`, `delete`, `range`, `prefix`
- Transaction API: `begin`, `commit`, `rollback` with LSN tracking
- Crash recovery: deterministic memtable and manifest rebuild from WAL replay
- Prefix-encoded namespace keys (`{ns_len:u16}{ns_bytes}{key_bytes}`)

#### Phase 2 — Segment Store
- Immutable sorted segments with ZSTD level 1 compression (4 KiB aligned blocks)
- Sparse index (`.idx`) for block-level seeks without full segment scans
- Xor filter (`.xf`) per segment for fast negative checks (~8 bits/key, 1% FPR)
- BLAKE3 content addressing and integrity verification
- Append-only manifest with CRC32C checksummed JSON-lines format
- Segment metadata (`.meta`) including key bounds, LSN range, cohort bucket, and death time

#### Phase 3 — Deathtime-Cohort Compaction
- Deathtime-cohort compaction algorithm (identify → collect → compact → cycle)
- TTL-aware grouping: `cohort_bucket = floor((write_time + ttl) / cohort_window)`
- Zero live-data relocation for fully expired cohorts
- Incremental compaction with configurable per-cycle write budget
- Snapshot RAII pinning: `Snapshot` holds segment pins; compaction defers deletion
- Range scans across overlapping segments with highest-LSN merged results
- Merkle root recomputation on output segments after compaction

#### Phase 4 — Replication + S3
- Range-level and segment-level Merkle trees for delta synchronization
- Transport-agnostic replication protocol (`ReplicationProtocol` trait)
- `compare_merkle` API for host-to-host divergence detection
- `import_segment` with LWW conflict resolution via wall-clock timestamp
- S3 integration (`RemoteStore`) for cold storage and replication mailbox
- SigV4 signing with configurable endpoint override (`EDGESTORE_S3_ENDPOINT_URL`)

#### Phase 4.1 — Engine Correctness & Edge Cases
- In-write WAL rotation: rotates inline without requiring Engine reopen
- Explicit TTL lazy-expiry contract: `get`/`range` return expired records until compaction
- Fixed `SegmentReader::range_scan` end-semantics (`k >= end` for exclusive upper bound)
- Fixed `Snapshot::get` LWW ordering: highest-LSN wins across all pinned segments

#### Phase 5 — Vector Search
- Typed vector header API: `{dims:u16}{dtype:u8}{data}` encoding
- Flat SIMD ANN search (cosine, dot product, euclidean) with scalar fallback
- f32, f16, and i8 dtype support
- `vector_put`, `vector_get`, `vector_search`, `vector_delete` APIs
- Pure KV-layer guarantee: removing vector module compiles and all KV tests pass

#### Phase 6 — SSD Optimization + HNSW
- `StorageBackend` trait abstraction for FDP/ZNS hardware hints
- `DefaultStorageBackend` (pread/pwrite) and `MockFdpBackend` implementations
- FDP placement hint emission per segment write on supported hardware
- HNSW approximate nearest neighbor index for large vector collections
- `edgestore-tokio` async wrapper: `AsyncEngine` with `spawn_blocking` for all I/O

#### Phase 7 — Full-Text Search
- Tokenization pipeline with English stemming support
- BM25 inverted index stored in segments
- `index_text`, `search`, `search_with_facets` APIs
- Faceting: filter and aggregate by structured facet values
- Typo-tolerant search with 1-edit-distance Levenshtein matching
- Posting list merging during compaction with correct tombstone semantics

#### Workspace & Tooling
- Cargo workspace with three crates: `edgestore`, `edgestore-tokio`, `edgestore-repl`
- HTTP replication transport (`HttpReplicationClient`, `HttpReplicationServer`)
- Pull-only anti-entropy background sync loop (`AntiEntropyLoop`)
- Metrics snapshot API for operational observability

### Changed
- Nothing for v1.0 (initial release).

### Deprecated
- Nothing for v1.0.

### Removed
- Nothing for v1.0.

### Fixed
- Nothing for v1.0 (all fixes tracked in Phase 4.1 above).

### Security
- Nothing for v1.0.

---

**Full commit range:** `v1.0.0` (see git tag).
