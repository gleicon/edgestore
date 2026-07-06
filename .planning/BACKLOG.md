# EdgeStore Backlog

Deferred features and future enhancements captured during development.

---

## Full-Text Search Enhancements (Phase 7 v2)

### FT-01: Pluggable Tokenizer Architecture

**Status:** Deferred from Phase 7 v1
**Rationale:** Hugging Face `tokenizers` crate adds ~100+ transitive dependencies. v1 uses lightweight English tokenizer to maintain minimal dependency footprint.

**Design:** Add a `Tokenizer` trait that the Engine accepts at initialization:
```rust
pub trait Tokenizer: Send + Sync {
    fn tokenize(&self, text: &str) -> Vec<Token>;
}
```
- Default: `EnglishTokenizer` (current simple implementation)
- Future: `HfTokenizer` wrapping `tokenizers` crate
- Config: accept `.json` tokenizer config path in `EdgestoreConfig`

---

### FT-02: LLM-Compatible Tokenization

**Status:** Deferred from Phase 7 v1
**Rationale:** Requires external `tokenizers` crate (see FT-01).

**Design:**
- Support BPE (GPT-2/3/4, Llama), WordPiece (BERT), Unigram (T5, XLNet)
- Load vocabulary from `.json` config file at Engine initialization
- Token IDs map to compact `u32` internal IDs for storage efficiency
- Enables exact same tokenization as the LLM used for embeddings

---

### FT-03: Bigram/Shingle Inverted Index

**Status:** Deferred from Phase 7 v1
**Rationale:** Adds vocabulary dictionary layer and more complex key encoding. v1 uses direct `term → postings` mapping.

**Design:**
1. **Vocabulary Index:** `vbf:<token_id_1>:<token_id_2>` → `shingle_id: u32`
   - Maps token bigrams to compact internal IDs
   - Saves space in main inverted index
2. **Inverted Index:** `inv:<shingle_id>:<doc_id>` → `term_freq: u32`
   - Uses native prefix scan for fast lookup
3. **Document Index:** `doc:<doc_id>` → `{length, raw_text, embedding}`

**Benefits:**
- Captures phrase-like context (2-gram co-occurrence)
- Better precision for multi-word queries
- Shared vocabulary across namespaces

---

### FT-04: RAG-Optimized Chunk-Level Indexing

**Status:** Deferred from Phase 7 v1
**Rationale:** Requires design decision on chunking strategy (fixed-size vs semantic). Can be implemented as a layer on top of existing text indexing.

**Design:**
- Treat each text chunk (100–500 tokens) as independent `doc_id`
- Store chunk text + embedding vector in same KV record
- BM25 scores measure keyword density within the exact LLM context window
- Combine with HNSW vector search for hybrid lexical+semantic retrieval

---

### FT-05: Multi-Language Tokenization

**Status:** Deferred indefinitely
**Rationale:** Internationalization is a long tail. English-first scope per Phase 7 plan.

**Design:**
- Unicode-aware tokenization (ICU or `unicode-segmentation`)
- Language-specific stemmers (Snowball stemmers)
- CJK bigram/character indexing

---

## Memory & Resource Management

### MEM-01: Configurable memtable flush threshold

**Status:** Deferred — 2026-07-06
**Rationale:** `segment_size_bytes` controls segment target size but there is no independent `memtable_max_bytes` ceiling. On constrained edge/IoT hardware, the memtable can grow beyond acceptable RAM limits before a flush is triggered.

**Design:** Add `memtable_max_bytes: usize` to `EdgestoreConfig`. Default: 8 MB. When memtable exceeds this threshold, auto-trigger `flush_to_segments()` before accepting the next write. Document the relationship between `memtable_max_bytes` and `segment_size_bytes`.

---

### MEM-02: Block cache default reduction + documentation

**Status:** Deferred — 2026-07-06
**Rationale:** Default `block_cache_bytes = 64 MB` is appropriate for servers but too large for edge/IoT targets. The knob exists but is not prominently documented.

**Design:** Reduce default to 8 MB. Add a "constrained hardware" configuration guide to crate docs showing recommended values for different deployment tiers (server, edge, IoT).

---

### MEM-03: ImmutableEngine reader LRU eviction

**Status:** Deferred — 2026-07-06
**Rationale:** `ImmutableEngine` loads full `.dat` segment bytes into RAM with no eviction. In long-lived processes accumulating readers, memory grows unbounded.

**Design:** Add an LRU policy over loaded `InMemorySegmentReader` instances keyed by segment hash, bounded by total bytes (default: 32 MB). On eviction, re-download from `RemoteStore` on next access (or return error if no remote configured). Cap: `ImmutableEngineConfig.max_resident_bytes`.

---

## Vector Search at Scale

### VEC-01: hnsw_max_ram_bytes guard + flat-scan fallback

**Status:** Deferred — 2026-07-06
**Rationale:** Current HNSW sidecar is loaded fully into RAM. At 100M+ vectors the sidecar exceeds available memory. No guard exists — the load silently OOMs.

**Design:** Add `hnsw_max_ram_bytes: usize` to `EdgestoreConfig` (default: 512 MB). If the sidecar file exceeds the threshold, skip the HNSW load and fall back to flat scan. Log a warning. Document best/worst case O notation:
- Best: HNSW loaded → O(log n) per query
- Worst: flat scan over S segments × V vectors → O(S·V); at 100M vectors × 32 dims with I8 SIMD ≈ 3 seconds per query. Documented ceiling.

