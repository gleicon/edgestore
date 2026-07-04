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
| **SSD optimization** | `edgestore` | ✅ v1.0 | FDP placement hints, deathtime-cohort WAF≈1 |

---

## Crate Overview

EdgeStore is organized as a Cargo workspace. Each crate has a distinct scope:

| Crate | What it is | What it is NOT |
|-------|-----------|----------------|
| **`edgestore`** | Core sync KV + vector + text engine. `Engine`, `WAL`, `SegmentStore`, `Compactor`. | Not a server. Not async. |
| **`edgestore-tokio`** | Async wrapper. Every call runs in `tokio::task::spawn_blocking`. Thin layer. | Does not re-implement storage logic. |
| **`edgestore-repl`** | **REPLication** transport — HTTP client/server + `RemoteStore` backends (filesystem, S3). Network and durability tier. | **Not a REPL shell.** "repl" = replication, not read-eval-print-loop. |
| **`edgestore-cli`** | Administrative CLI binary. `put`, `get`, `compact`, `stats`, `export`, `import`. | Not a database server. Not a SQL client. |

The core crate (`edgestore`) is **library-first**: you embed it in your application, call `Engine::open()`, and use it directly. No daemon, no port binding, no async runtime required.

`edgestore-repl` adds network and remote durability when you need it — Merkle delta sync between nodes, HTTP replication, or S3 cold archive. It is an **optional addition**, not a requirement.

## Installation

Add to `Cargo.toml`:

```toml
[dependencies]
edgestore = "1.0"
```

Or run:

```bash
cargo add edgestore
```

Optional async wrapper:

```toml
edgestore-tokio = "1.0"
```

Optional replication transport:

```toml
edgestore-repl = "1.0"
```

With S3 backend (adds `aws-sdk-s3` + `tokio`):

```toml
edgestore-repl = { version = "1.0", features = ["s3"] }
```

Minimum supported Rust version: **1.95.0**.

### CLI Installation

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

## Architecture Overview

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
│  Engine (single writer + group commit)                    │
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
│  SSD / NVMe / S3 (via StorageBackend trait)                 │
│    • Deathtime-cohort compaction → WAF → 1.0                │
│    • FDP placement hints on supported hardware              │
└─────────────────────────────────────────────────────────────┘
```

For a deep dive, see [ARCHITECTURE.md](ARCHITECTURE.md).

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
