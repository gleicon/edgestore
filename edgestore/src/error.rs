use std::fmt;

/// All error variants returned by EdgeStore operations.
#[derive(Debug)]
pub enum EdgestoreError {
    /// Underlying I/O error.
    Io(std::io::Error),
    /// CRC32C checksum mismatch.
    Checksum {
        /// Expected CRC32C value.
        expected: u32,
        /// Computed CRC32C value.
        got: u32,
    },
    /// Corrupt WAL record.
    CorruptRecord(String),
    /// Corrupt namespace-key encoding.
    CorruptKey,
    /// WAL rotation threshold reached.
    WalFull,
    /// Another writer holds the database lock.
    WriterBusy,
    /// Invalid operation (e.g. transaction not active).
    InvalidOperation(String),
    /// Namespace length exceeds u16::MAX bytes.
    NamespaceTooLong {
        /// Actual namespace length.
        len: usize,
        /// Maximum allowed namespace length.
        max: usize,
    },
    /// Key not found (used by storage backends).
    KeyNotFound,
    /// WAL or segment format version mismatch.
    FormatVersion {
        /// Expected format version.
        expected: u8,
        /// Actual format version found in file.
        got: u8,
    },
    /// Corrupt segment file.
    SegmentCorrupt(String),
    /// Corrupt manifest file.
    ManifestCorrupt(String),
    /// Compaction failed.
    CompactionError(String),
    /// Replication sync failed.
    ReplicationError(String),
    /// Vector dimension mismatch.
    DimensionMismatch {
        /// Expected byte count (dims * element_size).
        expected: usize,
        /// Actual byte count provided.
        actual: usize,
    },
    /// General corrupt data error.
    CorruptData(String),
    /// Write attempted on a read-only engine.
    ReadOnly,
}

impl fmt::Display for EdgestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EdgestoreError::Io(e) => write!(f, "I/O error: {}", e),
            EdgestoreError::Checksum { expected, got } => {
                write!(f, "CRC32C mismatch: expected {:#010x}, got {:#010x}", expected, got)
            }
            EdgestoreError::CorruptRecord(msg) => write!(f, "corrupt WAL record: {}", msg),
            EdgestoreError::CorruptKey => write!(f, "corrupt key encoding"),
            EdgestoreError::WalFull => write!(f, "WAL rotation threshold reached"),
            EdgestoreError::WriterBusy => write!(f, "another writer holds the database lock"),
            EdgestoreError::InvalidOperation(msg) => write!(f, "invalid operation: {}", msg),
            EdgestoreError::NamespaceTooLong { len, max } => {
                write!(f, "namespace length {} exceeds maximum {}", len, max)
            }
            EdgestoreError::KeyNotFound => write!(f, "key not found"),
            EdgestoreError::FormatVersion { expected, got } => {
                write!(f, "WAL format version mismatch: expected {}, got {}", expected, got)
            }
            EdgestoreError::SegmentCorrupt(msg) => write!(f, "segment corrupt: {}", msg),
            EdgestoreError::ManifestCorrupt(msg) => write!(f, "manifest corrupt: {}", msg),
            EdgestoreError::CompactionError(msg) => write!(f, "compaction error: {}", msg),
            EdgestoreError::ReplicationError(msg) => write!(f, "replication error: {}", msg),
            EdgestoreError::DimensionMismatch { expected, actual } => {
                write!(f, "dimension mismatch: expected {} bytes, got {}", expected, actual)
            }
            EdgestoreError::CorruptData(msg) => write!(f, "corrupt data: {}", msg),
            EdgestoreError::ReadOnly => write!(f, "write attempted on a read-only engine"),
        }
    }
}

impl std::error::Error for EdgestoreError {}

impl From<std::io::Error> for EdgestoreError {
    fn from(e: std::io::Error) -> Self {
        EdgestoreError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_variants() {
        let cases: &[(&str, EdgestoreError)] = &[
            ("corrupt WAL record: bad data", EdgestoreError::CorruptRecord("bad data".to_string())),
            ("corrupt key encoding", EdgestoreError::CorruptKey),
            ("WAL rotation threshold reached", EdgestoreError::WalFull),
            ("another writer holds the database lock", EdgestoreError::WriterBusy),
            ("invalid operation: not active", EdgestoreError::InvalidOperation("not active".to_string())),
            ("namespace length 70000 exceeds maximum 65535", EdgestoreError::NamespaceTooLong { len: 70000, max: 65535 }),
            ("key not found", EdgestoreError::KeyNotFound),
            ("WAL format version mismatch: expected 1, got 2", EdgestoreError::FormatVersion { expected: 1, got: 2 }),
            ("segment corrupt: bad block", EdgestoreError::SegmentCorrupt("bad block".to_string())),
            ("manifest corrupt: truncated", EdgestoreError::ManifestCorrupt("truncated".to_string())),
            ("compaction error: oops", EdgestoreError::CompactionError("oops".to_string())),
        ];

        for (expected, err) in cases {
            assert_eq!(format!("{}", err), *expected, "Display mismatch for {:?}", err);
        }
    }

    #[test]
    fn test_io_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let edge_err = EdgestoreError::from(io_err);
        assert!(matches!(edge_err, EdgestoreError::Io(_)));
    }
}
