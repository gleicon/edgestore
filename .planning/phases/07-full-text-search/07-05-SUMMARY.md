# Plan 07-05 Summary — Integration Tests + Benchmarks

## What Was Delivered

- `edgestore/tests/phase7_integration.rs`: 8 integration tests
- `edgestore/benches/text_search.rs`: 2 Criterion benchmarks
- `edgestore/Cargo.toml`: bench entry for `text_search`

## Integration Tests

- `test_index_and_search_basic`: 3 docs, search "quick brown" finds doc1 and doc3
- `test_bm25_ranking`: doc with double term occurrence ranks higher
- `test_search_empty_namespace`: no indexed docs → empty results
- `test_search_empty_query`: empty/stopwords query → empty results
- `test_delete_removes_from_search`: delete then search excludes doc
- `test_facet_filter`: facet filtering narrows results correctly
- `test_search_ranking_stability`: identical queries → identical ordering
- `test_index_text_record_retrieval`: raw text + facets recoverable via KV get

## Benchmarks

- `bench_text_search`: index 100/1K/10K docs, measure search QPS for "quick brown fox"
- `bench_index_throughput`: measure docs/sec indexing rate (100 docs per batch)

## Files Changed

- `edgestore/tests/phase7_integration.rs` (new)
- `edgestore/benches/text_search.rs` (new)
- `edgestore/Cargo.toml` (bench entry)

## Verification

- cargo build --workspace: ✅
- cargo clippy --workspace -D warnings: ✅
- cargo test --workspace: 421 tests pass (14 suites)
- cargo bench --no-run: 4 benchmarks compile (vector_search, hnsw_recall, throughput, text_search)
