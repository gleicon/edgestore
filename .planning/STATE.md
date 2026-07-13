---
gsd_state_version: 1.0
milestone: v1.3
milestone_name: milestone
  current_phase: post-09
  status: Maintenance
  last_updated: "2026-07-07T00:00:00.000Z"
  progress:
    total_phases: 9
    completed_phases: 9
    total_plans: 55
    completed_plans: 55
    percent: 100
---

# Project State — EdgeStore

## Current Status

**Phase:** Post-Phase 9 — Maintenance / patch releases
**Current Phase:** post-09
**Milestone:** v1.3.0 released (Vectoria feedback batch; all 9 phases complete)

## Now

**State:** v1.3.0 shipped. Decisions D26–D33 recorded. Two user feedback cycles resolved (Vectoria: D26–D31; Pierre: D32–D33). No open implementation work — all decisions translated to code or docs. Pierre's tiered replication pattern documented in DECISIONS.md; no edgestore code changes needed for Pierre.

**Next:** No active task. Next action is user-driven — either another user feedback cycle, or a new feature request.

**Open questions:** None blocking.

**Watch:** Pierre integrating TieredEngine + AntiEntropyLoop pattern (D32/D33) — may surface further gaps.

## Phase Progress

| Phase | Status | Started | Completed |
|-------|--------|---------|-----------|
| 1 — Core KV Engine | Complete | 2026-05-18 | 2026-05-18 |
| 2 — Segment Store | Complete | 2026-05-18 | 2026-05-18 |
| 3 — Deathtime Compaction | Complete | 2026-05-19 | 2026-05-20 |
| 4 — Replication + S3 | Complete | 2026-05-20 | 2026-05-23 |
| 4.1 — Engine Correctness & Edge Cases | Complete | 2026-05-21 | 2026-05-21 |
| 5 — Vector Search | Complete | 2026-05-23 | 2026-05-23 |
| 6 — SSD Optimization + HNSW | Complete | 2026-05-25 | 2026-05-25 |
| 7 — Full-Text Search (v2) | Complete | 2026-05-25 | 2026-05-25 |
| 8 — v1.0 Polish & Release | Complete | 2026-05-25 | 2026-05-26 |
| 9 — Read-Only Edge Engine | Complete | 2026-07-05 | 2026-07-05 |

## Phase 9 Wave Progress

| Wave | Status | Deliverables |
|------|--------|--------------|
| Wave 1: `InMemorySegmentReader` | Complete | `edgestore/src/segment/in_memory.rs` + 4 unit tests |
| Wave 2: `ImmutableEngine` | Complete | `edgestore/src/immutable.rs` + 8 unit tests + `export_manifest_json()` |
| Wave 3: Sidecars + `RemoteStore` extension | Complete | `upload_aux`/`download_aux` on trait + FilesystemRemoteStore + S3RemoteStore; `TieredEngine::with_sidecars()`; `archive_segments` sidecar upload; 7 new tests |
| Wave 4: WASM bindings | Deferred | Out of scope per D22 — platform owners build their own bindings |
| Wave 5: Serverless benchmarks | Complete | `edgestore/benches/immutable.rs` — 5 benches (cold start 1K/10K, hot get, range 1K, multi-segment merge) |

## Post-Release Patches

| Version | Date | Changes |
|---------|------|---------|
| v1.1.3 | 2026-07-05 | ImmutableEngine, InMemorySegmentReader, sidecars, serverless benchmarks, quality-gate passes |
| v1.1.4 | 2026-07-05 | TieredEngine range/prefix ephemeral read-through (D23); Engine::strip_text_index + with_text_stripping (D24); Makefile CRATES publish order fix (D25) |
| v1.2.0 | 2026-07-05 | Bug fixes: manifest-before-files ordering in remove_segment/replace_segment/compactor (4 sites); LRU byte-counter drift in TieredEngine; regression tests for both |
| v1.2.1 | 2026-07-05 | Patch: parquet_export example dev-deps; doc backlog VEC-02/VEC-04/API-02/API-03/API-04 |
| v1.3.0 | 2026-07-07 | Vectoria batch (D26–D31): Engine::open_readonly, EdgestoreConfig::readonly, EdgestoreError::ReadOnly, Engine::with_on_segment_flushed, Engine::vector_count, Drop WAL fsync, ReplicatedEngine (edgestore-repl), examples/unified_engine.rs, examples/production_patterns.rs, edgestore-repl/examples/replicated_engine.rs |

## Requirement Status

42 requirements total — 0 validated, 34 active, 8 blocked (Phase 9 RO-01–08)

