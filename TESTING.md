# EdgeStore Testing Guide

> *"Most of the SQLite source code is devoted purely to testing and verification. An automated test suite runs millions and millions of test cases... and achieves 100% branch test coverage."* — [sqlite.org/testing.html](https://sqlite.org/testing.html)

EdgeStore follows the same principle: **tests are the primary deliverable, not an afterthought.**

This document describes the philosophy, harnesses, and specific test categories that keep EdgeStore reliable on real SSD hardware.

---

## Philosophy

### Small, direct, deterministic

We do not chase coverage percentages as a vanity metric. We write tests that exercise the exact failure modes that happen on real SSDs:

- Power loss mid-flush
- `fsync` returning `EINTR`
- FTL garbage collection relocating pages EdgeStore thought were stable
- Silent bit flips in NAND

**Every test must be deterministic.** If a test fails at seed `0xdeadbeef`, replaying that seed must reproduce the exact same failure. This is the [FoundationDB DST](https://github.com/Lucent-Financial-Group/Zeta/blob/main/docs/FOUNDATIONDB-DST.md) model, and it is non-negotiable.

### Test code > source code

SQLite maintains ~1000x more test code than source code. EdgeStore targets at least 10:1 for the core `edgestore` crate and 5:1 for transport crates.

---

## Test Harnesses

| Harness | What it tests | How to run |
|---------|--------------|-----------|
| **Unit + Integration** | Invariants, round-trips, error paths | `cargo test --workspace` |
| **Deterministic Simulation** | Race conditions, crash recovery, reordering | `cargo test --workspace --features sim` (future) |
| **Fuzz** | WAL parsers, segment deserializers, manifest parsers | `cargo fuzz` (future) |
| **SSD Benchmark** | Real device write amplification | `cargo bench --workspace` |
| **Chaos / Fault Injection** | `fsync` failure, disk-full, torn writes | Custom `StorageBackend` impls |

---

## Unit + Integration Tests (Existing)

Run with:

```bash
cargo test --workspace
```

These are the tests that ship with every crate. They verify:

- `put` → `get` roundtrip
- `delete` → `get` returns `None`
- `range` boundaries are exclusive
- `prefix` scans stop at namespace boundary
- `flush` persists WAL
- `snapshot` pins segments for point-in-time reads
- `compact_once` reduces segment count
- Vector search returns same results as brute-force
- Text search BM25 ranking is monotonic

**Golden rule:** Every bug fix must include a test that would have caught the bug.

---

## Deterministic Simulation Tests (Future)

Inspired by [FoundationDB + DST](https://github.com/Lucent-Financial-Group/Zeta/blob/main/docs/FOUNDATIONDB-DST.md) and [Polar Signals' state-machine approach](https://news.lavx.hu/article/building-unshakeable-databases-how-state-machines-revolutionize-deterministic-testing-in-rust).

The core idea: replace real disk I/O, timers, and randomness with a test-harness-controlled event loop. From a single seed, any interleaving can be replayed exactly.

### What to simulate

| Failure | How to inject |
|---------|--------------|
| Power loss mid-flush | Stop the event loop after N operations, verify WAL replay |
| `pwrite` returns `EINTR` | Custom `StorageBackend` that randomly returns errors |
| `fsync` fails after WAL append | Same backend; verify Engine aborts gracefully |
| Disk full during compaction | Backend that tracks capacity, returns `ENOSPC` |
| Two processes open same DB | Verify file locking (`flock`) |
| Clock skew | Virtual clock that jumps forward/backward |
| Reordered segment writes | Backend that delays writes, applies them out of order |

### Minimal harness structure

```rust
/// A `StorageBackend` that records every operation and can replay them
/// deterministically from a seed.
pub struct SimulatedStorage {
    log: Vec<Op>,
    rng: StdRng,           // seeded
    delay_queue: BinaryHeap<DelayedOp>,
    fail_next_fsync: bool,
}

impl StorageBackend for SimulatedStorage {
    fn pwrite(&mut self, fd: Fd, offset: u64, buf: &[u8]) -> io::Result<()> {
        let op = Op::Pwrite { fd, offset, buf: buf.to_vec() };
        // Maybe delay, maybe reorder, maybe corrupt a byte
        self.schedule(op);
        Ok(())
    }
    // ...
}
```

**Test example:**

```rust
#[test]
fn test_power_loss_during_flush() {
    for seed in 0..1000 {
        let mut sim = SimulatedStorage::from_seed(seed);
        let mut engine = Engine::open_with_backend(Config::new("/tmp/db"), sim);

        // Write 100 keys
        for i in 0..100 {
            engine.put(b"ns", &i.to_be_bytes(), b"val").unwrap();
        }

        // Simulate power loss after a random number of fsyncs
        sim.crash_after_fsync(sim.rng.gen_range(1..10));

        // Re-open — WAL replay must recover to a consistent state
        let engine2 = Engine::open_with_backend(Config::new("/tmp/db"), sim.recover());
        // Verify: all acknowledged puts are present, no partial writes
        assert_consistent(&engine2);
    }
}
```

---

## Fuzz Tests (Future)

Use `cargo-fuzz` with `libfuzzer-sys`.

### Targets

| Target | Input | Invariant |
|--------|-------|-----------|
| WAL record parser | Arbitrary `&[u8]` | Never panic, never read out of bounds |
| Segment `.dat` parser | Arbitrary `&[u8]` | Returns `CorruptionError` or valid blocks |
| Segment `.meta` parser | Arbitrary JSON | Graceful rejection of malformed fields |
| Manifest line parser | Arbitrary `&[u8]` | CRC32C mismatch → skip line, don't panic |
| Namespace encoder | Arbitrary strings | Roundtrip: encode → decode == original |
| BLAKE3 hash parser | Arbitrary hex string | Invalid length → `Err`, never panic |

**Entry point:**

```rust
fuzz_target!(|data: &[u8]| {
    let _ = WalRecord::decode(data); // must not panic
});
```

---

## SSD-Specific Validation

### Deathtime-cohort compaction WAF

**Source:** [VLDB 2026 — Page Deathtime](https://www.vldb.org/pvldb/vol16/p3266-lee.pdf)

**Test:** Create records with known TTLs, verify compaction behavior.

```rust
#[test]
fn test_fully_expired_cohort_zero_relocation() {
    let mut engine = Engine::open(Config::new("/tmp/db"));

    // Write 1000 keys with TTL = 60s
    for i in 0..1000 {
        engine.put_with_ttl(b"ns", &i.to_be_bytes(), b"val", 60).unwrap();
    }
    engine.flush().unwrap();

    // Wait for expiry
    std::thread::sleep(Duration::from_secs(61));

    // Compact — fully expired cohort should require zero live-data relocation
    let stats = engine.compact_once().unwrap();
    assert_eq!(stats.bytes_relocated, 0,
        "fully expired cohort must not relocate any bytes");
}
```

### Real SSD performance validation

**Source:** [SSD-iq: Uncovering the Hidden Side of SSD Performance](https://www.vldb.org/pvldb/vol18/p4295-haas.pdf)

**Test:** Run benchmarks on actual NVMe, SATA SSD, and loopback file. Compare device-reported WAF vs EdgeStore-reported WAF.

```bash
# Monitor device write amplification during benchmark
cargo bench --bench throughput
# In parallel, watch nvme smart-log or iostat -x
```

If EdgeStore claims WAF≈1 but the device reports 3x, the cohort grouping is wrong or the FTL is interleaving EdgeStore writes with unrelated data.

### Content-addressing integrity

**Property:** For every segment on disk, `blake3(segment_bytes) == segment_hash`.

```rust
#[test]
fn test_segment_content_addressing_integrity() {
    let engine = Engine::open(Config::new("/tmp/db"));
    engine.put(b"ns", b"key", b"val").unwrap();
    engine.flush().unwrap();

    let segments = engine.list_segment_files();
    for seg in segments {
        let bytes = fs::read(&seg.path).unwrap();
        let hash = blake3::hash(&bytes);
        assert_eq!(hash.as_bytes(), &seg.meta.hash,
            "segment {} hash mismatch — corruption or bug", seg.path.display());
    }
}
```

**Corollary:** After crash recovery, recompute all hashes. If WAL replay rebuilds a memtable and re-flushes, the new segments must have identical hashes to the originals (deterministic flush).

---

## Chaos / Fault Injection

Inspired by [Antithesis driven testing](https://sqlsync.dev/posts/antithesis-driven-testing/) and [chaos-rs](https://github.com/Alpy16/chaos-rs).

### Malformed data tests

Corrupt each file type and verify graceful `CorruptionError`:

```rust
#[test]
fn test_corrupt_wal_trailing_garbage() {
    let mut engine = Engine::open(Config::new("/tmp/db"));
    engine.put(b"ns", b"k", b"v").unwrap();
    engine.flush().unwrap();

    // Append 4 garbage bytes to WAL
    let wal = fs::OpenOptions::new().append(true).open("/tmp/db/wal").unwrap();
    wal.write_all(b"XXXX").unwrap();

    // Re-open — must detect corruption, not panic
    let result = Engine::open(Config::new("/tmp/db"));
    assert!(matches!(result, Err(EdgestoreError::Corruption(_))));
}
```

Repeat for:
- Corrupt `.dat` block (wrong ZSTD magic)
- Corrupt `.idx` sparse index (out-of-order offsets)
- Corrupt `.xf` xor filter (wrong size)
- Corrupt `.meta` JSON (missing required field)
- Corrupt manifest line (wrong CRC32C)
- Swap two bytes in a WAL record CRC

---

## Regression Tests

Every bug fix must include a regression test that fails before the fix and passes after.

**Example (S3RemoteStore async-context panic):**

```rust
/// Regression: S3RemoteStore::new() must not panic when called from
/// inside an existing Tokio runtime (e.g. #[tokio::main]).
/// Previously it called Runtime::block_on() directly, which panics.
#[test]
fn test_new_from_async_context() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let _store = S3RemoteStore::new("bucket", None, None)
            .expect("must succeed from async context");
    });
}
```

**Label regression tests with the commit hash that introduced the fix:**

```rust
// Regression: fixed in commit 59e9890
// Bug: drop Arc<Runtime> from async context panics
#[test]
fn test_drop_from_async_context() { ... }
```

---

## Coverage Policy

We do not require 100% line coverage. We require **100% of the failure paths** to be tested:

- Every `?` in `Engine::open()`
- Every `map_err` in WAL append
- Every `continue` in segment iteration
- Every `Err` branch in `put_with_ttl`

Use `cargo-tarpaulin` to identify untested error branches:

```bash
cargo tarpaulin --workspace --out Html
open tarpaulin-report.html
```

Focus on the red lines in `error.rs`, `wal.rs`, and `segment_store.rs`.

---

## References

| Paper / Source | What we borrow |
|--------------|---------------|
| [SQLite Testing](https://sqlite.org/testing.html) | Philosophy: tests > source, MC/DC, malformed data |
| [FoundationDB DST](https://github.com/Lucent-Financial-Group/Zeta/blob/main/docs/FOUNDATIONDB-DST.md) | Deterministic simulation, single-seed replay |
| [Polar Signals state-machine testing](https://news.lavx.hu/article/building-unshakeable-databases-how-state-machines-revolutionize-deterministic-testing-in-rust) | Rust-specific DST architecture |
| [SSD-iq (VLDB 2025/2026)](https://www.vldb.org/pvldb/vol18/p4295-haas.pdf) | Real SSD validation methodology |
| [VLDB 2026 Deathtime](https://www.vldb.org/pvldb/vol16/p3266-lee.pdf) | Cohort compaction validation |
| [Antithesis testing](https://sqlsync.dev/posts/antithesis-driven-testing/) | Deterministic fault injection |
| [chaos-rs](https://github.com/Alpy16/chaos-rs) | Direct I/O emulation, power-loss simulation |

---

## Checklist for New Features

Before merging any feature:

- [ ] Unit tests for happy path
- [ ] Unit tests for every `Err` branch
- [ ] Integration test with real `Engine` (not mocks)
- [ ] Crash-recovery test: kill process mid-operation, verify WAL replay
- [ ] Malformed-data test: corrupt one byte, verify graceful error
- [ ] Deterministic simulation test (if feature touches I/O or ordering)
- [ ] Fuzz target (if feature parses untrusted bytes)
- [ ] Regression test with commit hash comment (if fixing a bug)
- [ ] Benchmark showing no regression (if feature touches hot path)
