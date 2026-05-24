---
phase: 5
plan: "03"
subsystem: vector
tags: [vector, distance, simd, metrics, cosine, l2, dotproduct]
dependency_graph:
  requires:
    - edgestore::Dtype
    - edgestore::VectorRecord
  provides:
    - edgestore::Metric
    - edgestore::distance
    - edgestore::distance_scalar
    - edgestore::distance_simd_f32
  affects:
    - edgestore/Cargo.toml
    - edgestore/src/vector/mod.rs
    - edgestore/src/lib.rs
tech_stack:
  added:
    - wide = "0.7" (portable SIMD on stable Rust)
  patterns:
    - SIMD path for f32 via wide::f32x8
    - Scalar fallback for f16/i8 (widen to f32)
    - SIMD-scalar parity verification
    - Negated dot product so lower = better (consistent with min-heap)
key_files:
  created:
    - edgestore/src/vector/distance.rs
  modified:
    - edgestore/Cargo.toml
    - edgestore/src/vector/mod.rs
    - edgestore/src/lib.rs
decisions:
  - "SIMD only for f32; f16/i8 always scalar (widen to f32) — simpler and correct"
  - "No unsafe code; wide crate abstracts all platform intrinsics"
  - "Manual f16→f32 conversion instead of half crate (no new dep for raw bytes)"
metrics:
  duration: "~15 minutes"
  completed: "2026-05-23"
  tasks_completed: 4
  tasks_total: 4
  files_created: 1
  files_modified: 3
---

# Phase 5 Plan 03: Distance Metrics — SIMD + Scalar Summary

## One-liner

All three distance metrics (Cosine, L2, DotProduct) with SIMD-accelerated f32 path via `wide` crate and scalar fallback for f16/i8.

## What Was Built

**`edgestore/src/vector/distance.rs`** — Distance computation layer.

- `Metric` enum: `Cosine`, `L2`, `DotProduct`
- `distance_scalar(query, candidate, metric)` — reference scalar implementation:
  - Cosine: `1 - dot / (|q| * |c|)` — range [0, 2], 0 = identical
  - L2: `sqrt(sum((q_i - c_i)^2))` — true euclidean distance
  - DotProduct: `-dot` — negated so lower = better (min-heap compatible)
- `distance_simd_f32(query, candidate, metric)` — SIMD path:
  - Uses `wide::f32x8` for 8-wide lanes on x86_64
  - Processes 8 elements at a time, scalar tail for remaining
  - Falls back to `distance_scalar` on non-x86_64
- `distance(bytes_a, bytes_b, dtype, metric)` — public API:
  - Decodes raw bytes to `Vec<f32>` based on dtype
  - f32: direct little-endian decode → SIMD path
  - f16: manual `f16_to_f32` widening → scalar path
  - i8: cast to f32 → scalar path
- `decode_to_f32()` — dtype-aware byte-to-f32 decoder
- `f16_to_f32()` — manual IEEE 754 half→single conversion

**`edgestore/Cargo.toml`** — Added `wide = "0.7"` dependency.

**`edgestore/src/vector/mod.rs`** — Declared `pub mod distance` and re-exported.

**`edgestore/src/lib.rs`** — Re-exported `Metric` and `distance`.

## Verification

```
cargo test -p edgestore     → 164 passed (6 suites)
cargo build --workspace     → exits 0
cargo clippy -p edgestore -- -D warnings → clean
```

## Deviations from Plan

- Replaced `rand` dependency in parity test with deterministic LCG pseudo-random sequence to avoid adding a new dev-dependency.

## Threat Model Coverage

- **T-05-03 (DoS/distance)**: Length mismatch assertion prevents division by zero in cosine normalization.

## Commits

| Task | Hash | Description |
|------|------|-------------|
| All  | (HEAD) | feat(05-03): distance metrics — SIMD + scalar, all three metrics |

## Self-Check: PASSED

- [x] Metric enum has Cosine, L2, DotProduct
- [x] distance_scalar implements all 3 metrics correctly
- [x] distance_simd_f32 uses wide::f32x8 on x86_64
- [x] SIMD and scalar return identical results on f32 (diff < 1e-4)
- [x] distance() dispatches SIMD for f32, scalar for f16/i8
- [x] f16 distance works via manual widening
- [x] i8 distance works via cast to f32
- [x] 9 distance tests pass
- [x] cargo build --workspace exits 0
- [x] cargo clippy -D warnings clean