## Phase 3 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 03-01 | 1 | Compactor + Snapshot scaffold, config, error | COMPACT-03, COMPACT-04, COMPACT-06 |
| 03-02 | 2 | Compactor core algorithm | COMPACT-04, COMPACT-07 |
| 03-03 | 2 | Snapshot implementation | COMPACT-06 |
| 03-04 | 3 | Engine integration | COMPACT-04, COMPACT-06 |
| 03-05 | 4 | Integration tests | COMPACT-01–07 |

## Completed Plans: 03-01, 03-02, 03-03, 03-04, 03-05

Plan 03-05 (Integration tests) completed 2026-05-20.

- 5 integration tests in edgestore/tests/integration_compaction.rs; all pass
- SC1 (COMPACT-04): TTL expiry → compact_once → live_records_relocated == 0
- SC2 (COMPACT-05): range scan across 3+ overlapping segments returns correct LWW value
- SC3 (COMPACT-06): snapshot pins survive compaction; readable after compact; drop releases pins
- SC4 (COMPACT-04): write_budget_bytes=1 stops compaction after first partial cohort
- SC5 (COMPACT-07): output segment merkle_root matches recomputed blake3 hash of sorted key hashes
- 100 tests pass workspace-wide; cargo clippy -D warnings clean
- Commit: 888c15a

Plan 03-04 (Engine integration) completed 2026-05-20.

- Engine.snapshot_registry: SnapshotRegistry field added (COMPACT-06 wiring)
- SegmentStore.segment_ids() helper added (readers field is private)
- Engine::compact_once: wall-clock now_nanos, pinned_ids, Compactor, manifest.mf reload (COMPACT-04)
- Engine::snapshot: register current segments, return Snapshot with RAII pin release (COMPACT-06)
- 95 tests pass workspace-wide; cargo clippy -D warnings clean
- Commits: fe60746, 5a8011b, fc14138

Plan 03-01 (Compactor + Snapshot scaffold) completed 2026-05-19.

- EdgestoreConfig.compaction_write_budget_bytes = 256 MB default
- EdgestoreError.CompactionError(String) variant
- compactor.rs: Compactor, CohortInfo, CompactionStats
- snapshot.rs: Snapshot, SnapshotRegistry with Drop pin-release
- All modules declared and re-exported in lib.rs
- cargo build --workspace and cargo clippy -D warnings clean

Plan 03-02 (Compactor core algorithm) completed 2026-05-19.

- CohortInfo.is_fully_expired field added
- identify_cohorts: group by cohort_bucket, sort expired-first
- collect_expired_cohort: zero-live-relocation expired cohort removal (COMPACT-04)
- compact_partial_cohort: LWW merge, dead-record filter, SegmentWriter output (COMPACT-07)
- compact_cycle: budget-bounded, pinned-segment-aware, fully-expired-first dispatch (COMPACT-04)
- 8 unit tests; all pass; clippy -D warnings clean
- Commit: 31873ef

Plan 03-03 (Snapshot implementation) completed 2026-05-19.

- SnapshotRegistry: register/release/is_pinned/pinned_ids (COMPACT-06)
- Snapshot::new, Drop (RAII pin release), get (LWW by LSN), range (LWW merge + decode)
- 5 unit tests pass; clippy -D warnings clean
- Commit: 0bdb1aa

## Phase 4 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 04-01 | 1 | Range-level Merkle + Replication types | REPL-01, REPL-02, REPL-03 |
| 04-02 | 2 | Engine replication public API | REPL-05, REPL-06 |
| 04-03 | 3 | edgestore-repl: HTTP client + server | REPL-03 |
| 04-04 | 4 | edgestore-repl: S3 backend | REPL-04 |
| 04-05 | 5 | Integration tests: all 5 success criteria | REPL-01–06 |

## Phase 4.1 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 04.1-01 | 1 | WAL in-write rotation | CORE-02 |
| 04.1-02 | 2 | TTL lazy-expiry contract: document and test | CORE-05 |
| 04.1-03 | 1 | Fix SegmentReader::range_scan end-inclusive bug | CORE-05 |
| 04.1-04 | 2 | Snapshot::get LWW ordering — multi-segment divergence test | CORE-04 |

## Phase 5 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 05-01 | 1 | Vector types, encoding, dtype support | VECTOR-01, VECTOR-04 |
| 05-02 | 2 | Vector KV API (vector_put, vector_get, vector_delete) | VECTOR-02 |
| 05-03 | 3 | Distance metrics — SIMD + scalar, all three metrics | VECTOR-03, VECTOR-04 |
| 05-04 | 4 | Flat scan search with top-k heap | VECTOR-02, VECTOR-03 |
| 05-05 | 5 | Integration tests + benchmarks (all 5 SC) | VECTOR-01–05 |

