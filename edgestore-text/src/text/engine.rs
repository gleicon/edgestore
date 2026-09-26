use std::collections::HashMap;

use crate::text::facet::FacetFilter;
use crate::text::types::FacetValue;

/// Result of a text search: document key and BM25 score.
#[derive(Debug, Clone)]
pub struct TextSearchResult {
    /// Document key.
    pub doc_id: Vec<u8>,
    /// BM25 relevance score (higher = more relevant).
    pub score: f32,
}

/// A matched text fragment extracted from a document.
#[derive(Debug, Clone)]
pub struct Snippet {
    /// The context window around the match.
    pub text: String,
    /// Start of the matching term within `text` (byte offset).
    pub byte_start: usize,
    /// End of the matching term within `text` (byte offset, exclusive).
    pub byte_end: usize,
}

/// A search result that includes extracted snippets alongside the BM25 score.
#[derive(Debug, Clone)]
pub struct SnippetResult {
    /// Document key.
    pub doc_id: Vec<u8>,
    /// BM25 relevance score.
    pub score: f32,
    /// Extracted text snippets for the query terms found in this document.
    pub snippets: Vec<Snippet>,
}

/// Search options for fine-grained control over text search behavior.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Maximum number of results to return.
    pub k: usize,
    /// Optional facet filters to narrow results.
    pub facet_filters: Vec<FacetFilter>,
    /// Enable typo tolerance (edit distance ≤ 1).
    pub typo_tolerance: bool,
}

/// Generate the synthetic namespace for text storage.
pub fn text_namespace(ns: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + ns.len());
    out.extend_from_slice(b"__text__");
    out.extend_from_slice(ns);
    out
}
