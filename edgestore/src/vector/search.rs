use crate::engine::{Engine, ScanBudget};
use crate::error::EdgestoreError;
use crate::vector::api::vector_namespace;
use crate::vector::distance::{distance, Metric};
use crate::vector::types::VectorRecord;

/// Result of a vector search: key and distance.
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    /// Opaque vector key.
    pub key: Vec<u8>,
    /// Distance to the query vector (lower = closer).
    pub distance: f32,
}

/// Heap item for top-k selection.
///
/// Ordered by distance descending so that BinaryHeap::peek() returns
/// the worst (largest distance) element among the current k. When we
/// find a better (smaller distance) candidate, we pop the worst and
/// push the new one.
#[derive(Debug, Clone)]
struct HeapItem {
    distance: f32,
    key: Vec<u8>,
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance
    }
}

impl Eq for HeapItem {}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Standard ordering: larger distance = Greater.
        // BinaryHeap (max-heap) peek() returns the maximum = worst item (largest distance).
        // When we find a new item with smaller distance, we pop the worst and push the new one.
        crate::vector::distance::total_cmp_f32(self.distance, other.distance)
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Brute-force flat scan over all vector records in a namespace.
///
/// Scans all keys in the synthetic namespace `__vec__{ns}`, computes
/// distance to the query vector for each record, and returns the top-k
/// closest results ordered by ascending distance.
pub fn vector_search(
    engine: &Engine,
    ns: &[u8],
    query: &VectorRecord,
    k: usize,
    metric: Metric,
) -> Result<Vec<VectorSearchResult>, EdgestoreError> {
    if k == 0 {
        return Ok(vec![]);
    }

    let vec_ns = vector_namespace(ns);

    // Scan all keys in the synthetic namespace using a wide range.
    // range(ns, start, end) uses [start, end) semantics.
    let all_keys = engine.range(&vec_ns, b"", &[0xFF; 1024])?;

    let mut heap = std::collections::BinaryHeap::with_capacity(k.min(16));

    for (key, val_bytes) in all_keys {
        // Decode the stored vector record
        let candidate = crate::vector::types::decode_vector_record(&val_bytes)
            .map_err(|e| EdgestoreError::CorruptData(format!("decode candidate: {}", e)))?;

        // Validate query and candidate have matching dims and dtype
        if candidate.dims != query.dims || candidate.dtype != query.dtype {
            continue; // Skip mismatched records rather than erroring
        }

        let dist = distance(&query.data, &candidate.data, query.dtype, metric)?;

        if heap.len() < k {
            heap.push(HeapItem {
                distance: dist,
                key: key.clone(),
            });
        } else if let Some(top) = heap.peek() {
            if dist < top.distance {
                heap.pop();
                heap.push(HeapItem {
                    distance: dist,
                    key: key.clone(),
                });
            }
        }
    }

    // Extract from heap and sort by ascending distance
    let mut results: Vec<VectorSearchResult> = heap
        .into_iter()
        .map(|item| VectorSearchResult {
            key: item.key,
            distance: item.distance,
        })
        .collect();
    results.sort_by(|a, b| crate::vector::distance::total_cmp_f32(a.distance, b.distance));

    Ok(results)
}

/// A page of decoded vector records returned by [`vector_page`].
///
/// Allows async callers to iterate through a namespace in bounded chunks so
/// that the engine lock is not held across distance computations.
#[derive(Debug)]
pub struct VectorPage {
    /// Decoded `(user_key, record)` pairs for this page.
    pub records: Vec<(Vec<u8>, VectorRecord)>,
    /// If `Some`, more records remain. Pass this value as `cursor` to the next
    /// [`vector_page`] call. `None` means the scan is complete.
    pub next_key: Option<Vec<u8>>,
}