## Phase 6 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 06-01 | 1 | StorageBackend trait + default/local/test backends | SSD-01 |
| 06-02 | 2 | FDP placement hints + mock backend | SSD-01, SSD-02 |
| 06-03 | 1 | HNSW core — graph, search, serialization | SSD-03 |
| 06-04 | 2 | HNSW integration — build, search routing, sidecar | SSD-03 |
| 06-05 | 3 | edgestore-tokio async wrapper | SSD-04 |
| 06-06 | 3 | Benchmarks + integration tests | SSD-05 |

## Phase 7 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 07-01 | 1 | Tokenizer + text types (English, stopwords, stemming) | SEARCH-01 |
| 07-02 | 1 | Inverted index core — posting lists, BM25 scoring | SEARCH-02 |
| 07-03 | 2 | TextEngine trait + Engine integration | SEARCH-01, SEARCH-02 |
| 07-04 | 2 | Faceting + typo tolerance (Levenshtein ≤ 1) | SEARCH-03, SEARCH-04 |
| 07-05 | 3 | Integration tests + benchmarks | SEARCH-01–04 |

## Phase 2 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 02-01 | 1 | Segment types + deps | STORE-01–07 |
| 02-02 | 2 | Segment writer | STORE-01, STORE-02, STORE-03, STORE-04, STORE-05, STORE-07 |
| 02-03 | 3 | Segment reader | STORE-01, STORE-02, STORE-04, STORE-05 |
| 02-04 | 3 | Manifest | STORE-06, STORE-07 |
| 02-05 | 4 | SegmentStore + Engine | STORE-01–03, STORE-06–07 |
| 02-06 | 5 | Integration tests | STORE-01–07 |

## Phase 1 Plans

| Plan | Wave | Title | Requirements |
|------|------|-------|--------------|
| 01-01 | 1 | Cargo workspace scaffold + core types | CORE-01–08 |
| 01-02 | 2 | WAL implementation | CORE-01, CORE-02, CORE-07 |
| 01-03 | 2 | MemTable trait + BTreeMap impl | CORE-03, CORE-08 |
| 01-04 | 3 | KV Engine — single-writer + KV API | CORE-04, CORE-05, CORE-08 |
| 01-05 | 4 | Transaction API | CORE-04, CORE-06 |
| 01-06 | 5 | Crash recovery | CORE-01, CORE-02, CORE-07 |
| 01-07 | 6 | Integration tests | CORE-01–08 |

## Roadmap Evolution

- Phase 4.1 inserted after Phase 4 (2026-05-21): Engine Correctness & Edge Cases — WAL in-write rotation, TTL lazy-expiry contract, range_scan end-inclusive fix, Snapshot::get LWW ordering fix. Discovered during Phase 3 test coverage review.

## Notes

- Project initialized 2026-05-18
- All 16 architectural decisions resolved via grill-me session before initialization
- prod.md contains full design spec with references
- No gsd-sdk installed — research subagents unavailable; roadmap generated inline

### Phase 4 Verification Gap Fixes (2026-05-23)

Three critical blockers in `import_segment` fixed post-execution:

- **C-01**: Delete tombstones now replicated — added `delete_with_timestamp()` and Delete arm in LWW apply block
- **C-02**: xor filter built from decoded keys — imported segments readable after engine restart
- **C-03**: min_key/max_key tracked during LWW scan — range/prefix queries work on imported segments after restart
- **W-03**: .meta file fsync added for durability

All fixes committed. 147 tests pass workspace-wide. cargo clippy -D warnings clean.

### Phase 5 Completion (2026-05-23)

All 5 plans executed successfully:

- **05-01**: Vector types (Dtype: F32/F16/I8), VectorRecord, encode/decode with 3-byte big-endian header
- **05-02**: VectorEngine trait with vector_put, vector_get, vector_delete on Engine; synthetic `__vec__{ns}` namespace isolation
- **05-03**: All 3 distance metrics (Cosine, L2, DotProduct) with SIMD (wide::f32x8) + scalar fallback; f16/i8 widening
- **05-04**: Brute-force flat scan with top-k BinaryHeap; results sorted ascending; deletes excluded
- **05-05**: 5 success criteria verified + Criterion benchmarks (10K, 100K collections)

**Key fixes during execution:**
- HeapItem Ord: fixed reverse ordering bug so BinaryHeap peek returns worst (largest distance) item
- SC2 tests use set-equality comparison to tolerate SIMD vs scalar near-tie differences

