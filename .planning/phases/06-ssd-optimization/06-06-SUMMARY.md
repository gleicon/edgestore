# Plan 06-06 Summary — Benchmarks + Integration Tests

## What Was Delivered

- `edgestore/benches/hnsw_recall.rs`: recall@10 measurement for HNSW vs brute-force reference on clustered data (500–5000 vectors)
- `edgestore/benches/throughput.rs`: ops/sec for put (1000), get (1000 hot), vector_put (100), vector_search flat (1000), vector_search HNSW (1000)
- `edgestore/benches/vector_search.rs`: flat scan vs HNSW latency comparison
- `edgestore/tests/phase6_integration.rs`: 8 integration tests covering all Phase 6 deliverables

## Files Changed

- `edgestore/benches/hnsw_recall.rs` (new)
- `edgestore/benches/throughput.rs` (new)
- `edgestore/benches/vector_search.rs` (updated)
- `edgestore/tests/phase6_integration.rs` (new)
- `edgestore/Cargo.toml` (bench entries)

## Integration Tests

- `test_hnsw_build_and_search`: build index + search with HNSW
- `test_hnsw_survives_restart`: sidecar persistence across engine restarts
- `test_hnsw_falls_back_to_flat_scan`: no index → flat scan fallback
- `test_hnsw_metrics_tracked`: metrics after reload + search
- `test_hnsw_preload_vector_index`: explicit preload API
- `test_storage_backend_trait`: Default + Memory backend round-trip
- `test_fdp_disabled_by_default`: config field default
- `test_fdp_mock_records_hint`: MockFdpBackend verification

## Verification

- cargo build --workspace: ✅
- cargo clippy --workspace -D warnings: ✅
- cargo test --workspace: 214 tests pass (13 suites)
- cargo bench --no-run: all 3 benchmarks compile
