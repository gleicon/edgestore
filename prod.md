# EdgeStore — SSD-Aware Local-First KV + Vector Database

## Vision

EdgeStore is a local-first embedded database optimized for SSDs, edge deployments, replication efficiency, and predictable durability.

The system is designed around append-oriented storage, immutable sorted segments, ordered key ranges, deathtime-cohort compaction, and Merkle-based replication.

The architecture intentionally avoids classic in-place page rewrites and mutable BTree storage patterns that increase SSD write amplification, following the principles established in the VLDB 2026 paper (Lee et al.) on out-of-place writes and deathtime-based grouping.

The system prioritizes:

- Fast local reads and writes
- SSD-friendly write behavior (SSD WAF → 1)
- Efficient range scans
- Low write amplification via deathtime-cohort grouping
- Durable crash recovery
- Replication via deltas and Merkle synchronization
- S3-backed cold storage and synchronization
- Small operational footprint
- Embedded library deployment model

---

# Language

**Rust.**

Rationale:
- No GC pauses — deathtime-cohort compaction requires precise control over when segments are released
- Memory safety without runtime overhead
- Excellent SSD/IO control via direct syscall access
- `std::collections::BTreeMap` for memtable (zero dep)
- `xorf` for xor filters
- `lz4` and `zstd` crates are mature
- Sync core with no mandatory async runtime dependency

---

# Deployment Model

**Library-first.** EdgeStore is a Rust crate, linked directly into the application.

- No mandatory server process
- No mandatory runtime dependency
- SQLite/RocksDB embedding model
- Optional thin async wrapper crate (`edgestore-tokio`) for Tokio callers
- Optional CLI binary for administration

---

# v1 Scope

**KV store + vector search.**

Full-text search (Algolia-style) is v2.

The KV layer is pure: ordered byte keys, byte values, namespaced, transactional.
The vector layer sits on top of KV: typed records, flat SIMD scan for ANN search.

---

# Design Principles

## 1. Out-of-place writes only

All updates are append-oriented. Existing pages or records are never rewritten in place. New versions are appended; older versions become obsolete through deathtime-cohort compaction. This aligns write patterns with SSD internals per the VLDB 2026 paper.

## 2. Immutable segments

Persistent storage consists of immutable sorted segment files. Segments are:

- Append-created, never modified
- Compactable within deathtime cohorts
- Replicable as atomic units
- Hashable (content-addressed via BLAKE3)
- Compressible (ZSTD level 1)

## 3. Ordered key space with prefix-encoded namespaces

Keys are stored as: `{ns_len:u16}{ns_bytes}{key_bytes}`

Single flat sorted keyspace. Namespace isolation enforced at API layer via prefix-bounded range scans. No ColumnFamily complexity.

This enables:

- Range scans bounded by namespace prefix
- Prefix queries
- Time-series layouts
- Secondary indexes
- SQL layers later

## 4. WAL-first durability

Every mutation is appended to a WAL before acknowledgment. Recovery reconstructs memtables and manifests from WAL state. WAL uses LZ4 compression.

WAL rotation: **64 MB OR 60 seconds**, whichever comes first. Both configurable.

## 5. Deathtime-cohort compaction

Segments are grouped by predicted invalidation time (death time), not by size tiers or levels. This is the key differentiator from LevelDB/RocksDB and the primary mechanism for achieving low SSD write amplification.

- Records with TTL: `death_time = write_time + ttl`
- Records without TTL: `death_time = write_time + cohort_window` (temporal locality fallback)
- Default cohort window: **1 hour**, configurable at DB open
- Compact fully-expired cohorts first — zero live data relocation

## 6. Replication as a first-class primitive

Replication metadata embedded in storage from day one. Each segment includes Merkle roots, content hashes, LSN ranges, and range metadata. Conflict resolution: **last-write-wins by wall clock timestamp**. LWW limitation documented; vector clock support reserved for v2 via WAL format extension field.

---

# Non-goals (v1)

- Full-text search (v2)
- Full SQL compatibility
- Distributed consensus
- Multi-primary replication
- Automatic CRDT semantics
- Cluster orchestration
- Cross-node transactions
- Query planner / joins
- HNSW vector index (v1.1)

