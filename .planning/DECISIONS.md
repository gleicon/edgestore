# EdgeStore Architecture Decisions

Resolved via grill-me session 2026-05-22.

---

## D01 — Sync unit: segment

**Decision:** The segment is the atomic unit of replication.

**Rationale:** Segments are immutable once written and content-addressed by BLAKE3. Two nodes holding a segment with the same hash hold identical data — no per-record comparison is required. This makes sync verification O(1) per segment and allows blind transfer without re-checking content.

**Implication:** Sync granularity equals segment size (4 MB default). Smaller segment size = finer sync granularity but more manifest overhead. There is no sub-segment partial transfer in Phase 4.

---

## D02 — Replication protocol: manifest-diff primary, Merkle as probe

**Decision:** The primary sync path is manifest-diff (set difference of segment hashes between peers). The Merkle tree is used only as a cheap anti-entropy probe: compare roots first; if equal, skip manifest exchange entirely; if different, proceed to manifest diff.

**Rationale:** Key-range Merkle routing adds significant complexity for the common case (small diffs, small clusters). Manifest diff is O(segments) and trivially correct. The Merkle probe saves bandwidth and CPU on the frequent "already in sync" case without adding routing logic.

**Implication:** `ReplicationProtocol` exposes `merkle_root()`, `list_segments()`, `fetch_segment()` — not a range-partitioned delta API. `RangeMerkleTree` exists and is built (for future per-range optimization) but is not used for routing in Phase 4.

---

## D03 — Default segment size: 4 MB

**Decision:** Default segment size target is 4 MB (changed from 16 MB in STORE-03).

**Rationale:** Smaller segments yield finer replication granularity and faster individual segment transfers. The tradeoff is more segments per GB of data and more manifest entries, which is acceptable at current scale targets.

**Implication:** `EdgestoreConfig.segment_size_bytes` defaults to `4 * 1024 * 1024`. STORE-03 requirement text is superseded by this decision. All plan files and test configs use 4 MB.

---

## D04 — S3 role: durability tier, not primary

**Decision:** S3 is a durability/archive tier only. Phase 4 does not implement S3. The `RemoteStore` trait is defined in Phase 4, and `FilesystemRemoteStore` is the only implementation.

**Rationale:** S3 integration adds AWS credential management, SigV4 signing complexity, and network I/O concerns that are orthogonal to getting the core replication protocol right. The `RemoteStore` trait abstracts the backend so S3 can be added in a future phase without changing the anti-entropy loop.

**Implication:** Plan 04-04 builds `FilesystemRemoteStore` (a local directory that acts like S3 for testing). Real S3 (`S3RemoteStore`) is a future phase deliverable. No AWS SDK or SigV4 code in Phase 4.

---

## D05 — RemoteStore trait: defined Phase 4, FilesystemRemoteStore only

**Decision:** The `RemoteStore` trait is defined in `edgestore-repl` with four methods. `FilesystemRemoteStore` is the sole Phase 4 implementation.

**Rationale:** The trait boundary lets the anti-entropy loop operate against any durable backend without caring whether it is a local directory, S3, GCS, or SFTP. Defining the trait now locks in the interface before any implementation exists.

**Implication:** The trait signature is:
```rust
pub trait RemoteStore: Send + Sync {
    fn upload(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError>;
    fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError>;
    fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError>;
    fn delete(&self, hash: &[u8; 32]) -> Result<(), EdgestoreError>;
}
```
`FilesystemRemoteStore` stores files as `{base_dir}/{hash_hex}.seg`. No new external dependencies.

---

## D06 — LWW tiebreaker: wall clock + HostId

**Decision:** Last-write-wins conflict resolution uses wall-clock timestamp (unix nanoseconds) as the primary comparator. On timestamp collision, `HostId` lexicographic order breaks the tie (lower HostId wins).

**Rationale:** Pure wall-clock LWW is simple and correct when clocks are synchronized. The HostId tiebreaker makes resolution deterministic on collision rather than arbitrary.

