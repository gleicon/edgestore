# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`AsyncTieredEngine::flush_notify()`** — exposes a `tokio::sync::Notify` handle (via `notify_one`) wired to the local engine's `with_on_segment_flushed` at construction. Lets async callers (e.g. a caller-driven tiering worker) race a polling interval against this handle to react to a flush immediately instead of waiting up to the full interval — the same latency win `with_on_segment_flushed` gives replication anti-entropy, extended to any `AsyncTieredEngine` user. Always wired, no new constructor parameters. (`edgestore-tokio/src/tiered.rs`)

## [1.3.0] - 2026-07-06

### Added

- **`Engine::open_readonly(config)`** — opens the engine in read-only mode. All write methods (`put`, `delete`, `vector_put`, `vector_delete`, `index_text`, `delete_text`, `flush_to_segments`, `compact_once`, `import_segment`) return `Err(EdgestoreError::ReadOnly)`. Use for replica instances to prevent accidental divergence from the primary. Controlled by `EdgestoreConfig::readonly: bool` (default `false`). (`edgestore/src/engine.rs`, `edgestore/src/config.rs`, `edgestore/src/error.rs`)

- **`Engine::with_on_segment_flushed(cb)`** — registers a callback fired after every successful segment flush, both explicit (`flush_to_segments`) and auto-triggered by `put` when `memtable_max_bytes` is exceeded. Callback signature: `Fn(&SegmentMeta) + Send + Sync`. Use to wake a replication anti-entropy loop (drops lag from ≤30s to ≤1 RTT), update external metrics, or trigger downstream processing. Runs synchronously on the calling thread — keep it fast (send on a channel, set an atomic flag). (`edgestore/src/engine.rs`)

- **`Engine::vector_count(ns) -> Option<u64>`** — returns the number of vectors in the HNSW index for the given namespace, or `None` if the index is not currently loaded in memory. Never triggers a disk scan or implicit index load. Call `preload_vector_index(ns)` first if a guaranteed count is needed. (`edgestore/src/engine.rs`)

- **`EdgestoreError::ReadOnly`** — new error variant returned by all write methods on a read-only engine. (`edgestore/src/error.rs`)

- **`ReplicatedEngine` in `edgestore-repl`** — convenience wrapper eliminating the three-step boilerplate of wiring `Engine` + `HttpReplicationServer` + `AntiEntropyLoop`. `ReplicatedEngine::open_primary(config, bind_addr)` opens a writable primary and starts the HTTP server. `ReplicatedEngine::open_replica(config, primary_url)` opens a read-only replica and starts the anti-entropy pull loop. (`edgestore-repl/src/replicated_engine.rs`)

- **`examples/unified_engine.rs`** — runnable example showing KV + vector + text search all in one `Engine` instance at one path, distinguished by namespace. Documents the recommended pattern: one lock file, one WAL, one flush timer, one replication target. (`edgestore/examples/unified_engine.rs`)

### Changed

- **`Drop` now fsyncs the WAL** in addition to persisting text indices. On clean shutdown (SIGTERM, normal drop), in-flight WAL writes are durable before the file handle closes. Errors from fsync in Drop are logged (`log::warn!`) and not propagated. (`edgestore/src/engine.rs`)

### Fixed

- **`index_text` was effectively O(n²) for indexing n documents into one namespace — now amortized O(n).** `Engine::index_text` unconditionally called `InvertedIndex::remove_document` before every `add_document`, even for a document never indexed before; `remove_document` scans every posting in every term (O(total index size)) regardless of whether the doc_id was ever present. For an append-only workload (unique key per document, the common case), that scan was pure waste on every call, scaling with however much had already been indexed. Measured: 10K documents in one namespace went from ~750ms to ~130-155ms; 100K documents from ~134s to ~1.5-1.8s; 1M documents, which never completed in any prior benchmark run, now indexes in ~18.5s. Fixed with a new `InvertedIndex::doc_bloom` (`edgestore/src/text/bloom.rs`) — a small, hand-rolled, capacity-adaptive Bloom filter checked before `remove_document`; zero false negatives means it's always safe to skip the scan when it reports "definitely not present". Capacity is not fixed — the filter doubles (rebuilding from `postings`) whenever it saturates, since a fixed-capacity version was found (before shipping) to degrade silently and just as badly once actual usage exceeded its designed capacity. Not part of `InvertedIndex`'s on-disk format — a pure in-memory optimization aid, rebuilt on load or growth. (`edgestore/src/text/bloom.rs`, `edgestore/src/text/index.rs`, `edgestore/src/engine.rs`)

