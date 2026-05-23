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
pub trait RemoteStore: Send + Sync {
    /// Store segment bytes under content hash. Idempotent.
    ///
    /// If a segment with the same hash already exists, the implementation MAY skip the
    /// write without returning an error.
    fn upload(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError>;

    /// Retrieve segment bytes by content hash.
    ///
    /// Returns `EdgestoreError::KeyNotFound` if the hash is not present.
    fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError>;

    /// List all stored segment hashes.
    fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError>;

    /// Remove a segment by content hash. No-op if the segment is not present.
    fn delete(&self, hash: &[u8; 32]) -> Result<(), EdgestoreError>;
}
