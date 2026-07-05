# EdgeStore Architecture

EdgeStore is built around three architectural principles:

1. **Append-only, out-of-place writes.** Every mutation appends to a WAL and eventually
   to an immutable segment. Existing pages are never rewritten in place.
2. **SSD-aware design.** Deathtime-cohort compaction groups data by predicted
   invalidation time, minimizing erase cycles and driving device write amplification
   toward 1.0.
3. **Layered purity.** The KV layer is pure byte storage. Vector and full-text APIs
   sit on top as typed wrappers; they never pollute the core keyspace.
4. **Crate separation.** `edgestore` is the sync core. `edgestore-repl` adds network
   and remote durability (HTTP replication, S3). You can use `edgestore` alone
   without ever pulling in `edgestore-repl`.

For the full technical specification, see [`prod.md`](../prod.md).

---

## Component Diagram

```
┌──────────────────────────────────────────────────────────────────────────┐
│                              Application                                 │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌────────────────┐ │
│  │   KV API    │  │ Vector API  │  │  Text API   │  │ Replication    │ │
│  │  put / get  │  │ vector_put  │  │ index_text  │  │ compare_merkle │ │
│  │  range / tx │  │ vector_get  │  │  search     │  │ import_segment │ │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘  └───────┬────────┘ │
└─────────┼────────────────┼────────────────┼───────────────────┼──────────┘
          │                │                │                   │
          └────────────────┴────────────────┴───────────────────┘
                                    │
                                    ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                        Engine (coordination)                             │
│  • Single-writer mutex + group commit batching                           │
│  • Namespace isolation via prefix-encoded keys                           │
│  • LWW conflict resolution (wall-clock timestamp in WAL)                 │
│  • Snapshot RAII pinning for point-in-time reads                         │
└───────────────────────┬──────────────────────────────────────────────────┘
                        │ writes batches
                        ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                          WAL (durability)                                │
│  • Append-only sequential writes                                         │
│  • LZ4 frame compression, CRC32C per record                              │
│  • Rotated at 64 MB OR 60 s (configurable)                             │
│  • Recovery: replay all WAL files → rebuild memtable + manifest        │
└──────────────────────────────────────────────────────────────────────────┘
                        │
                        ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                       Memtable (BTreeMap)                                │
│  • In-memory write buffer behind swappable MemTable trait              │
│  • Ordered iteration for deterministic flush                           │
│  • Rebuilt from WAL on recovery                                        │
└───────────────────────┬──────────────────────────────────────────────────┘
                        │ flushes immutable sorted run
                        ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                       SegmentStore (persistent storage)                  │
│  • Immutable sorted segments (.dat + .idx + .xf + .meta)               │
│  • Sparse index → block-level seek without full scan                   │
│  • Xor filter per segment (~8 bits/key, 1% FPR)                      │
│  • BLAKE3 content addressing for integrity & replication               │
│  • Manifest: append-only live segment list with Merkle roots           │
└───────────────────────┬──────────────────────────────────────────────────┘
                        │
            ┌───────────┴────────────┐
            │                        │
            ▼                        ▼
┌──────────────────────┐  ┌──────────────────────────────────────────────┐
│  Compactor           │  │  Replication Sidecar (edgestore-repl)      │
│  • Identifies cohorts│  │  • Merkle tree over segments & ranges        │
│  • Zero-reloc when   │  │  • Delta exchange (transport-agnostic)       │
│    fully expired     │  │  • S3 object log + mailbox                   │
│  • Bounded budget per│  │  • LWW import with timestamp resolution      │
│    cycle             │  └──────────────────────────────────────────────┘
└──────────────────────┘
                        │
                        ▼
┌──────────────────────────────────────────────────────────────────────────┐
│  StorageBackend abstraction (NVMe / FDP / ZNS / Memory)                │
│  • Default: pread/pwrite on local filesystem                             │
│  • FDP: placement hints per segment write (NVMe 2.0)                     │
│  • S3: cold archive + replication mailbox via `edgestore-repl::RemoteStore`│
└──────────────────────────────────────────────────────────────────────────┘
```

---

## Component Descriptions

### Engine
The top-level coordinator. Owns the write mutex, dispatches KV / vector / text
operations, and manages the lifecycle of memtable, segment store, compactor,
and snapshot registry. All public APIs live here.

### WAL
Write-ahead log for durability and crash recovery. Every mutation is appended
before acknowledgment. LZ4 compression keeps the hot-path write latency low.
Files are rotated at a configurable size or time threshold.

### Memtable
In-memory sorted buffer (`BTreeMap` by default) behind a `MemTable` trait.
Swappable at DB open. Flushed to an immutable segment when the size threshold
is reached. Rebuilt entirely from WAL during recovery.

### SegmentStore
Manages immutable on-disk segments. Each flush produces:

