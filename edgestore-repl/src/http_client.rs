//! HTTP replication client implementing the `ReplicationProtocol` trait.
//!
//! Uses MessagePack (rmp-serde) for control messages (D07).
//! Raw bytes for segment data transfer.
//!
//! All network errors are mapped to `EdgestoreError::ReplicationError`.

use std::io::Read;

use serde::{Deserialize, Serialize};

use edgestore::replication::{ReplicationProtocol, SegmentRef};
use edgestore::{EdgestoreError, WatermarkResponse};

/// MessagePack wire struct for GET /merkle response.
#[derive(Serialize, Deserialize)]
struct MerkleResponse {
    root: Vec<u8>,
}

/// MessagePack wire struct for one item in GET /segments response.
#[derive(Serialize, Deserialize)]
struct SegmentEntry {
    segment_id: u64,
    segment_hash: Vec<u8>,
}

/// HTTP replication client that implements `ReplicationProtocol` against an `HttpReplicationServer`.
///
/// Uses MessagePack (rmp-serde) for control messages and raw bytes for segment transfer.
pub struct HttpReplicationClient {
    base_url: String,
}

impl HttpReplicationClient {
    /// Create a new client pointing at `base_url` (e.g. `"http://127.0.0.1:8900"`).
    pub fn new(base_url: impl Into<String>) -> Self {
        HttpReplicationClient {
            base_url: base_url.into(),
        }
    }
}

impl ReplicationProtocol for HttpReplicationClient {
    /// Fetch the remote peer's current Merkle root.
    ///
    /// Calls `GET {base_url}/merkle`, deserializes MessagePack body as `{root: Vec<u8>}`,
    /// and converts to `[u8; 32]`.
    fn merkle_root(&self) -> Result<[u8; 32], EdgestoreError> {
        let url = format!("{}/merkle", self.base_url);
        let response = ureq::get(&url)
            .call()
            .map_err(|e| EdgestoreError::ReplicationError(format!("GET /merkle: {}", e)))?;

        let resp: MerkleResponse = rmp_serde::from_read(response.into_reader())
            .map_err(|e| EdgestoreError::ReplicationError(format!("GET /merkle decode: {}", e)))?;

        if resp.root.len() != 32 {
            return Err(EdgestoreError::ReplicationError(format!(
                "GET /merkle: expected 32-byte root, got {} bytes",
                resp.root.len()
            )));
        }

        let mut hash = [0u8; 32];
        hash.copy_from_slice(&resp.root);
        Ok(hash)
    }

    /// Fetch the remote peer's full segment manifest.
    ///
    /// Calls `GET {base_url}/segments`, deserializes MessagePack as a list of segment entries,
    /// and converts each to `SegmentRef`.
    fn list_segments(&self) -> Result<Vec<SegmentRef>, EdgestoreError> {
        let url = format!("{}/segments", self.base_url);
        let response = ureq::get(&url)
            .call()
            .map_err(|e| EdgestoreError::ReplicationError(format!("GET /segments: {}", e)))?;

        let entries: Vec<SegmentEntry> =
            rmp_serde::from_read(response.into_reader()).map_err(|e| {
                EdgestoreError::ReplicationError(format!("GET /segments decode: {}", e))
            })?;

        let refs = entries
            .into_iter()
            .map(|e| {
                let mut hash = [0u8; 32];
                let copy_len = e.segment_hash.len().min(32);
                hash[..copy_len].copy_from_slice(&e.segment_hash[..copy_len]);
                SegmentRef {
                    segment_hash: hash,
                    segment_id: e.segment_id,
                }
            })
            .collect();

        Ok(refs)
    }

    /// Fetch the primary's commit watermark.
    ///
    /// Calls `GET {base_url}/watermark`, deserializes MessagePack as
    /// `{confirmed_lsn, wtoken}`.  Used by `AntiEntropyLoop` to detect
    /// primary failovers when `wtoken` increases.
    fn watermark(&self) -> Result<WatermarkResponse, EdgestoreError> {
        let url = format!("{}/watermark", self.base_url);
        let response = ureq::get(&url)
            .call()
            .map_err(|e| EdgestoreError::ReplicationError(format!("GET /watermark: {e}")))?;
        rmp_serde::from_read(response.into_reader())
            .map_err(|e| EdgestoreError::ReplicationError(format!("GET /watermark decode: {e}")))
    }

    /// Fetch one segment's raw bytes by content hash.
    ///
    /// Calls `GET {base_url}/segments/{hash_hex}`, reads raw bytes from body.
    /// Caller MUST verify BLAKE3 before applying (T-04-01).
    fn fetch_segment(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
        let hash_hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();
        let url = format!("{}/segments/{}", self.base_url, hash_hex);

        let response = ureq::get(&url).call().map_err(|e| {
            EdgestoreError::ReplicationError(format!("GET /segments/{}: {}", hash_hex, e))
        })?;

        let mut data = Vec::new();
        response.into_reader().read_to_end(&mut data).map_err(|e| {
            EdgestoreError::ReplicationError(format!("GET /segments/{} read body: {}", hash_hex, e))
        })?;

        Ok(data)
    }
}