**Implication:** NTP (or equivalent time synchronization) is required. This is a documented operational requirement, not enforced by the library. Clock skew > segment flush interval can cause incorrect merge outcomes. The WAL record `timestamp` field carries the authoritative wall-clock time. The `HostId` field is part of `EdgestoreConfig` and written into WAL records. A future phase may replace this with vector clocks (see v2 requirements).

---

## D07 — Wire format: MessagePack control, raw bytes segments

**Decision:** All control messages (merkle probe, segment manifest, cursor state) use MessagePack serialization via `rmp-serde`. Segment data is transferred as raw bytes (Content-Type: `application/octet-stream`). All HTTP endpoints support a `?debug=json` query parameter that re-serializes the MessagePack response to JSON for human inspection.

**Rationale:** MessagePack is more compact than JSON for binary-heavy payloads (segment hashes, byte arrays). Raw bytes for segment transfer avoids base64 overhead. The `?debug=json` escape hatch makes debugging and integration testing practical without requiring a MessagePack client.

**Implication:** `rmp-serde` is added to `edgestore-repl/Cargo.toml`. The `serde_json` dependency is retained for `?debug=json` re-serialization only. All structs in `replication.rs` must derive both `Serialize` and `Deserialize` (they already do via serde).

---

## D08 — Sync model: pull-only + per-peer cursor

**Decision:** Phase 4 implements pull-only anti-entropy. There is no push path. Each node runs an `AntiEntropyLoop` that periodically contacts configured peers, probes their Merkle root, and pulls missing segments. Progress is tracked in a per-peer cursor file.

**Rationale:** Pull-only is simpler to reason about (no concurrent push/pull races), easier to throttle, and naturally handles node restarts without sender-side state. Per-peer cursor files make partial progress durable across crashes.

**Implication:**
- `AntiEntropyLoop` runs in a background `std::thread::spawn` thread.
- Default probe interval: 30 seconds (configurable).
- Per-peer cursor file path: `{db_path}/sync/{peer_id}.cursor`
- Cursor fields (MessagePack-serialized):
  - `last_known_merkle_root: [u8; 32]`
  - `segments_pending: Vec<[u8; 32]>`
  - `last_attempt_secs: u64`
  - `segments_applied_total: u64`
- Segment download procedure: write to `{hash_hex}.tmp`, verify BLAKE3, rename to final location, update manifest, remove hash from `segments_pending`, flush cursor to disk. Progress is durable after each segment.
- No push endpoint is implemented in Phase 4.

---

## D09 — Vector namespace: synthetic `__vec__{ns}` prefix

**Decision:** Vector records are stored in the KV layer under a synthetic namespace derived by prepending `__vec__` to the user-supplied namespace name.

**Rationale:** This keeps the KV layer pure (no special-casing for vector data) while giving vector records their own isolated key space that cannot collide with user keys. The KV layer sees opaque bytes; only the vector API knows the encoding.

**Implication:** A user namespace named `"images"` stores vector records under namespace `"__vec__images"`. User code cannot accidentally read vector records via the plain KV `get` API unless it constructs the synthetic namespace manually. The `vector_put` / `vector_get` / `vector_search` API handles namespace translation internally.

---

## D10 — SIMD: `wide` crate, stable Rust

**Decision:** SIMD distance computations use the `wide` crate on stable Rust. No nightly features, no `std::simd` (unstable).

**Rationale:** The `wide` crate provides portable SIMD across x86_64 (SSE2/AVX2) and aarch64 (NEON) with stable Rust semantics. Nightly `std::simd` is a moving target that breaks toolchain upgrades.

**Implication:** `wide = "0.7"` added to `edgestore-vector` (or `edgestore`) Cargo.toml in Phase 5. Distance metric implementations use `wide::f32x8` or `wide::f32x4` lanes. Scalar fallback is always correct; SIMD is an optimization.

---

## D11 — Distance metrics: all three (Cosine, L2, DotProduct)

**Decision:** Phase 5 implements all three distance metrics: Cosine similarity, L2 (Euclidean) distance, and Dot Product.

