---
phase: 4
plan: "04"
subsystem: replication
tags: [remote-store, filesystem, anti-entropy, replication]
dependency_graph:
  requires: ["04-03"]
  provides: ["FilesystemRemoteStore implementing RemoteStore", "AntiEntropyLoop.with_remote_store"]
  affects: ["04-05"]
tech_stack:
  added: []
  patterns: ["content-addressed storage", "atomic write (.tmp + rename)", "idempotent upload", "optional builder pattern"]
key_files:
  created:
    - edgestore-repl/src/filesystem_remote_store.rs
  modified:
    - edgestore-repl/src/anti_entropy.rs
    - edgestore-repl/src/lib.rs
decisions:
  - "D04: FilesystemRemoteStore is Phase 4 impl; S3RemoteStore is future phase"
  - "D08: remote_store upload after Applied import is non-fatal — loop continues on error"
  - "upload idempotency via dest.exists() check before write; atomic via .tmp rename"
  - "list() silently skips filenames that are not exactly 64-char hex + .seg (T-04-12)"
requirements: [REPL-04]
metrics:
  duration: "~10 minutes"
  completed: "2026-05-23"
  tasks_completed: 2
  tasks_total: 2
  files_created: 1
  files_modified: 2
---

# Phase 4 Plan 04: edgestore-repl FilesystemRemoteStore — Summary

**One-liner:** Local-filesystem RemoteStore implementation with atomic upload, idempotent content-addressed segments, and optional AntiEntropyLoop integration uploading each applied segment.

## Tasks Completed

| Task | Name | Commit | Files |
|------|------|--------|-------|
| 1 | Create FilesystemRemoteStore | `0c4af7a` | edgestore-repl/src/filesystem_remote_store.rs, edgestore-repl/src/lib.rs |
| 2 | Wire into AntiEntropyLoop | `0fa3ade` | edgestore-repl/src/anti_entropy.rs, edgestore-repl/src/lib.rs |

## What Was Built

### FilesystemRemoteStore (edgestore-repl/src/filesystem_remote_store.rs)

Implements `edgestore::RemoteStore` over a local directory using `std::fs` only — no new external dependencies.

- `new(base_dir: PathBuf) -> Result<Self, EdgestoreError>`: creates directory via `create_dir_all`
- `upload`: content-addressed idempotency (skips if `{hash_hex}.seg` exists), atomic write via `.tmp` + `rename`
- `download`: reads `{hash_hex}.seg`, maps `NotFound` to `ReplicationError`
- `list`: scans `base_dir`, filters on `.seg` extension, validates stem is exactly 64 hex chars, silently skips malformed names
- `delete`: idempotent — `Ok(())` on `NotFound`
- 5 unit tests via `tempfile::TempDir`: roundtrip, idempotent upload, list, delete, not-found

### AntiEntropyLoop updates (edgestore-repl/src/anti_entropy.rs)

- Field added: `remote_store: Option<Arc<dyn RemoteStore>>`
- Builder method: `pub fn with_remote_store(mut self, store: Arc<dyn RemoteStore>) -> Self`
- `run_once` extended to accept `Option<&dyn RemoteStore>` parameter
- After `ImportResult::Applied`: calls `rs.upload(&hash, &data)` if remote_store is `Some`
- Upload failure logs `[anti_entropy] remote_store upload warning` and continues — non-fatal (D08)

### lib.rs

- `pub mod filesystem_remote_store;` declared
- `pub use filesystem_remote_store::FilesystemRemoteStore;` re-exported

## Verification Results

```
cargo build --workspace          # exits 0
cargo clippy -p edgestore-repl -- -D warnings  # clean, 0 errors
cargo test -p edgestore-repl filesystem_remote_store  # 5/5 pass
grep "FilesystemRemoteStore" edgestore-repl/src/lib.rs  # found
grep "with_remote_store" edgestore-repl/src/anti_entropy.rs  # found
```

## Deviations from Plan

None — plan executed exactly as written.

## Threat Flags

None. This plan introduces no new network endpoints or auth paths. All I/O is local filesystem under operator-controlled permissions (T-04-11 accepted). Upload atomicity (T-04-10) and malformed filename filtering (T-04-12) are both implemented as planned.

## Self-Check: PASSED

- `edgestore-repl/src/filesystem_remote_store.rs` exists
- `edgestore-repl/src/anti_entropy.rs` modified with `remote_store` field and `with_remote_store` builder
- Commits `0c4af7a` and `0fa3ade` confirmed in git log
- 5 unit tests pass; `cargo build --workspace` exits 0; `cargo clippy -D warnings` clean