**Stats:**
- 187 tests pass workspace-wide (10 test suites)
- cargo clippy --workspace -D warnings clean
- Benchmarks compile successfully

### Phase 6 Completion (2026-05-25)

All 6 plans executed successfully:

- **06-01**: StorageBackend trait (read/write/flush/read_all) + DefaultStorageBackend + MemoryStorageBackend for tests
- **06-02**: FDP placement hints — PlacementHint struct, write_with_hint default impl, MockFdpBackend for verification
- **06-03**: HNSW core — HnswIndex with probabilistic layer assignment, greedy beam search, diversity heuristic neighbor selection, serialize/deserialize
- **06-04**: HNSW integration — build_vector_index, get_vector_index with lazy load, vector_search routing (HNSW vs flat scan), sidecar file persistence
- **06-05**: edgestore-tokio async wrapper — AsyncEngine with spawn_blocking for all Engine methods
- **06-06**: 3 Criterion benchmarks (hnsw_recall, throughput, vector_search) + 8 Phase 6 integration tests

**Key decisions during execution:**
- MemTable trait bound expanded to `Send + Sync` to enable `Arc<RwLock<Engine>>` in tokio wrapper
- HNSW recall threshold set to 0.70 for clustered data (v1 without full diversity heuristic optimization)
- `vector_search` returns decoded raw keys (not encoded); `build_vector_index` uses them directly

**Stats:**
- 214 tests pass workspace-wide (13 test suites)
- cargo clippy --workspace -D warnings clean
- All benchmarks compile (`cargo bench --no-run`)

### Phase 7 Completion (2026-05-25)

All 5 plans executed successfully:

- **07-01**: Text tokenizer — English tokenization, stopwords (~100 words), simple stemming (-ing, -ed, -ies, -s), `Token` with position info
- **07-02**: Inverted index core — `InvertedIndex` with `HashMap<String, Vec<Posting>>`, BM25 scoring (k1=1.2, b=0.75), serialize/deserialize with `INVX` magic header
- **07-03**: TextEngine integration — `index_text`, `search_text`, `delete_text` on Engine; `__text__{ns}` synthetic namespace; `TextSearchResult` with BM25 score; `SearchOptions` with facet filters and typo tolerance
- **07-04**: Faceting + typo tolerance — `FacetFilter` struct, `filter_by_facets` function, `levenshtein` distance (Wagner-Fischer), `is_one_edit_away`, fuzzy matching with 0.5 weight penalty
- **07-05**: 8 integration tests + 2 Criterion benchmarks (text_search QPS, index throughput)

**Deferred to BACKLOG.md:**
- FT-01: Pluggable Tokenizer trait (for Hugging Face `tokenizers` crate integration)
- FT-02: LLM-compatible BPE/WordPiece/Unigram tokenization
- FT-03: Bigram/shingle inverted index with vocabulary dictionary
- FT-04: RAG-optimized chunk-level indexing with shared text+embedding storage
- FT-05: Multi-language tokenization (CJK, unicode segmentation)

