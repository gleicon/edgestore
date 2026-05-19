# Plan 01-04 Summary — KV Engine

**Status:** Complete
**Date:** 2026-05-18

## What was built

- `edgestore/src/engine.rs`: Full KV Engine with single-writer lock and all KV API methods
- `edgestore/Cargo.toml`: Added fs2 dependency

## Interfaces exported

- `Engine::open(config) -> Result<Engine>`
  - Creates DB dir, acquires OS-exclusive lockfile, creates WAL, creates memtable
  - Returns WriterBusy if another process holds the lock
  - On reopen after drop: detects existing WAL and calls `WalWriter::open` (append) instead of `WalWriter::create`
- `Engine::put(ns, key, val) -> Result<Lsn>` — appends WAL, inserts memtable
- `Engine::put_with_ttl(ns, key, val, ttl_secs) -> Result<Lsn>`
- `Engine::get(ns, key) -> Result<Option<Vec<u8>>>`
- `Engine::delete(ns, key) -> Result<Lsn>` — tombstone in WAL + memtable
- `Engine::range(ns, start, end) -> Result<KvPairs>`
- `Engine::prefix(ns, prefix) -> Result<KvPairs>`
- `Engine::flush() -> Result<()>` — fsync WAL (group commit point)

## Key behaviors

- Lockfile at config.path/LOCK; OS-enforced exclusive lock via fs2
- WAL hardcoded to wal-0000000000000000.log — plan 01-06 replaces with scanning+recovery
- txid=0 for direct puts — plan 01-05 sets real txids for transactions
- value_hash computed via blake3::hash(val).into()
- get() returns None for Delete tombstones
- range/prefix filter out Delete entries and decode namespace prefix from keys
- `KvPairs` type alias used to satisfy clippy::type_complexity

## Clippy fixes applied

- `#[allow(dead_code)]` on `config`, `txid_counter`, `lockfile` fields (reserved for future plans)
- `.truncate(false)` on lockfile OpenOptions (satisfies clippy::suspicious_open_options)
- `type KvPairs = Vec<(Vec<u8>, Vec<u8>)>` alias for `range` and `prefix` return types

## Verification

- All 25 unit tests pass (7 engine + 18 pre-existing)
- cargo clippy -D warnings: 0 errors
- cargo build: 0 errors
- 7 KV API methods present (put, put_with_ttl, get, delete, range, prefix, flush)
