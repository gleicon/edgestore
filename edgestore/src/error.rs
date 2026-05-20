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
        }
    }
}

impl std::error::Error for EdgestoreError {}

impl From<std::io::Error> for EdgestoreError {
    fn from(e: std::io::Error) -> Self {
        EdgestoreError::Io(e)
    }
}
