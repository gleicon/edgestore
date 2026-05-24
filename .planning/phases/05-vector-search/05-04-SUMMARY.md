---
phase: 5
plan: "04"
subsystem: vector
tags: [vector, search, flat-scan, top-k, brute-force, ann]
dependency_graph:
  requires:
    - edgestore::VectorEngine
    - edgestore::distance
    - edgestore::Metric
    - edgestore::VectorRecord
  provides:
    - edgestore::vector_search
    - edgestore::VectorSearchResult
  affects:
    - edgestore/src/engine.rs
    - edgestore/src/vector/mod.rs
    - edgestore/src/lib.rs
tech_stack:
  added: []
  patterns:
    - BinaryHeap max-heap for top-k (reverse Ord so peek = worst of k)
    - Brute-force flat scan over synthetic namespace
    - Dimension/dtype validation with skip-on-mismatch
key_files:
  created:
    - edgestore/src/vector/search.rs
  modified:
    - edgestore/src/engine.rs
    - edgestore/src/vector/mod.rs
    - edgestore/src/lib.rs
decisions:
  - "Skip mismatched records rather than error — allows mixed-dtype collections"
  - "k=0 returns empty vec immediately"
metrics:
  duration: "~12 minutes"
  completed: "2026-05-23"
  tasks_completed: 3
  tasks_total: 3
  files_created: 1
  files_modified: 3
---

# Phase 5 Plan 04: Flat Scan Search — Brute-Force ANN with Top-K Summary

## One-liner

Brute-force flat scan over all vector records in a namespace, maintaining top-k closest results in a max-heap ordered by distance.

## What Was Built

**`edgestore/src/vector/search.rs`** — Search implementation.

- `VectorSearchResult { key: Vec<u8>, distance: f32 }` — search output type
- `HeapItem` wrapper with reverse `Ord` so `BinaryHeap::peek()` returns the worst (largest distance) of the current k items
- `vector_search(engine, ns, query, k, metric)`:
  1. Builds synthetic namespace `__vec__{ns}`
  2. Scans all records via `Engine::range` with a wide key range
  3. For each record: decodes VectorRecord, validates dims/dtype match query
  4. Computes distance using SIMD (f32) or scalar (f16/i8) path
  5. Maintains top-k in a max-heap of size k: if heap < k, push; else if new < peek, pop+push
  6. Extracts results, sorts by ascending distance, returns
- `k = 0` returns empty vec immediately
- Mismatched dims/dtype records are skipped (not errored)

**`edgestore/src/engine.rs`** — Added `Engine::vector_search(&self, ns, query, k, metric)` public method.

**`edgestore/src/vector/mod.rs`** — Declared `pub mod search` and re-exported.

**`edgestore/src/lib.rs`** — Re-exported `vector_search` and `VectorSearchResult`.

## Verification

```
cargo test -p edgestore     → 173 passed (6 suites)
cargo build --workspace     → exits 0
cargo clippy -p edgestore -- -D warnings → clean
```

## Deviations from Plan

None.

## Threat Model Coverage

- **T-05-04 (DoS/huge k)**: k is caller-controlled; the heap is bounded to k elements. Memory usage is O(k) regardless of collection size.

## Commits

| Task | Hash | Description |
|------|------|-------------|
| All  | (HEAD) | feat(05-04): flat scan search with top-k heap |

## Self-Check: PASSED

- [x] vector_search scans all records in synthetic namespace
- [x] Top-k selection returns exactly k results (or fewer if collection is smaller)
- [x] Results ordered by ascending distance
- [x] Delete tombstones excluded from search
- [x] Dimension/dtype mismatches skipped gracefully
- [x] All 3 metrics (Cosine, L2, DotProduct) produce correct ordering
- [x] 8 search tests pass
- [x] cargo build --workspace exits 0
- [x] cargo clippy -D warnings clean
