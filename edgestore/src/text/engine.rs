use std::collections::HashMap;

use crate::error::EdgestoreError;
use crate::text::types::FacetValue;
use crate::types::Lsn;

/// Result of a text search: document key and BM25 score.
#[derive(Debug, Clone)]
pub struct TextSearchResult {
    pub doc_id: Vec<u8>,
    pub score: f32,
}

/// Trait for full-text search operations on a KV engine.
///
/// Text records are stored under synthetic namespaces (`__text__{ns}`)
/// so they are isolated from plain KV and vector data.
pub trait TextEngine {
    /// Index a text document under the given namespace and key.
    fn index_text(
        &mut self,
        ns: &[u8],
        key: &[u8],
        text: &str,
        facets: HashMap<String, FacetValue>,
    ) -> Result<Lsn, EdgestoreError>;

    /// Search for the k most relevant documents matching the query.
    fn search_text(
        &self,
        ns: &[u8],
        query: &str,
        k: usize,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError>;

    /// Delete a text document from the index.
    fn delete_text(&mut self, ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError>;
}

/// Generate the synthetic namespace for text storage.
pub fn text_namespace(ns: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(7 + ns.len());
    out.extend_from_slice(b"__text__");
    out.extend_from_slice(ns);
    out
}
