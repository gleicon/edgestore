# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`TieredEngine::fetch_segment()`** — selective download+import of a single archived segment by hash. Enables callers to rehydrate only the segments they need instead of calling `fetch_all_archived()`.
- **`TieredEngine::fetch_archived_overlapping()`** — selective range-aware warm-up. Downloads only archived segments whose `[min_key, max_key]` bounds overlap a given query range. This is the key primitive for range-query-shaped workloads (e.g. time-series / log ingestion) that cannot afford to fully rehydrate before every scan.
- **`AsyncTieredEngine`** (`edgestore-tokio/src/tiered.rs`, feature-gated behind `tier`). Async wrapper around `TieredEngine` with the same `spawn_blocking` pattern as `AsyncEngine`. Includes `fetch_archived_overlapping()` for async callers.
- **Selective fetch tests** (`edgestore-tokio/tests/tiered.rs`): `fetch_archived_overlapping_rehydrates_only_segments_in_range` verifies that only overlapping segments are pulled, leaving non-overlapping ranges empty until warmed.

### Changed

- **CHANGELOG rewritten** — retroactively filled empty 1.1.0 and 1.1.1 sections from git history.

## [1.1.1] - 2026-07-05

### Added

- **`TieredEngine::register_archived()`** — restore the archived segment list after restart without re-uploading. Deduplicates by hash.
- **5 new edgestore-tier consistency tests**:
  - `test_idempotent_fetch` — second fetch of same segment is a no-op.
  - `test_lww_local_wins_over_archived` — local newer write wins over archived older data via `import_segment` LWW merge.
  - `test_partial_archive_mixed_local_remote` — `get()` finds data whether key is in local or archived segment.
  - `test_range_after_warming` — `range()` is empty before warming, complete after `fetch_all_archived()`.
  - `test_archived_not_found_in_remote` — graceful `None` when remote segment is deleted after archive.
- **Expanded benchmark suite** (`edgestore-tier/benches/tiered_get.rs`):
  - `tiered_get_readthrough` — measures full download+import+retry path (~24 ms for 1000 records).
  - `tiered_archive_segments` — measures upload throughput (~309 µs for 1000 records).
  - `tiered_fetch_all_archived` — measures bulk rehydration (~24 ms for 1000 records).
- **S3 integration tests for `TieredEngine`** (`edgestore-tier/tests/integration_s3.rs`):
  - `test_tiered_archive_and_readthrough_s3` — end-to-end archive + read-through with live `S3RemoteStore`.
  - `test_tiered_fetch_all_archived_s3` — bulk warming then verification of 100 keys against LocalStack.
- **AsyncEngine API expansion** (`edgestore-tokio/src/lib.rs`):
  - `flush_to_segments()`, `export_manifest()`.
  - Full text search: `index_text()`, `search_text()`, `search_text_with_options()`, `delete_text()`.

### Fixed

- **Benchmark `WriterBusy` race** — `tiered_get_readthrough` and `tiered_fetch_all_archived` now use unique local paths per iteration to avoid single-writer contention during batched benchmark runs.
- **Clippy warnings** in `edgestore-tier` tests (unused variables, unused mut).

## [1.1.0] - 2026-07-05

### Added

- **edgestore-tier crate** — transparent S3 read-through tiering. Local hot cache + remote cold archive.
  - `TieredEngine::get()` read-through: local miss → scans archived segments by key-bounds → downloads + imports matching segment(s) from `RemoteStore` → retries.
  - `TieredEngine::archive_segments()` — uploads local segments to remote and records them as archived. Does not delete local files (caller decides).
  - `TieredEngine::fetch_all_archived()` — bulk rehydration of all archived segments.
  - `Engine::list_segment_metas()` — new public API exposing segment bounds for tiering key-range matching.
- **TESTING.md** — comprehensive testing guide: philosophy, unit/integration, deterministic simulation, fuzz, SSD validation, chaos/fault injection, regression tests, coverage policy.

### Fixed

- **S3RemoteStore async-context safety** (`edgestore-repl/src/s3_remote_store.rs`):
  - `new()` no longer panics when called inside an existing Tokio runtime. Uses `Handle::try_current()` + `block_in_place` instead of `Runtime::block_on()`.
  - Drop-from-async-context panic fixed via `Option<Arc<Runtime>>` — only owns runtime when it created one.
  - Regression test: `test_new_from_async_context`.

### Changed

- **Documentation scrubbed** — removed all "future" / "may provide" language about tiering. `edgestore-tier` documented as shipped in README.md, ARCHITECTURE.md, and `edgestore-repl` module docs.
- **Version bumped to 1.1.0** across all workspace crates.

## [1.0.12] - 2026-07-04

### Added

- **S3RemoteStore** (`edgestore-repl/src/s3_remote_store.rs`). AWS S3 implementation of `RemoteStore` using `aws-sdk-s3` in blocking mode via an internal Tokio runtime. Feature-gated behind `s3`.
  - Path layout: `s3://{bucket}/{prefix}segments/{blake3_hash_hex}.dat`
  - LocalStack support: `force_path_style(true)` for custom endpoints.
