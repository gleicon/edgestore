//! FilesystemRemoteStore — local-filesystem implementation of `RemoteStore`.
//!
//! Files are content-addressed: named `{hash_hex}.seg` where `hash_hex` is the
//! 64-character lowercase hex encoding of the 32-byte BLAKE3 hash. Listing the
//! directory is equivalent to listing stored segments.
//!
//! This is the Phase 4 implementation (Plan 04-04, D04). Real S3 (`S3RemoteStore`)
//! is a future phase deliverable.

use std::path::PathBuf;

use edgestore::error::EdgestoreError;
use edgestore::RemoteStore;

/// Local-filesystem implementation of `RemoteStore`.
///
/// All operations are idempotent and atomic where applicable.
/// `upload` uses a `.tmp` write + rename to prevent torn writes (T-04-10).
pub struct FilesystemRemoteStore {
    base_dir: PathBuf,
}

impl FilesystemRemoteStore {
    /// Create a new `FilesystemRemoteStore` rooted at `base_dir`.
    ///
    /// Creates `base_dir` (and all parent directories) if it does not exist.
    pub fn new(base_dir: PathBuf) -> Result<Self, EdgestoreError> {
        std::fs::create_dir_all(&base_dir)
            .map_err(|e| EdgestoreError::ReplicationError(e.to_string()))?;
        Ok(Self { base_dir })
    }

    /// Encode a 32-byte hash as a 64-character lowercase hex string.
    fn hash_hex(hash: &[u8; 32]) -> String {
        hash.iter().map(|b| format!("{:02x}", b)).collect::<String>()
    }

    /// Return the path `{base_dir}/{hash_hex}.seg` for the given hash.
    fn seg_path(&self, hash: &[u8; 32]) -> PathBuf {
        self.base_dir.join(format!("{}.seg", Self::hash_hex(hash)))
    }
}

impl RemoteStore for FilesystemRemoteStore {
    /// Store `data` under `hash`. Idempotent: if the file already exists, returns `Ok(())`.
    ///
    /// Writes to a `.tmp` file first, then renames atomically (T-04-10).
    fn upload(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError> {
        let dest = self.seg_path(hash);

        // Content-addressed: if it already exists, nothing to do.
        if dest.exists() {
            return Ok(());
        }

        let tmp = self
            .base_dir
            .join(format!("{}.tmp", Self::hash_hex(hash)));

        std::fs::write(&tmp, data)
            .map_err(|e| EdgestoreError::ReplicationError(e.to_string()))?;

        std::fs::rename(&tmp, &dest)
            .map_err(|e| EdgestoreError::ReplicationError(e.to_string()))?;

        Ok(())
    }

    /// Download the segment bytes for `hash`.
    ///
    /// Returns `EdgestoreError::ReplicationError` if the segment is not present.
    fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
        let path = self.seg_path(hash);
        std::fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                EdgestoreError::ReplicationError(format!(
                    "segment not found: {}",
                    Self::hash_hex(hash)
                ))
            } else {
                EdgestoreError::ReplicationError(e.to_string())
            }
        })
    }

    /// List all stored segment hashes by scanning `{base_dir}/*.seg`.
    ///
    /// Filenames that are not exactly 64 lowercase hex characters followed by `.seg`
    /// are silently skipped (T-04-12).
    fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError> {
        let entries = std::fs::read_dir(&self.base_dir)
            .map_err(|e| EdgestoreError::ReplicationError(e.to_string()))?;

        let mut hashes = Vec::new();

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = match file_name.to_str() {
                Some(n) => n.to_owned(),
                None => continue,
            };

            // Must end with ".seg"
            if !name.ends_with(".seg") {
                continue;
            }

            // Stem must be exactly 64 characters (32 bytes * 2 hex digits).
            let stem = &name[..name.len() - 4]; // strip ".seg"
            if stem.len() != 64 {
                continue;
            }

            // Parse 64 hex chars → [u8; 32]
            let parsed: Option<[u8; 32]> = (0..32)
                .map(|i| u8::from_str_radix(&stem[i * 2..i * 2 + 2], 16).ok())
                .collect::<Option<Vec<u8>>>()
                .and_then(|v| v.try_into().ok());

            if let Some(hash) = parsed {
                hashes.push(hash);
            }
        }

        Ok(hashes)
    }

    /// Remove the segment for `hash`. No-op if the segment does not exist (idempotent).
    fn delete(&self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
        let path = self.seg_path(hash);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(EdgestoreError::ReplicationError(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, FilesystemRemoteStore) {
        let dir = TempDir::new().expect("tempdir");
        let store = FilesystemRemoteStore::new(dir.path().to_path_buf())
            .expect("FilesystemRemoteStore::new");
        (dir, store)
    }

    #[test]
    fn test_upload_download_roundtrip() {
        let (_dir, store) = make_store();
        let hash = [0x42u8; 32];
        let data = b"hello edgestore";

        store.upload(&hash, data).expect("upload");
        let got = store.download(&hash).expect("download");
        assert_eq!(got, data);
    }

    #[test]
    fn test_upload_idempotent() {
        let (_dir, store) = make_store();
        let hash = [0x42u8; 32];
        let data = b"original";

        store.upload(&hash, data).expect("first upload");
        // Second upload with same hash must succeed without error.
        store.upload(&hash, b"different").expect("second upload (idempotent)");

        // File should still contain the original data (idempotent — skipped overwrite).
        let got = store.download(&hash).expect("download after idempotent upload");
        assert_eq!(got, data);
    }

    #[test]
    fn test_list_returns_uploaded_hashes() {
        let (_dir, store) = make_store();
        let hash1 = [0x01u8; 32];
        let hash2 = [0x02u8; 32];
        let hash3 = [0x03u8; 32];

        store.upload(&hash1, b"a").expect("upload 1");
        store.upload(&hash2, b"b").expect("upload 2");
        store.upload(&hash3, b"c").expect("upload 3");

        let mut listed = store.list().expect("list");
        listed.sort();

        let mut expected = vec![hash1, hash2, hash3];
        expected.sort();

        assert_eq!(listed, expected);
    }

    #[test]
    fn test_delete_removes_file() {
        let (_dir, store) = make_store();
        let hash = [0x42u8; 32];

        store.upload(&hash, b"segment data").expect("upload");
        store.delete(&hash).expect("delete");

        // Download must now fail.
        let result = store.download(&hash);
        assert!(result.is_err(), "download after delete should return Err");
    }

    #[test]
    fn test_download_not_found() {
        let (_dir, store) = make_store();
        let hash = [0xFFu8; 32];

        let result = store.download(&hash);
        assert!(result.is_err(), "download of non-existent hash should return Err");
    }
}
