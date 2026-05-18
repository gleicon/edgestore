# EdgeStore — Project Guide

## What We're Building

Local-first embedded KV + vector database in Rust. SSD-aware, append-oriented, deathtime-cohort compaction. Library-first — no mandatory server. See `.planning/prod.md` for full spec and `.planning/PROJECT.md` for project context.

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

## GSD Workflow

This project uses GSD for structured phase execution.

```
/gsd:plan-phase N      — plan a phase before coding
/gsd:execute-phase N   — execute the plan
/gsd:verify-work N     — verify phase deliverables
/gsd:progress          — check current state
```

**Current phase:** Not started. Run `/gsd:plan-phase 1` to begin.

## File Layout

```
.planning/
  PROJECT.md        — project context and decisions
  REQUIREMENTS.md   — all requirements with REQ-IDs
  ROADMAP.md        — 7 phases, success criteria, risks
  STATE.md          — current progress
  config.json       — GSD workflow config
prod.md             — full design spec (source of truth for architecture)
```

## References

- VLDB 2026 (Lee et al.): https://www.vldb.org/pvldb/vol19/p1469-lee.pdf — deathtime-based GC, primary design reference
- VLDB SSD WAF: https://www.vldb.org/pvldb/vol16/p2769-durner.pdf
- SlateDB: https://github.com/slatedb/slatedb
- NVMe + S3: https://www.eloqdata.com/blog/2025/10/24/how-nvme-and-s3-reshape-decoupling
