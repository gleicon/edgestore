//! InMemorySegmentReader — parses a `.dat` segment from a memory buffer.
//!
//! Used by `ImmutableEngine` (Phase 9) to serve reads in environments with no
//! local filesystem. Builds sparse index and xor filter by scanning blocks once at
//! construction time.

use crate::error::EdgestoreError;
use crate::segment::{
    build_xor_filter, deserialize_entry, filter_contains, find_block_offset, SEGMENT_BLOCK_MAGIC,
    SEGMENT_BLOCK_SIZE,
};
use crate::types::{MemEntry, SegmentId, SegmentMeta};

/// Read-only segment that lives entirely in memory.
///
/// Constructed from raw `.dat` bytes (e.g. downloaded from S3). Parses all
/// blocks at construction to build the sparse index and xor filter, so point
/// lookups and range scans are fast afterwards.
#[derive(Clone)]
pub struct InMemorySegmentReader {
    /// Segment identifier (matches the original on-disk segment ID).
    pub segment_id: SegmentId,
    /// BLAKE3 content hash of the `.dat` bytes.
    pub segment_hash: [u8; 32],
    /// Raw .dat bytes including file header + blocks.
    data: std::sync::Arc<Vec<u8>>,
    /// Sparse index: first key of each block → block offset.
    index: Vec<(Vec<u8>, u64)>,
    /// Xor filter for fast negative checks.
    filter: xorf::Xor8,
    /// Metadata (bounds, LSN range, etc.).
    pub meta: SegmentMeta,
}

impl InMemorySegmentReader {
    /// Parse a `.dat` file from a memory buffer.
    ///
    /// Scans all blocks, decompresses each one, collects keys for the xor filter,
    /// and builds a sparse index (first key per block).
    pub fn from_bytes(
        segment_id: SegmentId,
        segment_hash: [u8; 32],
        meta: SegmentMeta,
        bytes: &[u8],
    ) -> Result<Self, EdgestoreError> {
        if bytes.len() < 8 {
            return Err(EdgestoreError::SegmentCorrupt(
                "in-memory segment too short".to_string(),
            ));
        }

        let mut all_keys: Vec<Vec<u8>> = Vec::new();
        let mut index: Vec<(Vec<u8>, u64)> = Vec::new();

        let mut offset = 8usize; // skip file header
        while offset < bytes.len() {
            if offset + 8 > bytes.len() {
                break;
            }
            let magic = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            if magic != SEGMENT_BLOCK_MAGIC {
                break; // hit padding or end
            }
            let compressed_len =
                u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
            if offset + 8 + compressed_len > bytes.len() {
                break;
            }

            let compressed = &bytes[offset + 8..offset + 8 + compressed_len];
            let decompressed = zstd::decode_all(compressed).map_err(|e| {
                EdgestoreError::SegmentCorrupt(format!("in-memory zstd decode: {}", e))
            })?;

            // Parse entries from this block.
            let mut pos = 0usize;
            let mut first_key_in_block: Option<Vec<u8>> = None;
            while pos < decompressed.len() {
                match deserialize_entry(&decompressed, &mut pos) {
                    Ok((key, _entry)) => {
                        all_keys.push(key.clone());
                        if first_key_in_block.is_none() {
                            first_key_in_block = Some(key);
                        }
                    }
                    Err(_) => break,
                }
            }

            if let Some(key) = first_key_in_block {
                index.push((key, offset as u64));
            }

            let payload_size = 8 + compressed_len;
            let aligned_size = if payload_size.is_multiple_of(SEGMENT_BLOCK_SIZE) {
                payload_size
            } else {
                (payload_size / SEGMENT_BLOCK_SIZE + 1) * SEGMENT_BLOCK_SIZE
            };
            offset += aligned_size;
        }

        let filter = build_xor_filter(&all_keys)?;

        Ok(InMemorySegmentReader {
            segment_id,
            segment_hash,
            data: std::sync::Arc::new(bytes.to_vec()),
            index,
            filter,
            meta,
        })
    }

