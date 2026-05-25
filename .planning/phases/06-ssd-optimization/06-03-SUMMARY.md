# Plan 06-03 Summary — HNSW Core

## What Was Delivered

- `HnswIndex` struct: nodes, entry_point, max_layer, M, ef_construction, dims, dtype, metric
- `HnswNode` struct: vector_id, vector_data, neighbors per layer
- Probabilistic layer assignment: `layer = floor(-ln(uniform) / ln(M))`
- Greedy beam search: `search_layer` with min-heap candidates + max-heap results
- Diversity heuristic neighbor selection (Algorithm 2 from HNSW paper)
- Bidirectional edge connection with pruning
- Binary serialization format with magic `HNSW` header
- `serialize()` → `Vec<u8>` and `deserialize()` → `HnswIndex`

## Files Changed

- `edgestore/src/vector/hnsw.rs` (new)
- `edgestore/src/vector/mod.rs` (module declaration)

## Tests

- `test_hnsw_insert_and_search_single`: self-search returns distance ≈ 0
- `test_hnsw_search_empty`: empty index returns empty results
- `test_hnsw_recall_vs_brute_force`: clustered 500 vectors, recall ≥ 0.70
- `test_hnsw_self_search`: 100 vectors, self-search distance < 1e-5
- `test_hnsw_serialize_roundtrip`: search results identical before/after serialize
- `test_hnsw_deserialize_invalid_magic`: rejects bad magic
- `test_hnsw_deserialize_too_short`: rejects truncated data
- `test_hnsw_linear_data`: 1D line search finds correct neighbors

## Verification

- cargo build --workspace: ✅
- cargo clippy -D warnings: ✅
- cargo test --workspace: 200 tests pass
