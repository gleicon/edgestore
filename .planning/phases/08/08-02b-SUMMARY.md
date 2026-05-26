---
phase: "08"
plan: "08-02b"
subsystem: "docs-examples-licenses"
tags: ["benchmarks", "examples", "licensing", "documentation"]
dependency_graph:
  requires: []
  provides: ["POLISH-03", "POLISH-07"]
  affects: []
tech-stack:
  added: []
  patterns: []
key-files:
  created:
    - BENCHMARKS.md
    - LICENSE-MIT
    - LICENSE-APACHE
    - edgestore/examples/basic_kv.rs
    - edgestore/examples/vector_search.rs
    - edgestore/examples/replication.rs
  modified:
    - edgestore/Cargo.toml
---

# Phase 8 Plan 08-02b: Benchmarks, Examples, and Licenses Summary

**One-liner:** Created benchmark documentation, three runnable API examples, and dual MIT/Apache-2.0 license files.

## What Was Built

### BENCHMARKS.md (repo root)
- Documents all 4 in-repo benchmark binaries from `edgestore/Cargo.toml`
- Describes Criterion.rs methodology and hardware/environment requirements
- Includes placeholder results tables for all required measurements:
  - Write throughput (sequential/random puts, batch transactions)
  - Read throughput (point gets, range scans, prefix scans)
  - Vector search latency (flat scan vs HNSW, 10K/100K/500K vectors)
  - HNSW recall vs latency tradeoff
  - Text search QPS
  - Compaction WAF measurements
- Instructions for running benchmarks and interpreting results

### Examples (`edgestore/examples/`)
- **basic_kv.rs**: Opens DB, puts keys across 3 namespaces, demonstrates get, range scan, prefix scan, delete, and flush. Cleans up on exit.
- **vector_search.rs**: Inserts 1000 random 32-dim f32 vectors, builds HNSW index, performs ANN search, and prints top-5 results with distances.
- **replication.rs**: Sets up primary and replica engines, writes 100 keys, flushes to segments, exports manifest from primary, imports segments into replica, and verifies sync via merkle root comparison.

### License Files (repo root)
- **LICENSE-MIT**: Standard MIT license, Copyright 2026 EdgeStore Contributors
- **LICENSE-APACHE**: Full Apache-2.0 license text
- `edgestore/Cargo.toml` updated with `license = "MIT OR Apache-2.0"`

## Deviations from Plan

### Location Adjustment
- **Found during:** Task 2 planning
- **Issue:** Plan said "examples/ directory at repo root", but for a Cargo workspace `cargo build --examples` and `cargo run --example <name>` only work when examples are inside a workspace member crate.
- **Fix:** Created examples in `edgestore/examples/` alongside existing `demo.rs`. This is the canonical Rust workspace location and satisfies the acceptance criterion `cargo build --examples passes`.
- **Files created:** `edgestore/examples/basic_kv.rs`, `edgestore/examples/vector_search.rs`, `edgestore/examples/replication.rs`

## Acceptance Criteria Verification

| Criterion | Status | Evidence |
|-----------|--------|----------|
| BENCHMARKS.md exists at repo root | ✅ | `/Users/gleicon/code/markdown/edgestore/BENCHMARKS.md` |
| Documents all benchmark binaries in Cargo.toml | ✅ | Lists vector_search, hnsw_recall, throughput, text_search |
| Contains tables for all required measurements | ✅ | 6 measurement categories with placeholder ranges |
| Instructions for running each benchmark | ✅ | `cargo bench --bench <name>` commands documented |
| Hardware/environment section | ✅ | CPU, RAM, SSD, OS, Rust version specified |
| Placeholder results | ✅ | Expected ranges for v1.0; actual numbers TBD after hardware run |
| examples/ directory exists | ✅ | `edgestore/examples/` (canonical Cargo location) |
| basic_kv.rs compiles and runs | ✅ | `cargo run --example basic_kv` → output verified |
| vector_search.rs compiles and runs | ✅ | `cargo run --example vector_search` → top-5 results printed |
| replication.rs compiles and runs | ✅ | `cargo run --example replication` → sync verified |
| All examples have doc comments | ✅ | Each file starts with `//!` module doc comment |
| `cargo build --examples` passes | ✅ | Builds successfully (debug) |
| LICENSE-MIT exists with MIT text | ✅ | `/Users/gleicon/code/markdown/edgestore/LICENSE-MIT` |
| LICENSE-APACHE exists with Apache-2.0 text | ✅ | `/Users/gleicon/code/markdown/edgestore/LICENSE-APACHE` |
| Copyright year is 2026 | ✅ | Both files state "2026 EdgeStore Contributors" |
| License files are complete | ✅ | Full standard texts, not truncated |
| Cargo.toml references licenses | ✅ | `license = "MIT OR Apache-2.0"` added |

## Commits

| Hash | Type | Description |
|------|------|-------------|
| 0981f2e | docs | BENCHMARKS.md with benchmark suite documentation |
| 0fd7011 | feat | Three runnable examples (basic_kv, vector_search, replication) |
| c3f00e3 | chore | Dual MIT/Apache-2.0 license files + Cargo.toml update |

## Metrics

- **Duration:** ~5 minutes
- **Tasks completed:** 3/3
- **Files created:** 6
- **Files modified:** 1

## Self-Check: PASSED

All created files exist, all examples compile and run, all acceptance criteria met.