**Rationale:** Different embedding models require different metrics (text embeddings typically use Cosine; image embeddings often use L2; some retrieval models use Dot Product). Supporting all three in the flat scan avoids forcing users into a suboptimal metric.

**Implication:** `vector_search(ns, query, k, metric)` accepts a `Metric` enum: `Metric::Cosine`, `Metric::L2`, `Metric::DotProduct`. All three have SIMD implementations using the `wide` crate.

---

## D12 — Index: explicit `build_vector_index(ns)`, flat scan default

**Decision:** Flat SIMD scan is the default for `vector_search`. An HNSW index is built only via explicit `engine.build_vector_index(ns)` call. No automatic index building.

**Rationale:** Automatic index building on write would add latency spikes and complexity to the write path. Explicit build gives operators control over when the index is built (e.g., after bulk load, during a maintenance window).

**Implication:** `vector_search` always works without a pre-built index (flat scan). If an HNSW index exists and is fresh, `vector_search` uses it. The caller does not need to know which path is taken — the selection is internal. See D13 for staleness handling.

---

## D13 — HNSW: sidecar file, lazy load, metrics-tracked, staleness by segment-id hash

**Decision:** The HNSW graph is stored in a sidecar file at `{db_path}/vector/{ns_slug}.hnsw`. It is lazy-loaded on the first HNSW search request (not at `Engine::open`). Staleness is detected by comparing the hash of current segment IDs against the segment-id hash stored in the file header. If stale, the engine falls back to flat scan silently. `preload_vector_index(ns)` is available for explicit preload. Index load time is tracked in `MetricsSnapshot`. A warning is logged if load time exceeds 2 seconds.

**Rationale:** Lazy loading avoids startup latency for workloads that do not use HNSW. Segment-id-hash staleness detection is cheap and correct: if any segment was added or removed since the index was built, the hash differs and the index is considered stale (fall back to flat scan, which is always correct).

**Implication:** The HNSW sidecar header must store the serialized segment-id set hash. `build_vector_index(ns)` writes this header. The `MetricsSnapshot` struct gains `vector_index_load_ms: Option<u64>` and `vector_index_stale: bool` fields.

---

## D14 — Block cache: LRU 64 MB, evict on compaction, skip dying cohorts

**Decision:** A decompressed block cache lives in `SegmentStore`. Cache key is `(segment_id, block_offset)`. Policy is LRU using the `lru` crate. Default size is 64 MB, configurable via `EdgestoreConfig.block_cache_bytes`. Cache is per-engine (no `Arc` sharing between engines). On `compact_once`, all cached blocks for removed segments are evicted immediately (correctness). Blocks from segments whose cohort is past expiry are not cached (they will be collected soon). No cache pre-warm on `Engine::open`.

**Rationale:** The SSD WAF paper shows that caching decompressed blocks avoids re-decompression on repeated point lookups. Evicting stale-cohort blocks prevents wasting cache space on soon-to-be-collected data.

**Implication:** `lru` crate added to `edgestore/Cargo.toml`. `SegmentStore` gains a `block_cache: LruCache<(u64, u64), Vec<u8>>` field. Compaction calls `cache.pop_entry` for each removed segment's cached blocks after manifest update.

---

## D16 — ImmutableEngine crate placement: core, not separate

**Decision:** `ImmutableEngine` lives in `edgestore` core as `edgestore::immutable::ImmutableEngine`. A separate crate is deferred until bundle size becomes a measurable issue.

**Rationale:** Reuses existing `segment`, `types`, `snapshot` modules with zero duplication. Moving to a separate crate would require extracting shared internals or duplicating code.

**Implication:** `ImmutableEngine` stays in core. No separate crate needed unless a future platform-specific crate emerges.

---

## D22 — WASM bindings: out of scope; platform owners build their own

**Decision:** EdgeStore does not ship WASM bindings (`wasm-bindgen`, `wasm-pack`, `edgestore-wasm`). Serverless and edge platforms that want to use EdgeStore in WASM runtimes must build their own bindings on top of the Rust `ImmutableEngine` API.