---

# Architecture

```
Application
 |
 | KV API / Vector API
 v
Transaction Layer (single writer + group commit)
 |
 | writes batches
 v
Memtable (BTreeMap behind MemTable trait)
 |
 | flushes immutable sorted runs
 v
Segment Store
 |
 | append-only, ZSTD compressed pages
 | out-of-place writes
 | 4 KiB aligned blocks
 | 16 MB segment target
 v
SSD-optimized local files

Replication Sidecar
 |
 | Merkle tree over key ranges / segments
 | delta exchange
 | S3 object log
 | host-to-host sync
```

---

# Core Components

## 1. WAL (Write-Ahead Log)

### Responsibilities

- Durability
- Crash recovery
- Replication feed
- Ordering source

### Properties

- Append-only, sequential writes
- Checksummed (CRC32C)
- LZ4 compressed
- Segment-rotated: **64 MB OR 60 seconds**

### WAL Record

```
txid        u64
lsn         u64
timestamp   i64   (unix nanoseconds, wall clock — LWW conflict resolution key)
ttl         u32   (optional, seconds; 0 = no expiry)
namespace   bytes (variable, max 255 bytes)
key         bytes (variable)
operation   u8    (put=1, delete=2)
value_hash  [u8; 32]  (BLAKE3)
value_bytes bytes (variable)
```

---

## 2. Memtable

### Responsibilities

- In-memory write buffering
- Ordered writes
- Fast reads before flush

### Implementation

`std::collections::BTreeMap<Vec<u8>, MemEntry>` behind a `MemTable` trait.

Trait interface:

```rust
trait MemTable: Send {
    fn insert(&mut self, key: Vec<u8>, entry: MemEntry);
    fn get(&self, key: &[u8]) -> Option<&MemEntry>;
    fn range(&self, start: &[u8], end: &[u8]) -> impl Iterator<Item = (&[u8], &MemEntry)>;
    fn iter(&self) -> impl Iterator<Item = (&[u8], &MemEntry)>;
    fn len(&self) -> usize;
}
```

Swap implementation by providing alternate `MemTable` impl at DB open. No generics leaking into call sites — `Box<dyn MemTable>` at engine level.

Flushed into immutable segment when memtable reaches flush threshold. Rebuilt from WAL during recovery.

---

## 3. Immutable Segments

### Responsibilities

- Persistent storage
- Ordered key ranges
- Range scanning
- Replication unit
- Deathtime cohort membership

### Segment Structure

```
segment-{id:08}.dat    compressed data blocks (ZSTD level 1, 4 KiB aligned)
segment-{id:08}.idx    sparse offset index (every N keys)
segment-{id:08}.xf     xor filter (built at flush, ~8 bits/key)
segment-{id:08}.meta   metadata (JSON, human-readable)
```

### Metadata

```
segment_id       u64
segment_hash     [u8; 32]   BLAKE3 of .dat file
min_key          bytes
max_key          bytes
min_lsn          u64
max_lsn          u64
record_count     u64
compressed_bytes u64
uncompressed_bytes u64
compression      string     "zstd:1"
cohort_bucket    i64        unix timestamp truncated to cohort window
death_time       i64        max death_time across all records (unix nanoseconds)
merkle_root      [u8; 32]
created_at       i64
```

Target size: **16 MB**, configurable.

---

## 4. Sparse Index

Sparse offsets every N keys within `.idx` file:

```
key_a -> block_offset_1
key_b -> block_offset_2
key_c -> block_offset_3
```

Enables fast range seeks and block lookups with minimal read amplification.

---

## 5. Xor Filter

Per-segment probabilistic filter built once at flush time using the `xorf` crate.

- ~8 bits/key (more space-efficient than bloom or cuckoo)
- Faster lookup than bloom (fixed 3 memory accesses)
- Truly static — perfect for immutable segments
- Default FPR: **1%**, configurable at DB open
- Stored in `.xf` file alongside segment

No bloom filters. No cuckoo filters. Segments are immutable — xor filter is the correct primitive.

---

## 6. Deathtime-Cohort Compaction

