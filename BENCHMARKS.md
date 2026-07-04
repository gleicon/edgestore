# EdgeStore Benchmarks

This document describes the EdgeStore benchmark suite, methodology, and measured performance characteristics.

## Overview

The benchmark suite uses [Criterion.rs](https://bheisler.github.io/criterion.rs/book/) for statistically rigorous measurements. Each benchmark runs for a minimum sampling period, reports confidence intervals, and produces HTML reports in `target/criterion/`.

**Design principles:**
- Measure end-to-end API performance (not micro-benchmarks of internal helpers)
- Use `TempDir` for isolation; results reflect cold-start behavior
- Statistical rigor: outlier detection, confidence intervals, throughput reports

## Hardware / Environment

Results below were measured on the following hardware:

| Component | Specification |
|-----------|---------------|
| CPU | Apple M5 |
| RAM | 16 GB |
| Storage | Apple Silicon integrated SSD |
| OS | macOS |
| Rust | Stable 1.85+ |

**Recording your environment:**
```bash
# CPU info
lscpu | grep 'Model name'  # Linux
sysctl -n machdep.cpu.brand_string  # macOS

# Disk type
lsblk -d -o NAME,ROTA,TYPE,SIZE,MODEL  # Linux
diskutil info disk0 | grep 'Device Identifier'  # macOS
```

> **Note:** Benchmarks tagged with **[hardware]** require specific SSD/NVMe setups and are not run in CI. In-repo benchmarks run on any development machine.

## Benchmark Binaries

The following benchmarks are defined in `edgestore/Cargo.toml`:

| Benchmark | File | What it measures | In-repo |
|-----------|------|------------------|---------|
| `vector_search` | `benches/vector_search.rs` | Flat scan vs HNSW latency | Yes |
| `hnsw_recall` | `benches/hnsw_recall.rs` | HNSW recall@10 vs brute force | Yes |
| `throughput` | `benches/throughput.rs` | KV + vector put/get throughput | Yes |
| `text_search` | `benches/text_search.rs` | Text indexing and search QPS | Yes |

## How to Run

### All benchmarks
```bash
cd edgestore
cargo bench
```

### Specific benchmark
```bash
cargo bench --bench throughput
cargo bench --bench vector_search
cargo bench --bench hnsw_recall
cargo bench --bench text_search
```

### HTML reports
After running, open:
```bash
open target/criterion/report/index.html  # macOS
xdg-open target/criterion/report/index.html  # Linux
```

## Results

### a) Write Throughput (ops/sec)

Measures `put` operations to an empty engine (WAL + memtable only, no segment flush).

| Workload | Measured | Notes |
|----------|----------|-------|
| Sequential put (1K keys) | **~395,000 ops/sec** | LZ4 compression + CRC32C per record |
| Random put (1K keys) | *see sequential* | Memtable is BTreeMap; similar performance |
| Batch transaction (1K ops) | *same as put* | Single WAL fsync at commit boundary |

> Measurement: `cargo bench --bench throughput` → `throughput/put_1000` = 2.53 ms/1K ops

### b) Read Throughput (ops/sec)

| Workload | Measured | Notes |
|----------|----------|-------|
| Point get (hot, 1K keys) | **~8,940,000 ops/sec** | Memtable hit path |
| Point get (cold, flushed) | *not measured in bench* | Segment store + xor filter |
| Range scan (100 keys) | *not measured in bench* | Merges memtable + segment results |
| Prefix scan (all keys) | *not measured in bench* | Bound by prefix encoding + BTreeMap range |

> Measurement: `cargo bench --bench throughput` → `throughput/get_1000_hot` = 111.83 µs/1K ops

### c) Vector Search Latency (µs)

Flat SIMD scan vs HNSW index. Query time for top-10 nearest neighbors.

| Collection Size | Flat Scan p50 | HNSW p50 | Speedup |
|-----------------|---------------|----------|---------|
| 500 vectors     | 8.2 µs        | 8.2 µs   | ~1.0x  |
| 1,000 vectors   | 8.6 µs        | 8.7 µs   | ~1.0x  |
| 5,000 vectors   | 11.1 µs       | 11.0 µs  | ~1.0x  |

**Note:** On this benchmark, HNSW search is comparable to flat scan for small collections because the HNSW graph overhead balances the reduced distance computations. For larger collections (100K+), HNSW should show significant speedup.

**Dimensions:** 32, **Dtype:** F32, **Metric:** L2  
> Measurement: `cargo bench --bench vector_search`

### d) HNSW Search Latency

Recall@10 compared against brute-force flat scan reference.

| Vectors | Search p50 | Notes |
|---------|------------|-------|
| 500     | 377.8 µs   | Includes recall verification overhead |
| 1,000   | 654.1 µs   | Includes recall verification overhead |
| 5,000   | 3.19 ms    | Includes recall verification overhead |

> The `hnsw_recall` benchmark measures the full search + brute-force comparison cycle, not just the HNSW query time. For pure HNSW query latency, see the `vector_search` benchmark.
> Measurement: `cargo bench --bench hnsw_recall`

### e) Text Search QPS

BM25-based full-text search over indexed documents. Uses a single merged inverted
index per namespace, cached in memory. Search reads the cached index directly —
O(1) HashMap lookup with no per-query deserialization.

| Document Count | Search QPS | Latency per query | Notes |
|----------------|-----------|-------------------|-------|
| 100 docs       | ~30,000   | ~33 µs            | Warm cache |
| 1,000 docs     | ~8,000    | ~125 µs           | Warm cache |
| 10,000 docs    | ~300      | ~3.2 ms           | Warm cache, ~30 unique terms |
| 50,000 docs    | ~60       | ~16 ms            | Warm cache |
| 100,000 docs   | ~30       | ~33 ms            | Warm cache |

**Previous (buggy) implementation:** Per-document micro-indexes caused O(N)
deserialize+merge, collapsing to ~6 QPS at 10K docs. Fixed in v1.0.9.

| Index Throughput | Measured |
|------------------|----------|
| 100 docs         | ~250 docs/sec (~4 ms/doc, includes disk write) |

> Search measurement: `cargo bench --bench text_search`
> Index measurement: `cargo bench --bench text_search -- index`
> Warm cache: run search once before benchmarking to populate in-memory index cache.

### f) Compaction Overhead (WAF)

Write Amplification Factor = physical bytes written / logical bytes inserted.

| Scenario | Expected WAF | Notes |
|----------|-------------|-------|
| No compaction (WAL only) | 1.2 – 1.5x | LZ4 + CRC32C overhead |
| With TTL, cohort expiry | 1.0 – 1.2x | Zero-copy collection of fully-expired cohorts |
| Steady-state mixed workload | 2.0 – 4.0x | Deathtime-cohort compaction rewrites live data |

> **[hardware]** Requires device-level write counters (`nvme smart-log`) for precise measurement.  
> In-repo proxy: compare `CompactionStats::bytes_written` to logical data volume.

## Interpreting Results

### Confidence Intervals
Criterion reports `x.y ± z.w µs` — the interval is the 95% confidence interval. If the interval is wide relative to the mean, increase sample time or close background applications.

### Regression Detection
Criterion compares against previous runs stored in `target/criterion/`. A red arrow in the HTML report indicates regression vs the last run on the same machine.

### Noise Sources
- **Background processes:** Close browsers, compilers, and indexing services.
- **Thermal throttling:** Run on AC power with active cooling.
- **Filesystem caches:** Cold-start benchmarks (TempDir) are not affected, but repeated runs may warm the page cache.
- **WAL rotation:** Benchmarks that cross the 64 MB WAL threshold will see a latency spike from fsync + file creation.

### Interpreting WAF
- WAF < 2.0: Excellent — most data is immutable or expired in place.
- WAF 2.0 – 5.0: Normal for LSM-style stores under update-heavy workloads.
- WAF > 5.0: Investigate — compaction may be too aggressive or cohort window too small.

## Known Limitations

- **Small collections:** The vector search benchmarks use 500–5,000 vectors. For 100K+ collections, HNSW should show more pronounced speedup over flat scan.
- **No SSD WAF measurement:** WAF numbers are theoretical/expected. Actual device-level WAF measurement requires `nvme smart-log` or equivalent.
- **Apple Silicon:** Results are from an M5 MacBook. x86_64 AVX2 systems may show different absolute numbers but similar relative trends.

---

*Last updated: 2026-05-25 (measured results, Apple M5, 16 GB RAM)*