- **Bloom filter hashing uses a per-instance random seed, not a fixed one.** A fixed hash seed would let a caller who controls `doc_id`/key values (true for edgestore as a general-purpose library) craft keys that collide into the same bit positions, inflating the false-positive rate and defeating the fix above under adversarial input. Seeded via `std::collections::hash_map::RandomState` — no new dependency, the same mechanism every `HashMap` in this codebase already gets by default. (`edgestore/src/text/bloom.rs`)

## [1.2.1] - 2026-07-06

### Fixed

- **`SegmentStore::remove_segment` — manifest updated before file deletion.** Previously files were deleted from disk before `manifest.remove_segments` was called. On manifest-write failure (most likely: disk-full, which is exactly the pressure under which this function runs), the manifest still listed the segment while the files were already gone. Any subsequent `get` for a key in that segment would surface an I/O error instead of a clean miss. Fixed: manifest and in-memory readers are updated first; a file-deletion failure after that leaves only orphaned bytes, not a broken store. (`edgestore/src/segment.rs`)

- **`SegmentStore::replace_segment` — same ordering fix.** Files were removed before the old segment was cleared from the manifest and the new one added. (`edgestore/src/segment.rs`)

- **`Compactor::collect_expired_cohort` and the all-dead path of `compact_partial_cohort` — same ordering fix.** Both called `remove_file` in a loop before calling `manifest.remove_segments`. Under disk-full conditions this left the manifest pointing at non-existent files. (`edgestore/src/compactor.rs`)

- **`TieredEngine::cache_segment` — LRU byte-counter drift under high segment count.** The `LruCache` was constructed with a 64-item capacity cap alongside the manual byte-based eviction loop. When the 65th distinct segment was inserted, the LRU library silently evicted the oldest entry by item count — without decrementing `segment_cache_bytes`. The byte counter then overcounted permanently (one-directional; never self-corrects), causing the byte-eviction loop to fire earlier than intended and evict live entries prematurely. Fixed: item cap raised to 65,536 — far beyond any realistic byte budget — so the byte loop is the only eviction path that fires. (`edgestore-tier/src/lib.rs`)

## [1.2.0] - 2026-07-06

### Added

- **`EdgestoreConfig::memtable_max_bytes`** — dedicated memtable flush threshold (default 8 MB). Previously the flush trigger used `segment_size_bytes` via a fixed 256-byte-per-entry estimate, which under-counted large values (blobs, embeddings). The new field decouples "when to flush" from "how big to make segments". Lower on constrained hardware, raise for high-throughput servers. (`edgestore/src/config.rs`, `edgestore/src/engine.rs`)

- **`EdgestoreConfig::hnsw_max_ram_bytes`** — HNSW sidecar file-size guard (default 512 MB). When the `.hnsw` file exceeds this threshold, `vector_search` skips loading the index and falls back to flat SIMD scan. Prevents silent OOM on large vector collections. Logs a warning with the file size and threshold. (`edgestore/src/config.rs`, `edgestore/src/engine.rs`)

- **`Engine::get_into(ns, key, buf: &mut Vec<u8>)`** — buffer-reuse variant of `get()`. Returns `true`/`false` for hit/miss; fills `buf` in-place on hit. For hot loops over large values where repeated `Vec<u8>` allocation is measurable. (`edgestore/src/engine.rs`)

- **`TieredEngine::with_segment_cache_bytes(max_bytes)`** — builder enabling an LRU byte cache for ephemeral segment downloads. When `range()` or `prefix()` downloads segment bytes from the remote for an ephemeral read, those bytes are cached up to `max_bytes` total. Subsequent range queries over the same segment are served from cache without re-downloading. Eviction is LRU by insertion order; on eviction, bytes are re-downloaded on next access. Default: 32 MB. Set to 0 to disable. (`edgestore-tier/src/lib.rs`)

### Changed

- **`TieredEngine::range()` and `prefix()` take `&mut self`** — required by the LRU segment byte cache, which must update access order on each lookup. `AsyncTieredEngine::range()` and `prefix()` now use `blocking_write()` instead of `blocking_read()`. Concurrent range queries are serialized; this is acceptable for the single-writer edge-first use case. Callers that held a `&TieredEngine` reference for range queries must bind `mut`. (`edgestore-tier/src/lib.rs`, `edgestore-tokio/src/tiered.rs`)

