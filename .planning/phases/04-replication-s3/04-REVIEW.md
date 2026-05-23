---
status: issues
phase: "04"
phase_name: replication-s3
files_reviewed: 15
findings:
  critical: 5
  warning: 5
  info: 2
  total: 12
depth: standard
---

# Phase 04 Code Review — replication-s3

## CRITICAL

### C-01: Delete tombstones never applied during import — data divergence on delete replication

**File:** `edgestore/src/engine.rs`
**Severity:** Critical — Deletes on remote nodes are permanently lost during sync

The `apply` block inside `import_segment` only handles `Operation::Put`. When LWW decides
to apply a Delete record, the inner `if incoming.op == Put` guard silently does nothing.
The tombstone is never written to the local WAL or memtable.

**Impact:** Keys deleted on Node A reappear on Node B after sync.

**Fix:** Add a Delete arm inside the apply block:
```rust
if incoming.op == crate::types::Operation::Put {
    if let Some(ref val) = incoming.value {
        self.put_with_timestamp(&ns, &key, val, incoming.timestamp)?;
        keys_written += 1;
    }
} else if incoming.op == crate::types::Operation::Delete {
    self.delete_with_timestamp(&ns, &key, incoming.timestamp)?;
    keys_written += 1;
}
```

---

### C-02: Empty xor filter written for imported segments — all post-restart lookups return None

**File:** `edgestore/src/engine.rs`
**Severity:** Critical — Imported data becomes invisible after engine restart

`import_segment` builds the xor filter from an empty key list (`vec![]`). `SegmentReader::get`
uses the filter as a mandatory positive-membership gate — it returns `Ok(None)` immediately
for any key not in the filter. After a restart (when the memtable is cold), every `get` on a
key that came via `import_segment` returns `None`. The data is on disk but unreachable.

**Impact:** SC1/SC2 integration tests pass only because keys are still in the memtable at
assertion time. After a restart, all replicated data vanishes from the read path.

**Fix:** Build the xor filter from the actual decoded keys in the incoming segment during
the LWW scan loop, then pass them to `build_xor_filter`.

---

### C-03: Imported segments have empty min_key/max_key — range scans always skip them

**File:** `edgestore/src/engine.rs`
**Severity:** Critical — Range/prefix queries return no data for imported segments

`import_segment` stores `SegmentMeta` with `min_key: vec![]` and `max_key: vec![]`.
`SegmentReader::range_scan` uses these bounds for early exit: `start > self.meta.max_key`
evaluates as `start > []` which is `true` for any non-empty start key. Range and prefix
calls immediately return empty results for all imported segments.

**Fix:** Track min_key and max_key from decoded records during the LWW scan loop and
populate those fields in `SegmentMeta` before writing the `.meta` file.

---

### C-04: Path traversal via unsanitized peer_id in cursor file path

**File:** `edgestore-repl/src/anti_entropy.rs`
**Severity:** Critical — Attacker-controlled peer_id can write/read arbitrary files

```rust
fn cursor_file_path(db_path: &Path, peer_id: &str) -> PathBuf {
    db_path.join("sync").join(format!("{}.cursor", peer_id))
}
```

`peer_id` is caller-supplied with no validation. A value like `"../../etc/cron.d/evil"`
produces a write path outside `{db_path}/sync/`.

**Fix:**
```rust
fn cursor_file_path(db_path: &Path, peer_id: &str) -> Result<PathBuf, std::io::Error> {
    if peer_id.contains('/') || peer_id.contains('\\') || peer_id.contains("..") {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid peer_id"));
    }
    Ok(db_path.join("sync").join(format!("{}.cursor", peer_id)))
}
```

---

### C-05: Two-stage rename in import_segment leaves orphan .dat file on crash (Windows-fatal)

**File:** `edgestore/src/engine.rs`
**Severity:** Critical — Crash between two renames leaves stale file; permanent import failure on Windows

`import_segment` does two renames: `{hash}.tmp → {hash}.dat`, then LWW loop, then
`{hash}.dat → segment-{id}.dat`. A crash between rename #1 and #2 leaves a stray
`{hash}.dat`. On Linux, the next import replaces it atomically (safe but wasteful).
On Windows, rename fails if target exists, making that segment permanently unimportable.

**Fix:** Use a single-rename protocol — allocate segment ID first, write to
`segment-{id:08}.tmp`, rename to `segment-{id:08}.dat`.

---

## WARNING

### W-01: Anti-entropy loop sleeps before first probe — sync delayed by full interval

**File:** `edgestore-repl/src/anti_entropy.rs`
**Severity:** Warning — First sync delayed by `interval_secs` after node start

Sleep comes before `run_once`. Default 30-second interval means no sync for 30s on startup.

**Fix:** Move sleep to end of loop body so first probe runs immediately.

---

### W-02: Unbounded request body read in HTTP server — memory exhaustion

**File:** `edgestore-repl/src/http_server.rs`
**Severity:** Warning — No size cap on body drain; single request can OOM the server

**Fix:** Cap body drain at 4 KiB (all three endpoints are GET-only):
```rust
let mut _body = vec![0u8; 4096];
let _ = request.as_reader().read(&mut _body);
```

---

### W-03: .meta file not fsynced before segment registration

**File:** `edgestore/src/engine.rs`
**Severity:** Warning — Power loss after .dat/.idx/.xf sync but before OS flushes .meta
corrupts segment on restart (`.meta` truncated, `SegmentReader::open` fails)

**Fix:** Call `meta_file.sync_all()` after writing the JSON.

---

### W-04: Put records with value=None silently dropped during import

**File:** `edgestore/src/engine.rs`
**Severity:** Warning — Malformed Put records are dropped silently (no log, no counter)

Makes debugging import discrepancies very difficult.

**Fix:** Log and count as `keys_skipped` when `incoming.value == None` for a Put.

---

### W-05: All edgestore dependency versions are unpinned wildcards

**File:** `edgestore/Cargo.toml`
**Severity:** Warning — Any `cargo update` can silently break xorf filter deserialization

`lz4_flex = "*"`, `zstd = "*"`, `blake3 = "*"`, `xorf = { version = "*" }`.

**Fix:** Pin to minimum compatible versions (e.g. `blake3 = "1"`, `xorf = "0.11"`).

---

## INFO

### I-01: LWW tiebreaker comment misleading — host_id comparison not implemented

**File:** `edgestore/src/engine.rs`
**Severity:** Info — Comment says "lower host_id wins" but implementation always keeps local

Replace with explicit TODO:
```rust
// TODO(D06-v2): HostId tiebreaker not implemented — host_id absent from MemEntry.
// Current policy: local record always wins on timestamp tie.
```

---

### I-02: SC1 integration test uses hardcoded 4-second sleep — fragile in CI

**File:** `edgestore-repl/tests/integration_replication.rs`
**Severity:** Info — Fixed sleep can produce false failures on loaded CI; wastes time otherwise

Replace with a polling loop with a 10-second deadline and 100ms poll interval.

---

## Non-Issues (Explicitly Reviewed and Cleared)

- `filesystem_remote_store.rs` path construction: hash is locally computed hex — no traversal
- HTTP server `/segments/{hash_hex}`: validated to 64 hex chars before any fs join — safe
- BLAKE3 verification in `import_segment`: occurs before any write — correct
- `RangeMerkleTree` bucket collision: by design — correct
- Thread safety of `HttpReplicationServer`: `Arc<Mutex<Engine>>` correctly serialized
- `FilesystemRemoteStore` TOCTOU: content-addressed store makes concurrent writes safe