/// Fetch one page of raw vector records from the KV layer.
///
/// `cursor` is the last key returned by the previous page (exclusive lower
/// bound for this page). Pass `None` to start from the beginning.
/// Records whose dims or dtype don't match the caller's query are still
/// returned — filtering happens at the call site so mismatches don't silently
/// consume budget.
pub fn vector_page(
    engine: &Engine,
    ns: &[u8],
    cursor: Option<&[u8]>,
    page_size: usize,
) -> Result<VectorPage, EdgestoreError> {
    if page_size == 0 {
        return Ok(VectorPage {
            records: vec![],
            next_key: None,
        });
    }

    let vec_ns = vector_namespace(ns);

    // Advance past cursor: append \x00 to make the start exclusive.
    let start_buf;
    let start: &[u8] = if let Some(c) = cursor {
        start_buf = {
            let mut v = c.to_vec();
            v.push(0);
            v
        };
        &start_buf
    } else {
        b""
    };

    let budget = ScanBudget {
        max_items: Some(page_size),
        max_bytes: None,
    };
    let scan = engine.range_budgeted(&vec_ns, start, &[0xFF; 32], &budget)?;

    let last_key = scan.items.last().map(|(k, _)| k.clone());
    let truncated = scan.truncated;

    let mut records = Vec::with_capacity(scan.items.len());
    for (key, val_bytes) in scan.items {
        let record = crate::vector::types::decode_vector_record(&val_bytes)
            .map_err(|e| EdgestoreError::CorruptData(format!("decode vector record: {}", e)))?;
        records.push((key, record));
    }

    Ok(VectorPage {
        records,
        next_key: if truncated { last_key } else { None },
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EdgestoreConfig, Engine, VectorEngine};
    use tempfile::TempDir;

    #[test]
    fn test_search_empty_namespace() {
        let dir = TempDir::new().unwrap();
        let engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        let query = VectorRecord {
            dims: 4,
            dtype: crate::vector::types::Dtype::F32,
            data: vec![0x00; 4 * 4],
        };
        let results = vector_search(&engine, b"ns", &query, 10, Metric::Cosine).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_k_limit() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Put 10 distinct vectors
        for i in 0..10u8 {
            let data = vec![i; 128 * 4];
            engine
                .vector_put(b"ns", &[i], 128, crate::vector::types::Dtype::F32, &data)
                .unwrap();
        }

        let query = VectorRecord {
            dims: 128,
            dtype: crate::vector::types::Dtype::F32,
            data: vec![0u8; 128 * 4],
        };
        let results = vector_search(&engine, b"ns", &query, 3, Metric::Cosine).unwrap();
        assert_eq!(results.len(), 3, "k=3 should return exactly 3 results");
    }

    #[test]
    fn test_search_cosine_ordering() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Put 5 vectors where vector 0 is all 1.0s, vector 1 is all 2.0s, etc.
        for i in 0..5u8 {
            let val = i + 1;
            let data = vec![val; 128 * 4];
            engine
                .vector_put(b"ns", &[i], 128, crate::vector::types::Dtype::F32, &data)
                .unwrap();
        }

        // Query matches vector 0 exactly → should be closest
        let query = VectorRecord {
            dims: 128,
            dtype: crate::vector::types::Dtype::F32,
            data: vec![1u8; 128 * 4],
        };
        let results = vector_search(&engine, b"ns", &query, 5, Metric::Cosine).unwrap();
        assert!(!results.is_empty(), "should have results");
        // All vectors are proportional (1,1,1...), (2,2,2...), etc.
        // Cosine distance to self = 0, to proportional = 0
        // So all should have distance ≈ 0
        assert!(
            results[0].distance < 1e-4,
            "first result should be ~0 distance, got {}",
            results[0].distance
        );
    }

    #[test]
    fn test_search_l2_ordering() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Put vectors with different magnitudes
        for i in 0..5u8 {
            let val = (i + 1) as f32;
            let bytes: Vec<u8> = (0..128).flat_map(|_| val.to_le_bytes().to_vec()).collect();
            engine
                .vector_put(b"ns", &[i], 128, crate::vector::types::Dtype::F32, &bytes)
                .unwrap();
        }

        // Query is vector 1 (all 1.0s)
        let query = VectorRecord {
            dims: 128,
            dtype: crate::vector::types::Dtype::F32,
            data: (0..128)
                .flat_map(|_| 1.0f32.to_le_bytes().to_vec())
                .collect(),
        };
        let results = vector_search(&engine, b"ns", &query, 5, Metric::L2).unwrap();
        assert!(!results.is_empty());
        // L2 to self should be 0
        assert!(
            results[0].distance < 1e-4,
            "first L2 result should be ~0, got {}",
            results[0].distance
        );
    }

    #[test]
    fn test_search_dot_product_ordering() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Put orthogonal-ish vectors on different axes:
        // key0 = [10, 0, 0, ...]  (x-axis)
        // key1 = [0, 20, 0, ...]  (y-axis)
        // key2 = [0, 0, 30, ...]  (z-axis)
        for i in 0..3u8 {
            let val = ((i + 1) * 10) as f32;
            let mut bytes = vec![0u8; 128 * 4];
            let offset = (i as usize) * 4;
            bytes[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
            engine
                .vector_put(b"ns", &[i], 128, crate::vector::types::Dtype::F32, &bytes)
                .unwrap();
        }

        // Query is [1, 0, 0, ...] (aligned with key0 on x-axis)
        let mut query_data = vec![0u8; 128 * 4];
        query_data[0..4].copy_from_slice(&1.0f32.to_le_bytes());
        let query = VectorRecord {
            dims: 128,
            dtype: crate::vector::types::Dtype::F32,
            data: query_data,
        };
        let results = vector_search(&engine, b"ns", &query, 3, Metric::DotProduct).unwrap();
        assert!(!results.is_empty());
        // Dot product with [1,0,0] is maximized by key0 ([10,0,0])
        // key1 and key2 are orthogonal → dot product = 0
        // key0 should have the smallest (most negative) distance = -10
        assert_eq!(
            results[0].key,
            vec![0u8],
            "key0 should have highest dot product with x-axis query"
        );
        // Verify key1 and key2 have distance 0 (orthogonal → dot = 0)
        assert!(
            results[1].distance.abs() < 1e-4,
            "orthogonal vectors should have dot product ~0"
        );
        assert!(
            results[2].distance.abs() < 1e-4,
            "orthogonal vectors should have dot product ~0"
        );
    }

    #[test]
    fn test_search_deleted_vector_excluded() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        for i in 0..3u8 {
            let data = vec![i; 128 * 4];
            engine
                .vector_put(b"ns", &[i], 128, crate::vector::types::Dtype::F32, &data)
                .unwrap();
        }

        engine.vector_delete(b"ns", &[1]).unwrap();

        let query = VectorRecord {
            dims: 128,
            dtype: crate::vector::types::Dtype::F32,
            data: vec![0u8; 128 * 4],
        };
        let results = vector_search(&engine, b"ns", &query, 10, Metric::Cosine).unwrap();
        // key1 was deleted, so only 2 results
        let keys: Vec<Vec<u8>> = results.iter().map(|r| r.key.clone()).collect();
        assert_eq!(keys.len(), 2);
        assert!(!keys.contains(&vec![1u8]));
    }

    #[test]
    fn test_search_dimension_mismatch_skipped() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Put 64-dim vector
        let data = vec![0u8; 64 * 4];
        engine
            .vector_put(b"ns", b"key", 64, crate::vector::types::Dtype::F32, &data)
            .unwrap();

        // Search with 128-dim query → should skip the mismatched record
        let query = VectorRecord {
            dims: 128,
            dtype: crate::vector::types::Dtype::F32,
            data: vec![0u8; 128 * 4],
        };
        let results = vector_search(&engine, b"ns", &query, 10, Metric::Cosine).unwrap();
        assert!(results.is_empty(), "mismatched dimension should be skipped");
    }

    #[test]
    fn test_search_dtype_mismatch_skipped() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Put f32 vector
        let data = vec![0u8; 128 * 4];
        engine
            .vector_put(b"ns", b"key", 128, crate::vector::types::Dtype::F32, &data)
            .unwrap();

        // Search with i8 query → should skip the mismatched record
        let query = VectorRecord {
            dims: 128,
            dtype: crate::vector::types::Dtype::I8,
            data: vec![0u8; 128],
        };
        let results = vector_search(&engine, b"ns", &query, 10, Metric::Cosine).unwrap();
        assert!(results.is_empty(), "mismatched dtype should be skipped");
    }

    #[test]
    fn test_search_results_sorted() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Put vectors with increasing distance from origin along x-axis
        for i in 0..5u8 {
            let val = ((i + 1) * 10) as f32;
            let mut bytes = vec![0u8; 128 * 4];
            bytes[0..4].copy_from_slice(&val.to_le_bytes());
            engine
                .vector_put(b"ns", &[i], 128, crate::vector::types::Dtype::F32, &bytes)
                .unwrap();
        }

        // Query is [1,0,0,...] (closest to key0 = [10,0,0,...])
        let mut query_data = vec![0u8; 128 * 4];
        query_data[0..4].copy_from_slice(&1.0f32.to_le_bytes());
        let query = VectorRecord {
            dims: 128,
            dtype: crate::vector::types::Dtype::F32,
            data: query_data,
        };
        let results = vector_search(&engine, b"ns", &query, 5, Metric::L2).unwrap();
        assert_eq!(results.len(), 5);

        // Verify ascending order
        for i in 1..results.len() {
            assert!(
                results[i - 1].distance <= results[i].distance,
                "results should be sorted by ascending distance"
            );
        }
    }

    #[test]
    fn test_vector_page_paginates_correctly() {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

        // Insert 10 vectors; page size 3 → 4 pages (3+3+3+1)
        for i in 0u8..10 {
            let bytes = vec![i; 4 * 4];
            engine
                .vector_put(b"ns", &[i], 4, crate::vector::types::Dtype::F32, &bytes)
                .unwrap();
        }

        let mut all_keys = vec![];
        let mut cursor: Option<Vec<u8>> = None;
        let mut pages = 0;

        loop {
            let page = vector_page(&engine, b"ns", cursor.as_deref(), 3).unwrap();
            for (k, _) in &page.records {
                all_keys.push(k.clone());
            }
            pages += 1;
            let done = page.next_key.is_none();
            cursor = page.next_key;
            if done {
                break;
            }
        }

        assert_eq!(all_keys.len(), 10, "all 10 vectors should be visited");
        // No duplicate keys
        all_keys.sort();
        all_keys.dedup();
        assert_eq!(all_keys.len(), 10, "no duplicates across pages");
        assert!(pages >= 4, "should need at least 4 pages for 10 vectors at size 3");
    }

    #[test]
    fn test_vector_page_empty_namespace() {
        let dir = TempDir::new().unwrap();
        let engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        let page = vector_page(&engine, b"empty", None, 10).unwrap();
        assert!(page.records.is_empty());
        assert!(page.next_key.is_none());
    }
}
