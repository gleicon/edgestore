# EdgeStore Internals

This document covers the internal structure of the `edgestore` crate for contributors. It describes the key abstractions, their invariants, the macros used to reduce duplication, and the style rules enforced by Tiger Style.

---

## 1. Architecture

```
┌─────────────────────────────┐
│  Engine (public API)        │  sync, &mut self writes
│  edgestore/src/engine/      │
├──────────────┬──────────────┤
│  MemTable    │  SegmentStore│  in-memory write buffer | on-disk read-only segments
│  memtable.rs │  segment.rs  │
├──────────────┴──────────────┤
│  WAL  (wal.rs)              │  append-only write-ahead log
├─────────────────────────────┤
│  Recovery (recovery.rs)     │  WAL replay at startup
└─────────────────────────────┘
```

`AsyncEngine` (`edgestore-tokio`) is a pure tokio glue layer: `Arc<RwLock<Engine>>` + `spawn_blocking`. It adds no storage logic.

---

## 2. Key Types

### `Engine` (`engine/mod.rs:103`)

The main entry point. All write methods take `&mut self` — exclusive lock required. Read methods take `&self`.

Fields:
- `memtable: Box<dyn MemTable>` — in-memory write buffer (default: `BBTreeConcurrentMemTable`)
- `segment_store: SegmentStore` — ordered set of on-disk immutable segments
- `wal: WalWriter` — append-only durability log
- `lsn_counter: u64` — monotone write sequence number; increments on every `put`/`delete`/commit
- `config: EdgestoreConfig` — includes `memtable_max_bytes` (auto-flush threshold)

### `MemEntry` (`types.rs`)

Single record in memtable or segment. Contains: `key`, `value` (None for tombstones), `op` (`Put`/`Delete`), `lsn`, `timestamp`, `ttl`.

### `FlushWork` (`segment.rs:389`)

Self-contained Phase 2 work package. Carries a snapshot of memtable entries + reserved segment ID. No lock held during `execute()`. Callers must pass `flushed_max_lsn` to `finalize_flush`.

### `BudgetedScan<T>` (`engine/mod.rs:60`)

Result of a range or prefix scan with optional byte/item limits: `items`, `truncated`, `stats`.

### `QueryStats` (`engine/mod.rs:37`)

Per-query cost counters: `segments_scanned`, `items_examined`, `bytes_scanned`.

### `ScanBudget` (`engine/mod.rs:51`)

Optional limits for a scan: `max_items: Option<usize>`, `max_bytes: Option<u64>`.

---

## 3. The 3-Phase Flush Protocol

Flush converts the current memtable into an immutable on-disk segment while keeping the engine writable during the I/O phase.

```
Phase 1 (write lock):  Engine::begin_flush()
  - Snapshot memtable entries into FlushWork.
  - Reserve a segment ID (next_segment_id++).
  - memtable is NOT cleared — writes continue.

Phase 2 (no lock):     FlushWork::execute()
  - Write .dat / .idx / .xf / .meta files to disk.
  - No engine state required.

Phase 3 (write lock):  Engine::finalize_flush(&meta, flushed_max_lsn)
  - Register segment in manifest and open a SegmentReader.
  - Rebuild memtable: keep only entries with lsn > flushed_max_lsn.
  - Retire old WAL files (keep current WAL only).
```

**Invariant**: entries at `lsn ≤ flushed_max_lsn` are durably in the segment after Phase 3. Entries at `lsn > flushed_max_lsn` arrived during Phase 2 and are retained in the rebuilt memtable.

`flush_to_segments_inner` orchestrates all three phases synchronously. `AsyncEngine::flush_to_segments` runs Phase 2 lock-free in `spawn_blocking`.

---

## 4. Read Path

```
get(ns, key):
  1. Encode key: encode_key(ns, key)
  2. Check memtable (tombstone returns None immediately)
  3. Check segment_store (K-way merge across readers, xor filter first)

range / prefix scan:
  1. Fetch seg_results from SegmentStore (already sorted)
  2. Fetch mem_results from MemTable (already sorted)
  3. merge_sorted_scans(seg, mem) → merged (ascending, may have duplicates per key)
  4. dedup_filter_scan(merged) → dedup by key (highest LSN wins), filter tombstones, decode
```