**Rationale:** EdgeStore is a Rust library first. The core team's focus is the Rust API surface. WASM is a target runtime, not a first-class platform we maintain. The `export_manifest_json()` API and `ImmutableEngine` provide everything a platform needs to build their own WASM wrapper.

**Implication:** The `edgestore-wasm` crate is removed from the workspace. `RO-07` (WASM bindings requirement) is deferred to platform owners. The core team does not track bundle size or JS interop.

---

## D17 — Xor filter deduplication for duplicate keys across blocks

**Decision:** `build_xor_filter()` deduplicates key hashes (via `HashSet`) before constructing the `xorf::Xor8` filter.

**Rationale:** The same key can appear in multiple blocks within a single segment (e.g. old + new version during compaction). `xorf::Xor8::from()` panics on duplicate hashes. Deduplication is cheap and preserves correctness.

**Implication:** `build_xor_filter` now allocates a temporary `HashSet<u64>` before building the filter. No API change.

---

## D18 — Error handling split: `get()` swallows, `fetch_segment()` propagates

**Decision:** `TieredEngine::get()` returns `None` on non-retryable remote fetch errors (network unreachable, auth failure, 404). `TieredEngine::fetch_segment()` propagates the same errors as `Result::Err`.

**Rationale:** `get()` is a point lookup where the caller expects an `Option`. Swallowing errors gracefully matches the "best effort" semantics of a tiered cache. `fetch_segment()` is an explicit operation where the caller must know if the segment is unavailable.

**Implication:** Callers that need error details use `fetch_segment()` directly; casual `get()` callers get resilient behavior.

---

## D19 — Retry policy: transient errors only, 3 retries, exponential backoff

**Decision:** `fetch_and_import()` and `upload_with_retry()` retry only on errors whose message contains "throttled", "timeout", or "503". Max 3 retries with backoff: 10ms, 20ms, 40ms.

**Rationale:** Other errors (auth failure, 404, corrupted data) are permanent and should fail fast. Exponential backoff with jitter avoids thundering herd against object storage.

**Implication:** The retry logic inspects error message strings (not typed variants). Future refactor may introduce a `RemoteError::is_transient()` method.

---

## D20 — WASM crate name: `edgestore-wasm`

**Decision:** The WASM bindings crate is named `edgestore-wasm`, following the workspace naming convention (`edgestore-{suffix}`).

**Rationale:** Consistent with `edgestore-repl`, `edgestore-tokio`, `edgestore-tier`, `edgestore-cli`.

**Implication:** Published crate name on crates.io will be `edgestore-wasm`.

---

## D21 — Manifest JSON format: `serde_bytes` for base64-encoded binary keys

**Decision:** `ImmutableEngine::export_manifest_json()` uses `serde_bytes` to encode `min_key`/`max_key` as base64 strings in the JSON manifest.

**Rationale:** Segment bounds are binary (`Vec<u8>`). Base64 is the standard JSON-safe encoding. `serde_bytes` provides this automatically without custom serializers.

**Implication:** Manifest consumers (WASM JS, serverless runtimes) receive `min_key`/`max_key` as lowercase hex strings. Decode with `hex::decode()` before use. The format is versioned (`format_version: 1`) for future evolution.

---

## D15 — edgestore-tokio: write uses spawn_blocking, read uses Arc<AsyncReader>

**Decision:** In the `edgestore-tokio` async wrapper crate: write operations (`put`, `delete`, `flush`) use `tokio::task::spawn_blocking` — serialized, correct for the single-writer constraint. Read operations (`get`, `range`, `prefix`) use `Arc<AsyncReader>` with `Arc<RwLock<BlockCache>>` — cache hits return without blocking the async thread; cache misses use `tokio::fs::read`. The `edgestore` core crate stays fully sync with no Tokio dependency.

**Rationale:** `spawn_blocking` for writes is the simplest correct async wrapper for a sync, single-writer system. Async reads avoid blocking the Tokio thread pool on disk I/O for cache misses, improving throughput under concurrent read load.