    /// Look up a single key in this segment.
    pub fn get(&self, key: &[u8]) -> Result<Option<MemEntry>, EdgestoreError> {
        if !filter_contains(&self.filter, key) {
            return Ok(None);
        }
        let start_offset = find_block_offset(&self.index, key) as usize;
        let mut current_offset = start_offset;
        let bytes = self.data.as_slice();
        let mut best: Option<MemEntry> = None;

        loop {
            if current_offset + 8 > bytes.len() {
                break;
            }
            let magic = u32::from_le_bytes(
                bytes[current_offset..current_offset + 4]
                    .try_into()
                    .unwrap(),
            );
            if magic != SEGMENT_BLOCK_MAGIC {
                break;
            }
            let compressed_len = u32::from_le_bytes(
                bytes[current_offset + 4..current_offset + 8]
                    .try_into()
                    .unwrap(),
            ) as usize;
            if current_offset + 8 + compressed_len > bytes.len() {
                break;
            }

            let compressed = &bytes[current_offset + 8..current_offset + 8 + compressed_len];
            let decompressed = zstd::decode_all(compressed).map_err(|e| {
                EdgestoreError::SegmentCorrupt(format!("in-memory get zstd: {}", e))
            })?;

            let mut pos = 0usize;
            let mut block_has_key = false;
            while pos < decompressed.len() {
                match deserialize_entry(&decompressed, &mut pos) {
                    Ok((k, entry)) => {
                        if k.as_slice() == key {
                            block_has_key = true;
                            let is_better = best.as_ref().is_none_or(|b| entry.lsn > b.lsn);
                            if is_better {
                                best = Some(entry);
                            }
                        }
                        if k.as_slice() > key {
                            // Passed the key range within this block.
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            // If this block had the key, the next block might also have it (duplicate keys
            // across block boundaries are possible). Continue scanning.
            // If this block's keys were all < key, continue to next block.
            // If this block's first key > key, we can stop (blocks are sorted).
            let block_first_key = self
                .index
                .iter()
                .find(|(_, off)| *off as usize == current_offset)
                .map(|(k, _)| k.clone());
            if let Some(first) = block_first_key {
                if first.as_slice() > key && !block_has_key {
                    // This block starts after our key; no later block can contain it.
                    break;
                }
            }

            let payload_size = 8 + compressed_len;
            let aligned_size = if payload_size.is_multiple_of(SEGMENT_BLOCK_SIZE) {
                payload_size
            } else {
                (payload_size / SEGMENT_BLOCK_SIZE + 1) * SEGMENT_BLOCK_SIZE
            };
            current_offset += aligned_size;
        }
        Ok(best)
    }

    /// Return all entries in the segment whose key falls in `[start, end]`.
    pub fn range_scan(
        &self,
        start: &[u8],
        end: &[u8],
    ) -> Result<Vec<(Vec<u8>, MemEntry)>, EdgestoreError> {
        if end < self.meta.min_key.as_slice() || start > self.meta.max_key.as_slice() {
            return Ok(vec![]);
        }
        let start_offset = find_block_offset(&self.index, start) as usize;
        let mut current_offset = start_offset;
        let bytes = self.data.as_slice();
        let mut results = Vec::new();

        loop {
            if current_offset + 8 > bytes.len() {
                break;
            }
            let magic = u32::from_le_bytes(
                bytes[current_offset..current_offset + 4]
                    .try_into()
                    .unwrap(),
            );
            if magic != SEGMENT_BLOCK_MAGIC {
                break;
            }
            let compressed_len = u32::from_le_bytes(
                bytes[current_offset + 4..current_offset + 8]
                    .try_into()
                    .unwrap(),
            ) as usize;
            if current_offset + 8 + compressed_len > bytes.len() {
                break;
            }

            let compressed = &bytes[current_offset + 8..current_offset + 8 + compressed_len];
            let decompressed = zstd::decode_all(compressed).map_err(|e| {
                EdgestoreError::SegmentCorrupt(format!("in-memory range zstd: {}", e))
            })?;

            let mut pos = 0usize;
            let mut past_end = false;
            while pos < decompressed.len() {
                match deserialize_entry(&decompressed, &mut pos) {
                    Ok((k, entry)) => {
                        if k.as_slice() >= end {
                            past_end = true;
                            break;
                        }
                        if k.as_slice() >= start {
                            results.push((k, entry));
                        }
                    }
                    Err(_) => break,
                }
            }
            if past_end {
                break;
            }

            let payload_size = 8 + compressed_len;
            let aligned_size = if payload_size.is_multiple_of(SEGMENT_BLOCK_SIZE) {
                payload_size
            } else {
                (payload_size / SEGMENT_BLOCK_SIZE + 1) * SEGMENT_BLOCK_SIZE
            };
            current_offset += aligned_size;
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::SegmentWriter;
    use crate::types::{encode_key, MemEntry, Operation};
    use tempfile::TempDir;

    fn make_put_entry(key: &[u8], value: &[u8], lsn: u64) -> MemEntry {
        MemEntry {
            key: key.to_vec(),
            value: Some(value.to_vec()),
            op: Operation::Put,
            lsn,
            timestamp: 3_600_000_000_000,
            ttl: 0,
        }
    }

    #[test]
    fn test_in_memory_roundtrip() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";
        let user_key = b"key1";
        let encoded_key = encode_key(ns, user_key);
        let value = b"hello-world";

        let entry = make_put_entry(&encoded_key, value, 1);
        let mut entries = vec![(encoded_key.clone(), entry)];
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let meta = writer.flush(&entries).unwrap();

        let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
        let reader = InMemorySegmentReader::from_bytes(
            0,
            meta.segment_hash.as_slice().try_into().unwrap(),
            meta,
            &dat_bytes,
        )
        .unwrap();

        let result = reader.get(&encode_key(ns, user_key)).unwrap();
        assert_eq!(result.map(|e| e.value), Some(Some(value.to_vec())));
    }

    #[test]
    fn test_in_memory_absent_key_fast_reject() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";
        let encoded_key = encode_key(ns, b"only-key");
        let entry = make_put_entry(&encoded_key, b"v", 1);
        let mut entries = vec![(encoded_key, entry)];
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let meta = writer.flush(&entries).unwrap();

        let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
        let reader = InMemorySegmentReader::from_bytes(
            0,
            meta.segment_hash.as_slice().try_into().unwrap(),
            meta,
            &dat_bytes,
        )
        .unwrap();

        let result = reader.get(&encode_key(ns, b"not-present")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_in_memory_range_scan_sorted() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";

        let mut entries: Vec<(Vec<u8>, MemEntry)> = (0..10u64)
            .map(|i| {
                let enc = encode_key(ns, format!("key-{:04}", i).as_bytes());
                let e = make_put_entry(&enc, format!("val-{}", i).as_bytes(), i + 1);
                (enc, e)
            })
            .collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let meta = writer.flush(&entries).unwrap();

        let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
        let reader = InMemorySegmentReader::from_bytes(
            0,
            meta.segment_hash.as_slice().try_into().unwrap(),
            meta,
            &dat_bytes,
        )
        .unwrap();

        let results = reader
            .range_scan(&encode_key(ns, b"key-0002"), &encode_key(ns, b"key-0007"))
            .unwrap();
        assert_eq!(results.len(), 5, "range should return 5 entries");
        let raw_keys: Vec<&[u8]> = results.iter().map(|(k, _)| k.as_slice()).collect();
        let mut sorted = raw_keys.clone();
        sorted.sort();
        assert_eq!(raw_keys, sorted, "range results must be sorted");
    }

    #[test]
    fn test_in_memory_large_segment_1000_keys() {
        let dir = TempDir::new().unwrap();
        let ns = b"ns";

        let mut entries: Vec<(Vec<u8>, MemEntry)> = (0..1000u64)
            .map(|i| {
                let enc = encode_key(ns, &i.to_be_bytes());
                let e = make_put_entry(&enc, b"value", i + 1);
                (enc, e)
            })
            .collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut writer = SegmentWriter::new(dir.path().to_path_buf(), 0, 3600);
        let meta = writer.flush(&entries).unwrap();

        let dat_bytes = std::fs::read(dir.path().join("segment-00000000.dat")).unwrap();
        let reader = InMemorySegmentReader::from_bytes(
            0,
            meta.segment_hash.as_slice().try_into().unwrap(),
            meta,
            &dat_bytes,
        )
        .unwrap();

        // Spot-check lookups.
        for i in [0u64, 500, 999] {
            let result = reader.get(&encode_key(ns, &i.to_be_bytes())).unwrap();
            assert!(result.is_some(), "key {} must be found", i);
        }

        // Range scan over all keys.
        let results = reader
            .range_scan(
                &encode_key(ns, &0u64.to_be_bytes()),
                &encode_key(ns, &1000u64.to_be_bytes()),
            )
            .unwrap();
        assert_eq!(results.len(), 1000, "range must include all 1000 keys");
    }
}
