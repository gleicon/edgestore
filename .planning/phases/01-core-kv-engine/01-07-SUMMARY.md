# Plan 01-07 Summary — Integration Tests

**Status:** Complete
**Date:** 2026-05-18

## What was built

- `edgestore/tests/integration_core.rs`: Full integration test suite for Phase 1

## Tests

1. `test_wal_file_header_format` — validates exact 8-byte magic+version layout
2. `test_wal_encode_decode_round_trip` — WAL encode/decode through recovery
3. `test_wal_corruption_detection` — bit-flip corruption does not panic
4. `test_wal_truncation_safety` — truncated WAL does not panic
5. `test_crash_recovery_no_acknowledged_writes_lost` — 50 keys survive engine reopen (Success Criterion 2)
6. `test_namespace_isolation` — prefix scan returns zero cross-namespace keys (Success Criterion 1)
7. `test_transaction_group_commit` — 100 puts committed in single transaction (Success Criterion 3)
8. `test_transaction_tx_commit_convenience` — CORE-06 tx.commit(engine) + tx.rollback(engine)
9. `test_wal_rotation` — multiple WAL files created; all keys recoverable (Success Criterion 4)
10. `test_put_with_ttl_stored_in_wal` — TTL field stored in WAL, key readable after reopen
11. `test_wal_file_seek_write_does_not_panic` — seek-based corruption does not panic engine open

## Key implementation note

WAL rotation in this engine happens at `Engine::open()` time (not during `append()`). The rotation test therefore uses the drop-and-reopen pattern: each batch of writes is flushed, the engine is dropped (releasing the lock), and the next iteration reopens with `wal_max_bytes = 512` — which triggers rotation if the previous WAL file exceeded the threshold. After 10 batches of 20 keys, multiple WAL files are present.

## Phase 1 Success Criteria Coverage

1. put -> get round-trip: test_namespace_isolation
2. No acknowledged writes lost: test_crash_recovery_no_acknowledged_writes_lost
3. Group commit: test_transaction_group_commit
4. WAL rotation: test_wal_rotation
5. Format tests: test_wal_encode_decode_round_trip + test_wal_corruption_detection + test_wal_truncation_safety

## Verification

- All 11 integration tests pass
- All 39 unit tests still pass
- cargo clippy --tests -D warnings clean (0 errors)