**Key decisions during execution:**
- Lightweight English tokenizer for v1 to minimize dependencies (vs Hugging Face's 100+ dep crate)
- Facets stored in each posting for filtering during search (not separate index)
- Typo tolerance scans all indexed terms for edit distance ≤ 1 (acceptable for small indexes)
- BM25 sort uses doc_id as tiebreaker for stable, deterministic ordering

**Stats:**
- 421 tests pass (180 lib + 237 integration + 4 tokio)
- cargo clippy --workspace -D warnings clean
- 4 benchmarks compile (vector_search, hnsw_recall, throughput, text_search)

## Phase 8 Plans

| Plan | Wave | Title | Requirements | Status |
|------|------|-------|--------------|--------|
| 08-01 | 1 | API polish: rustdoc, clippy, visibility, feature flags, metadata | POLISH-01, POLISH-02, POLISH-05 | **Complete** |
| 08-02a | 1 | Core documentation: README, ARCHITECTURE.md, CHANGELOG | POLISH-03 | **Complete** |
| 08-02b | 1 | Benchmarks, examples, and license files | POLISH-03, POLISH-07 | **Complete** |
| 08-03a | 2 | CLI scaffold and core subcommands | POLISH-04 | **Complete** |
| 08-03b | 2 | CLI advanced subcommands and build config | POLISH-04 | **Complete** |
| 08-04a | 3 | Final integration test and metadata finalization | POLISH-05, POLISH-06 | **Complete** |
| 08-04b | 3 | Publish dry-run, benchmarks, and final validation | POLISH-07, POLISH-08 | **Complete** |

### Plan 08-02b Completion (2026-05-25)

- **BENCHMARKS.md**: Documents all 4 benchmark binaries, methodology, hardware requirements, and placeholder results for 6 measurement categories
- **Examples**: 3 runnable examples in `edgestore/examples/`
  - `basic_kv.rs`: namespace put/get/range/prefix/delete
  - `vector_search.rs`: 1000 vectors, HNSW build, ANN search top-5
  - `replication.rs`: primary→replica segment export/import with merkle sync
- **Licenses**: Dual MIT/Apache-2.0 at repo root; Cargo.toml updated with `license = "MIT OR Apache-2.0"`
- All examples compile and run with `cargo run --example <name>`
- Commits: 0981f2e, 0fd7011, c3f00e3

### Plan 08-01 Completion (2026-05-25)

- **Rustdoc coverage**: `#![warn(missing_docs)]` added to lib.rs; crate-level and module-level docs added; all public items documented
- **Visibility audit**: Many `pub` items changed to `pub(crate)`; integration-test-used items kept as `pub` with docs added
- **Clippy clean**: `RUSTFLAGS="-D warnings" cargo clippy --workspace` passes with zero warnings
- **Clean build**: `cargo clean && cargo test --workspace` — 249 tests pass
- **Feature flags**: Verified `cargo build --no-default-features -p edgestore` compiles; no changes needed
- **Cargo.toml metadata**: description, repository, homepage, keywords, categories, readme added to all 3 crates; license added to edgestore-repl and edgestore-tokio
- **Publish dry-run**: `cargo publish --dry-run -p edgestore` passes
- **Commits**: docs(edgestore): add rustdoc coverage and visibility audit for 08-01; ci(edgestore): verify clippy clean build and test pass for 08-01; ci(edgestore): verify feature flag cleanup for 08-01; ci(edgestore): add Cargo.toml metadata for 08-01

### Plan 08-04a Completion (2026-05-25)

- **integration_v1.rs**: Comprehensive end-to-end test covering KV operations, TTL + compaction, snapshots, vector storage/search, text search with BM25, replication (manifest export/import + merkle compare), and transactions (commit/rollback)
- **Test passes in ~2.4s** (well under 60s target); uses temporary directories for isolation
- **Metadata finalization**: All crates version 1.0.0 via `[workspace.package]` inheritance; dependency versions pinned; edgestore keywords/categories updated for crates.io discoverability
- **RELEASE_CHECKLIST.md**: Step-by-step release process including verification, tagging, crates.io publish order, and GitHub release notes
- Commits: `ce70f2f`, `fedce85`

### Plan 08-04b Completion (2026-05-25)

- **Publish dry-run**: `cargo publish --dry-run -p edgestore` passes with 55 files packaged (512 KiB, 113 KiB compressed); includes README.md, LICENSE-MIT, LICENSE-APACHE
- **Benchmark suite**: All 4 Criterion benchmarks execute successfully on Apple M5 / 16 GB RAM
  - `hnsw_recall`: 500 vec = 378 µs, 1000 vec = 654 µs, 5000 vec = 3.19 ms
  - `text_search`: 100 docs = 215 µs/query, 1000 docs = 3.77 ms/query, 10000 docs = 164.7 ms/query
  - `throughput`: put 1K = 2.53 ms (395K ops/sec), get 1K hot = 111.8 µs (8.9M ops/sec), vector flat 1K = 57.6 µs, vector HNSW 1K = 20.6 µs
  - `vector_search`: flat scan 500 = 8.2 µs, HNSW 500 = 8.2 µs
- **Validation gates**: All pass — 250 tests, clippy -D warnings clean, doc clean, release build clean, publish dry-run clean
- **CLI install check**: `cargo install --path edgestore-cli && edgestore-cli --version` → `edgestore-cli 1.0.0`
- **Auto-fixes during execution**:
  - NaN panic in HNSW benchmark: added `total_cmp_f32` helper for NaN-safe f32 ordering across 5 files
  - Clippy errors in edgestore-cli: fixed single_match, redundant_pattern_matching, manual_is_multiple_of, dead_code
  - LICENSE files not in package: added symlinks from repo root into each crate directory
- Commits: `13a1792`, `7628fa8`, `clippy fix commit`

## Phase 8 Planning Notes

- Phase 8 planning completed 2026-05-25
- 7 plans across 3 waves
- All 8 POLISH requirements mapped
- Plans verified by plan-checker agent — no blockers remaining