**Implication:** `edgestore-tokio/Cargo.toml` depends on `edgestore` (path) and `tokio` (with `rt`, `fs` features). The `Engine` core does not gain any `async` methods. `AsyncEngine` in `edgestore-tokio` wraps `Arc<Mutex<Engine>>` for writes and `Arc<Engine>` for reads (using a separate read path). `Arc<RwLock<BlockCache>>` is passed to the async reader so cache hits avoid blocking.

---

## D23 — TieredEngine range/prefix: ephemeral read-through, not local-only

**Decision:** `TieredEngine::range()` and `prefix()` download overlapping archived segments ephemerally and merge results with local data. No segment import, no disk growth. Local data wins on key collision (LWW).

**Rationale:** The previous local-only behavior silently omitted archived keys from scan results, violating user expectations for a tiered store. Ephemeral download avoids permanent local growth while keeping scans correct. `fetch_archived_overlapping()` remains for callers that want explicit warming.

**Implication:** `range()` / `prefix()` stay `&self` (read lock, no write lock needed). Download errors for individual archived segments are logged and skipped; partial results are returned. Both `TieredEngine` and `AsyncTieredEngine` reflect this behavior.

---

## D24 — Text index stripping: Engine::strip_text_index + SegmentMeta flag

**Decision:** `Engine::strip_text_index(segment_id)` rewrites the target segment filtering out all `__text__*` namespace entries and sets `SegmentMeta::text_index_stripped = true`. `TieredEngine::with_text_stripping(true)` auto-strips after each successful `archive_segments()` upload. `SegmentMeta::text_index_stripped` uses `#[serde(default)]` for backward compat.

**Rationale:** Text index entries commingle with user records in ordinary segments. Once a segment is archived, there is no mechanism to GC the index weight from local storage without a rewrite. Auto-stripping after archive is the lifecycle hook tiered deployments need.

**Implication:** Stripped segments cannot contribute to `rebuild_text_indices()` after a crash-recovery cycle. Tiered deployments that need crash-safe text index reconstruction should NOT enable text stripping, or must re-index from source data after recovery.

---

## D25 — Publish order: edgestore-tier was missing from Makefile CRATES

**Decision:** Correct publish order is `edgestore edgestore-repl edgestore-tier edgestore-tokio edgestore-cli`. `edgestore-tier` was missing from the Makefile `CRATES` list prior to v1.1.4, causing `make publish` to skip it silently.

**Rationale:** `edgestore-tier` depends on `edgestore` and `edgestore-repl`; `edgestore-tokio` and `edgestore-cli` depend on `edgestore-tier`. Publish order must be topological.

**Implication:** Fixed in Makefile line 18. Use `make tag && make tags-push && make publish` for all future releases.

---

## D26 — Drop impl: WAL fsync only, no flush_to_segments

**Decision:** `Engine::Drop` calls `persist_text_indices()` then `wal.fsync()` only. No `flush_to_segments()` on drop.

**Rationale:** `flush_to_segments` can fail silently on drop and may discard partial data. WAL replay covers recovery on next open. Caller decides when to segment-flush.

**Implication:** Last memtable writes since the previous flush are recoverable via WAL replay, not via segment files, after a non-graceful shutdown. `wal.fsync()` ensures they survive a process exit.

---

## D27 — vector_count returns Option<u64>, never triggers scan

**Decision:** `Engine::vector_count(ns: &[u8]) -> Option<u64>`. Returns `None` if HNSW index not in memory; `Some(n)` from `self.vector_indices.get(ns).map(|idx| idx.nodes.len() as u64)`. Never triggers a prefix scan or disk read.

**Rationale:** `None` honestly signals "not loaded" vs `Some(0)` which is ambiguous. Avoids the O(n) prefix scan and external `AtomicU64` that resets on restart. Caller loads index with `build_vector_index` or `preload_vector_index` when count is needed.

