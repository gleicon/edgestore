use crate::text::facet::FacetFilter;

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

/// Statistics returned by [`TextIndex::search_with_stats`].
#[derive(Debug, Clone, Default)]
pub struct TextSearchStats {
    /// Total documents currently in the index for this namespace.
    /// Zero means the index has not been built or persisted yet.
    pub total_docs_indexed: u64,
    /// Number of distinct documents that matched at least one query term
    /// (before the top-k cutoff). Use this to detect under-indexing: if
    /// `docs_examined == 0` and `total_docs_indexed > 0` the query terms
    /// simply don't appear in the corpus.
    pub docs_examined: u64,
    /// Approximate bytes of posting-list data scanned during this query.
    /// Useful for diagnosing index bloat; zero when no matching terms exist.
    pub bytes_scanned: u64,
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
