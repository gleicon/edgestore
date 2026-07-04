# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.11] - 2026-07-04

### Fixed

- **Text search write amplification + self-healing crash recovery** (`engine.rs`, `text/index.rs`).
  - `index_text` no longer persists merged index on every write. Index stays in memory, written only on `flush()` / `Drop`. Raw text records remain durable via WAL.
  - `InvertedIndex::sidecar_lsn` (u64) added for staleness detection. Serialization bumped to v2 (backward compatible with v1 sidecars). After crash, `Engine::open()` rebuilds stale sidecars from raw records.
  - `Engine::rebuild_text_indices()` rebuilds from raw records. `Engine::persist_text_indices()` writes dirty indices (called from `flush()` and `Drop`).
  - Tests: `test_cold_cache_search`, `test_typo_tolerance`, `test_delete_fallback_cache_miss`, `test_reindex_with_facets`, `test_crash_recovery_rebuilds_stale_sidecar`, `test_no_rebuild_when_sidecar_fresh`.

### Changed

- `flush()` now calls `persist_text_indices()`.
- `test_search_performance_at_scale` uses profile-aware thresholds: 50 ms debug, 5 ms release.

### Known Issues

- Full-rebuild-on-staleness cost scales with total namespace size (not incremental). Unbenchmarked. Determines worst-case search-unavailable-after-restart duration.

## [1.0.6] – [1.0.10] - 2026-06-29 to 2026-07-04

Version bumps with no substantive changes. Published as intermediate tags.

## [1.0.5] - 2026-06-29

### Changed

- **MSRV raised to Rust 1.95.0** (all crates).

### Fixed

- **`fdp_backend.rs:43` — `as_raw_fd()` type mismatch on Rust 1.88+ + cross-platform failure.** Best-effort `if let Ok(file) = open(path)` — skips hint if file not on disk. Verified on macOS ARM, Linux x86_64, Linux ARM64.

## [1.0.4] - 2026-06-17

### Fixed

- **`fdp_backend.rs:43` — `as_raw_fd()` type mismatch on Rust 1.88+.** Removed `if let Ok(...)` pattern around `as_raw_fd()` (returns `i32`, not `Result`).

## [1.0.3] - 2026-06-17

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security
