use std::sync::atomic::{AtomicU64, Ordering};

/// Per-operation counters and cumulative nanoseconds. All fields are AtomicU64
/// so a monitoring thread can read them without holding &mut Engine.
/// The engine is single-writer, so Ordering::Relaxed is correct everywhere.
pub struct EngineMetrics {
    pub puts: AtomicU64,
    pub gets: AtomicU64,
    pub deletes: AtomicU64,
    pub ranges: AtomicU64,
    pub prefixes: AtomicU64,
    pub transactions_committed: AtomicU64,
    pub transactions_rolled_back: AtomicU64,
    pub compactions: AtomicU64,
    pub segment_flushes: AtomicU64,
    pub wal_rotations: AtomicU64,

    pub put_nanos: AtomicU64,
    pub get_nanos: AtomicU64,
    pub delete_nanos: AtomicU64,
    pub range_nanos: AtomicU64,
    pub prefix_nanos: AtomicU64,
    pub transaction_commit_nanos: AtomicU64,
    pub compaction_nanos: AtomicU64,
    pub segment_flush_nanos: AtomicU64,
    pub vector_index_loads: AtomicU64,
    pub vector_index_stales: AtomicU64,
    pub vector_index_load_nanos: AtomicU64,
}

impl EngineMetrics {
    pub fn new() -> Self {
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
    pub puts: u64,
    pub gets: u64,
    pub deletes: u64,
    pub ranges: u64,
    pub prefixes: u64,
    pub transactions_committed: u64,
    pub transactions_rolled_back: u64,
    pub compactions: u64,
    pub segment_flushes: u64,
    pub wal_rotations: u64,

    pub put_nanos_total: u64,
    pub get_nanos_total: u64,
    pub delete_nanos_total: u64,
    pub range_nanos_total: u64,
    pub prefix_nanos_total: u64,
    pub transaction_commit_nanos_total: u64,
    pub compaction_nanos_total: u64,
    pub segment_flush_nanos_total: u64,
    pub vector_index_loads: u64,
    pub vector_index_stales: u64,
    pub vector_index_load_ms: u64,
    pub vector_index_stale: bool,
}

impl MetricsSnapshot {
    pub fn put_avg_ns(&self) -> u64 {
        self.put_nanos_total / self.puts.max(1)
    }
    pub fn get_avg_ns(&self) -> u64 {
        self.get_nanos_total / self.gets.max(1)
    }
    pub fn delete_avg_ns(&self) -> u64 {
        self.delete_nanos_total / self.deletes.max(1)
    }
    pub fn range_avg_ns(&self) -> u64 {
        self.range_nanos_total / self.ranges.max(1)
    }
    pub fn prefix_avg_ns(&self) -> u64 {
        self.prefix_nanos_total / self.prefixes.max(1)
    }
    pub fn transaction_commit_avg_ns(&self) -> u64 {
        self.transaction_commit_nanos_total / self.transactions_committed.max(1)
    }
}

impl std::fmt::Display for MetricsSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "puts:                  {} ({} ns avg)", self.puts, self.put_avg_ns())?;
        writeln!(f, "gets:                  {} ({} ns avg)", self.gets, self.get_avg_ns())?;
        writeln!(f, "deletes:               {} ({} ns avg)", self.deletes, self.delete_avg_ns())?;
        writeln!(f, "ranges:                {} ({} ns avg)", self.ranges, self.range_avg_ns())?;
        writeln!(f, "prefixes:              {} ({} ns avg)", self.prefixes, self.prefix_avg_ns())?;
        writeln!(f, "transactions committed: {} ({} ns avg)", self.transactions_committed, self.transaction_commit_avg_ns())?;
        writeln!(f, "transactions rolled back: {}", self.transactions_rolled_back)?;
        writeln!(f, "compactions:           {}", self.compactions)?;
        writeln!(f, "segment flushes:       {}", self.segment_flushes)?;
        write!(  f, "wal rotations:         {}", self.wal_rotations)
    }
}
