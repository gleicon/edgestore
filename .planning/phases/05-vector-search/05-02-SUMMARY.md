---
phase: 5
plan: "02"
subsystem: vector
tags: [vector, api, kv-overlay, synthetic-namespace]
dependency_graph:
  requires:
    - edgestore::Dtype
    - edgestore::VectorRecord
    - edgestore::encode_vector_record
    - edgestore::decode_vector_record
  provides:
    - edgestore::VectorEngine
    - edgestore::vector_namespace
    - Engine::vector_put
    - Engine::vector_get
    - Engine::vector_delete
  affects:
    - edgestore/src/engine.rs
    - edgestore/src/vector/mod.rs
    - edgestore/src/lib.rs
tech_stack:
  added: []
  patterns:
    - Trait overlay on Engine (no changes to KV core)
    - Synthetic namespace prefix `__vec__` for isolation (D09)
key_files:
  created:
    - edgestore/src/vector/api.rs
  modified:
    - edgestore/src/engine.rs
    - edgestore/src/vector/mod.rs
    - edgestore/src/lib.rs
decisions:
  - "VectorEngine is a trait so future wrappers (e.g. async) can implement it"
  - "Synthetic namespace is hardcoded __vec__ prefix; not configurable"
metrics:
  duration: "~10 minutes"
  completed: "2026-05-23"
  tasks_completed: 3
  tasks_total: 3
  files_created: 1
  files_modified: 3
---

# Phase 5 Plan 02: Vector KV API Summary

## One-liner

VectorEngine trait with vector_put, vector_get, vector_delete — thin overlays on the existing KV API using synthetic `__vec__{ns}` namespace isolation.

## What Was Built

**`edgestore/src/vector/api.rs`** — VectorEngine trait and implementation.

- `VectorEngine` trait (object-safe, no generics):
  - `vector_put(ns, key, dims, dtype, data)` → validates dims → encodes → `put(__vec__{ns}, key, encoded)`
  - `vector_get(ns, key)` → `get(__vec__{ns}, key)` → decodes → `Option<VectorRecord>`
  - `vector_delete(ns, key)` → `delete(__vec__{ns}, key)`
- `vector_namespace(ns)` helper: prepends `__vec__` to user namespace bytes

**`edgestore/src/engine.rs`** — `impl VectorEngine for Engine`.

- All three methods implemented as thin wrappers around existing put/get/delete
- No new state, no new locks — purely additive

**`edgestore/src/vector/mod.rs`** — Declared `pub mod api` and re-exported `VectorEngine`, `vector_namespace`.

**`edgestore/src/lib.rs`** — Re-exported `VectorEngine` and `vector_namespace`.

## Verification

```
cargo test -p edgestore     → 155 passed (6 suites)
cargo build --workspace     → exits 0
cargo clippy -p edgestore -- -D warnings → clean
```

## Deviations from Plan

None.

## Threat Model Coverage

- **T-05-02 (DoS/vector_put)**: DimensionMismatch check prevents using `dims` as an allocation multiplier.

## Commits

| Task | Hash | Description |
|------|------|-------------|
| All  | (HEAD) | feat(05-02): Vector KV API — vector_put, vector_get, vector_delete |

## Self-Check: PASSED

- [x] VectorEngine trait exists with 3 methods
- [x] Engine implements VectorEngine
- [x] vector_put validates dimension match
- [x] vector_get round-trips all 3 dtypes
- [x] vector_delete removes record
- [x] vector_namespace isolates vector data from plain KV
- [x] 7 vector API tests pass
- [x] cargo build --workspace exits 0
- [x] cargo clippy -D warnings clean
