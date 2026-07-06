use std::path::PathBuf;
use crate::types::Compression;
use crate::memtable::{MemTable, BTreeMemTable};

/// Database configuration.
pub struct EdgestoreConfig {
    /// Filesystem path for the database directory.
    pub path: PathBuf,
    /// Maximum WAL file size in bytes before rotation.
    pub wal_max_bytes: u64,
    /// Maximum WAL file age in seconds before rotation.
    pub wal_max_age_secs: u64,
    /// Target uncompressed segment size in bytes.
    pub segment_size_bytes: u64,
    /// Maximum memtable size in bytes before an automatic flush to a segment.
    ///
    /// When the in-memory write buffer exceeds this threshold, `put` triggers
    /// `flush_to_segments` automatically. Defaults to 8 MB. Lower this on
    /// memory-constrained hardware (IoT, edge); raise it for high-throughput
    /// server workloads where you want larger segments.
    pub memtable_max_bytes: u64,
    /// Cohort window width in seconds for deathtime compaction.
    pub cohort_window_secs: u64,
    /// Compression algorithm for WAL records.
    pub compression_wal: Compression,
    /// Compression algorithm for segment blocks.
    pub compression_segments: Compression,
    /// Target false-positive rate for xor filters.
    pub xor_filter_fpr: f64,
    /// Write-amplification budget per compaction cycle in bytes.
    pub compaction_write_budget_bytes: u64,
    /// Enable FDP (Flexible Data Placement) hints on NVMe 2.0 hardware.
    pub fdp_enabled: bool,
    /// Maximum HNSW index file size in bytes before falling back to flat scan.
    ///
    /// When the `.hnsw` sidecar file exceeds this threshold, `vector_search`
    /// skips loading the index and uses the flat SIMD scan instead. Prevents
    /// silent OOM on large vector collections. Defaults to 512 MB.
    pub hnsw_max_ram_bytes: u64,
    /// Factory function that returns a new empty memtable.
    pub memtable_factory: Box<dyn Fn() -> Box<dyn MemTable> + Send + Sync>,
}

impl std::fmt::Debug for EdgestoreConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgestoreConfig")
            .field("path", &self.path)
            .field("wal_max_bytes", &self.wal_max_bytes)
            .field("wal_max_age_secs", &self.wal_max_age_secs)
            .field("segment_size_bytes", &self.segment_size_bytes)
            .field("memtable_max_bytes", &self.memtable_max_bytes)
            .field("cohort_window_secs", &self.cohort_window_secs)
            .field("compression_wal", &self.compression_wal)
            .field("compression_segments", &self.compression_segments)
            .field("xor_filter_fpr", &self.xor_filter_fpr)
            .field("compaction_write_budget_bytes", &self.compaction_write_budget_bytes)
            .field("fdp_enabled", &self.fdp_enabled)
            .field("hnsw_max_ram_bytes", &self.hnsw_max_ram_bytes)
            .field("memtable_factory", &"<fn>")
            .finish()
    }
}

impl EdgestoreConfig {
    /// Create a new `EdgestoreConfig` with sensible defaults.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        EdgestoreConfig {
            path: path.into(),
            wal_max_bytes: 64 * 1024 * 1024,
            wal_max_age_secs: 60,
            segment_size_bytes: 16 * 1024 * 1024,
            memtable_max_bytes: 8 * 1024 * 1024,
            cohort_window_secs: 3600,
            compression_wal: Compression::Lz4,
            compression_segments: Compression::Zstd(1),
            xor_filter_fpr: 0.01,
            compaction_write_budget_bytes: 256 * 1024 * 1024,
            fdp_enabled: false,
            hnsw_max_ram_bytes: 512 * 1024 * 1024,
            memtable_factory: Box::new(|| Box::new(BTreeMemTable::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults() {
        let cfg = EdgestoreConfig::new("/tmp/test_db");
        assert_eq!(cfg.wal_max_bytes, 64 * 1024 * 1024);
        assert_eq!(cfg.wal_max_age_secs, 60);
        assert_eq!(cfg.segment_size_bytes, 16 * 1024 * 1024);
        assert_eq!(cfg.memtable_max_bytes, 8 * 1024 * 1024);
        assert_eq!(cfg.cohort_window_secs, 3600);
        assert_eq!(cfg.xor_filter_fpr, 0.01);
        assert_eq!(cfg.compaction_write_budget_bytes, 256 * 1024 * 1024);
        assert_eq!(cfg.fdp_enabled, false);
        assert_eq!(cfg.hnsw_max_ram_bytes, 512 * 1024 * 1024);
    }

    #[test]
    fn test_path_stored() {
        let cfg = EdgestoreConfig::new("/some/path");
        assert_eq!(cfg.path, std::path::PathBuf::from("/some/path"));
    }

    #[test]
    fn test_memtable_factory_produces_empty_memtable() {
        let cfg = EdgestoreConfig::new("/tmp");
        let mt = (cfg.memtable_factory)();
        assert!(mt.is_empty());
    }

    #[test]
    fn test_memtable_max_bytes_override() {
        let mut cfg = EdgestoreConfig::new("/tmp");
        cfg.memtable_max_bytes = 1024;
        assert_eq!(cfg.memtable_max_bytes, 1024);
    }

    #[test]
    fn test_hnsw_max_ram_bytes_override() {
        let mut cfg = EdgestoreConfig::new("/tmp");
        cfg.hnsw_max_ram_bytes = 64 * 1024 * 1024;
        assert_eq!(cfg.hnsw_max_ram_bytes, 64 * 1024 * 1024);
    }
}
