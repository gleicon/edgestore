# Scaling EdgeStore — Read Patterns, Replication, and Tiering

EdgeStore is a **single-writer, local-first** embedded database. That constraint is intentional and non-negotiable — it enables crash safety, deterministic recovery, and zero write amplification on SSDs. But it also means scaling reads requires architecture, not just configuration.

This document covers:
1. What EdgeStore supports today for scaling reads
2. The anti-patterns that will cost you money and latency
3. The replication pattern for multi-node deployments
4. The tiering pattern for object-storage-backed cold storage
5. The planned **ImmutableEngine** for read-only environments (WASM, serverless, edge functions)

---

## 1. Single-Process Scaling (Supported Today)

Within one process, EdgeStore scales reads via **snapshots**.

```rust
let snap = engine.snapshot();
// snap.get(), snap.range(), snap.prefix() are all thread-safe
// and do not block the writer.
```

A `Snapshot` pins the segments it references. The compactor will not delete those segments until the snapshot is dropped. Snapshots are **in-process only** — you cannot share them across process boundaries.

**Best for:** Single-node applications with concurrent request handlers (e.g. a web server with `tokio` threads). The writer thread does all mutations; request handler threads clone the snapshot and serve reads.

**Limit:** One `Engine` per `db_path`. A second `Engine::open()` on the same directory returns `WriterBusy`.

---

## 2. Replication-Based Scaling (Supported Today)

For multi-node or multi-process deployments, use **pull-only replication** via `edgestore-repl`.

```
┌─────────────┐      HTTP replication       ┌─────────────┐
│   Writer    │  ←──────────────────────────  │  Replica 1  │
│  (local)    │      pull-only segments       │  (local)    │
│             │  ←──────────────────────────  │  Replica 2  │
│             │      (edgestore-repl)         │  (local)    │
└─────────────┘                               └─────────────┘
```

### How it works

1. **Writer** runs `Engine` with `HttpReplicationServer`.
2. **Replicas** run `Engine` in separate directories with `AntiEntropyLoop` pulling from the writer.
3. Replicas fetch only missing segments (Merkle root comparison → delta).
4. Replicas never write to the writer; the protocol is pull-only.

### Code example

```rust
use edgestore::Engine;
use edgestore_repl::{HttpReplicationServer, AntiEntropyLoop};

// Writer
let engine = Arc::new(Mutex::new(Engine::open(config)?));
let server = HttpReplicationServer::new(engine.clone());
server.start("0.0.0.0:9000")?;

// Replica
let local_engine = Engine::open(replica_config)?;
let mut loop_ = AntiEntropyLoop::new(local_engine, writer_url);
loop_.run()?; // blocks, pulling segments periodically
```

**Best for:** Small fleets (2–20 nodes) where you want each node to have a full local copy. Good for edge caching, read replicas, or disaster recovery.

**Limit:** Each replica stores a full copy. Not suitable for "petabyte dataset, query from 1000 edge nodes."

---

## 3. Tiered Scaling — Object Storage Cold + Local Hot (Supported Today)

For datasets larger than local disk, use `edgestore-tier`.

```
┌─────────────┐
│   Writer    │──archive_segments()──→┌─────────────────┐
│  (Tiered)   │                      │ Object Storage  │
│             │←fetch_archived_*()── │     (Cold)      │
└─────────────┘                      └─────────────────┘
```

### How it works

1. Writer flushes segments locally, then archives selected segments to object storage.
2. `get()` has transparent read-through: local miss → scan archived metadata by key bounds → download matching segment → import as local segment → retry.
3. `fetch_archived_overlapping(start, end)` selectively warms only the segments whose key range overlaps a query — no full rehydration needed.
4. Replicas pull from the writer via HTTP, not from object storage directly.

### Cost analysis

| Pattern | Object Storage Cost | Latency |
|---------|---------------------|---------|
| One writer, many replicas via HTTP | 1× upload | Local disk on replicas |
| One writer + selective fetch | 1× upload + selective download | Local for hot, ~20ms–100ms for cold |
| **Anti-pattern: every reader hits object storage** | N× download + N× list | Unpredictable + throttling |

