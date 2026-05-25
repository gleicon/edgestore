use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::error::EdgestoreError;
use crate::storage_backend::{PlacementHint, StorageBackend};

/// FDP-aware storage backend.
///
/// Wraps another StorageBackend and emits FDP placement hints before writes
/// on platforms that support NVMe 2.0 Flexible Data Placement.
/// On unsupported platforms, the hint is silently ignored (logged at `info!`).
pub struct FdpStorageBackend {
    inner: Box<dyn StorageBackend>,
}

impl FdpStorageBackend {
    pub fn new(inner: Box<dyn StorageBackend>) -> Self {
        FdpStorageBackend { inner }
    }
}

impl StorageBackend for FdpStorageBackend {
    fn read(&self, path: &Path, offset: u64, data: &mut [u8]) -> Result<usize, EdgestoreError> {
        self.inner.read(path, offset, data)
    }

    fn write(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), EdgestoreError> {
        self.inner.write(path, offset, data)
    }

    fn write_with_hint(
        &self,
        path: &Path,
        offset: u64,
        data: &[u8],
        hint: PlacementHint,
    ) -> Result<(), EdgestoreError> {
        #[cfg(target_os = "linux")]
        {
            // Attempt to emit FDP hint via fcntl on the target file descriptor.
            // Actual FDP ioctl constants vary by filesystem; this is a stub.
            if let Ok(_fd) = std::os::fd::AsRawFd::as_raw_fd(
                &std::fs::OpenOptions::new().write(true).open(path)?,
            ) {
                // FDP hint would be emitted here via fcntl(fd, F_SET_FILE_DATA_PLACEMENT_HINT, ...)
                // For now, just log the hint.
                log::info!("FDP hint: cohort_bucket={} for {:?}", hint.cohort_bucket, path);
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = hint; // suppress unused warning on non-Linux
        }
        self.inner.write_with_hint(path, offset, data, hint)
    }

    fn flush(&self, path: &Path) -> Result<(), EdgestoreError> {
        self.inner.flush(path)
    }
}

/// Mock FDP backend that records placement hints for test verification.
pub struct MockFdpBackend {
    inner: Box<dyn StorageBackend>,
    pub recorded_hints: Mutex<Vec<(PathBuf, u64, PlacementHint)>>,
}

impl MockFdpBackend {
    pub fn new(inner: Box<dyn StorageBackend>) -> Self {
        MockFdpBackend {
            inner,
            recorded_hints: Mutex::new(Vec::new()),
        }
    }
}

impl StorageBackend for MockFdpBackend {
    fn read(&self, path: &Path, offset: u64, data: &mut [u8]) -> Result<usize, EdgestoreError> {
        self.inner.read(path, offset, data)
    }

    fn write(&self, path: &Path, offset: u64, data: &[u8]) -> Result<(), EdgestoreError> {
        self.inner.write(path, offset, data)
    }

    fn write_with_hint(
        &self,
        path: &Path,
        offset: u64,
        data: &[u8],
        hint: PlacementHint,
    ) -> Result<(), EdgestoreError> {
        self.recorded_hints.lock().unwrap().push((path.to_path_buf(), offset, hint));
        self.inner.write_with_hint(path, offset, data, hint)
    }

    fn flush(&self, path: &Path) -> Result<(), EdgestoreError> {
        self.inner.flush(path)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage_backend::MemoryStorageBackend;

    #[test]
    fn test_mock_fdp_records_hint() {
        let inner = Box::new(MemoryStorageBackend::new());
        let backend = MockFdpBackend::new(inner);
        let path = Path::new("/tmp/test.bin");

        let hint = PlacementHint { cohort_bucket: 42 };
        backend.write_with_hint(path, 0, b"hello", hint).unwrap();

        let hints = backend.recorded_hints.lock().unwrap();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].0, path);
        assert_eq!(hints[0].1, 0);
        assert_eq!(hints[0].2.cohort_bucket, 42);
    }

    #[test]
    fn test_fdp_backend_no_panic() {
        let inner = Box::new(MemoryStorageBackend::new());
        let backend = FdpStorageBackend::new(inner);
        let path = Path::new("/tmp/test.bin");

        let hint = PlacementHint { cohort_bucket: 7 };
        backend.write_with_hint(path, 0, b"data", hint).unwrap();
        backend.flush(path).unwrap();

        let mut buf = [0u8; 4];
        let n = backend.read(path, 0, &mut buf).unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf, b"data");
    }
}
