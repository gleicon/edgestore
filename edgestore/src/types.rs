use crate::error::EdgestoreError;
use byteorder::{BigEndian, WriteBytesExt};
use serde::{Deserialize, Serialize};

/// Log sequence number (monotonically increasing per database).
pub type Lsn = u64;
/// Unique identifier for a segment file.
pub type SegmentId = u64;

/// KV operation type stored in WAL and memtable.
#[repr(u8)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Insert or update a key-value pair.
    Put = 1,
    /// Delete a key (tombstone).
    Delete = 2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// In-memory representation of a single record (key + metadata).
pub struct MemEntry {
    /// Namespace-prefixed key bytes.
    pub key: Vec<u8>,
    /// Value bytes (None for delete tombstones).
    pub value: Option<Vec<u8>>,
    /// Operation type (Put or Delete).
    pub op: Operation,
    /// Log sequence number (monotonically increasing).
    pub lsn: Lsn,
    /// Wall-clock timestamp in nanoseconds.
    pub timestamp: i64,
    /// TTL in seconds (0 = no TTL).
    pub ttl: u32,
}

#[derive(Debug, Clone)]
/// Single record in the Write-Ahead Log.
pub struct WalRecord {
    /// Transaction ID (0 for non-transactional operations).
    pub txid: u64,
    /// Log sequence number.
    pub lsn: Lsn,
    /// Wall-clock timestamp in nanoseconds.
    pub timestamp: i64,
    /// TTL in seconds (0 = no TTL).
    pub ttl: u32,
    /// Namespace length (redundant with ns_bytes, kept for forward compatibility).
    pub ns_len: u16,
    /// Namespace bytes.
    pub ns_bytes: Vec<u8>,
    /// Raw key bytes (without namespace prefix).
    pub key_bytes: Vec<u8>,
    /// Operation type.
    pub op: Operation,
    /// BLAKE3 hash of value_bytes (32 bytes).
    pub value_hash: [u8; 32],
    /// Value bytes (empty for deletes).
    pub value_bytes: Vec<u8>,
}

/// Compression algorithm for WAL or segments.
#[derive(Debug, Clone)]
pub enum Compression {
    /// LZ4 compression (fast, low ratio).
    Lz4,
    /// Zstd compression with level (e.g. 1 = fastest).
    Zstd(u32),
}

/// Segment metadata stored in .meta JSON file and in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMeta {
    /// Segment ID (monotonically increasing).
    pub segment_id: SegmentId,
    /// BLAKE3 hash of the .dat file (32 bytes, stored as Vec for serde compat).
    pub segment_hash: Vec<u8>,
    /// Smallest encoded key in this segment.
    pub min_key: Vec<u8>,
    /// Largest encoded key in this segment.
    pub max_key: Vec<u8>,
    /// Minimum LSN in this segment.
    pub min_lsn: Lsn,
    /// Maximum LSN in this segment.
    pub max_lsn: Lsn,
    /// Number of records (including tombstones).
    pub record_count: u64,
    /// Total compressed bytes on disk.
    pub compressed_bytes: u64,
    /// Total uncompressed bytes before compression.
    pub uncompressed_bytes: u64,
    /// Compression algorithm and level, e.g. "zstd:1".
    pub compression: String,
    /// Compaction cohort bucket (unix seconds / cohort_window_secs).
    pub cohort_bucket: i64,
    /// Maximum death time across all records (unix nanoseconds).
    pub death_time: i64,
    /// BLAKE3 hash of all key hashes (32 bytes, stored as Vec for serde compat).
    pub merkle_root: Vec<u8>,
    /// Creation timestamp (unix nanoseconds).
    pub created_at: i64,
    /// When `true`, all `__text__*` namespace records have been removed from this segment.
    /// Search queries should not expect text index data from this segment.
    /// Set by `Engine::strip_text_index` after a segment has been archived to cold storage.
    #[serde(default)]
    pub text_index_stripped: bool,
}

