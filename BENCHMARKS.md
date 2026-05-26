# EdgeStore Benchmarks

This document describes the EdgeStore benchmark suite, methodology, and expected performance characteristics.

## Overview

The benchmark suite uses [Criterion.rs](https://bheisler.github.io/criterion.rs/book/) for statistically rigorous measurements. Each benchmark runs for a minimum sampling period, reports confidence intervals, and produces HTML reports in `target/criterion/`.

**Design principles:**
- Measure end-to-end API performance (not micro-benchmarks of internal helpers)
- Use `TempDir` for isolation; results reflect cold-start behavior
- Statistical rigor: outlier detection, confidence intervals, throughput reports

## Hardware / Environment

Results below are placeholders with expected ranges. To reproduce, use:

| Component | Recommended |
|-----------|-------------|
| CPU | x86_64 with AVX2 (or Apple Silicon ARM64) |
| RAM | 16 GB+ |
| SSD | NVMe SSD with DRAM cache (avoid network-mounted volumes) |
| OS | Linux 6.x or macOS 14+ |
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

| Workload | Expected Range | Notes |
|----------|---------------|-------|
| Sequential put (1K keys) | 80,000 – 150,000 ops/sec | LZ4 compression + CRC32C per record |
| Random put (1K keys) | 70,000 – 130,000 ops/sec | Similar to sequential; memtable is BTreeMap |
| Batch transaction (1K ops) | 60,000 – 100,000 ops/sec | Single WAL fsync at commit boundary |

> To measure: `cargo bench --bench throughput` → look for `throughput/put_1000`

### b) Read Throughput (ops/sec)

| Workload | Expected Range | Notes |
|----------|---------------|-------|
| Point get (hot, 1K keys) | 200,000 – 400,000 ops/sec | Memtable hit path |
| Point get (cold, flushed) | 50,000 – 100,000 ops/sec | Segment store + xor filter |
| Range scan (100 keys) | 30,000 – 60,000 ops/sec | Merges memtable + segment results |
| Prefix scan (all keys) | 40,000 – 80,000 ops/sec | Bound by prefix encoding + BTreeMap range |

> To measure: `cargo bench --bench throughput` → look for `throughput/get_1000_hot`

### c) Vector Search Latency (ms)

Flat SIMD scan vs HNSW index. Query time for top-10 nearest neighbors.

| Collection Size | Flat Scan p50 | Flat Scan p99 | HNSW p50 | HNSW p99 |
|-----------------|---------------|---------------|----------|----------|
| 10,000 vectors  | 2 – 5 ms      | 5 – 10 ms     | 0.1 – 0.3 ms | 0.5 – 1 ms |
| 100,000 vectors | 20 – 40 ms    | 50 – 80 ms    | 0.2 – 0.5 ms | 1 – 2 ms |
| 500,000 vectors | 100 – 200 ms  | 250 – 400 ms  | 0.5 – 1 ms   | 3 – 5 ms |

**Dimensions:** 32, **Dtype:** F32, **Metric:** L2  
> To measure: `cargo bench --bench vector_search`

### d) HNSW Recall vs Latency Tradeoff

Recall@10 compared against brute-force flat scan reference.

| Vectors | Recall@10 | Build Time | Search p50 |
|---------|-----------|------------|------------|
| 500     | 0.90 – 0.99 | < 100 ms | 0.05 – 0.1 ms |
| 1,000   | 0.90 – 0.99 | < 200 ms | 0.08 – 0.15 ms |
| 5,000   | 0.85 – 0.95 | < 1 s   | 0.15 – 0.3 ms |

> HNSW parameters: `M=16`, `efConstruction=100`. Adjust in code if needed.  
> To measure: `cargo bench --bench hnsw_recall`

### e) Text Search QPS

BM25-based full-text search over indexed documents.

| Document Count | Index Throughput | Search QPS |
|----------------|-----------------|------------|
| 100 docs       | 2,000 – 4,000 docs/sec | 5,000 – 10,000 |
| 1,000 docs     | 1,500 – 3,000 docs/sec | 3,000 – 6,000 |
| 10,000 docs    | 1,000 – 2,000 docs/sec | 1,000 – 3,000 |

> Query: `"quick brown fox"`, top-10 results.  
> To measure: `cargo bench --bench text_search`

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

## Filling in Actual Results

To replace the placeholder ranges above with measured numbers:

1. Ensure you are on the target hardware.
2. Run the full suite: `cargo bench`
3. Extract throughput/latency from `target/criterion/` or console output.
4. Edit the tables in this file and commit.

---

*Last updated: 2026-05-25 (v1.0 placeholder results)*