---

### VEC-02: Time-windowed vector search via TieredEngine

**Status:** Deferred — 2026-07-06
**Rationale:** Use case: semantic search over logs going back N days (pierre pattern). Hot window fits HNSW; historical range must fall back to per-segment flat scan over archived segments.

**Design:**
- Local segments (hot window, configurable days): `vector_search()` → HNSW O(log n)
- Archived segments (historical range): per-segment flat scan via `ImmutableEngine`, loaded ephemerally; top-k merge across segments
- Document: best case O(log n) hot, worst case O(S·V) cold. Provide example showing time-range + vector search combined query pattern.

---

### VEC-03: Disk-backed ANN index design (future phase)

**Status:** Research deferred — 2026-07-06
**Rationale:** In-RAM HNSW is inadequate at 100M+ vectors. DiskANN / SPANN approaches co-locate graph node data on disk pages so traversal hops stay sequential.

**Design (research required):**
- DiskANN-style: serialize graph nodes with their neighbor vectors co-located on fixed-size pages; beam search reads pages sequentially
- SPANN-style: partition vectors into posting lists anchored to centroid IDs; HNSW per partition; requires offline clustering
- Likely delivered as a separate `edgestore-ann` crate to avoid bloating core

---

### VEC-04: Document I8/SQ8 quantization for vector storage

**Status:** Deferred — 2026-07-06
**Rationale:** `Dtype::I8` already exists in `VectorRecord` and enables scalar quantization. This is undocumented. PQ (Product Quantization) requires a training phase and is deferred indefinitely.

**Design:** Add a "Vector quantization" section to crate docs: show caller-side quantization from `f32` to `i8`, call `vector_put` with `Dtype::I8`. Document the storage/accuracy trade-off. No code change needed.

---

## API & Docs

### API-01: get_into(buf) for large-value workloads

**Status:** Deferred — 2026-07-06
**Rationale:** `get()` always allocates a fresh `Vec<u8>`. For large values in hot loops, callers may want to reuse a buffer. Rejected `rkyv`/`zerocopy` — EdgeStore values are opaque user bytes, there is nothing to zero-copy deserialize on our side.

**Design:** Add `get_into(&self, ns, key, buf: &mut Vec<u8>) -> Result<bool, EdgestoreError>`. Clears and fills `buf` in-place. No new dependencies.

---

### API-02: Parquet export example

**Status:** Deferred — 2026-07-06
**Rationale:** No concrete user yet. Shows how to use `ImmutableEngine` as a bridge from EdgeStore segments to analytics tools (DuckDB, DataFusion). `parquet` crate as dev-dependency only — no production dep.

**Design:** `examples/parquet_export.rs` — caller defines a schema struct, example iterates `ImmutableEngine::range()` over a segment and emits Parquet row groups. Document that EdgeStore does not own the schema; caller provides it.

---

### API-03: Async runtime cookbook

**Status:** Deferred — 2026-07-06
**Rationale:** `edgestore-tokio` already implements `spawn_blocking` patterns for axum/actix-web. Undocumented for first-time users.

**Design:** Add "Using EdgeStore with async runtimes" section to `edgestore-tokio` crate docs: `spawn_blocking` for writes, `blocking_read` for reads, `AsyncTieredEngine` usage pattern. Include axum state example.

---

### API-04: Tiering cookbook — monitor-and-archive pattern

**Status:** Deferred — 2026-07-06
**Rationale:** `TieredEngine` is caller-driven. Users need a reference implementation showing how to monitor local segment count/bytes, decide when to archive, and optionally strip text index.

**Design:** Add "Tiering cookbook" to `edgestore-tier` crate docs (or `docs/tiering.md`): check `local().list_segment_metas().len()` vs threshold, call `archive_segments`, call `strip_text_index` if enabled. Show the monitor loop pattern for long-running services.

---

## Research

### RES-01: Global range-delete tracker (GLORAN-inspired)

**Status:** Research only — 2026-07-06
**Rationale:** No current user-facing range-delete API. Deathtime-cohort TTL expiry handles bulk deletion today. Range-delete tracker (interval tree over deleted key ranges) would let `get()` short-circuit before scanning segments. Worth revisiting if high-throughput bulk-drop workloads emerge.

**Design (sketch):** Compact interval tree in `SegmentStore`; persisted in manifest. `get()` checks interval membership before scanning readers. Write amplification: zero (interval tree is metadata only).

---

## Other Backlog Items

### BK-01: SQLite WAL Mode Comparison

**Status:** Not started
**Note:** Evaluate SQLite's WAL mode for comparison with EdgeStore's append-only WAL design.

### BK-02: LZ4 Compression for WAL

**Status:** Implemented in config but not benchmarked
**Note:** `compression_wal: Compression::Lz4` exists in EdgestoreConfig but baseline benchmarks use uncompressed WAL.

---

## Adding Items to This Backlog

When deferring a feature during phase execution:
1. Add entry with status "Deferred from Phase X"
2. Include rationale for deferral
3. Include design sketch for future implementation
4. Reference this file from the phase SUMMARY.md