The merge+dedup is O(n) with 2 allocations. Using a HashMap would be O(n log n) with 4 allocations. Regression test: `test_range_scan_dedups_by_lsn_across_segments`.

---

## 5. Write Path

```
put(ns, key, val):
  1. Guard: readonly check, ns length ≤ u16::MAX
  2. write_entry(ns, key, val, ttl=0):
     a. Increment lsn_counter
     b. Write WAL record (blake3 value hash for integrity)
     c. Rotate WAL if needed
     d. memtable.insert(encode_key(ns, key), entry)
  3. Auto-flush if memtable.len() * AVG_ENTRY_SIZE_ESTIMATE ≥ memtable_max_bytes
     (propagates error — put fails if flush fails)
```

`put_with_ttl` is identical but skips the auto-flush step and passes `ttl_secs` to `write_entry`.

**Constant**: `AVG_ENTRY_SIZE_ESTIMATE = 256 bytes` (engine/mod.rs:28). Used only for the auto-flush heuristic — not for allocation. A segment flush may be triggered early if actual entries are smaller.

---

## 6. Segment File Format

Four files per segment (ID is zero-padded 8-digit decimal):

| File | Contents |
|------|----------|
| `segment-NNNNNNNN.dat` | 4096-byte aligned blocks; each block: 4-byte magic (`EDSG` = `0x45445347`), 4-byte payload length, LZ4-compressed entries |
| `segment-NNNNNNNN.idx` | Sparse index: first key of every 64th entry + byte offset in .dat |
| `segment-NNNNNNNN.xf` | Xor8 probabilistic filter — eliminates unnecessary .dat reads |
| `segment-NNNNNNNN.meta` | JSON `SegmentMeta` (segment_id, record_count, min/max_key, blake3 hash of .dat, cohort) |

**Constants** (`segment.rs`):

| Name | Value | Meaning |
|------|-------|---------|
| `SEGMENT_BLOCK_SIZE` | 4096 | Aligned block size in bytes |
| `BLOCK_TARGET_BYTES` | 3900 | Target payload before block boundary |
| `SEGMENT_FILE_MAGIC` | `0x45445347` | `.dat` file header magic (`EDSG`) |
| `SEGMENT_BLOCK_MAGIC` | `0x45445347` | Per-block magic (same value) |
| `SEGMENT_FORMAT_VERSION` | 1 | `.dat` format version byte |
| `SEGMENT_HEADER_LEN` | 8 | Bytes before first block (magic + version + padding) |
| `MAX_BLOCK_DECOMPRESSED` | 2 MiB | Upper bound on decompressed block size |

---

## 7. MemTable Implementations

Three implementations in `memtable.rs`:

| Type | Backend | Concurrency | Notes |
|------|---------|-------------|-------|
| `BTreeMemTable` | `std::collections::BTreeMap` | `&mut self` writes | Reference implementation |
| `BBTreeMemTable` | `bbtree::unsync::BBTreeMap` | `&mut self` writes | ~5× faster point lookups; SIMD node search |
| `BBTreeConcurrentMemTable` | `bbtree::BBTreeMap` (concurrent) | `&mut self` writes, concurrent reads | Default; readers don't write cache lines during read |

**Default**: `BBTreeConcurrentMemTable` (set in `EdgestoreConfig::default()`).

### `impl_simple_memtable!` macro (`memtable.rs:58`)

`BTreeMemTable` and `BBTreeMemTable` share identical `MemTable` impl bodies — both backing types expose the same `.get()`, `.range()`, `.iter()`, `.len()`, `.clear()` API. The macro generates the impl from a single body to prevent silent divergence:

```rust
macro_rules! impl_simple_memtable { ($name:ident) => { impl MemTable for $name { ... } }; }
impl_simple_memtable!(BTreeMemTable);
impl_simple_memtable!(BBTreeMemTable);
```

