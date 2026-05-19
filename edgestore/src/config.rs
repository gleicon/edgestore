use std::path::PathBuf;
use crate::types::Compression;
use crate::memtable::{MemTable, BTreeMemTable};

pub struct EdgestoreConfig {
    pub path: PathBuf,
    pub wal_max_bytes: u64,
    pub wal_max_age_secs: u64,
    pub segment_size_bytes: u64,
    pub cohort_window_secs: u64,
    pub compression_wal: Compression,
    pub compression_segments: Compression,
    pub xor_filter_fpr: f64,
    pub memtable_factory: Box<dyn Fn() -> Box<dyn MemTable> + Send + Sync>,
}

impl std::fmt::Debug for EdgestoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgestoreConfig")
            .field("path", &self.path)
            .field("wal_max_bytes", &self.wal_max_bytes)
            .field("wal_max_age_secs", &self.wal_max_age_secs)
            .field("segment_size_bytes", &self.segment_size_bytes)
            .field("cohort_window_secs", &self.cohort_window_secs)
            .field("compression_wal", &self.compression_wal)
            .field("compression_segments", &self.compression_segments)
            .field("xor_filter_fpr", &self.xor_filter_fpr)
            .field("memtable_factory", &"<fn>")
            .finish()
    }
}

impl EdgestoreConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        EdgestoreConfig {
            path: path.into(),
            wal_max_bytes: 64 * 1024 * 1024,
            wal_max_age_secs: 60,
            segment_size_bytes: 16 * 1024 * 1024,
            cohort_window_secs: 3600,
            compression_wal: Compression::Lz4,
            compression_segments: Compression::Zstd(1),
            xor_filter_fpr: 0.01,
            memtable_factory: Box::new(|| Box::new(BTreeMemTable::new())),
        }
    }
}
