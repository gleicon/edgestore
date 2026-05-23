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

## D15 — edgestore-tokio: write uses spawn_blocking, read uses Arc<AsyncReader>

**Decision:** In the `edgestore-tokio` async wrapper crate: write operations (`put`, `delete`, `flush`) use `tokio::task::spawn_blocking` — serialized, correct for the single-writer constraint. Read operations (`get`, `range`, `prefix`) use `Arc<AsyncReader>` with `Arc<RwLock<BlockCache>>` — cache hits return without blocking the async thread; cache misses use `tokio::fs::read`. The `edgestore` core crate stays fully sync with no Tokio dependency.

**Rationale:** `spawn_blocking` for writes is the simplest correct async wrapper for a sync, single-writer system. Async reads avoid blocking the Tokio thread pool on disk I/O for cache misses, improving throughput under concurrent read load.

**Implication:** `edgestore-tokio/Cargo.toml` depends on `edgestore` (path) and `tokio` (with `rt`, `fs` features). The `Engine` core does not gain any `async` methods. `AsyncEngine` in `edgestore-tokio` wraps `Arc<Mutex<Engine>>` for writes and `Arc<Engine>` for reads (using a separate read path). `Arc<RwLock<BlockCache>>` is passed to the async reader so cache hits avoid blocking.