## [1.1.4] - 2026-07-05

### Fixed

- **`TieredEngine::range()` and `prefix()` now read through archived segments** — previously these methods returned only local data, silently omitting keys that had been archived to remote storage. Both methods now download overlapping archived segments ephemerally (no local import, no disk growth) and merge the results with local data using LWW semantics (local wins on key collision). `fetch_archived_overlapping()` remains available for callers that prefer explicit warming over per-query downloads. (`edgestore-tier/src/lib.rs`)

- **`Engine::strip_text_index()` — new lifecycle hook for removing text index records from local segments** — text index entries (`__text__*` namespace) are stored in the same segment files as user data, making them invisible to external GC once segments are archived. `strip_text_index(segment_id)` rewrites the target segment filtering out all `__text__*` entries and sets `SegmentMeta::text_index_stripped = true` on the new segment. This is safe to call on archived segments so hot-storage tiering does not accumulate index overhead indefinitely. (`edgestore/src/engine.rs`, `edgestore/src/types.rs`)

- **`TieredEngine::with_text_stripping(bool)` builder** — when enabled, `archive_segments()` automatically calls `strip_text_index()` after each successful upload, so tiered deployments shed index weight without additional application code. (`edgestore-tier/src/lib.rs`)

- **`SegmentMeta::text_index_stripped` field** — backward-compatible (`#[serde(default)]`) boolean flag that marks segments whose text index records have been removed. Existing manifests and remote archives deserialize correctly with the field defaulting to `false`. (`edgestore/src/types.rs`)

## [1.1.3] - 2026-07-05

### Added

- **`ImmutableEngine`** (`edgestore/src/immutable.rs`) — read-only in-memory engine for serverless, edge, and WASM environments (Phase 9). No WAL, no memtable, no local filesystem. Initializes from downloaded segment bytes and serves `get`, `range`, and `prefix` queries entirely in-memory with LWW merge across segments.
  - `ImmutableEngine::from_readers(readers)` — construct from pre-built `InMemorySegmentReader` instances.
  - `ImmutableEngine::from_segment_bytes(segments)` — eager-init from `(SegmentMeta, Vec<u8>)` pairs; parses all segments upfront.
  - `ImmutableEngine::export_manifest_json()` — emit a single JSON manifest (format v1) describing all segments by BLAKE3 hash, key bounds, LSN range, and record count. `min_key`/`max_key` are hex-encoded strings.
  - K-way BinaryHeap merge for `range()` and `prefix()` — correct LWW deduplication across any number of segments with deletes filtered.
  - 8 unit tests in `immutable.rs`: single-segment get, absent-key, LWW across two segments, sorted-deduped range, prefix scan, delete tombstone filtered, `from_segment_bytes`, 10K-record large segment.
- **`InMemorySegmentReader`** (`edgestore/src/segment/in_memory.rs`) — in-memory variant of `SegmentReader`. Parses `.dat` bytes (file header → ZSTD blocks → sparse index + xor filter) without filesystem I/O after init. Exported from `edgestore` root.
- **`RemoteStore::upload_aux` / `download_aux`** — sidecar file support added to the `RemoteStore` trait. Stores per-segment auxiliary files (`"idx"`, `"xf"`, `"meta"`) alongside `.dat` blobs. Default implementations return `Err(InvalidOperation)` so all existing `RemoteStore` implementations compile unchanged without modification.
  - `FilesystemRemoteStore` — implements both methods; keys as `{hash_hex}.{ext}` files; atomic write via `.{ext}.tmp` rename.
  - `S3RemoteStore` — implements both methods; S3 key `{prefix}segments/{hash_hex}.{ext}`.
- **`TieredEngine::with_sidecars(bool)`** — builder method enabling sidecar upload during `archive_segments`. When `true`, uploads `.idx`, `.xf`, and `.meta` alongside each `.dat` segment. Sidecar upload errors are logged but do not fail the archive — the `.dat` is sufficient for correctness.
- **Serverless benchmarks** (`edgestore/benches/immutable.rs`):
  - `immutable_cold_start_1k` — init + first `get` from 1K-record segment.
  - `immutable_cold_start_10k` — init + first `get` from 10K-record segment.
  - `immutable_get_hot` — warm `get` on pre-initialized 1K engine.
  - `immutable_range_1k` — range scan across all 1K records.
  - `immutable_multi_segment_merge` — K-way merge over 5 segments × 200 records each.
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
