//! Pull-only replication protocol.
//!
//! Merkle root = anti-entropy probe.
//! Manifest diff = sync routing.
//! Segment fetch = transfer unit.
//! No push in Phase 4 (D02, D08).

use crate::error::EdgestoreError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Opaque identifier for a replication host.
///
/// Advisory only in Phase 4 — no authentication is performed (T-04-02).
/// Used as a tiebreaker in LWW conflict resolution (D06).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostId(pub String);

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for HostId {
    fn from(s: String) -> Self {
        HostId(s)
    }
}

impl From<&str> for HostId {
    fn from(s: &str) -> Self {
        HostId(s.to_string())
    }
}

/// A reference to a segment on a remote peer, identified by content hash and local segment ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRef {
    /// BLAKE3 content hash of the segment (32 bytes).
    pub segment_hash: [u8; 32],
    /// Peer's local segment ID for correlation.
    pub segment_id: u64,
}

impl SegmentRef {
    /// Returns the segment hash as a lowercase hex string.
    ///
    /// Does not require an external hex crate.
    #[allow(dead_code)] // used by Plan 04-03 HTTP server
    pub fn hash_hex(&self) -> String {
        self.segment_hash
            .iter()
            .map(|x| format!("{:02x}", x))
            .collect::<String>()
    }
}

/// Pull-only replication protocol trait (D02, D08).
///
/// Two nodes compare Merkle roots first (`merkle_root`); if equal, sync is skipped entirely.
/// If roots differ, the caller fetches the peer's full segment manifest (`list_segments`) and
/// computes the set difference locally. Missing segments are fetched one at a time
/// (`fetch_segment`). Caller MUST verify `BLAKE3(data) == hash` before applying (T-04-01).
///
/// The trait is object-safe: no generic parameters, no associated types.
pub trait ReplicationProtocol {
    /// Returns the peer's current Merkle root for anti-entropy probe (D02).
    ///
    /// Caller compares to local root; if equal, sync is skipped.
    fn merkle_root(&self) -> Result<[u8; 32], EdgestoreError>;

    /// Returns the peer's full segment manifest as `Vec<SegmentRef>` (D02).
    ///
    /// Called only when roots differ. Caller computes set diff locally.
    fn list_segments(&self) -> Result<Vec<SegmentRef>, EdgestoreError>;

    /// Downloads one segment by content hash (D01).
    ///
    /// Caller MUST verify `BLAKE3(data) == hash` before applying (T-04-01).
    fn fetch_segment(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError>;
}
