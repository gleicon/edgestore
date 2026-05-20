use std::path::PathBuf;
use crate::types::SegmentId;

/// Information about a single deathtime cohort to be compacted.
#[derive(Debug, Default)]
pub struct CohortInfo {
    /// Unix seconds truncated to cohort window (identifies this cohort).
    pub cohort_bucket: i64,
    /// Segment IDs whose death_time falls in this cohort.
    pub segment_ids: Vec<SegmentId>,
    /// Maximum death_time (nanoseconds) across all records in this cohort.
    pub max_death_time_nanos: i64,
    /// Total records across all segments in this cohort.
    pub total_records: u64,
    /// Estimated number of dead/expired records in this cohort.
    pub dead_record_estimate: u64,
}

/// Aggregated statistics for a completed compaction run.
#[derive(Debug, Default)]
pub struct CompactionStats {
    /// Number of cohorts whose segments were collected and rewritten.
    pub cohorts_collected: u64,
    /// Number of old segments removed from the manifest.
    pub segments_removed: u64,
    /// Number of new segments written as compaction output.
    pub segments_written: u64,
    /// Total bytes written to new segments.
    pub bytes_written: u64,
    /// Number of live records relocated into the new segments.
    pub live_records_relocated: u64,
}

/// Drives deathtime-cohort compaction for an EdgeStore database.
///
/// The compactor groups segments by their cohort bucket, waits until all
/// records in a cohort are past their death time, then rewrites only the
/// live records into new segments (removing dead ones).  No in-place writes
/// are performed; all output is append-oriented.
#[derive(Debug)]
pub struct Compactor {
    /// Base directory of the EdgeStore database being compacted.
    pub base_path: PathBuf,
    /// Maximum bytes the compactor may write per `compact_once` call.
    pub write_budget_bytes: u64,
    /// Cohort window width in seconds (matches `EdgestoreConfig::cohort_window_secs`).
    pub cohort_window_secs: u64,
}

impl Compactor {
    /// Create a new `Compactor`.
    ///
    /// `base_path`         — database directory.
    /// `write_budget_bytes`— write-amplification cap per compaction pass.
    /// `cohort_window_secs`— must match the value used when segments were written.
    pub fn new(
        base_path: PathBuf,
        write_budget_bytes: u64,
        cohort_window_secs: u64,
    ) -> Self {
        Compactor {
            base_path,
            write_budget_bytes,
            cohort_window_secs,
        }
    }
}
