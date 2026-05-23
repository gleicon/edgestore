use std::fmt;

#[derive(Debug)]
pub enum EdgestoreError {
    Io(std::io::Error),
    Checksum { expected: u32, got: u32 },
    CorruptRecord(String),
    CorruptKey,
    WalFull,
    WriterBusy,
    InvalidOperation(String),
    NamespaceTooLong { len: usize, max: usize },
    KeyNotFound,
    FormatVersion { expected: u8, got: u8 },
    SegmentCorrupt(String),
    ManifestCorrupt(String),
    CompactionError(String),
    ReplicationError(String),
    DimensionMismatch { expected: usize, actual: usize },
    CorruptData(String),
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
