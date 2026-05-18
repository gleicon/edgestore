# EdgeStore

## What This Is

EdgeStore is a local-first embedded KV + vector database written in Rust. It targets SSD-optimized edge deployments where local NVMe is the fast path and S3 is the durable archive.

The differentiator: deathtime-cohort compaction (from VLDB 2026, Lee et al.) drives device write amplification toward 1.0 — no existing embedded database does this. Combined with the NVMe-local + S3-remote split that cloud VMs impose, EdgeStore makes ephemeral instance storage safe without sacrificing local write latency.

It is not a LevelDB rewrite. It is not SlateDB (S3-first). It is a library, not a server.

## Core Value

**Local NVMe fast path. S3-safe recovery. Device WAF → 1.**

An edge app can embed EdgeStore, get SQLite-like ergonomics, RocksDB-like storage discipline, flat SIMD vector search, and Merkle-indexed delta replication — all in a single Rust crate with no mandatory server process.

## Context

- Language: Rust (no GC, precise memory control for deathtime grouping)
- Deployment: library-first (`edgestore` crate), optional `edgestore-tokio` async wrapper
- API: sync core, no mandatory async runtime dependency
- Target hardware: NVMe SSDs (standard), FDP hints in Phase 6, ZNS optional later
- Replication: Merkle-based delta sync, transport-agnostic, S3 as mailbox/archive
- v1 scope: KV + vector search
- v2 scope: full-text search (Algolia-like, no server)

## Key Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Language | Rust | No GC pauses; precise deathtime-cohort memory control |
| Deployment | Library-first | SQLite/RocksDB model; no server overhead |
| Concurrency | Single writer + readers + group commit | Simplest recovery model; MVCC adds compaction complexity |
| Memtable | `BTreeMap` behind `MemTable` trait | Zero deps; swappable; not the hot path |
| Namespace encoding | Prefix-encoded key `{ns_len:u16}{ns_bytes}{key_bytes}` | Single keyspace; no ColumnFamily complexity |
| Compaction | Deathtime-cohort (1h default, configurable) | VLDB 2026 paper; groups by predicted invalidation time |
| WAL compression | LZ4 | Lowest write latency on hot path |
| Segment compression | ZSTD level 1 | Written once; better ratio reduces device WAF |
| Segment size | 16 MB default (configurable) | Above SSD erase block min; practical delta sync unit |
| WAL rotation | 64 MB OR 60s (configurable) | Bounds recovery time and idle flush lag |
| Probabilistic filter | Xor filter per segment (`xorf`) | Static sets; ~8 bits/key; faster than bloom/cuckoo |
| Conflict resolution | LWW by wall clock timestamp | Simple; documented limitation; timestamp in WAL record |
| Vector index v1 | Flat SIMD scan | Keeps KV pure; HNSW added in Phase 6 |
| Vector format | Typed header at API layer, opaque KV | `{dims:u16}{dtype:u8}{data}` |
| API style | Sync core + optional async wrapper | Embeds anywhere; no forced runtime dep |
| Bloom FPR | 1% default (xor filter, configurable) | Well-tested; ~125 KB per 16 MB segment |

## Requirements

### Validated

(None yet — greenfield)

### Active

- [ ] WAL append-only with LZ4, CRC32C, versioned format
- [ ] WAL rotation at 64 MB / 60s
- [ ] BTreeMap MemTable behind swappable trait
- [ ] Single writer + group commit
- [ ] KV API: put, put_with_ttl, get, delete, range, prefix
- [ ] Transaction API: begin, commit, rollback
- [ ] Crash recovery from WAL
- [ ] Prefix-encoded namespace keys
- [ ] Immutable sorted segments (ZSTD, 4 KiB aligned, 16 MB)
- [ ] Sparse index + xor filter + manifest + BLAKE3 content addressing
- [ ] Deathtime-cohort compaction with TTL support
- [ ] Range scans across segments
- [ ] Snapshots
- [ ] Merkle trees (segment + range level)
- [ ] Delta replication protocol (transport-agnostic)
- [ ] S3 integration (cold storage, replication mailbox)
- [ ] LWW conflict resolution
- [ ] Vector typed header API
- [ ] Flat SIMD vector search (cosine, dot, euclidean)
- [ ] StorageBackend trait (abstraction for FDP/ZNS)
- [ ] HNSW index (Phase 6)
- [ ] edgestore-tokio async wrapper

### Out of Scope (v1)

- Full-text search — v2 (requires separate indexing pipeline)
- SQL layer — v2+ (SQLite virtual table possible later)
- Distributed consensus — never (local-first model)
- Multi-primary replication — never in v1
- Automatic CRDT semantics — out of scope
- Cluster orchestration — not a library concern
- Vector clocks — v2 (WAL has reserved extension field)
- ZNS explicit zone management — post-FDP

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition:**
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions

---
*Last updated: 2026-05-18 after initialization*
