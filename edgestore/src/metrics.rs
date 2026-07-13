use std::sync::atomic::{AtomicU64, Ordering};

/// Per-operation counters and cumulative nanoseconds. All fields are AtomicU64
/// so a monitoring thread can read them without holding &mut Engine.
/// The engine is single-writer, so Ordering::Relaxed is correct everywhere.
pub(crate) struct EngineMetrics {
    pub(crate) puts: AtomicU64,
    pub(crate) gets: AtomicU64,
    pub(crate) deletes: AtomicU64,
    pub(crate) ranges: AtomicU64,
    pub(crate) prefixes: AtomicU64,
    pub(crate) transactions_committed: AtomicU64,
    pub(crate) transactions_rolled_back: AtomicU64,
    pub(crate) compactions: AtomicU64,
    pub(crate) segment_flushes: AtomicU64,
    pub(crate) wal_rotations: AtomicU64,

    pub(crate) put_nanos: AtomicU64,
    pub(crate) get_nanos: AtomicU64,
    pub(crate) delete_nanos: AtomicU64,
    pub(crate) range_nanos: AtomicU64,
    pub(crate) prefix_nanos: AtomicU64,
    pub(crate) transaction_commit_nanos: AtomicU64,
    pub(crate) compaction_nanos: AtomicU64,
    pub(crate) segment_flush_nanos: AtomicU64,
    pub(crate) vector_index_loads: AtomicU64,
    pub(crate) vector_index_stales: AtomicU64,
    pub(crate) vector_index_load_nanos: AtomicU64,
}

impl EngineMetrics {
    pub(crate) fn new() -> Self {
        Self {
            puts: AtomicU64::new(0),
            gets: AtomicU64::new(0),
            deletes: AtomicU64::new(0),
            ranges: AtomicU64::new(0),
            prefixes: AtomicU64::new(0),
            transactions_committed: AtomicU64::new(0),
            transactions_rolled_back: AtomicU64::new(0),
            compactions: AtomicU64::new(0),
            segment_flushes: AtomicU64::new(0),
            wal_rotations: AtomicU64::new(0),
            put_nanos: AtomicU64::new(0),
            get_nanos: AtomicU64::new(0),
            delete_nanos: AtomicU64::new(0),
            range_nanos: AtomicU64::new(0),
            prefix_nanos: AtomicU64::new(0),
            transaction_commit_nanos: AtomicU64::new(0),
            compaction_nanos: AtomicU64::new(0),
            segment_flush_nanos: AtomicU64::new(0),
            vector_index_loads: AtomicU64::new(0),
            vector_index_stales: AtomicU64::new(0),
            vector_index_load_nanos: AtomicU64::new(0),
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            puts: self.puts.load(Ordering::Relaxed),
            gets: self.gets.load(Ordering::Relaxed),
            deletes: self.deletes.load(Ordering::Relaxed),
            ranges: self.ranges.load(Ordering::Relaxed),
            prefixes: self.prefixes.load(Ordering::Relaxed),
            transactions_committed: self.transactions_committed.load(Ordering::Relaxed),
            transactions_rolled_back: self.transactions_rolled_back.load(Ordering::Relaxed),
            compactions: self.compactions.load(Ordering::Relaxed),
            segment_flushes: self.segment_flushes.load(Ordering::Relaxed),
            wal_rotations: self.wal_rotations.load(Ordering::Relaxed),
            put_nanos_total: self.put_nanos.load(Ordering::Relaxed),
            get_nanos_total: self.get_nanos.load(Ordering::Relaxed),
            delete_nanos_total: self.delete_nanos.load(Ordering::Relaxed),
            range_nanos_total: self.range_nanos.load(Ordering::Relaxed),
            prefix_nanos_total: self.prefix_nanos.load(Ordering::Relaxed),
            transaction_commit_nanos_total: self.transaction_commit_nanos.load(Ordering::Relaxed),
            compaction_nanos_total: self.compaction_nanos.load(Ordering::Relaxed),
            segment_flush_nanos_total: self.segment_flush_nanos.load(Ordering::Relaxed),
            vector_index_loads: self.vector_index_loads.load(Ordering::Relaxed),
            vector_index_stales: self.vector_index_stales.load(Ordering::Relaxed),
            vector_index_load_ms: self.vector_index_load_nanos.load(Ordering::Relaxed) / 1_000_000,
            vector_index_stale: false, // set by caller
        }
    }
}