- `.dat` — ZSTD-compressed data blocks (4 KiB aligned)
- `.idx` — sparse offset index for block-level seeks
- `.xf` — xor filter for fast negative checks (~8 bits/key)
- `.meta` — JSON metadata (key bounds, LSN range, BLAKE3 hash, cohort info)

The manifest tracks live segments with Merkle roots and content hashes.

### Compactor
Deathtime-cohort garbage collector. Groups segments by predicted invalidation
time (`death_time`), not by size tier or level. Fully expired cohorts are
rewritten with **zero live-data relocation**. Partially expired cohorts are
compacted incrementally within a configurable write budget. Snapshots pin
segments, preventing their removal until the snapshot is dropped.

### VectorIndex / VectorEngine
Typed vector API layered on KV. Records encode a header (`{dims:u16}{dtype:u8}{data}`)
into the KV value. Search supports flat SIMD scan (cosine, dot, euclidean) and
HNSW approximate search for large collections. Removing the vector module does
not break KV compilation or tests.

### TextIndex / TextEngine
Full-text search API (v2) layered on KV. Tokenization produces posting lists
stored in a single merged inverted index per namespace (key `__index__` in the
synthetic text namespace). `index_text` incrementally updates this merged index
(read-modify-write). `search_text` reads the single merged index directly —
O(1) deserialize, not O(N) per-document micro-index merging. BM25 scoring,
faceting, and typo-tolerant search (1-edit Levenshtein) are supported. The merged
index is a regular KV record, so compaction handles it naturally.

---

## Data Flows

### Write Path

```
put(key, value, ttl?)
  │
  ▼
append WAL record (LZ4, CRC32C)
  │
  ▼
insert into Memtable (BTreeMap)
  │
  ▼ (flush triggered by size threshold)
write sorted run → segment-{id}.dat (ZSTD blocks)
build sparse index → segment-{id}.idx
build xor filter   → segment-{id}.xf
write metadata     → segment-{id}.meta
append to manifest → manifest file
remove flushed keys from memtable
```

### Read Path

```
get(namespace, key)
  │
  ▼
search Memtable first (fastest)
  │   hit → return value
  │   miss → continue
  ▼
for each live segment (newest → oldest):
    xor filter check → if absent, skip segment
    sparse index seek → locate target block
    decompress block → scan for key
    hit → return value with highest LSN
  │
  ▼
not found → return None
```

### Compaction Path

```
compact_once(write_budget)
  │
  ▼
identify cohorts with max(death_time) < now
  │
  ▼ (fully expired cohorts first)
for each selected cohort within budget:
    read all segments in cohort
    merge-sort by key, keep highest-LSN winner
    write new output segments with updated cohort buckets
    recompute Merkle roots + BLAKE3 hashes
    update manifest (append new segments, mark old as obsolete)
    delete obsolete segment files (if not pinned by snapshot)
  │
  ▼
return CompactionStats { segments_in, segments_out, bytes_relocated }
```

---

## File Formats

### WAL Record

| Field | Type | Description |
|-------|------|-------------|
| `txid` | `u64` | Transaction ID (reserved for MVCC) |
| `lsn` | `u64` | Monotonic log sequence number |
| `timestamp` | `i64` | Unix nanoseconds (LWW conflict resolution) |
| `ttl` | `u32` | Seconds until expiry; `0` = none |
| `namespace` | `bytes` | Max 255 bytes |
| `key` | `bytes` | Variable length |
| `operation` | `u8` | `1` = put, `2` = delete |
| `value_hash` | `[u8; 32]` | BLAKE3 of value |
| `value_bytes` | `bytes` | Variable length |

### Segment Files

```
segment-{id:08}.dat   → compressed data blocks (ZSTD level 1, 4 KiB aligned)
segment-{id:08}.idx   → sparse offset index (key → block_offset)
segment-{id:08}.xf    → xor filter (~8 bits/key, static set)
segment-{id:08}.meta  → JSON metadata
```

### Segment Metadata (`.meta`)

| Field | Type | Description |
|-------|------|-------------|
| `segment_id` | `u64` | Monotonic ID |
| `segment_hash` | `[u8; 32]` | BLAKE3 of `.dat` file |
| `min_key` / `max_key` | `bytes` | Key bounds |
| `min_lsn` / `max_lsn` | `u64` | LSN bounds |
| `record_count` | `u64` | Number of KV pairs |
| `compressed_bytes` | `u64` | On-disk size |
| `uncompressed_bytes` | `u64` | Decompressed size |
| `compression` | `string` | `"zstd:1"` |
| `cohort_bucket` | `i64` | Truncated to cohort window |
| `death_time` | `i64` | Max death time (unix nanoseconds) |
| `merkle_root` | `[u8; 32]` | Root of segment Merkle tree |
| `created_at` | `i64` | Flush timestamp |

### Manifest

Append-only JSON-lines file tracking live segments. Each line is a segment
metadata record. Corruption is detected via per-line CRC32C. On startup the
latest valid line is used as the authoritative manifest state. WAL replay
rebuilds the manifest if it is missing or truncated.

---

