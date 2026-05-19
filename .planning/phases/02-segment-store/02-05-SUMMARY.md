# Plan 02-05 Summary — SegmentStore + Engine Integration

**Status:** Complete
**Date:** 2026-05-18

## What was built

### edgestore/src/segment.rs — SegmentStore

- `SegmentStore::open(base_path, cohort_window_secs)` opens Manifest, loads SegmentReaders for all live segments
- `flush_memtable(memtable)` flushes sorted entries → new segment, adds to manifest, opens reader, increments next_segment_id
- `get(key)` checks readers newest-first; returns first match
- `range_scan(start, end)` merges across all readers; highest LSN wins per key; tombstones removed

### edgestore/src/engine.rs — Engine integration

- `Engine` struct gains `segment_store: SegmentStore` field
- `Engine::open()` initializes `SegmentStore::open(config.path, config.cohort_window_secs)`
- `Engine::get()` checks memtable first; on None falls through to `segment_store.get()`; Delete tombstones from segment return None
- `Engine::range()` merges memtable + segment_store results; highest LSN wins; tombstones excluded
- `Engine::prefix()` merges memtable prefix + segment_store range_scan (prefix-bounded); same merge semantics
- `Engine::flush_to_segments()` flushes memtable to segment store, clears memtable; returns SegmentMeta
- Auto-flush in `put()`: if `memtable.len() * 256 >= config.segment_size_bytes`, auto-flush

### edgestore/src/memtable.rs — MemTable trait

- Added `fn clear(&mut self)` to `MemTable` trait and `BTreeMemTable` impl

## All Phase 1 tests pass — no regressions

62 unit tests + 11 integration tests = 73 total, all green.