### Cohort Assignment

On segment flush:
- Records with TTL: `cohort_bucket = floor((write_time + ttl) / cohort_window)`
- Records without TTL: `cohort_bucket = floor(write_time / cohort_window)`
- Default `cohort_window`: **1 hour**

### Compaction Trigger

1. Identify cohorts where `now > max(death_time)` across all segments in cohort — 100% dead, compact first (zero live data relocation)
2. Then compact partially-expired cohorts by dead-record ratio
3. Compaction is incremental and bounded — never exceeds configured write budget per cycle

### Constraints

- Never rewrite in place
- Output segments get new cohort assignments based on surviving record death times
- Snapshots pin segments against compaction
- Merkle roots recomputed after compaction

---

## 7. Merkle Trees

### Levels

**Segment-level Merkle**: determines whether two hosts share identical segments.

**Range-level Merkle**: tree over key ranges `[a-f] → hash`, `[g-m] → hash`, etc. Lets two hosts compare trees and exchange only changed ranges.

### Responsibilities

- Delta synchronization
- Replication comparison
- Integrity verification

---

# Concurrency Model

**Single writer + multiple readers + group commit.**

- One write transaction active at a time
- Readers never block writers (snapshot of current segment manifest)
- Multiple `tx.Commit()` calls batched into one WAL fsync (group commit)
- No MVCC, no version chains, no OCC retry logic

This is the correct model for an embedded local-first library. MVCC can be layered later without on-disk format changes (WAL `txid` field already present).

---

# API

## Concurrency

Sync core. No Tokio dependency in `edgestore` crate. Async callers use `edgestore-tokio` wrapper (`spawn_blocking` internally).

## KV API (v1)

```rust
db.put(namespace, key, value) -> Result<()>
db.put_with_ttl(namespace, key, value, ttl_secs) -> Result<()>
db.get(namespace, key) -> Result<Option<Value>>
db.delete(namespace, key) -> Result<()>
db.range(namespace, start, end) -> Result<impl Iterator<Item = (Key, Value)>>
db.prefix(namespace, prefix) -> Result<impl Iterator<Item = (Key, Value)>>

tx = db.begin() -> Transaction
tx.put(namespace, key, value)
tx.put_with_ttl(namespace, key, value, ttl_secs)
tx.delete(namespace, key)
tx.commit() -> Result<Lsn>
tx.rollback()

db.snapshot() -> Result<Snapshot>
db.export_manifest() -> Result<Manifest>
db.import_segment(path) -> Result<()>
db.compare_merkle(root) -> Result<MerkleDiff>
```

## Vector API (v1)

Vectors sit on top of KV. The KV layer stores opaque bytes. The vector API encodes/decodes a typed header before writing.

**Vector record format** (encoded into KV value bytes):
```
dims    u16     number of dimensions
dtype   u8      0=f32, 1=f16, 2=i8
data    bytes   raw packed array
```

```rust
db.vector_put(namespace, key, dims, dtype, data: &[u8]) -> Result<()>
db.vector_get(namespace, key) -> Result<Option<VectorRecord>>
db.vector_search(namespace, query: &[f32], k: usize, metric: Metric) -> Result<Vec<(Key, f32)>>
```

`vector_search` uses flat SIMD scan for v1. HNSW index added in v1.1.

Supported metrics: cosine, dot product, euclidean.

## Later API (v2+)

```
db.search(namespace, query) -> full-text search results
db.index_text(namespace, key, text)
CREATE TABLE / INSERT / SELECT (SQL layer or SQLite virtual table)
```

---

# Storage Format Strategy

## SSD-Aware Alignment

All data blocks aligned to **4 KiB** logical boundaries. Compressed records packed into aligned blocks — variable-size compression never creates arbitrary-offset reads. From VLDB 2026: careless variable-sized writes cause read amplification; page packing into 4 KiB-aligned reads is the correct pattern.

## Compression

| Layer    | Algorithm    | Rationale |
|----------|-------------|-----------|
| WAL      | LZ4         | Lowest write latency on hot path |
| Segments | ZSTD level 1 | Written once, better ratio reduces device WAF |

