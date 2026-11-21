# Scaling EdgeStore — Read Patterns, Replication, and Tiering

EdgeStore is a **single-writer, local-first** embedded database. That constraint is intentional and non-negotiable — it enables crash safety, deterministic recovery, and zero write amplification on SSDs. But it also means scaling reads requires architecture, not just configuration.

This document covers:
1. What EdgeStore supports today for scaling reads
2. The anti-patterns that will cost you money and latency
3. The replication pattern for multi-node deployments
4. The tiering pattern for object-storage-backed cold storage

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

## 5. Decision Matrix: Which Pattern Should I Use?

| Use Case | Pattern | Crate |
|----------|---------|-------|
| Single web server, concurrent handlers | Snapshots | `edgestore` |
| 2–20 nodes, full replication | HTTP replication | `edgestore` + `edgestore-repl` |
| Dataset > local disk, time-series queries | Tiered (object storage cold) | `edgestore-tier` |
| Many independent processes, shared NFS path | **Not supported** | — |
| Many edge nodes each querying object storage directly | **Anti-pattern** | — |

---

## 6. Summary

EdgeStore scales reads in three ways:
1. **In-process snapshots** — lightweight, thread-safe, zero copy
2. **HTTP replication** — each replica gets a full copy; good for small fleets
3. **Tiered storage** — writer selectively fetches from object storage; replicas pull from writer

If you are running serverless functions that need to query EdgeStore data, the recommended approach is:
1. Run a writer on a traditional server (or VM) with `TieredEngine`
2. Expose a query API over the writer
3. Call that API from your serverless function

Do not give each serverless instance its own object storage client and `TieredEngine`. That path leads to billing surprises.
