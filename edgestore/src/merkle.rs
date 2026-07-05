//! Range-level Merkle tree over 16 key-range buckets.
//!
//! Used as an anti-entropy probe only (D02). Two nodes compare roots; if equal, sync is
//! skipped. The tree does not route which segments to sync — manifest-diff handles that.
//!
//! Bucket assignment: `bucket_idx = segment.min_key[0] >> 4` (leading nibble, 0..16).

use crate::types::SegmentMeta;

/// A 16-bucket Merkle tree built over segment key ranges.
///
/// Used as an anti-entropy probe only (D02). Two nodes compare roots; if equal, sync is
/// skipped. The tree does not route which segments to sync — manifest-diff handles that.
pub struct RangeMerkleTree {
    /// One 32-byte BLAKE3 hash per bucket (0..16). Empty bucket = all-zeros.
    pub buckets: [[u8; 32]; 16],
}

impl RangeMerkleTree {
    /// Returns an empty tree with all-zero bucket hashes.
    pub fn empty() -> Self {
        RangeMerkleTree {
            buckets: [[0u8; 32]; 16],
        }
    }

    /// Build a RangeMerkleTree from a slice of SegmentMeta references.
    ///
    /// Each segment is assigned to a bucket by `segment.min_key[0] >> 4` (leading nibble).
    /// Within each bucket, segments are sorted by `segment_id` ascending before hashing.
    /// Empty buckets receive an all-zero hash.
    pub fn build(segments: &[&SegmentMeta]) -> Self {
        // Group segment merkle roots by bucket index
        let mut groups: [Vec<(u64, Vec<u8>)>; 16] = Default::default();

        for seg in segments {
            let bucket_idx = seg.min_key.first().copied().unwrap_or(0) >> 4;
            groups[bucket_idx as usize].push((seg.segment_id, seg.merkle_root.clone()));
        }

        let mut buckets = [[0u8; 32]; 16];
        for (i, group) in groups.iter_mut().enumerate() {
            if group.is_empty() {
                // Empty bucket stays all-zero
                continue;
            }
            // Sort by segment_id ascending
            group.sort_by_key(|(id, _)| *id);

            // Hash all merkle_root bytes concatenated
            let mut hasher = blake3::Hasher::new();
            for (_, root_bytes) in group.iter() {
                hasher.update(root_bytes);
            }
            let hash = hasher.finalize();
            buckets[i].copy_from_slice(hash.as_bytes());
        }

        RangeMerkleTree { buckets }
    }

    /// Returns the overall Merkle root: BLAKE3 of all 16 bucket hashes concatenated (512 bytes).
    pub fn root(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        for bucket in &self.buckets {
            hasher.update(bucket);
        }
        let hash = hasher.finalize();
        let mut result = [0u8; 32];
        result.copy_from_slice(hash.as_bytes());
        result
    }

    /// Returns the bucket indices (0..15) where `self` and `other` have differing hashes.
    pub fn differing_buckets(&self, other: &RangeMerkleTree) -> Vec<u8> {
        let mut diffs = Vec::new();
        for i in 0..16u8 {
            if self.buckets[i as usize] != other.buckets[i as usize] {
                diffs.push(i);
            }
        }
        diffs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_segment(segment_id: u64, min_key_first_byte: u8, merkle_root: [u8; 32]) -> SegmentMeta {
        SegmentMeta {
            segment_id,
            segment_hash: vec![0u8; 32],
            min_key: vec![min_key_first_byte, 0, 0],
            max_key: vec![min_key_first_byte, 0xff, 0xff],
            min_lsn: 0,
            max_lsn: 1,
            record_count: 1,
            compressed_bytes: 1024,
            uncompressed_bytes: 2048,
            compression: "zstd:1".to_string(),
            cohort_bucket: 0,
            death_time: 0,
            merkle_root: merkle_root.to_vec(),
            created_at: 0,
            text_index_stripped: false,
        }
    }

    #[test]
    fn test_empty_tree_roots_equal() {
        let a = RangeMerkleTree::empty();
        let b = RangeMerkleTree::empty();
        assert_eq!(a.root(), b.root(), "two empty trees must have equal roots");
    }

    #[test]
    fn test_build_single_segment() {
        // A segment with min_key[0] = 0x30 goes into bucket 3 (0x30 >> 4 == 3).
        let merkle_root = [0xABu8; 32];
        let seg = make_segment(1, 0x30, merkle_root);
        let tree = RangeMerkleTree::build(&[&seg]);

        // Bucket 3 should be BLAKE3 of the single merkle_root bytes
        let expected_bucket: [u8; 32] = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&merkle_root);
            let h = hasher.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(h.as_bytes());
            out
        };
        assert_eq!(tree.buckets[3], expected_bucket);

        // All other buckets should be zero
        for (i, b) in tree.buckets.iter().enumerate() {
            if i != 3 {
                assert_eq!(b, &[0u8; 32], "bucket {} should be zero", i);
            }
        }
    }

    #[test]
    fn test_differing_buckets() {
        // Tree A has a segment in bucket 3 (min_key[0] = 0x30)
        let seg_a = make_segment(1, 0x30, [0xAAu8; 32]);
        let tree_a = RangeMerkleTree::build(&[&seg_a]);

        // Tree B has a different segment in bucket 3
        let seg_b = make_segment(1, 0x30, [0xBBu8; 32]);
        let tree_b = RangeMerkleTree::build(&[&seg_b]);

        let diffs = tree_a.differing_buckets(&tree_b);
        assert_eq!(diffs, vec![3u8], "only bucket 3 should differ");
    }
}