Both configurable at DB open.

---

# File Format Requirements

All on-disk structures must be:

- Checksummed (CRC32C for blocks, BLAKE3 for segment content addressing)
- Versioned (format version header on every file)
- Deterministic
- Recoverable
- Language-neutral (no Rust-specific encoding)

No format may depend on in-memory structures.

---

# Replication Model

## Goals

- Low bandwidth
- Efficient synchronization
- Delta-only transfers
- Content-addressed segment exchange

## Conflict Resolution

Last-write-wins by `timestamp` (wall clock, unix nanoseconds). Documented limitation: clock skew can cause lost updates on concurrent cross-node writes. Vector clock support reserved for v2 — WAL record includes reserved extension field for future use.

## Synchronization Flow

```
Compare manifests
Compare Merkle roots at range level
Identify missing/different ranges
Transfer missing segments (content-addressed by BLAKE3 hash)
Apply locally
Update manifest
```

## Protocol (transport-agnostic)

```
ListManifests()
GetManifest(host_id)
CompareMerkle(root)
RequestDelta(range, since_lsn)
SendSegments(segment_hashes)
Ack(lsn)
```

Transport implementations (in order of priority): HTTP, gRPC, S3 polling, QUIC.

---

# S3 Integration

S3 is not the primary database path.

S3 is used for:

- Cold segment storage
- Replication mailbox
- Snapshot storage
- Delta archive
- Disaster recovery

## Layout

```
s3://bucket/dbname/hosts/{host_id}/manifests/latest.json
s3://bucket/dbname/segments/{segment_hash}.dat
s3://bucket/dbname/wal/{host_id}/{lsn}.log
s3://bucket/dbname/snapshots/{snapshot_id}/manifest.json
```

---

# Configuration

Key parameters exposed at DB open:

```rust
EdgestoreConfig {
    path: PathBuf,
    segment_size_bytes: u64,          // default: 16 MB
    cohort_window_secs: u64,          // default: 3600 (1 hour)
    wal_max_bytes: u64,               // default: 64 MB
    wal_max_age_secs: u64,            // default: 60
    bloom_fpr: f64,                   // default: 0.01 (1%)
    compression_segments: Compression, // default: Zstd(1)
    compression_wal: Compression,      // default: Lz4
    memtable: Box<dyn MemTable>,       // default: BTreeMap
}
```

---

# Test Suite

## 1. Format Tests

- WAL encode/decode (LZ4)
- Segment encode/decode (ZSTD)
- Manifest parsing
- Xor filter serialize/deserialize
- Merkle serialization
- Backward compatibility
- Corruption detection
- Truncation handling

## 2. Crash Recovery Tests

Crash at every critical point:

- Before WAL fsync
- After WAL fsync
- During memtable flush
- During segment creation
- During manifest update
- During deathtime-cohort compaction

Verify:
- No acknowledged writes lost
- Partial writes safely ignored
- Recovery deterministic

## 3. Property Tests

Randomized operations against in-memory reference implementation:

```
put  put_with_ttl  delete  range  prefix  snapshot  compact  crash  recover  replicate
```

## 4. Range Query Tests

- Empty ranges
- Prefix ranges
- Large scans
- Deleted keys
- Overlapping segments
- Shadowed keys
- Cross-namespace isolation

## 5. Deathtime-Cohort Compaction Tests

- Cohort bucket assignment correctness
- Fully-expired cohorts collect with zero live relocation
- Partially-expired cohorts collect by dead-record ratio
- Snapshots pin segments against compaction
- Merkle roots correct after compaction
- Latest value wins within cohort
- Tombstones durable across compaction

## 6. Replication Tests

- Empty sync
- Incremental sync
- Interrupted transfer
- Duplicate segments
- Missing segments
- S3-only recovery
- Manifest rollback
- Merkle mismatch repair
- LWW conflict resolution under clock skew

## 7. Performance Tests

Benchmark matrix:

- 100% writes / 100% reads / 50-50 mixed
- Range scans / prefix scans
- Time-series append (natural cohort alignment)
- Hot-key workloads
- Zipfian workloads
- Large values / small values
- Vector flat scan (10K / 100K / 500K vectors)