**Implication:** Callers must call `build_vector_index` or `preload_vector_index` before `vector_count` returns `Some`. `vector_count` is a cheap in-memory read once the index is loaded.

---

## D28 — on_segment_flushed callback: Engine field, Fn(&SegmentMeta) + Send + Sync, all paths

**Decision:** `Engine` holds `on_segment_flushed: Option<Box<dyn Fn(&SegmentMeta) + Send + Sync>>`. Set via builder method `Engine::with_on_segment_flushed(cb)`. Fires synchronously inside `flush_to_segments_inner` after manifest update, on all paths (explicit call and auto-flush).

**Rationale:** Callback on `Engine` (not `EdgestoreConfig`) keeps config as plain data. `Fn(&SegmentMeta)` lets callers inspect the flushed segment (size, id) for decisions. Firing on all `flush_to_segments_inner` paths ensures no flush is silently missed. Synchronous keeps it simple — caller must not block.

**Implication:** Callback runs on the calling thread. Must be fast (set an atomic, send on a channel). Long-running work inside the callback will block the flush caller. `ReplicatedEngine` and `AsyncTieredEngine::flush_notify` both wire to this callback.

---

## D29 — EdgestoreConfig::readonly + Engine::open_readonly + EdgestoreError::ReadOnly

**Decision:** `EdgestoreConfig` gains `readonly: bool` (default `false`). `Engine::open_readonly(config)` sets `config.readonly = true` before calling `Engine::open`. All write paths (`put_inner`, `put_with_ttl_inner`, `delete_inner`) check the flag and return `Err(EdgestoreError::ReadOnly)` immediately. `ReadOnly` is a new `EdgestoreError` variant with display `"write attempted on a read-only engine"`.

**Rationale:** Runtime error (not compile-time type) chosen to keep surprises minimal and at runtime — user confirmed this. No separate `ReadOnlyEngine` type avoids API surface explosion. All write guards are in the innermost shared paths so `vector_put` and `index_text` are covered without separate guards.

**Implication:** `Engine::open_readonly` is the intended API. `EdgestoreConfig::readonly` is public for direct use in `ReplicatedEngine::open_replica`. Read operations are unaffected.

---

## D30 — ReplicatedEngine in edgestore-repl; replica uses open_readonly internally

**Decision:** `ReplicatedEngine` lives in `edgestore-repl` (not core). `open_primary(config, bind_addr)` opens a writable `Engine` + starts `HttpReplicationServer`. `open_replica(config, primary_url)` calls `Engine::open_readonly(config)` internally + starts `AntiEntropyLoop`. Exposes `engine() -> Arc<Mutex<Engine>>` and `bound_port() -> Option<u16>`.

**Rationale:** Core (`edgestore`) stays dep-free and sync. Network wiring belongs in `edgestore-repl`. `open_readonly` on replicas prevents write divergence at the API level without extra documentation burden.

**Implication:** Pierre and other tiered users should NOT use `ReplicatedEngine` — they should wire `TieredEngine` + `HttpReplicationServer` / `AntiEntropyLoop` directly (see D32).

---

## D31 — Production examples: production_patterns.rs + replicated_engine.rs

**Decision:** Two example files added. `edgestore/examples/production_patterns.rs`: runnable, covers flush callback, `vector_count`, and `open_readonly` guard. `edgestore-repl/examples/replicated_engine.rs`: integration demo of `ReplicatedEngine::open_primary` + `open_replica` with in-process HTTP server. Generic (not Vectoria-specific).

**Rationale:** Covers all v1.3.0 patterns from the Vectoria feedback in runnable form. `production_patterns.rs` is the "what patterns to use" doc; `replicated_engine.rs` is the "how to wire replication" doc.

**Implication:** `edgestore-repl/examples/` directory created (was absent). Both committed in `1ef1c8d`.

---

## D32 — Tiered replication pattern: TieredEngine on both nodes, no ReplicatedTieredEngine

