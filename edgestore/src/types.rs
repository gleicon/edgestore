use byteorder::{BigEndian, WriteBytesExt};
use crate::error::EdgestoreError;
use serde::{Deserialize, Serialize};

pub type Lsn = u64;
pub type SegmentId = u64;

#[repr(u8)]
#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    Put = 1,
    Delete = 2,
}

#[derive(Debug, Clone)]
pub struct MemEntry {
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
    pub op: Operation,
    pub lsn: Lsn,
    pub timestamp: i64,
    pub ttl: u32,
}

#[derive(Debug, Clone)]
pub struct WalRecord {
    pub txid: u64,
    pub lsn: Lsn,
    pub timestamp: i64,
    pub ttl: u32,
    pub ns_len: u16,
    pub ns_bytes: Vec<u8>,
    pub key_bytes: Vec<u8>,
    pub op: Operation,
    pub value_hash: [u8; 32], // BLAKE3 of value_bytes — permanent format field
    pub value_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Compression {
    Lz4,
    Zstd(u32),
}

/// Segment metadata stored in .meta JSON file and in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentMeta {
    pub segment_id: SegmentId,
    pub segment_hash: Vec<u8>,   // BLAKE3 of .dat file (32 bytes, stored as Vec for serde compat)
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
    pub min_lsn: Lsn,
    pub max_lsn: Lsn,
    pub record_count: u64,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub compression: String,     // e.g. "zstd:1"
    pub cohort_bucket: i64,      // unix seconds truncated to cohort window
    pub death_time: i64,         // max death_time across records (unix nanoseconds)
    pub merkle_root: Vec<u8>,    // BLAKE3 (32 bytes, stored as Vec for serde compat)
    pub created_at: i64,         // unix nanoseconds
}

/// Cohort bucket for a record: which compaction cohort it belongs to.
/// TTL>0: floor((write_time_secs + ttl) / cohort_window_secs)
/// TTL=0: floor(write_time_secs / cohort_window_secs)
pub fn cohort_bucket_for(write_time_nanos: i64, ttl: u32, cohort_window_secs: u64) -> i64 {
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
pub fn death_time_for(write_time_nanos: i64, ttl: u32, cohort_window_secs: u64) -> i64 {
    if ttl > 0 {
        write_time_nanos.saturating_add((ttl as i64).saturating_mul(1_000_000_000))
    } else {
        write_time_nanos.saturating_add((cohort_window_secs as i64).saturating_mul(1_000_000_000))
    }
}

pub fn encode_key(ns: &[u8], key: &[u8]) -> Vec<u8> {
    assert!(ns.len() <= u16::MAX as usize, "namespace too long");
    let mut buf = Vec::with_capacity(2 + ns.len() + key.len());
    buf.write_u16::<BigEndian>(ns.len() as u16).unwrap();
    buf.extend_from_slice(ns);
    buf.extend_from_slice(key);
    buf
}

pub fn decode_key(encoded: &[u8]) -> Result<(Vec<u8>, Vec<u8>), EdgestoreError> {
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
