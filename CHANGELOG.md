# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.0] - 2026-07-05

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security

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
