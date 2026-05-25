use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::EdgestoreError;

/// Placement hint for FDP (Flexible Data Placement, NVMe 2.0) backends.
///
/// The `cohort_bucket` maps to an FDP placement handle, allowing
/// cohort-aware data placement on supported hardware.
#[derive(Debug, Clone, Copy)]
pub struct PlacementHint {
    pub cohort_bucket: i64,
}

/// Pluggable storage backend abstraction.
///
/// All segment I/O goes through this trait so backends can be swapped
/// (local fs, remote object store, FDP-aware, in-memory for tests).
pub trait StorageBackend: Send + Sync {
    /// Read `data_len` bytes from `path` starting at `offset`.
    ///
    /// Returns the exact number of bytes read (may be < data_len if EOF).
    fn read(&self, path: &Path, offset: u64, data: &mut [u8]) -> Result<usize, EdgestoreError>;

    /// Write `data` to `path` starting at `offset`.
    ///
    /// Creates or truncates the file if necessary.
    fn write(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), EdgestoreError>;

    /// Write `data` with an optional placement hint.
    ///
    /// Default implementation ignores the hint and delegates to `write`.
    fn write_with_hint(
        &self,
        path: &Path,
        offset: u64,
        data: &[u8],
        _hint: PlacementHint,
    ) -> Result<(), EdgestoreError> {
        self.write(path, offset, data)
    }

    /// Ensure all prior writes to `path` are durable.
    fn flush(&self, path: &Path) -> Result<(), EdgestoreError>;

    /// Read the entire file contents at `path`.
    fn read_all(&self, path: &Path) -> Result<Vec<u8>, EdgestoreError> {
        let mut buf = Vec::new();
        let mut offset = 0u64;
        let chunk_size = 64 * 1024;
        loop {
            let mut chunk = vec![0u8; chunk_size];
            let n = self.read(path, offset, &mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            offset += n as u64;
            if n < chunk_size {
                break;
            }
        }
        Ok(buf)
    }
}

/// Default local-filesystem backend.
#[derive(Default)]
pub struct DefaultStorageBackend;

impl DefaultStorageBackend {
    pub fn new() -> Self {
        DefaultStorageBackend
    }
}

impl StorageBackend for DefaultStorageBackend {
    fn read(&self, path: &Path, offset: u64, data: &mut [u8]) -> Result<usize, EdgestoreError> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(offset))?;
        let n = f.read(data)?;
        Ok(n)
    }

    fn write(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), EdgestoreError> {
        use std::io::{Seek, SeekFrom, Write};
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new("")))?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        f.seek(SeekFrom::Start(offset))?;
        f.write_all(data)?;
        Ok(())
    }

    fn flush(&self, path: &Path) -> Result<(), EdgestoreError> {
        use std::io::Write;
        let f = std::fs::OpenOptions::new().write(true).open(path)?;
        let mut f = std::io::BufWriter::new(f);
        f.flush()?;
        let inner = f.into_inner().map_err(|e| EdgestoreError::Io(e.into_error()))?;
        inner.sync_all()?;
        Ok(())
    }
}

/// In-memory backend for unit tests.
pub struct MemoryStorageBackend {
    files: Mutex<HashMap<PathBuf, Vec<u8>>>,
}

impl Default for MemoryStorageBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStorageBackend {
    pub fn new() -> Self {
        MemoryStorageBackend {
            files: Mutex::new(HashMap::new()),
        }
    }
}

impl StorageBackend for MemoryStorageBackend {
    fn read(&self, path: &Path, offset: u64, data: &mut [u8]) -> Result<usize, EdgestoreError> {
        let files = self.files.lock().unwrap();
        let contents = files.get(path).cloned().unwrap_or_default();
        let start = offset as usize;
        if start >= contents.len() {
            return Ok(0);
        }
        let end = (start + data.len()).min(contents.len());
        let n = end - start;
        data[..n].copy_from_slice(&contents[start..end]);
        Ok(n)
    }

    fn write(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), EdgestoreError> {
        let mut files = self.files.lock().unwrap();
        let contents = files.entry(path.to_path_buf()).or_default();
        let start = offset as usize;
        let end = start + data.len();
        if end > contents.len() {
            contents.resize(end, 0);
        }
        contents[start..end].copy_from_slice(data);
        Ok(())
    }

    fn flush(&self, _path: &Path) -> Result<(), EdgestoreError> {
        Ok(())
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_backend_write_read() {
        let backend = DefaultStorageBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");

        backend.write(&path, 0, b"hello world").unwrap();
        backend.flush(&path).unwrap();

        let mut buf = [0u8; 11];
        let n = backend.read(&path, 0, &mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn test_default_backend_partial_read() {
        let backend = DefaultStorageBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");

        backend.write(&path, 0, b"hello world").unwrap();
        backend.flush(&path).unwrap();

        let mut buf = [0u8; 5];
        let n = backend.read(&path, 6, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn test_memory_backend_write_read() {
        let backend = MemoryStorageBackend::new();
        let path = Path::new("/tmp/test.bin");

        backend.write(path, 0, b"hello world").unwrap();
        backend.flush(path).unwrap();

        let mut buf = [0u8; 11];
        let n = backend.read(path, 0, &mut buf).unwrap();
        assert_eq!(n, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn test_memory_backend_overwrite() {
        let backend = MemoryStorageBackend::new();
        let path = Path::new("/tmp/test.bin");

        backend.write(path, 0, b"hello").unwrap();
        backend.write(path, 3, b"XX").unwrap();

        let mut buf = [0u8; 5];
        let n = backend.read(path, 0, &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"helXX");
    }

    #[test]
    fn test_memory_backend_isolated() {
        let backend_a = MemoryStorageBackend::new();
        let backend_b = MemoryStorageBackend::new();
        let path = Path::new("/tmp/test.bin");

        backend_a.write(path, 0, b"a").unwrap();
        backend_b.write(path, 0, b"b").unwrap();

        let mut buf = [0u8; 1];
        backend_a.read(path, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"a");
        backend_b.read(path, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"b");
    }
}
