# Plan 01-06 Summary — Crash Recovery

**Status:** Complete
**Date:** 2026-05-18

## What was built

- `edgestore/src/recovery.rs`: WAL file scanning + sequential replay
- `edgestore/src/engine.rs`: Engine::open now uses recovery; hardcoded WAL path removed

## Interfaces exported

- `pub struct RecoveryResult { records_replayed, records_skipped, max_lsn, max_txid, wal_files_read }`
- `pub fn recover_from_wal(db_path, memtable) -> Result<RecoveryResult>` — replays all WAL files into memtable
- `pub(crate) fn list_wal_files(db_path) -> Result<Vec<PathBuf>>` — finds + sorts WAL files

## Key behaviors

- WAL filename pattern: `wal-{016x}.log` (24 chars) — validated: len=24, prefix="wal-", suffix=".log", middle=16 hex
- Lexicographic sort of filenames = chronological order (zero-padded hex)
- Last-write-wins via sequential replay: later records overwrite earlier ones via memtable.insert
- Corrupt WAL header on open → skip that file (log), continue
- Corrupt records within file → WalReader skips them, recovery counts replayed vs skipped
- Empty DB (no WAL files) → RecoveryResult with all zeros
- Engine::open: after recovery, lsn_counter = max_lsn, txid_counter = max_txid
- WAL rotation check on open: if latest WAL needs_rotation(), create new WAL file
- next_wal_path(db_path, lsn) = db_path/wal-{lsn:016x}.log

## Verification

- All recovery and engine unit tests pass (39 total, up from 34)
- cargo clippy -D warnings clean