**Decision:** For tiered (S3-backed) deployments with replication, use `TieredEngine` on both primary and replica. Primary: `TieredEngine` + `HttpReplicationServer` (serves hot local segments). Replica: `TieredEngine` (own S3 connection, same bucket/prefix) + `AntiEntropyLoop` (pulls hot segments from primary). No `ReplicatedTieredEngine` wrapper type.

**Rationale:** Cold/archived data lives in shared S3 — both nodes read it independently. Replication covers only the hot window (pre-prune segments). Adding a `ReplicatedTieredEngine` would be premature abstraction over two independently composable concerns.

**Implication:** `ReplicatedEngine` is for hot-data-only deployments (no S3). Tiered users wire `TieredEngine` + replication components directly. Document this pattern; do not add new types. Source: Pierre (log processing service) feedback 2026-07-07.

---

## D33 — Prune race in tiered replication: not a bug under shared S3 pattern

**Decision:** The apparent race — primary prunes a local segment before replica syncs it via `HttpReplicationServer` — is not a correctness bug when both nodes use `TieredEngine` with a shared S3 remote. Pruned segments exist in S3; replica's `TieredEngine` serves them via read-through without needing the primary's replication path.

**Rationale:** `HttpReplicationServer` serves from the primary's local manifest intentionally (hot data only). Cold data ownership is S3, not the primary node. Pierre confirmed shared S3 is acceptable.

**Implication:** No changes to `HttpReplicationServer`. No segment-prune fencing or replica registration needed. Caller must ensure replica `TieredEngine` points to the same S3 bucket/prefix as primary. Source: Pierre feedback 2026-07-07.

---

## D34 — Async vector search: HNSW fast path via spawn_blocking + write lock; flat scan via cooperative VectorPage chunks

**Decision:** `edgestore-tokio::AsyncEngine::vector_search` uses two paths:
1. **HNSW fast path** — when `vector_count(ns)` returns `Some`, a single `tokio::task::spawn_blocking` with `blocking_write()` handles the search. `get_vector_index` may mutate engine state (lazy sidecar load), so write lock is correct. HNSW completes in <5 ms; a single blocking window is acceptable.
2. **Flat scan path** — when no HNSW index is loaded, the scan uses `Engine::vector_page` (takes `&self`, read lock only) in a loop. Each page is fetched under a short `spawn_blocking` + read lock; the lock is released before distance computation. `tokio::task::yield_now()` between pages yields to the scheduler. No extra dependencies (no rayon).

`Engine::vector_page(ns, cursor, page_size)` is a new `&self` method added to core. It uses `range_budgeted` internally and returns `VectorPage { records, next_key }`. `next_key` is the last key in the page; callers pass it as the cursor for the next page (internally advanced by appending `\x00` to skip the cursor key). This API is also usable directly by callers that want streaming access to vector data without loading all records into memory.

**Rationale:** The core constraint ("no async in edgestore crate") rules out native async vector ops. Rayon would parallelize the flat scan but adds a dependency and a second thread pool; for PAGE_SIZE=512 at 128 dims the per-page distance computation is ~100μs — acceptable on the async thread. The HNSW / flat-scan split avoids regressing the common production case (HNSW loaded). `vector_page` as a `&self` read-only primitive is reusable for streaming exports, agent loops, and other non-search use cases.

**Implication:** `VectorPage` is exported from `edgestore::vector::search` and re-exported from `edgestore`. Callers who load an HNSW index before querying see no change in behavior. Callers without an HNSW index on large collections (>512 vectors) now cooperate with the async scheduler instead of holding the write lock for the full flat-scan duration.

---

## D35 — DeferredChunkAppend-inspired scan: streaming cursors, budget propagation, cursor pagination

**Decision:** Three structural changes to the range scan stack, plus two new APIs:

1. **P1 — Store-level metadata pruning**: `SegmentCursor::open` compares the query range `[start, end)` against `reader.meta.min_key/max_key` before opening any file. Non-overlapping segments produce `None` and are skipped entirely.

