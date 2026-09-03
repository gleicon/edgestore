//! Trait for durable segment backends.
//!
//! `FilesystemRemoteStore` is the Phase 4 impl (Plan 04-04). S3 is a future phase (D04).
//!
//! All operations are content-addressed by BLAKE3 hash. `upload` is idempotent.

use crate::error::EdgestoreError;

/// Abstraction over a durable, content-addressed segment store.
///
/// Phase 4 provides `FilesystemRemoteStore` (Plan 04-04). S3 (`S3RemoteStore`) is a
/// future-phase deliverable (D04, D05). Implementations must be `Send + Sync` so they
/// can be shared across threads.
///
/// ## Sidecar methods
///
/// `upload_aux` / `download_aux` store per-segment sidecar files (e.g. `.idx`, `.xf`,
/// `.meta`). These eliminate the one-time ZSTD decompression cost on `ImmutableEngine`
/// init by letting the engine download pre-built index and filter data instead of
/// deriving them from raw `.dat` blocks.
///
/// Default implementations return `Err(InvalidOperation)` so existing stores compile
/// unchanged. Stores that want sidecar support override both methods.
pub trait RemoteStore: Send + Sync {
    /// Store segment bytes under content hash. Idempotent.
    ///
    /// If a segment with the same hash already exists, the implementation MAY skip the
    /// write without returning an error.
    fn upload(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError>;

    /// Store segment bytes only if absent; returns `true` when newly written.
    ///
    /// Semantics (BtrLog §5.1 — arXiv:2606.27051, Kuschewski et al., VLDB 2026):
    /// - Returns `Ok(true)`  when the segment was written for the first time.
    /// - Returns `Ok(false)` when a segment with the same hash already existed.
    /// - Returns `Err(_)`    on any real I/O or network failure.
    ///
    /// The default implementation calls `upload()` and returns `Ok(true)` — fully
    /// correct but not atomic.  Override for true idempotent-create semantics:
    /// - `FilesystemRemoteStore`: `OpenOptions::new().create_new(true)` — the OS
    ///   rejects concurrent writers via `EEXIST`.
    /// - `S3RemoteStore`: `put_object().if_none_match("*")` — S3 returns HTTP 412
    ///   when the key already exists.
    ///
    /// Used by `TieredEngine::archive_segments` and `AntiEntropyLoop::run_once` to
    /// prevent double-uploads on retry without requiring a prior `list()` call.
    fn upload_if_absent(&self, hash: &[u8; 32], data: &[u8]) -> Result<bool, EdgestoreError> {
        self.upload(hash, data)?;
        Ok(true)
    }

    /// Retrieve segment bytes by content hash.
    ///
    /// Returns `EdgestoreError::KeyNotFound` if the hash is not present.
    fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError>;

    /// List all stored segment hashes.
    fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError>;

    /// Remove a segment by content hash. No-op if the segment is not present.
    fn delete(&self, hash: &[u8; 32]) -> Result<(), EdgestoreError>;

    /// Store a sidecar file for the segment identified by `hash`.
    ///
    /// `ext` names the sidecar type: `"idx"` (sparse index), `"xf"` (xor filter), or
    /// `"meta"` (segment metadata). Must consist entirely of lowercase ASCII letters
    /// (`[a-z]+`); implementations reject any other value with `Err(InvalidOperation)`.
    /// Idempotent: re-uploading the same bytes is fine.
    ///
    /// Default: returns `Err(InvalidOperation)` — override to enable sidecar support.
    fn upload_aux(&self, hash: &[u8; 32], ext: &str, data: &[u8]) -> Result<(), EdgestoreError> {
        let _ = (hash, ext, data);
        Err(EdgestoreError::InvalidOperation(
            "upload_aux not implemented for this RemoteStore".to_string(),
        ))
    }

    /// Retrieve a sidecar file for the segment identified by `hash`.
    ///
    /// `ext` names the sidecar type: `"idx"`, `"xf"`, or `"meta"`. Must be lowercase
    /// ASCII letters only; invalid values return `Err(InvalidOperation)`.
    ///
    /// Default: returns `Err(InvalidOperation)` — override to enable sidecar support.
    fn download_aux(&self, hash: &[u8; 32], ext: &str) -> Result<Vec<u8>, EdgestoreError> {
        let _ = (hash, ext);
        Err(EdgestoreError::InvalidOperation(
            "download_aux not implemented for this RemoteStore".to_string(),
        ))
    }
}
