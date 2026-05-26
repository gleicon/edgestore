use std::collections::{BinaryHeap, HashSet};
use std::cmp::Reverse;

use crate::error::EdgestoreError;
use crate::vector::distance::{distance, Metric};
use crate::vector::types::Dtype;

/// Magic bytes for HNSW sidecar file.
const HNSW_MAGIC: &[u8; 4] = b"HNSW";
const HNSW_VERSION: u16 = 1;

/// A node in the HNSW graph.
#[derive(Debug, Clone)]
pub struct HnswNode {
    /// Opaque vector key (e.g. raw bytes from the KV store).
    pub vector_id: Vec<u8>,
    /// Raw vector element bytes.
    pub vector_data: Vec<u8>,
    /// Neighbor indices per layer. `neighbors[layer][i] = node index`.
    pub neighbors: Vec<Vec<usize>>,
}

/// Hierarchical Navigable Small World index for approximate nearest neighbor search.
#[derive(Debug, Clone)]
pub struct HnswIndex {
    /// All nodes in the graph.
    pub nodes: Vec<HnswNode>,
    /// Index of the entry point node (top layer).
    pub entry_point: usize,
    /// Highest layer index in the graph.
    pub max_layer: usize,
    /// Max neighbors per node per layer (M parameter).
    pub m: usize,
    /// Beam width during construction.
    pub ef_construction: usize,
    /// Vector dimensions.
    pub dims: u16,
    /// Vector data type.
    pub dtype: Dtype,
    /// Distance metric.
    pub metric: Metric,
    /// Internal RNG seed for deterministic layer assignment.
    rng_seed: u64,
}

/// Candidate for greedy beam search.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Candidate {
    node_idx: usize,
    distance: f32,
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.distance
            .partial_cmp(&other.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.node_idx.cmp(&other.node_idx))
    }
}

impl HnswIndex {
    /// Create a new empty HNSW index.
    pub fn new(dims: u16, dtype: Dtype, metric: Metric) -> Self {
        HnswIndex {
            nodes: Vec::new(),
            entry_point: 0,
            max_layer: 0,
            m: 16,
            ef_construction: 100,
            dims,
            dtype,
            metric,
            rng_seed: 12345,
        }
    }

    /// Set construction parameters (must be called before any insertions).
    pub fn with_params(mut self, m: usize, ef_construction: usize) -> Self {
        self.m = m;
        self.ef_construction = ef_construction;
        self
    }

