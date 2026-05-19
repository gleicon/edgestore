# Plan 01-03 Summary — MemTable trait + BTreeMap implementation

**Status:** Complete
**Date:** 2026-05-18

## What was built

- `edgestore/src/memtable.rs`: MemTable trait + BTreeMemTable implementation
- `edgestore/src/config.rs`: Updated with memtable_factory field

## Interfaces exported

- `pub trait MemTable: Send` — object-safe; Box<dyn MemTable> compiles
  - `insert(key, entry)` — namespace-encoded key, overwrites on duplicate
  - `get(key) -> Option<&MemEntry>` — exact match
  - `range(start, end) -> Vec<...>` — inclusive start, exclusive end, sorted
  - `prefix(prefix) -> Vec<...>` — all keys starting with prefix, sorted
  - `iter() -> Vec<...>` — all entries sorted
  - `len() -> usize`
  - `is_empty() -> bool` — default impl
- `pub struct BTreeMemTable` — default MemTable implementation
- `EdgestoreConfig::memtable_factory: Box<dyn Fn() -> Box<dyn MemTable> + Send + Sync>`
  - Default: creates BTreeMemTable
  - EdgestoreConfig no longer derives Clone (Box<dyn Fn> is not Clone); Debug implemented manually

## Key behaviors

- BTreeMap storage: sorted by namespace-encoded key bytes
- prefix() scans from first matching key, stops when key no longer has prefix — O(k)
- Namespace isolation: encode_key(b"ns_a", b"") as prefix returns ONLY ns_a keys
- Overwrites: second insert with same key replaces entry (LWW at memtable level)

## Verification

- All memtable unit tests pass (object-safe, insert/get, overwrite, len, namespace isolation, range isolation)
- cargo clippy -D warnings clean
