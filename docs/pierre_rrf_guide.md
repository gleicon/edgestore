# Hybrid Search with Reciprocal Rank Fusion (Pierre guide)

**ENG-8 is a caller-side pattern, not an engine API.**

edgestore returns scored results from both its BM25 text index and its vector index.
Fusing those two ranked lists into a single ranking — Reciprocal Rank Fusion (RRF) —
is the caller's responsibility. This keeps the engine dependency-free and lets you
tune the fusion without an engine upgrade.

## Why caller-side?

RRF operates on rank positions, not raw scores. To compute a fused rank you need
both complete result lists first — there is no iterleaving or short-circuit opportunity
inside the engine. The cost of fusion is O(k) where k is the result set size, which
is negligible compared to the I/O cost of retrieval. Nothing is gained by moving it
into the engine.

## Implementing RRF

```rust
use edgestore::{Engine, EdgestoreConfig, TextEngine, VectorEngine};
use edgestore::vector::distance::Metric;
use edgestore::vector::types::{Dtype, VectorRecord};
use std::collections::HashMap;

/// Standard RRF constant. 60 is the conventional default from the original paper.
const RRF_K: f32 = 60.0;

#[derive(Debug)]
pub struct HybridResult {
    pub doc_id: Vec<u8>,
    pub rrf_score: f32,
    pub bm25_rank: Option<usize>,
    pub vector_rank: Option<usize>,
}

/// Fuse lexical and vector results using Reciprocal Rank Fusion.
///
/// Both `text_results` and `vector_results` are ordered best-first (rank 0 = best).
/// Returns results ordered by descending RRF score, deduplicated by doc_id.
pub fn rrf_merge(
    text_results: &[Vec<u8>],   // doc_ids in BM25 rank order
    vector_results: &[Vec<u8>], // doc_ids in vector rank order
    k: usize,                   // how many fused results to return
) -> Vec<HybridResult> {
    let mut scores: HashMap<Vec<u8>, (f32, Option<usize>, Option<usize>)> = HashMap::new();

    for (rank, doc_id) in text_results.iter().enumerate() {
        let entry = scores.entry(doc_id.clone()).or_insert((0.0, None, None));
        entry.0 += 1.0 / (RRF_K + rank as f32 + 1.0);
        entry.1 = Some(rank);
    }
    for (rank, doc_id) in vector_results.iter().enumerate() {
        let entry = scores.entry(doc_id.clone()).or_insert((0.0, None, None));
        entry.0 += 1.0 / (RRF_K + rank as f32 + 1.0);
        entry.2 = Some(rank);
    }

    let mut results: Vec<HybridResult> = scores
        .into_iter()
        .map(|(doc_id, (rrf_score, bm25_rank, vector_rank))| HybridResult {
            doc_id,
            rrf_score,
            bm25_rank,
            vector_rank,
        })
        .collect();

    results.sort_by(|a, b| b.rrf_score.partial_cmp(&a.rrf_score).unwrap());
    results.truncate(k);
    results
}
```

## Using it with edgestore

```rust
fn hybrid_search(
    engine: &mut Engine,
    ns: &[u8],
    text_query: &str,
    vector_query: &VectorRecord,
    k: usize,
) -> Vec<HybridResult> {
    // Retrieve more than k from each index so fusion has room to rerank.
    let fetch_n = k * 3;

    let text_hits = engine
        .search_text(ns, text_query, fetch_n)
        .unwrap_or_default();
    let vec_hits = engine
        .vector_search(ns, vector_query, fetch_n, Metric::Cosine)
        .unwrap_or_default();

    let text_ids: Vec<Vec<u8>> = text_hits.into_iter().map(|r| r.doc_id).collect();
    let vec_ids: Vec<Vec<u8>> = vec_hits.into_iter().map(|r| r.key).collect();

    rrf_merge(&text_ids, &vec_ids, k)
}
```

## With cost accounting (ENG-12)

```rust
fn hybrid_search_with_stats(
    engine: &mut Engine,
    ns: &[u8],
    text_query: &str,
    vector_query: &VectorRecord,
    k: usize,
) -> (Vec<HybridResult>, u64) { // (results, total_bytes_scanned)
    let fetch_n = k * 3;

    let (text_hits, text_stats) = engine
        .search_text_with_stats(ns, text_query, fetch_n)
        .unwrap_or_default();
    let (vec_hits, vec_stats) = engine
        .vector_search_with_stats(ns, vector_query, fetch_n, Metric::Cosine)
        .unwrap_or_default();

    let total_bytes = text_stats.bytes_scanned + vec_stats.bytes_scanned;

    let text_ids: Vec<Vec<u8>> = text_hits.into_iter().map(|r| r.doc_id).collect();
    let vec_ids: Vec<Vec<u8>> = vec_hits.into_iter().map(|r| r.key).collect();

    (rrf_merge(&text_ids, &vec_ids, k), total_bytes)
}
```

## Tuning

- **`fetch_n = k * 3`**: A good starting point. If recall is low (important documents
  missing from fused results), increase the multiplier. If latency is too high, reduce it.
- **`RRF_K`**: The original paper uses 60. Smaller values (e.g. 10) give more weight to
  top-ranked results; larger values flatten the distribution.
- **Asymmetric fetch**: If your text index is much larger than your vector index (or vice
  versa), you can fetch different `n` values from each and still use the same `rrf_merge`.
