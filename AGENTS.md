# EdgeStore — Agent Guide

## What We're Building

Local-first embedded KV + vector database in Rust. SSD-aware, append-oriented, deathtime-cohort compaction. Library-first — no mandatory server. See `prod.md` for full spec and `.planning/PROJECT.md` for project context.

**Status:** v1.0.4 (bugfix release — HNSW staleness detection + Rust 1.88+ FDP compilation fix).

## Architecture Constraints (non-negotiable)

- **No in-place writes ever.** All mutations append-oriented.
- **Deathtime-cohort compaction only.** No level-based or size-tiered compaction.
- **KV layer stays pure.** Vector and search APIs sit on top; never pollute KV core.
- **Single writer.** No MVCC in v1. WAL txid field exists for future use.
- **Sync core.** No `async` in `edgestore` crate. Async goes in `edgestore-tokio`.
- **No ColumnFamily complexity.** Namespace = prefix-encoded key only.

## Key Design Decisions

| What | How |
|------|-----|
| Namespace encoding | `{ns_len:u16}{ns_bytes}{key_bytes}` |
| WAL compression | LZ4 |
| Segment compression | ZSTD level 1 |
| Segment size | 16 MB default |
| WAL rotation | 64 MB OR 60s |
| Probabilistic filter | Xor filter (`xorf`) — NOT bloom |
| Memtable | `BTreeMap` behind `MemTable` trait |
| Cohort window | 1 hour default |
| Conflict resolution | LWW by wall clock (`timestamp` in WAL record) |
| Vector index v1 | Flat SIMD scan — HNSW in Phase 6 |
| Vector header | `{dims:u16}{dtype:u8}{data}` |
| Content addressing | BLAKE3 |

## Academic Foundations

EdgeStore is built on ideas from peer-reviewed database research:

- **VLDB 2026 (Lee et al.)** — deathtime-based garbage collection. Primary design reference for cohort-grouped compaction that achieves near-zero write amplification on TTL workloads.
- **VLDB SSD WAF (Durner et al., 2023)** — SSD write amplification analysis. Informs the append-only, out-of-place write design that keeps device WAF near 1.0.
- **SlateDB** — cloud-native LSM design reference for segment formats and manifest patterns.
- **NVMe + S3 (EloqData, 2025)** — decoupled storage architecture patterns for replication and cold archive.

See `website/papers.html` and `ARCHITECTURE.md` for detailed citations.

## File Layout

```
edgestore/          — Core sync KV + vector + text engine
edgestore-tokio/    — Async wrapper (Tokio)
edgestore-repl/     — HTTP replication + S3 remote store
edgestore-cli/      — Administrative CLI binary
website/            — Static documentation site (Tailwind CSS)
examples/           — Runnable Rust examples (edgestore/examples/)
.planning/          — GSD project plans, roadmaps, requirements
prod.md             — Full design spec (source of truth)
ARCHITECTURE.md     — Component overview and data flows
README.md           — Quick-start and feature matrix
```

## Developer Quick-Start

```bash
# Build everything
cargo build --workspace

# Run all tests
cargo test --workspace

# Run clippy
cargo clippy --workspace -- -D warnings

# Build docs
cargo doc --workspace --no-deps --open

# Run examples
cargo run --example basic_kv
cargo run --example vector_search
cargo run --example replication

# Run benchmarks
cargo bench --workspace
```

## Release Workflow (Makefile)

```bash
# Full release: test, tag, publish
make release

# Individual steps
make test          # Run all tests
make tag           # Create git tag (reads version from Cargo.toml)
make tags-push     # Push tag to origin
make publish       # Publish all crates to crates.io in order
make publish-dryrun  # Verify without publishing
```

## Public API Surface

| Type | Item | Description |
|------|------|-------------|
| Engine | `Engine` | Main KV engine (open, get, put, delete, range, prefix, flush, snapshot, compact) |
| Config | `EdgestoreConfig` | Database configuration |
| Error | `EdgestoreError` | All error variants |
| Transaction | `Transaction` | Multi-record atomic batch |
| Vector | `VectorEngine` trait, `VectorRecord`, `Dtype`, `Metric` | Vector storage & search |
| Text | `TextEngine` trait, `TextSearchResult`, `SearchOptions` | Full-text search |
| Snapshot | `Snapshot`, `SnapshotRegistry` | Point-in-time reads |
| Replication | `ReplicationProtocol`, `HostId`, `SegmentRef` | Pull-only sync |
| Storage | `StorageBackend`, `DefaultStorageBackend`, `MemoryStorageBackend` | Pluggable I/O |

## GSD Workflow

This project uses GSD for structured phase execution.

```
/gsd:plan-phase N      — plan a phase before coding
/gsd:execute-phase N   — execute the plan
/gsd:verify-work N     — verify phase deliverables
/gsd:progress          — check current state
```

## References

- VLDB 2026 (Lee et al.): https://www.vldb.org/pvldb/vol19/p1469-lee.pdf
- VLDB SSD WAF: https://www.vldb.org/pvldb/vol16/p2769-durner.pdf
- SlateDB: https://github.com/slatedb/slatedb
- NVMe + S3: https://www.eloqdata.com/blog/2025/10/24/how-nvme-and-s3-reshape-decoupling
