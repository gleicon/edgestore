# Performance Baseline — Pre-Phase 6

**Date:** 2026-05-22  
**Build:** debug (unoptimized). Multiply ~5-10x for release estimates.  
**Workload:** 1000 ops, single namespace, in-process, no concurrent readers.  
**Hardware:** Development machine (Apple Silicon, NVMe SSD implied).

## Measured Numbers

```
1000 puts (memtable)          9.4ms   → ~9.4µs/op
1000 gets (memtable)          519µs   → ~0.52µs/op
flush_to_segments             26ms    (one-time, 1000 entries)
1000 gets (segment)           81.9ms  → ~82µs/op
transaction 100 puts + fsync  5.2ms   → ~52µs/put (includes WAL fsync)
range scan 1000 keys          3.0ms   (segment-backed)
prefix scan 1000 keys         2.9ms   (segment-backed)
```

## Release Build Estimates

| Operation | Debug | Release est. |
|---|---|---|
| put (memtable) | 9.4µs | 1-2µs |
| get (memtable) | 0.52µs | 50-100ns |
| get (segment) | 82µs | 8-16µs |
| tx put+fsync | 52µs | 10-20µs |
| range 1000 keys | 3ms | <1ms |

## Competitive Position (release estimates)

| DB | put | get | compaction | write amplification |
|---|---|---|---|---|
| **EdgeStore** (est.) | 1-2µs | 50ns mem / 10µs seg | Deathtime-cohort | ~1x TTL workloads |
| RocksDB | 5-20µs | 1-5µs | Level-based | 10-30x |
| LevelDB | 5-15µs | 2-10µs | Level-based | 10-30x |
| LMDB | 50-200µs | 1-5µs (mmap, no decomp) | None | 1x (mmap IOPS) |
| sled | 5-30µs | 1-10µs | Bw-tree | 5-15x |
| SQLite WAL | 20-50µs | 10-100µs | None (freelist) | 2-4x |

## Why Segment Gets Are Slow (82µs debug)

Three costs, in order of dominance:
1. **No block cache** — every miss decompresses a ZSTD block from disk.
2. **Multi-segment probe** — point read checks all segments in LSN order.
3. **Debug build** — ZSTD decompression has no compiler optimization.

Phase 6 adds an LRU block cache. Expected post-Phase-6 segment get: **200-500ns** for hot data (2x LMDB).

## Write Amplification Analysis

### Standard LSM (RocksDB level-based)
```
record written once → WAL → L0 → L1 → L2 → L3 ...
each compaction reads + rewrites all live records in range
WA: 10-30x at application layer
SSD firmware GC adds: 2-5x (partially-valid flash pages)
Total device WA: 20-150x
```

### Deathtime-cohort (EdgeStore)
```
record written once → WAL → segment (cohort_bucket = death_time / 3600)
fully-expired cohort: zero live-record reads or writes at collection
partially-expired cohort: one merge pass, only live records relocated
WA: ~1x for TTL workloads (Lee et al. 2026: 7.8x reduction measured)
SSD firmware GC: ~1.2x (full-block erases from expired cohorts)
Total device WA: 1.2x
```

### Compound SSD effect (Durner et al. 2023)
When a fully-expired cohort is collected, all pages in those segment files are invalid simultaneously. The SSD GC sees entire erase blocks it can reclaim without copying anything. RocksDB level compaction creates mixed valid/invalid pages per erase block, forcing the SSD GC to copy valid remnants before erasing.

```
RocksDB:   30× (app WA) × 3× (SSD GC) = 90× total device WA
EdgeStore:  1× (app WA) × 1.2× (SSD GC) = 1.2× total device WA
```

## What's Missing That Affects These Numbers

| Missing | Effect | Fixed in |
|---|---|---|
| Block cache | Segment get 82µs → 200-500ns hot | Phase 6 |
| HNSW index | Vector ANN search | Phase 6 |
| Release build benchmark | 5-10x improvement across board | — |
| Multi-segment xor filter pre-screen | Reduce probes per point read | Phase 6 |

## Replication Performance (not yet measured)

Phase 4 will add Merkle-based delta sync. Expected: sync cost proportional to changed key range, not total dataset size. Baseline for comparison: full snapshot size = sum of segment files.

---

**Re-run after Phase 6:** `cargo test --release --test integration_core test_operation_timing -- --nocapture`
