//! Vector search API layer.
//!
//! Vector records are stored in the KV layer under synthetic namespaces
//! (`__vec__{ns}` per D09). This module provides the encoding, distance,
//! and search logic that sits on top of pure KV.

/// Vector engine trait and namespace helpers.
pub mod api;
/// Distance metrics (cosine, L2, dot product) with SIMD.
pub mod distance;
/// HNSW approximate nearest neighbor index.
pub mod hnsw;
/// Brute-force and HNSW vector search.
pub mod search;
/// Vector record types and encoding.
pub mod types;

pub use api::{vector_namespace, VectorEngine};
pub use distance::{distance, distance_scalar, distance_simd_f32, Metric};
pub use hnsw::HnswIndex;
pub use search::{vector_search, VectorSearchResult};