## Namespace Encoding

EdgeStore uses a single flat keyspace. Namespace isolation is enforced by prefix
encoding at the API layer:

```
Encoded key = { ns_len: u16 }{ ns_bytes }{ key_bytes }
```

```
┌──────────┬────────────┬────────────┐
│ ns_len   │ namespace  │ user key   │
│ (2 bytes)│ (variable) │ (variable) │
└──────────┴────────────┴────────────┘
```

This enables:

- Range scans bounded by namespace prefix
- Prefix queries within a namespace
- Time-series and secondary-index layouts
- No ColumnFamily complexity

---

## Concurrency Model

**Single writer + multiple readers + group commit.**

- One active write transaction at a time.
- Readers never block writers; they snapshot the current manifest.
- Multiple `tx.commit()` calls are batched into a single WAL `fsync`.
- No MVCC in v1. The WAL `txid` field is reserved for future MVCC layering.

For async callers, `edgestore-tokio` wraps every operation in
`tokio::task::spawn_blocking`, keeping the core crate free of runtime dependencies.

---

## Cold Storage & Tiering Patterns

EdgeStore is **local-first**: the `Engine` operates on local SSD/NVMe only. It does not know S3 exists. This is a deliberate architectural boundary — the core crate has zero network dependencies.

`edgestore-repl` provides the **transport primitives** (`RemoteStore` trait, `S3RemoteStore`, `HttpReplicationClient`) that let you move segments between nodes or to S3. What it does **not** provide is a tiering policy: cache eviction, read-through logic, or cold/hot data classification. Those are application concerns.

### What `edgestore-repl` gives you

- `S3RemoteStore::upload(hash, data)` — store a segment in S3
- `S3RemoteStore::download(hash)` — retrieve a segment from S3
- `Engine::import_segment(data, hash)` — LWW-merge a downloaded segment into the local engine
- `Engine::export_manifest()` — list local segments for comparison with a remote manifest

### What it does not give you

- Automatic eviction of local segments to S3
- Transparent `get()` fallback to S3 on cache miss
- A per-namespace secondary index for S3 segment lookup
- Cache warming, prefetch, or LRU policy

### Building your own tiering (Application-level)

If you need transparent read-through (local hot cache + S3 cold archive), the cleanest pattern is a wrapper in your application code:

```rust
use edgestore::{Engine, EdgestoreConfig, EdgestoreError};
use edgestore_repl::S3RemoteStore;
use edgestore::RemoteStore;

pub struct MyTieredEngine {
    local: Engine,
    remote: S3RemoteStore,
    // Your own cache policy, index, eviction logic...
}

impl MyTieredEngine {
    pub fn get(&mut self, ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError> {
        // 1. Try local
        if let Some(v) = self.local.get(ns, key)? {
            return Ok(Some(v));
        }
        // 2. Your logic: find segment hash in S3, download, import, retry
        // 3. Or return None if you choose not to fetch
        Ok(None)
    }
}
```

This keeps `edgestore` pure and lets you control:
- Which namespaces stay local vs. go to S3
- When to fetch (on first miss? on range scan? eagerly?)
- When to evict (LRU, TTL, size threshold)
- Whether to pre-warm before a query

### `edgestore-tier` (Shipped in v1.1)

`edgestore-tier` provides a reference implementation of transparent read-through:

```rust
use edgestore::{Engine, EdgestoreConfig};
use edgestore_repl::S3RemoteStore;
use edgestore_tier::TieredEngine;

let local = Engine::open(EdgestoreConfig::new("/tmp/db")).unwrap();
let remote = S3RemoteStore::new("bucket", Some("prefix/"), None).unwrap();
let mut tiered = TieredEngine::new(local, Box::new(remote));
```

What it does:
- `put`/`delete` — pass through to local engine (hot path unchanged)
- `get` — local first; on miss, scan archived segments by key bounds, download from S3 via `RemoteStore`, import via `Engine::import_segment` (LWW merge), retry
- `archive_segments` — upload a list of local segments to remote and register them for read-through

What it does **not** do:
- Decide when to archive or evict (application policy)
- Background prefetch or warming
- Range scan read-through (local only — call `fetch_all_archived()` first if needed)

It is a thin orchestration layer sitting between `edgestore` (core) and `edgestore-repl` (transport). You can use it directly, wrap it with your own policy, or ignore it and build your own using the same primitives.

---

## References

- **Deep technical spec:** [`prod.md`](../prod.md)
- **Project context & decisions:** [`.planning/PROJECT.md`](../.planning/PROJECT.md)
- **Roadmap & phases:** [`.planning/ROADMAP.md`](../.planning/ROADMAP.md)
- **VLDB 2026 (Lee et al.):** [Deathtime-based GC](https://www.vldb.org/pvldb/vol19/p1469-lee.pdf)
- **VLDB SSD WAF:** [SSD write amplification](https://www.vldb.org/pvldb/vol16/p2769-durner.pdf)