**Best for:** Time-series / log data where recent data is hot (local) and old data is cold (object storage). Queries over old data pay the latency cost only for the segments they touch.

**Limit:** The writer still holds the `LOCK` file. You cannot have two writers on the same local path. And you should not have N readers each running `TieredEngine` pointing at object storage — see Anti-patterns below.

---

## 4. Anti-Patterns (Do Not Do These)

### ❌ Many processes each with `Engine::open()` on the same path

The second process gets `WriterBusy`. This is by design. If you need concurrent writers, EdgeStore is the wrong tool.

### ❌ Many edge nodes each running `TieredEngine` with object storage

```
# Bad: 1000 edge functions each doing this
let engine = TieredEngine::new(local_dir, Box::new(s3));
engine.get("logs", "2020-01-01").await?;
```

**Why it fails:**
- Each node downloads the same 16 MB segment independently
- `ListObjectsV2` (or equivalent) is a paginated scan; 1000 nodes × periodic listing = bill explosion and throttling
- Ephemeral environments have no durable local cache; segments are re-downloaded on every cold start
- No shared warming; no coordination

**Costs explode.** A single `ListObjectsV2` call on a bucket with 10K segments costs ~$0.005. 1000 nodes doing this every 60 seconds = **$7,200/month just for listing**, before any data transfer or egress fees. Add per-GB egress at $0.05–$0.09 and the bill compounds fast.

### ❌ Opening object storage as a filesystem

Tools like `s3fs` or FUSE mounts make S3 look like a local disk. EdgeStore's segment I/O (sparse index seek, xor filter check, block-aligned reads) assumes local disk latency. Over S3-FUSE, a single `get()` that would do 2–3 local disk seeks becomes 2–3 HTTP round-trips. Performance degrades 100–1000×.

---

## 5. The Future: ImmutableEngine for Read-Only Environments (Planned — Phase 9)

Serverless and edge function environments have a different contract:
- **No local filesystem** (or ephemeral only)
- **No persistent writer process**
- **Must start fast** (<50ms cold start)
- **Must read data** from object storage without writing or locking

EdgeStore cannot solve this with `Engine` or `TieredEngine` — both require a `db_path`, a `LOCK` file, and WAL. We need a new component.

### The Vision: ImmutableEngine

```rust
// No local path. No WAL. No lock.
let engine = ImmutableEngine::from_remote(
    &remote_store,
    "my-bucket",
    "mydb/manifest.json",
)?;

// Reads from in-memory segments. Zero remote calls after init.
let val = engine.get("logs", "2020-01-01")?;
let results = engine.range("logs", "2020-01-01", "2020-02-01")?;
```

### Architecture

```
Edge Function / Serverless / WASM Runtime
│
├─ ImmutableEngine (in-memory)
│  ├─ Vec<InMemorySegmentReader>
│  │   ├─ .dat bytes (cached in runtime memory or cache API)
│  │   ├─ .idx (sparse index, parsed once)
│  │   ├─ .xf (xor filter, parsed once)
│  │   └─ .meta (SegmentMeta, parsed once)
│  └─ K-way merge get()/range()/prefix()
│
└─ Object Storage (source of truth, read-only)
    ├─ manifest.json (single object: list of all segments)
    ├─ segments/{hash}.dat (content-addressed)
    ├─ segments/{hash}.idx (sidecar — optional, derivable)
    ├─ segments/{hash}.xf (sidecar — optional, derivable)
    └─ segments/{hash}.meta (sidecar — optional, derivable)
```

### Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **No WAL, no memtable, no manifest file** | Serverless environments have no durable local state. Everything is in-memory or cached via the runtime's cache API. |
| **Manifest as single JSON object** | One `GetObject` to know all segments. No `ListObjectsV2` per request. Manifest can be cached by CDN or runtime cache. |
| **Segments content-addressed by BLAKE3** | Same hash = same bytes. Safe to cache indefinitely. Cache eviction is LRU, not correctness-critical. |
| **Sidecars bundled or derived** | Option A: upload `.dat` + `.idx` + `.xf` + `.meta` as 4 objects. Option B: upload only `.dat`; derive sidecars on first open (one-time CPU cost). Option A is faster; Option B is simpler to deploy. |
| **K-way merge reuses Snapshot code** | `Snapshot::get()` and `Snapshot::range()` already do the right thing: scan all segments, highest-LSN wins. `ImmutableEngine` can reuse this logic. |
| **No LWW merge on read** | Immutable segments are read-only. If two segments have the same key, the one with higher LSN wins — same as Snapshot. No need to write to WAL. |

### WASM / JS Integration

```typescript
// In a WASM runtime (e.g. edge function with JS glue)
import { ImmutableEngine } from "edgestore-wasm";

export default {
  async fetch(request, env, ctx) {
    // env.MY_BUCKET is an object storage binding
    const engine = await ImmutableEngine.fromStorage(env.MY_BUCKET, "manifest.json");
    
    // All segments cached in runtime cache; no remote calls on cache hit
    const results = engine.range("logs", "2020-01-01", "2020-02-01");
    return new Response(JSON.stringify(results));
  }
};
```

The Rust crate would expose a `wasm-bindgen` API. The heavy lifting (segment parsing, K-way merge) stays in Rust. The JS glue is thin.

### Cost Model for Serverless

| Scenario | Remote Calls | Cost |
|----------|--------------|------|
| First request (cold start) | 1× manifest + M× segments | ~$0.0001 + transfer |
| Subsequent requests (cache hit) | 0 | ~$0 |
| Manifest update (new segment flushed) | 1× manifest (if cache expired) | ~$0.000005 |
| 1000 workers, 1M requests/day | ~1000 manifests/day + segment cache hits | ~$0.15/month |

Compare to the anti-pattern (1000 nodes each listing object storage): **50,000× cheaper**.

Key cost drivers to watch:
- **List API calls** are expensive (paginated scans). The single-manifest design eliminates them entirely.
- **Egress fees** vary by provider ($0.05–$0.09/GB is common). Fetching a 16 MB segment costs ~$0.001 in egress. Fetching 100 segments/day = ~$0.10/day.
- **Ingress** is typically free. Uploading archived segments costs only storage ($0.023/GB/month on standard tiers).

### Status

Phase 9 is planned for the v1.2 milestone. See `.planning/phases/09-readonly-edge/` for the full plan.

---

## 6. Decision Matrix: Which Pattern Should I Use?

| Use Case | Pattern | Crate |
|----------|---------|-------|
| Single web server, concurrent handlers | Snapshots | `edgestore` |
| 2–20 nodes, full replication | HTTP replication | `edgestore` + `edgestore-repl` |
| Dataset > local disk, time-series queries | Tiered (object storage cold) | `edgestore-tier` |
| Serverless / edge functions / WASM | ImmutableEngine | `edgestore-wasm` (planned) |
| Many independent processes, shared NFS path | **Not supported** | — |
| Many edge nodes each querying object storage directly | **Anti-pattern** | — |

---

## 7. Summary

EdgeStore scales reads in three ways today:
1. **In-process snapshots** — lightweight, thread-safe, zero copy
2. **HTTP replication** — each replica gets a full copy; good for small fleets
3. **Tiered storage** — writer selectively fetches from object storage; replicas pull from writer

The missing piece is **read-only access in serverless environments** — environments with no local disk, no persistent process, and no writer. `ImmutableEngine` fills this gap by treating object storage as an immutable segment store, downloading segments into memory, and serving reads with the same K-way merge logic that `Snapshot` already uses. It requires no WAL, no locking, and no local state.

If you are building for serverless today, the recommended interim approach is:
1. Run the writer on a traditional server (or VM) with `TieredEngine`
2. Expose a query API over the writer
3. Call that API from your serverless function

Do not give each serverless instance its own object storage client and `TieredEngine`. That path leads to billing surprises.
