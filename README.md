# EdgeStore

[![CI](https://github.com/gleicon/edgestore/workflows/CI/badge.svg)](https://github.com/gleicon/edgestore/actions)
[![Crates.io](https://img.shields.io/crates/v/edgestore.svg)](https://crates.io/crates/edgestore)
[![docs.rs](https://docs.rs/edgestore/badge.svg)](https://docs.rs/edgestore)

**Local-first embedded KV + vector database in Rust.**

EdgeStore is an SSD-aware, append-only embedded database for edge deployments.
It pairs local NVMe fast-path writes with S3-safe recovery,
and uses **deathtime-cohort compaction** (VLDB 2026) to drive device write
amplification toward 1.0 — no existing embedded database does this.

Library-first. No mandatory server. No mandatory async runtime.

---

## Quick Start

```rust
use edgestore::{EdgestoreConfig, Engine};

let config = EdgestoreConfig::new("/tmp/mydb");
let mut db = Engine::open(config)?;

db.put(b"default", b"hello", b"world")?;
let value = db.get(b"default", b"hello")?;
assert_eq!(value, Some(b"world".to_vec()));

db.flush()?; // WAL fsync + optional memtable flush
```

See [`edgestore/examples/`](edgestore/examples/) for runnable examples (KV, vector search, replication).

For a rich documentation site with feature guides and paper references, open [`website/index.html`](website/index.html) in your browser.

---

## Choose Your Crate

EdgeStore is a Cargo workspace. Most users need only the first crate.

| I want... | Crate | Add to `Cargo.toml` |
|-----------|-------|---------------------|
| A local embedded database (sync, no network deps) | `edgestore` | `edgestore = "1.0"` |
| The same, but async in a Tokio app | `edgestore` + `edgestore-tokio` | `edgestore-tokio = "1.0"` |
| Replication between nodes via HTTP or S3 | `edgestore-repl` | `edgestore-repl = "1.0"` |
| An admin command-line tool | `edgestore-cli` | `cargo install edgestore-cli` |

**`edgestore-repl` is optional.** The core `edgestore` crate has zero network
dependencies and zero async runtime dependencies. It is a library you embed
and call directly — no daemon, no port binding, no server process.

### Crate details

| Crate | Scope |
|-------|-------|
| **`edgestore`** | Core engine: `Engine`, WAL, `SegmentStore`, `Compactor`, vector search, full-text search. Pure sync. |
| **`edgestore-tokio`** | Thin async wrapper. Every call runs inside `tokio::task::spawn_blocking`. No storage logic duplicated. |
| **`edgestore-repl`** | Replication transport: HTTP client/server, anti-entropy loop, `RemoteStore` implementations (filesystem, S3). |
| **`edgestore-tier`** | Tiered storage: local hot cache + transparent read-through to S3 cold archive. Optional — only if your data exceeds local disk. |
| **`edgestore-cli`** | Administrative binary: `create`, `put`, `get`, `compact`, `stats`, `export`, `import`. |

---

## Feature Matrix

| Feature | Crate | Status | Notes |
|---------|-------|--------|-------|
| **KV store** (put/get/delete/range/prefix) | `edgestore` | ✅ v1.0 | Ordered byte keys, namespaced |
| **Transactions** (begin/commit/rollback) | `edgestore` | ✅ v1.0 | Single-writer, group commit |
| **TTL / Lazy expiry** | `edgestore` | ✅ v1.0 | `put_with_ttl`; expired data removed at compaction |
| **Snapshots** | `edgestore` | ✅ v1.0 | RAII point-in-time reads |
| **Vector search** (flat SIMD) | `edgestore` | ✅ v1.0 | Cosine, dot, euclidean; f32/f16/i8 |
| **HNSW index** | `edgestore` | ✅ v1.0 | Approximate search for large collections |
| **Full-text search** (BM25) | `edgestore` | ✅ v1.0 | Tokenization, faceting, typo tolerance |
| **Replication** (Merkle delta sync) | `edgestore-repl` | ✅ v1.0 | Transport-agnostic; HTTP + S3 backends |
| **S3 cold storage** | `edgestore-repl` | ✅ v1.0 | Archive + replication mailbox (`s3` feature) |
| **Tiered storage** (local + S3 read-through) | `edgestore-tier` | ✅ v1.1 | Transparent fallback to S3 on cache miss |
| **SSD optimization** | `edgestore` | ✅ v1.0 | FDP placement hints, deathtime-cohort WAF≈1 |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Application                                                │
│    ┌──────────────┐  ┌──────────────┐  ┌──────────────┐   │
│    │   KV API     │  │ Vector API   │  │  Text API    │   │
│    └──────┬───────┘  └──────┬───────┘  └──────┬───────┘   │
└───────────┼─────────────────┼─────────────────┼───────────┘
            │                 │                 │
            └─────────────────┼─────────────────┘
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  Engine (single writer + group commit)                      │
│    • Transactions, namespace isolation, LWW conflict res.   │
└─────────────────────┬───────────────────────────────────────┘
                      │ writes batches
                      ▼
┌─────────────────────────────────────────────────────────────┐
│  WAL (LZ4, CRC32C)                    Memtable (BTreeMap)   │
│  • Append-only, rotated at 64 MB / 60 s  • In-memory buf   │
│  • Crash recovery source                 • Flushed → segment│
└─────────────────────────────────────────────────────────────┘
                      │ flushes
                      ▼
┌─────────────────────────────────────────────────────────────┐
│  Segment Store                                              │
│    • Immutable SSTables (ZSTD L1, 4 KiB blocks, 16 MB)      │
│    • Sparse index + xor filter + BLAKE3 content addressing  │
│    • Manifest: live segment tracking, Merkle roots           │
└─────────────────────┬───────────────────────────────────────┘
                      │
                      ▼
┌─────────────────────────────────────────────────────────────┐
│  Local Storage (SSD / NVMe)                                 │
│    • Deathtime-cohort compaction → WAF → 1.0                │
│    • FDP placement hints on supported hardware              │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│  Optional: edgestore-repl (network layer)                   │
│    • HTTP replication client + server                        │
│    • S3 / filesystem RemoteStore backends                    │
│    • Merkle delta sync, anti-entropy loop                    │
└─────────────────────────────────────────────────────────────┘
```

For a deep dive, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## Using S3 (Optional)

Only needed if you are replicating segments to S3 or using S3 as a cold archive.
Add the `s3` feature to `edgestore-repl`:

```toml
[dependencies]
edgestore-repl = { version = "1.0", features = ["s3"] }
```

```rust
use edgestore::RemoteStore;
use edgestore_repl::S3RemoteStore;

let store = S3RemoteStore::new(
    "my-bucket",           // S3 bucket name
    Some("mydb/"),         // optional key prefix
    None,                  // None for AWS; Some("http://localhost:4566") for LocalStack
).expect("S3RemoteStore::new");

store.upload(&hash, &data)?;
let bytes = store.download(&hash)?;
```

### S3 path layout

```
s3://{bucket}/{prefix}segments/{blake3_hash_hex}.dat
```

`{blake3_hash_hex}` is the 64-character lowercase hex encoding of the 32-byte BLAKE3 hash.

### What `edgestore-repl` does and does not do

**It does:** provide `upload`/`download`/`list`/`delete` primitives so you can move segments to/from S3.

**It does not:** implement tiering policy, cache eviction, or transparent `get()` fallback to S3. For that, use `edgestore-tier` below.

### Environment variables

| Variable | Purpose | Example |
|----------|---------|---------|
| `AWS_ACCESS_KEY_ID` | AWS access key | `AKIA...` |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key | `...` |
| `AWS_DEFAULT_REGION` | AWS region | `us-east-1` |
| `EDGESTORE_S3_ENDPOINT_URL` | Custom endpoint (LocalStack, MinIO) | `http://localhost:4566` |
| `EDGESTORE_S3_BUCKET` | Bucket name for tests | `edgestore-test` |

### LocalStack testing

```bash
make s3-test
```

This starts a LocalStack container, runs all S3 integration tests, and tears it down.

---

## Tiered Storage (Optional)

Use `edgestore-tier` when your dataset exceeds local disk and you need transparent read-through to S3:

```toml
[dependencies]
edgestore-tier = "1.1"
```

```rust
use edgestore::{Engine, EdgestoreConfig};
use edgestore_repl::S3RemoteStore;
use edgestore_tier::TieredEngine;

let local = Engine::open(EdgestoreConfig::new("/tmp/db")).unwrap();
let remote = S3RemoteStore::new("bucket", Some("prefix/"), None).unwrap();
let mut tiered = TieredEngine::new(local, Box::new(remote));

// Writes go to local only
tiered.put(b"ns", b"key", b"val").unwrap();

// Reads try local first; on miss, check archived segments in S3
tiered.get(b"ns", b"key").unwrap();

// Archive local segments to S3, then delete locally to reclaim space
let metas = tiered.local().list_segment_metas();
tiered.archive_segments(&metas).unwrap();
// (caller deletes local .dat files if desired)
```

`edgestore-tier` is a thin orchestration layer. It does not decide when to archive or evict — those are application-level policies. It simply provides the read-through mechanism using the `RemoteStore` primitives from `edgestore-repl` and the `import_segment` path from `edgestore`.

---

## CLI Installation

The `edgestore-cli` administrative tool can be installed from source:

```bash
# Clone the repository
git clone https://github.com/gleicon/edgestore.git
cd edgestore

# Install locally from source
cargo install --path edgestore-cli

# Or build the optimized release binary
cargo build --release -p edgestore-cli
# Binary will be at: target/release/edgestore-cli
```

The CLI provides commands for:
- Database management: `create`, `stats`, `compact`
- KV operations: `put`, `get`, `delete`, `range`
- Data exchange: `export`, `import` (JSON and binary formats)
- Vector search: `vector-put`, `vector-get`, `vector-search`
- Text search: `text-search`

Run `edgestore-cli --help` for full command reference.

---

## Documentation

- **API reference:** [docs.rs/edgestore](https://docs.rs/edgestore)
- **Architecture & file formats:** [ARCHITECTURE.md](ARCHITECTURE.md)
- **Changelog:** [CHANGELOG.md](CHANGELOG.md)
- **Design spec:** [prod.md](prod.md)

---

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

---

## Contributing

Issues and pull requests are welcome.
Please open an issue or PR on GitHub.

---

*EdgeStore is not affiliated with the VLDB organization. The deathtime-cohort
technique is described in Lee et al., VLDB 2026.*
