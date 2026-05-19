# Plan 01-05 Summary — Transaction API

**Status:** Complete
**Date:** 2026-05-18

## What was built

- `edgestore/src/transaction.rs`: Transaction struct with state machine
- `edgestore/src/engine.rs`: Added begin, commit_transaction, rollback_transaction
- `edgestore/src/lib.rs`: Added Transaction re-export

## Interfaces exported

- `Transaction::new(txid) -> Transaction`
- `tx.put(ns, key, val, lsn, timestamp) -> Result<()>` — buffers WalRecord
- `tx.put_with_ttl(ns, key, val, ttl_secs, lsn, timestamp) -> Result<()>`
- `tx.delete(ns, key, lsn, timestamp) -> Result<()>`
- `tx.rollback_self()` — clears buffer, sets state=RolledBack
- `tx.take_pending() -> Result<Vec<WalRecord>>` — moves buffer out; sets Committed; Err if not Active
- `tx.is_active() -> bool`
- `tx.commit(&mut engine) -> Result<Lsn>` — CORE-06 convenience wrapper
- `tx.rollback(&mut engine)` — convenience wrapper
- `Engine::begin() -> Transaction` — increments txid_counter
- `Engine::commit_transaction(tx) -> Result<Lsn>` — appends all records, single fsync
- `Engine::rollback_transaction(tx)` — calls rollback_self, no WAL change

## Key behaviors

- Transaction state machine: Active → Committed (via take_pending) or RolledBack
- Group commit: N records appended, then exactly 1 fsync call
- Double commit returns Err(InvalidOperation)
- Rollback discards buffer, no WAL write

## Verification

- All transaction and engine tests pass (34 total, 9 new in this plan)
- cargo clippy -D warnings clean