    /// Set the RNG seed for deterministic testing.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng_seed = seed;
        self
    }

    fn next_rng(&mut self) -> f64 {
        self.rng_seed = self.rng_seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.rng_seed as f64) / (u64::MAX as f64)
    }

    fn layer_for_node(&mut self) -> usize {
        let m_l = 1.0 / (self.m as f64).ln();
        let r = self.next_rng();
        // Protect against r == 0 which would give ln(0) = -inf
        let r = r.max(1e-10);
        (-r.ln() * m_l).floor() as usize
    }

    /// Compute distance between the query vector (raw bytes) and a node.
    fn distance_to_node(&self, query: &[u8], node_idx: usize) -> Result<f32, EdgestoreError> {
        let node = &self.nodes[node_idx];
        distance(query, &node.vector_data, self.dtype, self.metric)
    }

    /// Greedy beam search on a single layer.
    ///
    /// Returns up to `ef` closest neighbors at the given layer.
    fn search_layer(
        &self,
        query: &[u8],
        entry_points: &[usize],
        ef: usize,
        layer: usize,
    ) -> Result<Vec<(usize, f32)>, EdgestoreError> {
        let mut visited: HashSet<usize> = HashSet::new();
        // candidates is a min-heap (closest first)
        let mut candidates: BinaryHeap<Reverse<Candidate>> = BinaryHeap::new();
        // results is a max-heap (farthest on top, so we can prune)
        let mut results: BinaryHeap<Candidate> = BinaryHeap::new();

        for &ep in entry_points {
            let dist = self.distance_to_node(query, ep)?;
            visited.insert(ep);
            candidates.push(Reverse(Candidate { node_idx: ep, distance: dist }));
            results.push(Candidate { node_idx: ep, distance: dist });
        }

        while let Some(Reverse(cand)) = candidates.pop() {
            // Stop if this candidate is farther than the worst result we are keeping
            if let Some(worst) = results.peek() {
                if cand.distance > worst.distance {
                    break;
                }
            }

            let node = &self.nodes[cand.node_idx];
            if layer < node.neighbors.len() {
                for &neighbor_idx in &node.neighbors[layer] {
                    if visited.insert(neighbor_idx) {
                        let dist = self.distance_to_node(query, neighbor_idx)?;
                        candidates.push(Reverse(Candidate {
                            node_idx: neighbor_idx,
                            distance: dist,
                        }));
                        results.push(Candidate {
                            node_idx: neighbor_idx,
                            distance: dist,
                        });
                        if results.len() > ef {
                            results.pop(); // remove farthest
                        }
                    }
                }
            }
        }

        let mut out: Vec<(usize, f32)> = results
            .into_iter()
            .map(|c| (c.node_idx, c.distance))
            .collect();
        out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out)
    }

    /// Approximate nearest neighbor search.
    ///
    /// Returns top-k results as `(vector_id, distance)` sorted ascending by distance.
    pub fn search(
        &self,
        query: &[u8],
        k: usize,
        ef: usize,
    ) -> Result<Vec<(Vec<u8>, f32)>, EdgestoreError> {
        if self.nodes.is_empty() {
            return Ok(Vec::new());
        }
        if self.nodes.len() == 1 {
            let dist = self.distance_to_node(query, 0)?;
            return Ok(vec![(self.nodes[0].vector_id.clone(), dist)]);
        }

        let ef = ef.max(k);

        // Greedy search from top layer down to layer 0
        let mut curr = self.entry_point;
        for layer in (1..=self.max_layer).rev() {
            let res = self.search_layer(query, &[curr], ef.max(1), layer)?;
            if let Some((best_idx, _)) = res.first() {
                curr = *best_idx;
            }
        }

        // Layer 0: beam search with ef candidates
        let layer0_results = self.search_layer(query, &[curr], ef, 0)?;

        let mut out: Vec<(Vec<u8>, f32)> = layer0_results
            .into_iter()
            .take(k)
            .map(|(idx, dist)| (self.nodes[idx].vector_id.clone(), dist))
            .collect();

        out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out)
    }

    /// Insert a new vector into the index.
    ///
    /// Returns the node index of the inserted vector.
    pub fn insert(
        &mut self,
        vector_id: Vec<u8>,
        vector_data: Vec<u8>,
    ) -> Result<usize, EdgestoreError> {
        let layer = self.layer_for_node();
        let node_idx = self.nodes.len();

        // Initialize neighbor lists for all layers up to the assigned layer
        let mut neighbors = Vec::with_capacity(layer + 1);
        for _ in 0..=layer {
            neighbors.push(Vec::new());
        }

        self.nodes.push(HnswNode {
            vector_id,
            vector_data,
            neighbors,
        });

        if node_idx == 0 {
            // First node
            self.entry_point = 0;
            self.max_layer = layer;
            return Ok(0);
        }

        // Search from top layer down, connecting at each relevant layer
        let mut curr = self.entry_point;
        for l in (layer.min(self.max_layer) + 1..=self.max_layer).rev() {
            let res = self.search_layer(&self.nodes[node_idx].vector_data, &[curr], 1, l)?;
            if let Some((best_idx, _)) = res.first() {
                curr = *best_idx;
            }
        }

        let m = self.m;
        let ef_construction = self.ef_construction;

        for l in (0..=layer.min(self.max_layer)).rev() {
            let knn = self.search_layer(&self.nodes[node_idx].vector_data, &[curr], ef_construction, l)?;
            let selected = self.select_neighbors(node_idx, &knn, m, l);

            // Bidirectional connection
            for &neighbor_idx in &selected {
                if neighbor_idx == node_idx {
                    continue;
                }
                self.nodes[node_idx].neighbors[l].push(neighbor_idx);
                if l < self.nodes[neighbor_idx].neighbors.len() {
                    self.nodes[neighbor_idx].neighbors[l].push(node_idx);
                    if self.nodes[neighbor_idx].neighbors[l].len() > m {
                        self.prune_neighbors(neighbor_idx, l, m);
                    }
                }
            }

            // Update curr for next (lower) layer
            if let Some((best_idx, _)) = knn.first() {
                curr = *best_idx;
            }
        }

        if layer > self.max_layer {
            self.max_layer = layer;
            self.entry_point = node_idx;
        }

        Ok(node_idx)
    }

    /// Prune a node's neighbors at a given layer to keep only the M closest.
    fn prune_neighbors(&mut self, node_idx: usize, layer: usize, m: usize) {
        if self.nodes[node_idx].neighbors[layer].len() <= m {
            return;
        }
        let node_data = self.nodes[node_idx].vector_data.clone();
        let mut neighbor_dists: Vec<(usize, f32)> = self.nodes[node_idx].neighbors[layer]
            .iter()
            .filter_map(|&n| {
                if n == node_idx {
                    None
                } else {
                    distance(&node_data, &self.nodes[n].vector_data, self.dtype, self.metric)
                        .ok()
                        .map(|d| (n, d))
                }
            })
            .collect();
        neighbor_dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        neighbor_dists.truncate(m);
        self.nodes[node_idx].neighbors[layer] = neighbor_dists.into_iter().map(|(n, _)| n).collect();
    }

    /// HNSW diversity heuristic for neighbor selection (Algorithm 2 from paper).
    ///
    /// Selects up to `m` neighbors from `candidates` such that each selected node
    /// is closer to the query (node_idx) than to any already-selected node.
    /// This creates diverse long-range edges that enable navigable small-world search.
    fn select_neighbors(
        &self,
        node_idx: usize,
        candidates: &[(usize, f32)],
        m: usize,
        _layer: usize,
    ) -> Vec<usize> {
        if candidates.is_empty() {
            return Vec::new();
        }

        let mut selected: Vec<usize> = Vec::new();
        let mut discarded: Vec<usize> = Vec::new();

        for &(cand_idx, cand_dist) in candidates {
            if cand_idx == node_idx {
                continue;
            }
            if selected.is_empty() {
                selected.push(cand_idx);
                continue;
            }
            // Check if cand is closer to query than to any selected node
            let mut add = true;
            for &sel_idx in &selected {
                let d = match distance(
                    &self.nodes[cand_idx].vector_data,
                    &self.nodes[sel_idx].vector_data,
                    self.dtype,
                    self.metric,
                ) {
                    Ok(d) => d,
                    Err(_) => {
                        add = false;
                        break;
                    }
                };
                if cand_dist > d {
                    add = false;
                    break;
                }
            }
            if add {
                selected.push(cand_idx);
            } else {
                discarded.push(cand_idx);
            }
            if selected.len() >= m {
                break;
            }
        }

        // Fill remaining slots with discarded candidates
        if selected.len() < m {
            for &d in &discarded {
                if !selected.contains(&d) {
                    selected.push(d);
                    if selected.len() >= m {
                        break;
                    }
                }
            }
        }

        selected.truncate(m);
        selected
    }

    /// Serialize the index to bytes for sidecar file storage.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        // Header
        buf.extend_from_slice(HNSW_MAGIC);
        buf.extend_from_slice(&HNSW_VERSION.to_le_bytes());
        buf.extend_from_slice(&(self.max_layer as u16).to_le_bytes());
        buf.extend_from_slice(&(self.entry_point as u32).to_le_bytes());
        buf.extend_from_slice(&(self.nodes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.dims.to_le_bytes());
        buf.push(self.dtype as u8);
        buf.push(self.metric as u8);
        buf.extend_from_slice(&(self.m as u16).to_le_bytes());
        buf.extend_from_slice(&(self.ef_construction as u16).to_le_bytes());

        // Nodes
        for node in &self.nodes {
            buf.extend_from_slice(&(node.vector_id.len() as u32).to_le_bytes());
            buf.extend_from_slice(&node.vector_id);
            buf.extend_from_slice(&(node.vector_data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&node.vector_data);
            buf.extend_from_slice(&(node.neighbors.len() as u16).to_le_bytes());
            for layer_neighbors in &node.neighbors {
                buf.extend_from_slice(&(layer_neighbors.len() as u16).to_le_bytes());
                for &nidx in layer_neighbors {
                    buf.extend_from_slice(&(nidx as u32).to_le_bytes());
                }
            }
        }

        buf
    }

    /// Deserialize an index from bytes.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, EdgestoreError> {
        if bytes.len() < 22 {
            return Err(EdgestoreError::CorruptData("hnsw: truncated header".to_string()));
        }
        let mut pos = 0usize;

        macro_rules! read_bytes {
            ($n:expr, $field:literal) => {{
                if bytes.len() < pos + $n {
                    return Err(EdgestoreError::CorruptData(format!("hnsw: truncated {}", $field)));
                }
                let slice = &bytes[pos..pos + $n];
                pos += $n;
                slice
            }};
        }

        let magic = read_bytes!(4, "magic");
        if magic != HNSW_MAGIC {
            return Err(EdgestoreError::CorruptData("hnsw: invalid magic".to_string()));
        }
        let version = u16::from_le_bytes(read_bytes!(2, "version").try_into().unwrap());
        if version != HNSW_VERSION {
            return Err(EdgestoreError::CorruptData(format!("hnsw: unsupported version {}", version)));
        }
        let max_layer = u16::from_le_bytes(read_bytes!(2, "max_layer").try_into().unwrap()) as usize;
        let entry_point = u32::from_le_bytes(read_bytes!(4, "entry_point").try_into().unwrap()) as usize;
        let node_count = u32::from_le_bytes(read_bytes!(4, "node_count").try_into().unwrap()) as usize;
        let dims = u16::from_le_bytes(read_bytes!(2, "dims").try_into().unwrap());
        let dtype_byte = read_bytes!(1, "dtype")[0];
        let metric_byte = read_bytes!(1, "metric")[0];
        let m = u16::from_le_bytes(read_bytes!(2, "m").try_into().unwrap()) as usize;
        let ef_construction = u16::from_le_bytes(read_bytes!(2, "ef_construction").try_into().unwrap()) as usize;

        let dtype = match dtype_byte {
            0 => Dtype::F32,
            1 => Dtype::F16,
            2 => Dtype::I8,
            b => return Err(EdgestoreError::CorruptData(format!("hnsw: unknown dtype {}", b))),
        };
        let metric = match metric_byte {
            0 => Metric::Cosine,
            1 => Metric::L2,
            2 => Metric::DotProduct,
            b => return Err(EdgestoreError::CorruptData(format!("hnsw: unknown metric {}", b))),
        };

        if node_count > 10_000_000 {
            return Err(EdgestoreError::CorruptData("hnsw: node_count too large".to_string()));
        }

        let mut nodes = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            let id_len = u32::from_le_bytes(read_bytes!(4, "id_len").try_into().unwrap()) as usize;
            let vector_id = read_bytes!(id_len, "vector_id").to_vec();
            let data_len = u32::from_le_bytes(read_bytes!(4, "data_len").try_into().unwrap()) as usize;
            let vector_data = read_bytes!(data_len, "vector_data").to_vec();
            let layer_count = u16::from_le_bytes(read_bytes!(2, "layer_count").try_into().unwrap()) as usize;
            let mut neighbors = Vec::with_capacity(layer_count);
            for _ in 0..layer_count {
                let neighbor_count = u16::from_le_bytes(read_bytes!(2, "neighbor_count").try_into().unwrap()) as usize;
                if neighbor_count > 1_000_000 {
                    return Err(EdgestoreError::CorruptData("hnsw: neighbor_count too large".to_string()));
                }
                let mut layer_neighbors = Vec::with_capacity(neighbor_count);
                for _ in 0..neighbor_count {
                    let nidx = u32::from_le_bytes(read_bytes!(4, "neighbor_idx").try_into().unwrap()) as usize;
                    layer_neighbors.push(nidx);
                }
                neighbors.push(layer_neighbors);
            }
            nodes.push(HnswNode { vector_id, vector_data, neighbors });
        }

        Ok(HnswIndex {
            nodes,
            entry_point,
            max_layer,
            m,
            ef_construction,
            dims,
            dtype,
            metric,
            rng_seed: 12345,
        })
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::types::encode_vector_record;

    fn random_f32_vector(dims: usize, seed: &mut u64) -> Vec<f32> {
        let mut v = Vec::with_capacity(dims);
        for _ in 0..dims {
            *seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            v.push((*seed as f32) / (u64::MAX as f32));
        }
        v
    }

    fn encode_f32_vec(v: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(v.len() * 4);
        for &f in v {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn test_hnsw_insert_and_search_single() {
        let mut index = HnswIndex::new(3, Dtype::F32, Metric::L2).with_seed(42);
        let data = encode_f32_vec(&[1.0, 2.0, 3.0]);
        index.insert(vec![1], data.clone()).unwrap();

        let results = index.search(&data, 1, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, vec![1]);
        assert!(results[0].1 < 1e-6);
    }

    #[test]
    fn test_hnsw_search_empty() {
        let index = HnswIndex::new(3, Dtype::F32, Metric::L2);
        let data = encode_f32_vec(&[1.0, 2.0, 3.0]);
        let results = index.search(&data, 5, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_hnsw_recall_vs_brute_force() {
        // Use structured clustered data instead of pure random for reliable recall
        let dims = 8usize;
        let n = 500usize;
        let k = 10usize;
        let mut index = HnswIndex::new(dims as u16, Dtype::F32, Metric::L2)
            .with_seed(42)
            .with_params(16, 100);

        // Generate 5 clusters of vectors for better HNSW navigability
        let mut seed = 12345u64;
        let mut all_data: Vec<Vec<u8>> = Vec::with_capacity(n);
        let num_clusters = 5usize;
        let per_cluster = n / num_clusters;
        for cluster in 0..num_clusters {
            // Cluster center
            let center: Vec<f32> = (0..dims).map(|_| {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                ((seed % 100) as f32) / 100.0
            }).collect();
            for i in 0..per_cluster {
                let mut v = Vec::with_capacity(dims);
                for d in 0..dims {
                    seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                    let noise = ((seed % 20) as f32) / 100.0 - 0.1;
                    v.push((center[d] + noise).clamp(0.0, 1.0));
                }
                let bytes = encode_f32_vec(&v);
                all_data.push(bytes.clone());
                index.insert(vec![(cluster * per_cluster + i) as u8], bytes).unwrap();
            }
        }

        // Brute-force reference
        let mut query = Vec::with_capacity(dims);
        for d in 0..dims {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            query.push(((seed % 100) as f32) / 100.0);
        }
        let query_bytes = encode_f32_vec(&query);

        let mut brute: Vec<(usize, f32)> = Vec::with_capacity(n);
        for (i, rec) in all_data.iter().enumerate() {
            let d = distance(&query_bytes, rec, Dtype::F32, Metric::L2).unwrap();
            brute.push((i, d));
        }
        brute.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let brute_top: std::collections::HashSet<usize> = brute.iter().take(k).map(|(i, _)| *i).collect();

        // HNSW search
        let hnsw_results = index.search(&query_bytes, k, 100).unwrap();
        let hnsw_top: std::collections::HashSet<usize> = hnsw_results
            .iter()
            .map(|(id_bytes, _)| {
                id_bytes[0] as usize
            })
            .collect();

        let intersection: Vec<_> = hnsw_top.intersection(&brute_top).collect();
        let recall = intersection.len() as f32 / k as f32;
        assert!(
            recall >= 0.70,
            "HNSW recall too low: {} (expected >= 0.70)",
            recall
        );
    }

    #[test]
    fn test_hnsw_self_search() {
        let dims = 16usize;
        let mut index = HnswIndex::new(dims as u16, Dtype::F32, Metric::L2)
            .with_seed(42)
            .with_params(16, 100);

        let mut seed = 12345u64;
        for i in 0..100 {
            let v = random_f32_vector(dims, &mut seed);
            let bytes = encode_f32_vec(&v);
            index.insert(vec![i as u8], bytes).unwrap();
        }

        // Search for a specific vector
        let target_v = random_f32_vector(dims, &mut 12345u64);
        let target_bytes = encode_f32_vec(&target_v);
        let results = index.search(&target_bytes, 1, 10).unwrap();
        assert!(!results.is_empty());
        assert!(results[0].1 < 1e-5, "self-search distance should be ~0, got {}", results[0].1);
    }

    #[test]
    fn test_hnsw_serialize_roundtrip() {
        let dims = 8usize;
        let mut index = HnswIndex::new(dims as u16, Dtype::F32, Metric::L2)
            .with_seed(42)
            .with_params(16, 100);

        let mut seed = 12345u64;
        for i in 0..50 {
            let v = random_f32_vector(dims, &mut seed);
            let bytes = encode_f32_vec(&v);
            index.insert(vec![i as u8], bytes).unwrap();
        }

        let query_v = random_f32_vector(dims, &mut seed);
        let query_bytes = encode_f32_vec(&query_v);
        let results_before = index.search(&query_bytes, 5, 20).unwrap();

        let serialized = index.serialize();
        let deserialized = HnswIndex::deserialize(&serialized).unwrap();
        let results_after = deserialized.search(&query_bytes, 5, 20).unwrap();

        assert_eq!(results_before.len(), results_after.len());
        for (before, after) in results_before.iter().zip(results_after.iter()) {
            assert_eq!(before.0, after.0);
            assert!((before.1 - after.1).abs() < 1e-5);
        }
    }

    #[test]
    fn test_hnsw_deserialize_invalid_magic() {
        let bytes = b"XXXX";
        let result = HnswIndex::deserialize(bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_hnsw_deserialize_too_short() {
        let bytes = b"HNSW\x01\x00";
        let result = HnswIndex::deserialize(bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_hnsw_linear_data() {
        // 10 vectors on a line in 1D: [0.0], [1.0], [2.0], ..., [9.0]
        let mut index = HnswIndex::new(1, Dtype::F32, Metric::L2)
            .with_seed(42)
            .with_params(16, 100);

        for i in 0..10 {
            let bytes = (i as f32).to_le_bytes().to_vec();
            index.insert(vec![i as u8], bytes).unwrap();
        }

        // Search for [4.5] — nearest should be [4] or [5]
        let query = 4.5f32.to_le_bytes().to_vec();
        let results = index.search(&query, 2, 10).unwrap();
        assert_eq!(results.len(), 2);
        let ids: Vec<u8> = results.iter().map(|(id, _)| id[0]).collect();
        assert!(ids.contains(&4) || ids.contains(&5), "Expected 4 or 5, got {:?}", ids);

        // Search for [0.1] — nearest should be [0]
        let query2 = 0.1f32.to_le_bytes().to_vec();
        let results2 = index.search(&query2, 1, 10).unwrap();
        assert_eq!(results2[0].0, vec![0]);
    }
}