`BBTreeConcurrentMemTable` is NOT covered by this macro — it uses `scan()` instead of `range()`/`iter()`, has a manual `count` field (the concurrent variant has no `.len()`), and panics on allocator exhaustion (the only `MemTable` impl that can fail; documented on `MemTable::insert`).

---

## 8. Helper Functions

### `engine/mod.rs`

| Function | Purpose |
|----------|---------|
| `write_entry(ns, key, val, ttl)` | Shared WAL write + memtable insert for put operations |
| `merge_sorted_scans(seg, mem)` | O(n) K-way merge of two sorted entry slices (ascending) |
| `dedup_filter_scan(merged, …)` | Dedup by key (highest LSN), filter tombstones, decode keys, apply budget |
| `range_core(ns, start, end, budget)` | Full range scan pipeline using the two helpers above |
| `prefix_core(ns, prefix, budget)` | Prefix scan pipeline; converts prefix to `[prefix, upper_bound)` range |
| `flush_to_segments_inner()` | Synchronous orchestration of all three flush phases |

### `segment.rs`

| Function | Purpose |
|----------|---------|
| `find_block_offset(index, key)` | Binary search on sparse index → byte offset in .dat |
| `build_xor_filter(keys)` | Build Xor8 probabilistic filter from key slice |
| `filter_contains(filter, key)` | Xor8 membership test (no false negatives) |
| `read_idx_file(path)` | Deserialize sparse index from .idx file |
| `read_xf_file(path)` | Deserialize Xor8 filter from .xf file |
| `parse_dat_entries(bytes)` | Parse all entries from a raw .dat byte slice |
| `read_block_at_offset(file, offset)` | Read and decompress one 4096-byte block |
| `serialize_entry / deserialize_entry` | Entry wire format encode/decode |

### `recovery.rs`

| Function | Purpose |
|----------|---------|
| `list_wal_files(db_path)` | Enumerate WAL files matching `wal-[16 hex].log`; sorted ascending |
| `recover_from_wal(db_path, memtable)` | Replay WAL files into memtable; returns `RecoveryResult` |

---

## 9. Tiger Style Rules in Force

This codebase applies [Tiger Style](https://tigerstyle.dev/) constraints:

- **Assertions**: `debug_assert!` on all non-trivial invariants — entry/exit conditions, loop bounds, post-condition checks. These are not error handling; they document invariants that MUST hold.
- **Error handling**: every `Result` is handled explicitly. `let _ = expr` is banned on fallible paths that can affect correctness.
- **Loop bounds**: every non-trivial loop has an explicit upper bound with an assertion. `SegmentReader::get` asserts `blocks_visited ≤ (dat_len / SEGMENT_BLOCK_SIZE) + 1`.
- **Function size**: max 70 lines. `range_core` and `prefix_core` share helpers to stay under this limit.
- **One purpose per function**: `merge_sorted_scans` only merges; `dedup_filter_scan` only deduplicates; `write_entry` only writes one record.
- **No silent discard**: auto-flush failure in `put_inner` propagates — the put returns an error if flush fails.

---

## 10. Key Invariants (tested)

| Invariant | Test |
|-----------|------|
| `lsn > 0` for every write; LSN strictly monotone per operation | `test_put_lsn_monotone_and_positive` |
| Writes after `begin_flush` survive `finalize_flush` | `test_finalize_flush_retains_writes_after_snapshot` |
| Tombstone in memtable shadows segment value | `test_delete_from_segment_via_memtable_tombstone` |
| Range merge deduplicates same key across segments | `test_range_merge_dedups_same_key` |
| `put_with_ttl` stores the TTL field in the memtable entry | `test_put_with_ttl_stores_ttl_field` |
| Segment sparse index is cached at open() | `test_reader_caches_index_at_open` |
| `remove_segment` updates manifest before deleting files | `test_remove_segment_manifest_updated_before_files_gone` |
| flushed_max_lsn = max LSN across all snapshot entries | (invariant enforced in `prepare_flush`) |
