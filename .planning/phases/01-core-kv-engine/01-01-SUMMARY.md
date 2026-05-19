# Plan 01-01 Summary — Cargo workspace scaffold + core types

**Status:** Complete
**Date:** 2026-05-18

## What was built

- Workspace `Cargo.toml` with `edgestore` member, resolver="2"
- `edgestore/Cargo.toml`: library crate with lz4_flex, zstd, crc32c, byteorder, blake3 dependencies; tempfile in dev-dependencies
- `edgestore/src/lib.rs`: module declarations + re-exports
- `edgestore/src/error.rs`: `EdgestoreError` with 10 variants (Io, Checksum, CorruptRecord, CorruptKey, WalFull, WriterBusy, InvalidOperation, NamespaceTooLong, KeyNotFound, FormatVersion)
- `edgestore/src/types.rs`: Lsn, Operation, MemEntry, WalRecord, Compression + encode_key/decode_key
- `edgestore/src/config.rs`: EdgestoreConfig with 7 fields + defaults (memtable_factory added in plan 01-03)
- Stub files: memtable.rs, wal.rs, engine.rs, transaction.rs, recovery.rs

## Key decisions recorded

- WalRecord.value_hash is `[u8; 32]` between `op` and `value_bytes` — permanent on-disk format
- Namespace key encoding: `{ns_len:u16 BE}{ns_bytes}{key_bytes}` via encode_key/decode_key
- No async, no tokio in edgestore crate
- EdgestoreConfig derives Clone (required for test helpers)

## Interfaces exported

- `EdgestoreError` — all error variants downstream plans use
- `EdgestoreConfig` — 7 fields, new(path) constructor
- `Lsn = u64`
- `Operation { Put = 1, Delete = 2 }`
- `MemEntry { key, value, op, lsn, timestamp, ttl }`
- `WalRecord { txid, lsn, timestamp, ttl, ns_len, ns_bytes, key_bytes, op, value_hash:[u8;32], value_bytes }`
- `Compression { Lz4, Zstd(u32) }`
- `encode_key(ns, key) -> Vec<u8>`
- `decode_key(encoded) -> Result<(Vec<u8>, Vec<u8>), EdgestoreError>`

## Verification results

- `cargo build --workspace`: exits 0
- `cargo test -p edgestore types::`: all 3 unit tests pass
- `cargo clippy -p edgestore -- -D warnings`: exits 0
- error.rs grep count for 5 variants: 10 matches (each appears in enum definition + match arm)
- config.rs grep count for 5 fields: 10 matches (each appears in struct definition + constructor)
- types.rs value_hash field: confirmed present as `[u8; 32]` with BLAKE3 comment
