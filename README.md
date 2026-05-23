# EdgeStore

Local-first embedded KV database in Rust. Append-oriented, SSD-aware, single-writer, library-only.

No mandatory server process. Embed it like SQLite or RocksDB.

**Status:** Core KV engine, WAL, memtable, immutable segment store, deathtime-cohort compaction, and point-in-time snapshots are complete (Phases 1–3 + 4.1). Replication and vector search are in progress.

---

## Design

### Append-only, never in-place

All writes append to a WAL. The WAL is periodically flushed into immutable sorted segment files. Existing segments are never modified. Old versions of a key coexist with new versions until compaction removes them. This maps cleanly to SSD write patterns: sequential appends, no random overwrites, write amplification factor approaching 1.

### Deathtime-cohort compaction

Records carry a `death_time` derived from their creation timestamp and TTL. At compaction time, records are grouped by cohort bucket (1-hour window by default). When all records in a cohort have expired, the entire cohort's segments are collected without relocating any live data — zero live-record amplification for fully-expired cohorts. Live records are relocated only when a cohort is partially expired. This is the core insight from Lee et al. (VLDB 2026): grouping by expected death time, not by size, drives write amplification to near zero.

### Namespaces

Keys are prefix-encoded as `{ns_len:u16}{ns_bytes}{key_bytes}`. Namespace separation is logical, not structural — no per-namespace files or column families. All namespaces share one WAL and one segment pool. Prefix-encoded namespaces produce adjacent key ranges, making cross-namespace range scans impossible by construction.

### Probabilistic filters

Each segment carries an xor filter (not bloom) over its key set. Point reads that miss the filter skip the segment entirely without touching disk. False positive rate defaults to 1%.

### Content addressing

Segment files are content-addressed with BLAKE3. The manifest stores per-segment hashes for integrity verification and future Merkle-based replication.

### Lazy expiry

TTL-expired records are readable until `compact_once` removes their cohort. There are no per-read TTL checks. This keeps the hot read path branchless with respect to TTL.

---

## Quick Start

```toml
[dependencies]
edgestore = { path = "edgestore" }
```

```rust
use edgestore::{EdgestoreConfig, Engine};

let config = EdgestoreConfig::new("./mydb");
let mut engine = Engine::open(config)?;

engine.put(b"users", b"alice", b"active")?;
let val = engine.get(b"users", b"alice")?; // Some(b"active".to_vec())

engine.flush()?; // fsync WAL
```

Run the persistent demo:

```sh
cargo run --example demo   # run multiple times to observe accumulating state
```

Each execution opens the same `./edgestore_demo.db`, writes new records, and reads back the full history. On every third run it flushes the memtable to immutable segments; snapshots taken after a flush see the new data.

---

## API Reference

### Engine

```rust
Engine::open(config: EdgestoreConfig) -> Result<Engine, EdgestoreError>
```
Opens or creates a database at `config.path`. Acquires an exclusive write lock. Recovers from all WAL files present, sorted by LSN.

```rust
engine.put(ns: &[u8], key: &[u8], val: &[u8]) -> Result<Lsn, EdgestoreError>
engine.put_with_ttl(ns: &[u8], key: &[u8], val: &[u8], ttl_secs: u32) -> Result<Lsn, EdgestoreError>
engine.delete(ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError>
```
Appends to WAL and updates memtable. Returns the log sequence number. `put_with_ttl` sets a death time `now + ttl_secs`; the record remains readable until compaction collects its cohort.

```rust
engine.get(ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError>
```
Reads from memtable first, then segments (newest-first). Returns the highest-LSN live value.

```rust
engine.range(ns: &[u8], start: &[u8], end: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError>
```
Returns keys in `[start, end)` — exclusive end, matching standard range conventions. Merges memtable and segment results, LWW by LSN.

```rust
engine.prefix(ns: &[u8], prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError>
```
Returns all keys beginning with `prefix`. Uses an exclusive upper bound computed by incrementing the last non-`0xFF` byte.

```rust
engine.flush(&mut self) -> Result<(), EdgestoreError>
```
Fsyncs the current WAL file. Does not create a segment.

```rust
engine.flush_to_segments(&mut self) -> Result<SegmentMeta, EdgestoreError>
```
Serializes the entire memtable into a new immutable segment file (ZSTD compressed, xor filter, BLAKE3 hash), registers it in the manifest, and clears the memtable. After this call, snapshots will see the new data.

```rust
engine.compact_once(&mut self) -> Result<CompactionStats, EdgestoreError>
```
Runs one bounded compaction cycle. Collects fully-expired cohorts, partially rewrites mixed cohorts (LWW merge + dead-record filter), respects `compaction_write_budget_bytes`, skips pinned segments. Reloads the segment store from the manifest after completion.

```rust
engine.snapshot(&self) -> Result<Snapshot, EdgestoreError>
```
Returns a `Snapshot` that pins the current set of segment IDs. The snapshot reads from those segments only — writes after the snapshot was taken are not visible. Pins are released when the `Snapshot` is dropped.

### Transactions

```rust
let mut tx = engine.begin();
tx.put(ns, key, val, 0, 0)?;
tx.put_with_ttl(ns, key, val, ttl_secs, 0, 0)?;
tx.delete(ns, key, 0, 0)?;
engine.commit_transaction(tx)?;  // or: tx.commit(&mut engine)?
```
All operations in a transaction are written to WAL as a group and fsynced together. Commit is atomic. Rollback discards pending records without writing anything. A transaction that is neither committed nor rolled back before drop will leave pending records silently discarded.

### Snapshot

```rust
let snap = engine.snapshot()?;
snap.get(ns: &[u8], key: &[u8]) -> Result<Option<Vec<u8>>, EdgestoreError>
snap.range(ns: &[u8], start: &[u8], end: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, EdgestoreError>
```
Reads from the segment set that was pinned at snapshot creation time. Memtable writes after `engine.snapshot()` are invisible. Segments are not garbage-collected while any snapshot holds a pin.

---

## Configuration

```rust
EdgestoreConfig::new(path)
```

| Field | Default | Description |
|---|---|---|
| `wal_max_bytes` | 64 MiB | WAL file size threshold; rotation creates a new WAL file |
| `wal_max_age_secs` | 60 s | WAL age threshold for rotation |
| `segment_size_bytes` | 16 MiB | Target size for flushed segment files |
| `cohort_window_secs` | 3600 s | Cohort bucket width for deathtime grouping |
| `compression_wal` | LZ4 | WAL frame compression |
| `compression_segments` | ZSTD(1) | Segment block compression |
| `xor_filter_fpr` | 0.01 | Xor filter false positive rate |
| `compaction_write_budget_bytes` | 256 MiB | Max bytes written per `compact_once` call |

---

## References

- Lee et al., **"Deathtime-Based Grouping for Out-of-Place Key-Value Stores"**, VLDB 2026, Vol. 19(6): 1469–1482. https://www.vldb.org/pvldb/vol19/p1469-lee.pdf — primary design reference; deathtime-cohort compaction, 7.8× write amplification reduction vs. size-tiered.
- Durner et al., **"The SSD Survival Guide"**, VLDB 2023, Vol. 16(11): 2769–2782. https://www.vldb.org/pvldb/vol16/p2769-durner.pdf — SSD write amplification analysis and workload classification.
- SlateDB. https://github.com/slatedb/slatedb — S3-backed LSM reference implementation.
- EloqData, **"How NVMe and S3 Reshape Decoupled Storage"**, 2025. https://www.eloqdata.com/blog/2025/10/24/how-nvme-and-s3-reshape-decoupling

---

## License

MIT