impl Default for EngineMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Point-in-time snapshot of engine metrics. Cheap to clone and format.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    /// Number of put operations.
    pub puts: u64,
    /// Number of get operations.
    pub gets: u64,
    /// Number of delete operations.
    pub deletes: u64,
    /// Number of range scan operations.
    pub ranges: u64,
    /// Number of prefix scan operations.
    pub prefixes: u64,
    /// Number of committed transactions.
    pub transactions_committed: u64,
    /// Number of rolled-back transactions.
    pub transactions_rolled_back: u64,
    /// Number of compaction runs.
    pub compactions: u64,
    /// Number of segment flushes.
    pub segment_flushes: u64,
    /// Number of WAL rotations.
    pub wal_rotations: u64,

    /// Total nanoseconds spent in put operations.
    pub put_nanos_total: u64,
    /// Total nanoseconds spent in get operations.
    pub get_nanos_total: u64,
    /// Total nanoseconds spent in delete operations.
    pub delete_nanos_total: u64,
    /// Total nanoseconds spent in range scan operations.
    pub range_nanos_total: u64,
    /// Total nanoseconds spent in prefix scan operations.
    pub prefix_nanos_total: u64,
    /// Total nanoseconds spent committing transactions.
    pub transaction_commit_nanos_total: u64,
    /// Total nanoseconds spent in compactions.
    pub compaction_nanos_total: u64,
    /// Total nanoseconds spent flushing segments.
    pub segment_flush_nanos_total: u64,
    /// Number of vector index loads.
    pub vector_index_loads: u64,
    /// Number of stale vector index detections.
    pub vector_index_stales: u64,
    /// Total milliseconds spent loading vector indices.
    pub vector_index_load_ms: u64,
    /// True if the last vector index load was stale.
    pub vector_index_stale: bool,
}

impl MetricsSnapshot {
    /// Average put latency in nanoseconds.
    pub fn put_avg_ns(&self) -> u64 {
        self.put_nanos_total / self.puts.max(1)
    }
    /// Average get latency in nanoseconds.
    pub fn get_avg_ns(&self) -> u64 {
        self.get_nanos_total / self.gets.max(1)
    }
    /// Average delete latency in nanoseconds.
    pub fn delete_avg_ns(&self) -> u64 {
        self.delete_nanos_total / self.deletes.max(1)
    }
    /// Average range scan latency in nanoseconds.
    pub fn range_avg_ns(&self) -> u64 {
        self.range_nanos_total / self.ranges.max(1)
    }
    /// Average prefix scan latency in nanoseconds.
    pub fn prefix_avg_ns(&self) -> u64 {
        self.prefix_nanos_total / self.prefixes.max(1)
    }
    /// Average transaction commit latency in nanoseconds.
    pub fn transaction_commit_avg_ns(&self) -> u64 {
        self.transaction_commit_nanos_total / self.transactions_committed.max(1)
    }
}

impl std::fmt::Display for MetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "puts:                  {} ({} ns avg)",
            self.puts,
            self.put_avg_ns()
        )?;
        writeln!(
            f,
            "gets:                  {} ({} ns avg)",
            self.gets,
            self.get_avg_ns()
        )?;
        writeln!(
            f,
            "deletes:               {} ({} ns avg)",
            self.deletes,
            self.delete_avg_ns()
        )?;
        writeln!(
            f,
            "ranges:                {} ({} ns avg)",
            self.ranges,
            self.range_avg_ns()
        )?;
        writeln!(
            f,
            "prefixes:              {} ({} ns avg)",
            self.prefixes,
            self.prefix_avg_ns()
        )?;
        writeln!(
            f,
            "transactions committed: {} ({} ns avg)",
            self.transactions_committed,
            self.transaction_commit_avg_ns()
        )?;
        writeln!(
            f,
            "transactions rolled back: {}",
            self.transactions_rolled_back
        )?;
        writeln!(f, "compactions:           {}", self.compactions)?;
        writeln!(f, "segment flushes:       {}", self.segment_flushes)?;
        write!(f, "wal rotations:         {}", self.wal_rotations)
    }
}