/// Cohort bucket for a record: which compaction cohort it belongs to.
/// TTL>0: floor((write_time_secs + ttl) / cohort_window_secs)
/// TTL=0: floor(write_time_secs / cohort_window_secs)
pub(crate) fn cohort_bucket_for(write_time_nanos: i64, ttl: u32, cohort_window_secs: u64) -> i64 {
    let write_time_secs = write_time_nanos / 1_000_000_000;
    let cw = cohort_window_secs as i64;
    if ttl > 0 {
        (write_time_secs + ttl as i64) / cw
    } else {
        write_time_secs / cw
    }
}

/// Death time for a record in nanoseconds.
/// TTL>0: write_time_nanos + ttl_nanos
/// TTL=0: write_time_nanos + cohort_window_nanos (temporal locality fallback)
pub(crate) fn death_time_for(write_time_nanos: i64, ttl: u32, cohort_window_secs: u64) -> i64 {
    if ttl > 0 {
        write_time_nanos.saturating_add((ttl as i64).saturating_mul(1_000_000_000))
    } else {
        write_time_nanos.saturating_add((cohort_window_secs as i64).saturating_mul(1_000_000_000))
    }
}

/// Encode a namespace and key into the internal `{ns_len:u16-be}{ns}{key}` format.
pub fn encode_key(ns: &[u8], key: &[u8]) -> Vec<u8> {
    assert!(ns.len() <= u16::MAX as usize, "namespace too long");
    let mut buf = Vec::with_capacity(2 + ns.len() + key.len());
    buf.write_u16::<BigEndian>(ns.len() as u16).unwrap();
    buf.extend_from_slice(ns);
    buf.extend_from_slice(key);
    buf
}

/// Compute the exclusive upper bound for a prefix range scan.
///
/// Scans right-to-left for the first byte below `0xFF`, increments it, and
/// truncates the slice. Returns `None` when every byte is `0xFF` (no finite
/// upper bound exists in the byte-string ordering).
pub fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    for i in (0..end.len()).rev() {
        if end[i] < 0xFF {
            end[i] += 1;
            end.truncate(i + 1);
            return Some(end);
        }
    }
    None
}

pub(crate) fn decode_key(encoded: &[u8]) -> Result<(Vec<u8>, Vec<u8>), EdgestoreError> {
    if encoded.len() < 2 {
        return Err(EdgestoreError::CorruptKey);
    }
    let ns_len = u16::from_be_bytes([encoded[0], encoded[1]]) as usize;
    if encoded.len() < 2 + ns_len {
        return Err(EdgestoreError::CorruptKey);
    }
    let ns_bytes = encoded[2..2 + ns_len].to_vec();
    let key_bytes = encoded[2 + ns_len..].to_vec();
    Ok((ns_bytes, key_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_key_format() {
        let encoded = encode_key(b"users", b"alice");
        // ns_len = 5 as u16 BE = [0x00, 0x05]
        assert_eq!(&encoded[..2], &[0x00, 0x05]);
        assert_eq!(&encoded[2..7], b"users");
        assert_eq!(&encoded[7..], b"alice");
    }

    #[test]
    fn test_decode_key_round_trip() {
        let encoded = encode_key(b"ns", b"key");
        let (ns, key) = decode_key(&encoded).unwrap();
        assert_eq!(ns, b"ns");
        assert_eq!(key, b"key");
    }

    #[test]
    fn test_decode_key_corrupt() {
        let result = decode_key(&[0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_cohort_bucket_no_ttl() {
        // 1 hour in nanos / 1 hour window = bucket 1
        assert_eq!(cohort_bucket_for(3_600_000_000_000, 0, 3600), 1);
    }

    #[test]
    fn test_cohort_bucket_with_ttl() {
        // write=0, ttl=7200s (2h), death=2h, 2h/1h window = bucket 2
        assert_eq!(cohort_bucket_for(0, 7200, 3600), 2);
    }

    #[test]
    fn test_death_time_with_ttl() {
        // write=0, ttl=3600s → death = 3600 * 1e9 nanos
        assert_eq!(death_time_for(0, 3600, 3600), 3_600_000_000_000);
    }

    #[test]
    fn test_death_time_no_ttl() {
        // write=0, no TTL → death = cohort_window = 3600s in nanos
        assert_eq!(death_time_for(0, 0, 3600), 3_600_000_000_000);
    }
}
