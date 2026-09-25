//! Shared MessagePack wire types for the HTTP replication protocol.
//!
//! Both `HttpReplicationServer` and `HttpReplicationClient` use these structs.
//! Keeping them in one place ensures the server and client stay in sync when
//! the wire format changes.

use serde::{Deserialize, Serialize};

/// Wire struct for `GET /merkle` response.
#[derive(Serialize, Deserialize)]
pub(crate) struct MerkleResponse {
    pub root: Vec<u8>,
}

/// Wire struct for one entry in the `GET /segments` response.
#[derive(Serialize, Deserialize)]
pub(crate) struct SegmentEntry {
    pub segment_id: u64,
    pub segment_hash: Vec<u8>,
}

/// Encode a 32-byte BLAKE3 hash as a 64-character lowercase hex string.
pub(crate) fn hash_to_hex(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