Track:

- OPS/sec
- p50/p95/p99 latency
- Logical bytes written
- Physical bytes written
- Write amplification factor (logical / physical)
- SSD device WAF (target: approach 1.0)
- Read amplification
- Recovery time
- Compaction debt
- Space amplification

## 8. SSD-Aware Tests

- Sequentiality of writes
- fsync frequency
- Segment size distribution
- Deathtime-cohort compaction write amplification
- Device write amplification at steady state

Tests must run long enough to exceed device capacity multiple times to reach steady-state SSD behavior.

## 9. Fault Injection

- Short writes
- fsync failures
- Manifest corruption
- WAL truncation
- Segment truncation
- Out-of-disk conditions
- Permission failures
- S3 timeout
- Clock skew (LWW correctness under skew)

## 10. Replication Chaos Tests

- Node pause / kill
- Delayed messages
- Duplicate messages
- Partitioned nodes
- Delayed S3 visibility
- Concurrent cross-node writes (LWW resolution)

Verify convergence and durability.

## 11. Vector Tests

- Flat scan correctness vs brute-force reference
- SIMD vs scalar parity
- All three metrics (cosine, dot, euclidean)
- Typed header encode/decode (f32, f16, i8)
- Dimension validation on insert
- Large collections (100K+ vectors)

---

# Milestones

## Milestone 1 — Local KV engine

- WAL (LZ4, 64 MB / 60s rotation)
- Memtable (`BTreeMap` behind `MemTable` trait)
- KV API (sync)
- Single-writer + group commit
- Crash recovery

## Milestone 2 — Segment store

- Immutable segments (ZSTD level 1, 16 MB target)
- Prefix-encoded namespace keys
- Sparse index
- Xor filter per segment (`xorf`)
- Manifest file

## Milestone 3 — Deathtime-cohort compaction

- TTL field in WAL record and API
- Cohort bucket assignment (1-hour default)
- Incremental compaction within cohorts
- Range scans
- Snapshots

## Milestone 4 — Merkle + Replication

- Segment-level Merkle trees
- Range-level Merkle trees
- Host-to-host delta sync
- S3 snapshot and segment exchange
- LWW conflict resolution

## Milestone 5 — Vector search (v1)

- Vector record typed header (dims, dtype, data)
- `vector_put` / `vector_get` / `vector_search`
- Flat SIMD scan (cosine, dot, euclidean)
- Vector benchmark suite

## Milestone 6 — SSD optimization + HNSW (v1.1)

- SSD block alignment verification
- Compression tuning (LZ4/ZSTD profiles)
- Temporal grouping optimization
- HNSW index for vector search
- `edgestore-tokio` async wrapper crate

## Milestone 7 — Full-text search (v2)

- Tokenizer + stemmer pipeline
- Inverted index (BM25, postings compression)
- Per-segment posting lists merged during compaction
- `db.index_text` / `db.search` API

---

# Final Architectural Direction

EdgeStore must remain:

- SSD-aware (out-of-place writes, deathtime-cohort compaction, 4 KiB alignment)
- Append-oriented (no in-place rewrites, ever)
- Range-efficient (prefix-encoded ordered keyspace)
- Replication-native (Merkle metadata in every segment from day one)
- KV-pure (vector and search layers sit on top, never pollute the KV core)
- Library-first (no mandatory server, no mandatory async runtime)

Must never regress into:

- Mutable page rewrites
- Traditional in-place BTree storage
- Size-tiered or level-based compaction (use deathtime-cohort)
- Small random-write patterns
- Excessive compaction amplification
- Mandatory server process

---

# References

- https://www.vldb.org/pvldb/vol19/p1469-lee.pdf — VLDB 2026: out-of-place writes, deathtime-based GC, 7.8× write reduction (primary design reference)
- https://www.vldb.org/pvldb/vol16/p2769-durner.pdf — SSD write amplification analysis
- https://github.com/slatedb/slatedb — S3-backed LSM reference
- https://www.eloqdata.com/blog/2025/10/24/how-nvme-and-s3-reshape-decoupling — NVMe + S3 decoupled storage architecture
