//! Vector search API layer.
//!
//! Vector records are stored in the KV layer under synthetic namespaces
//! (`__vec__{ns}` per D09). This module provides the encoding, distance,
//! and search logic that sits on top of pure KV.

pub mod api;
pub mod types;

pub use api::{vector_namespace, VectorEngine};