2. **P2 — Budget-aware K-way merge**: `SegmentStore::range_scan_budgeted` returns `(Vec<MemEntry>, bool)` where the bool signals that the merge was stopped early by `max_items`. `range_core` and `prefix_core` initialise `truncated = seg_truncated` before their own budget loop so the flag is correctly propagated when the segment scan is exactly consumed by the merge.

3. **P3 — Streaming block reads (`SegmentCursor`)**: Replaces the pre-load-all approach. `SegmentCursor` holds an open `File` handle and reads one 4 KiB block at a time via `read_block_at_offset` (extracted free function, shared with `SegmentReader::read_block_at`). The K-way merge seeds from each cursor's first entry and advances per-cursor lazily. For `max_items=10` against 100 segments, I/O stops after the first ~10 live entries are collected — not after all 100 files are fully read.

4. **P4 — `range_page` cursor API**: `Engine::range_page(ns, start, end, cursor, page_size) -> Result<RangePage>`. Mirrors `vector_page`. Cursor = last returned key + `\x00` to advance exclusive start. `next_key = None` signals exhaustion. Async wrapper in `edgestore-tokio::AsyncEngine`. Re-exported from `edgestore` and `edgestore-tokio` as `RangePage`.

5. **P5 — `range_rev_page` descending scan**: `Engine::range_rev_page(ns, start, end, cursor, page_size) -> Result<RangePage>`. Items returned in descending key order. `next_key` = smallest key returned on this page (the caller passes it as the next `end_cursor`). Implemented via `SegmentStore::range_scan_rev_budgeted` (max-heap K-way merge over per-segment pre-loaded vecs served from tail, stopped by P2) merged with the memtable in descending order. Async wrapper in `edgestore-tokio::AsyncEngine`.

**Rationale:** Maps directly to TimescaleDB's DeferredChunkAppend insight: planning cost (opening all segments) was O(segments) regardless of LIMIT/budget. P1-P3 push the budget to the I/O level. P4-P5 give callers correct cursor-based pagination without loading full ranges. The `(Vec, bool)` return from `range_scan_budgeted` is necessary because `range_core`'s `i < merged.len()` check produces a false negative when the segment budget exactly fills `merged` and the memtable is empty.

**Implication:** `SegmentStore::range_scan` is removed; callers use `range_scan_budgeted(..., None)`. `read_block_at_offset` is now `pub(crate)` and shared between `SegmentReader` and `SegmentCursor`. Existing `range_budgeted` and `prefix_budgeted` semantics are unchanged; `truncated = true` is now correctly set when segments are budget-truncated even if the merged slice is fully consumed.

---

## D36 — Concurrent vector search via interior RwLock on HNSW cache (v1.8.0)

**Question:** `Engine::vector_search` and `vector_search_with_stats` took `&mut self` because `get_vector_index` inserts into `self.vector_indices: HashMap<Vec<u8>, HnswIndex>` on a cache miss. This forced external callers (including `Arc<Mutex<Engine>>` wrappers) to serialize all vector searches, making concurrent BM25 + vector queries impossible and blocking reads for the full duration of a `build_vector_index` call.

**Decision:** Change `vector_indices` to `std::sync::RwLock<HashMap<Vec<u8>, Arc<HnswIndex>>>` (interior mutability). `get_vector_index` now takes `&self`, acquires a read lock on cache hit, and upgrades to a write lock only on a cache miss (double-checked locking pattern). Returns `Arc<HnswIndex>` so the caller can hold the index after the lock is released.

**Consequence:**
- `vector_search`, `vector_search_with_stats`, `preload_vector_index`, `try_hnsw_search` all take `&self`.
- `build_vector_index` keeps `&mut self` — it is a write operation on WAL-backed data.
- `AsyncEngine::vector_search` drops the read-then-write-lock dance; both HNSW and flat-scan paths now use `blocking_read` throughout. A new `Engine::try_hnsw_search(&self, ...) -> Option<Vec<...>>` method is the boundary between the two strategies.
- Downstream wrappers using `Arc<Mutex<Engine>>` can switch to `Arc<RwLock<Engine>>` — all search methods are now compatible with shared read access.