- **LocalStack integration testing** (`docker-compose.yml`, `scripts/localstack-init.sh`, `Makefile`).
  - `make s3-test` starts LocalStack, runs S3 tests, tears down.
  - `make s3-up` / `make s3-down` for manual container management.
- **S3 integration tests** (`edgestore-repl/src/s3_remote_store.rs`):
  - upload/download roundtrip, idempotent upload, list, delete, not-found.

### Changed

- **README rewrite** for clarity. Crate decision tree appears immediately after Quick Start. `edgestore-repl` explicitly labeled as optional.
- **Architecture diagram** now shows `edgestore-repl` as an optional sidecar layer, not a core storage backend.
- **S3RemoteStore simplification** — removed `HeadObject` idempotency check (extra round-trip; `PutObject` is naturally idempotent for content-addressed segments). Removed redundant CRC32 checksum.

### Fixed

- `edgestore-repl` crate docs and website copy now clearly distinguish `edgestore` (core) from `edgestore-repl` (replication transport). "repl" = replication, not a REPL shell.

## [1.0.11] - 2026-07-04

### Fixed

- **Text search: LSN-watermarked sidecars for self-healing crash recovery** (`engine.rs`, `text/index.rs`).
  - `InvertedIndex::sidecar_lsn: u64` added. Serialization v2 (backward compatible with v1). After crash, `Engine::open()` rebuilds stale sidecars from raw records.
  - `persist_text_indices()` captures and stores LSN from `self.put()`.
  - Tests: `test_crash_recovery_rebuilds_stale_sidecar`, `test_no_rebuild_when_sidecar_fresh`.

## [1.0.10] - 2026-07-04

### Fixed

- **Text search: write amplification on every `index_text`** (`engine.rs`).
  - Merged index no longer persisted on every write. Stays in memory, written only on `flush()` / `Drop`. Raw text records remain durable via WAL.
  - `Engine::rebuild_text_indices()` rebuilds from raw records on open.
- **Performance test accuracy** (`phase7_integration.rs`).
  - `test_search_performance_at_scale` uses profile-aware thresholds: 50 ms debug, 5 ms release.

### Changed

- `flush()` now calls `persist_text_indices()`.

## [1.0.9] - 2026-07-04

### Fixed

- **Text search: per-document micro-index bug** (`engine.rs`, `text/index.rs`).
  - `index_text` now incrementally updates a single merged `InvertedIndex` per namespace (key `__index__`), cached in `Engine::text_indices`.
  - `search_text` reads cached index directly — O(1) HashMap lookup, no per-query deserialization.
  - `delete_text` removes from merged index.
  - `InvertedIndex::remove_document()` added.
  - Tests: `test_reindex_updates_merged_index`, `test_incremental_index_many_docs`, `test_namespace_isolation`, `test_delete_all_docs_removes_index`.
- **Code quality** (quality gate fixes).
  - Extracted `TEXT_INDEX_KEY` constant. Extracted `BM25_K1` / `BM25_B` constants.
  - Fixed `remove_document()` `doc_len` accumulation bug.
  - Fixed `delete_text` fallback path populating cache.
  - Removed narrating comments.
  - Fixed README CI badge (`edgestore/edgestore` → `gleicon/edgestore`).
- **Makefile safety** — `bump-*` targets now explicitly add known files instead of `git add -A`.

## [1.0.8] - 2026-07-01

### Fixed

- **FDP backend cross-platform compilation** (`fdp_backend.rs`).
  - `as_raw_fd()` returns `i32`, not `Result`. Fixed `if let Ok(...)` pattern.
  - `open(path)?` changed to best-effort `if let Ok(file) = open(path)` — skips hint if file not on disk (e.g. in-memory backends).
  - Verified: macOS ARM, Linux x86_64, Linux ARM64.

## [1.0.7] - 2026-07-01

### Fixed

- **FDP backend `as_raw_fd()` type mismatch on Rust 1.88+** (`fdp_backend.rs`).
  - Removed `if let Ok(...)` around `as_raw_fd()`. Compiles on Rust 1.88+.

## [1.0.6] - 2026-07-01

### Changed

- **MSRV raised to Rust 1.95.0** (all crates).

### Fixed

- **Build artifact cleanup** — removed 131 tracked files from `target/` (docs, fingerprints).
- **Makefile bump targets** — explicitly add known version files instead of `git add -A`.

## [1.0.5] - 2026-06-29

No substantive changes. Version bump only.

## [1.0.4] - 2026-06-17

### Fixed

- **`fdp_backend.rs:43` — `as_raw_fd()` type mismatch on Rust 1.88+.**
  - Removed `if let Ok(...)` pattern. `as_raw_fd()` returns `i32`, not `Result`.

## [1.0.3] - 2026-06-17

### Changed

- Performance improvements (range scan, snapshot reader caching).

### Fixed

- HNSW staleness detection (`is_index_stale()` always returned false after sidecar creation).
