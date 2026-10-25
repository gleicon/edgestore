//! Full-text search extension for edgestore.
//!
//! Provides [`TextIndex`], a BM25-based full-text search index that stores its
//! data in an [`edgestore::Engine`] via the `__text__` synthetic namespace.
//!
//! # Usage
//!
//! ```rust,no_run
//! use edgestore::{EdgestoreConfig, Engine};
//! use edgestore_text::{TextIndex, Language};
//! use std::collections::HashMap;
//!
//! let config = EdgestoreConfig::new("/tmp/mydb");
//! let mut engine = Engine::open(config).unwrap();
//! let mut text = TextIndex::new();
//!
//! text.index_document(&mut engine, b"articles", b"doc1",
//!     "Rust database storage engine", HashMap::new()).unwrap();
//!
//! let results = text.search(&mut engine, b"articles", "storage", 10).unwrap();
//! text.persist(&mut engine).unwrap();
//! ```

pub mod text;

use std::collections::HashMap;

use edgestore::{EdgestoreError, Engine};

use crate::text::types::{encode_text_record, TextRecord};

// Flat re-exports for convenience: `use edgestore_text::TextIndex;` etc.
pub use crate::text::{
    bm25_score, decode_text_record, filter_by_facets, is_one_edit_away, levenshtein,
    score_document, text_namespace, tokenize, FacetFilter, FacetValue, InvertedIndex,
    Language, Posting, SearchOptions, Snippet, SnippetResult, TextSearchResult,
    TextSearchStats, Token,
};

const TEXT_INDEX_KEY: &[u8] = b"__index__";

/// BM25 full-text search index backed by an [`Engine`].
///
/// Text records are stored in the engine under synthetic `__text__{ns}` namespaces.
/// The inverted index is cached in `self.indices` after the first operation on each
/// namespace. The sidecar (`__index__` key) is written by [`persist`] and enables
/// fast in-process index access, but is NOT used as a cold-start read source —
/// the WAL is always the ground truth.
///
/// ## Crash safety
///
/// Raw text records written by `index_document` are durable in the engine WAL
/// immediately. On cold start (no in-memory cache), `search` and `index_document`
/// scan the WAL via `engine.prefix()` to rebuild the inverted index. The result
/// is cached in `self.indices`, so subsequent same-process operations are fast.
/// This makes `persist()` a **startup optimisation** that avoids the WAL scan
/// after a crash — not a crash-safety requirement.
///
/// ## Flush strategy
///
/// Call [`persist`] **once after a batch** of `index_document` calls, not after
/// every individual document. Persisting after every document causes O(n²) total
/// bytes written (each persist serialises the full growing index). Dirty tracking
/// ensures repeated `persist()` calls with no intervening mutations are no-ops.
///
/// ## API note
///
/// `search`, `search_with_options`, `search_with_stats`, and `search_with_snippets`
/// take `&mut self` because a cold-start WAL reconstruction populates the internal
/// index cache.
///
/// [`persist`]: TextIndex::persist
pub struct TextIndex {
    indices: HashMap<Vec<u8>, InvertedIndex>,
    language: Language,
    /// Namespaces whose in-memory index is ahead of the persisted sidecar.
    dirty: std::collections::HashSet<Vec<u8>>,
}

impl Default for TextIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl TextIndex {
    /// Create a new index defaulting to English stemming and stopwords.
    pub fn new() -> Self {
        Self {
            indices: HashMap::new(),
            language: Language::English,
            dirty: std::collections::HashSet::new(),
        }
    }

    /// Create a new index with the given language.
    pub fn with_language(language: Language) -> Self {
        Self {
            indices: HashMap::new(),
            language,
            dirty: std::collections::HashSet::new(),
        }
    }

    /// Index a document for BM25 search.
    ///
    /// The raw text record is written to the engine via WAL (durable). The
    /// in-memory inverted index is updated but not persisted until [`persist`] is called.
    ///
    /// On cold start (no in-memory cache), the existing index is rebuilt from WAL
    /// records rather than the potentially-stale sidecar.
    ///
    /// [`persist`]: TextIndex::persist
    pub fn index_document(
        &mut self,
        engine: &mut Engine,
        ns: &[u8],
        key: &[u8],
        text: &str,
        facets: HashMap<String, FacetValue>,
    ) -> Result<u64, EdgestoreError> {
        assert!(!ns.is_empty(), "ns must not be empty");
        assert!(!key.is_empty(), "key must not be empty");
        let tokens = tokenize(text, self.language);
        let doc_len_tokens = tokens.len() as u32;
        let text_ns = text_namespace(ns);

        self.ensure_loaded(engine, &text_ns)?;

        debug_assert!(self.indices.contains_key(&text_ns), "ensure_loaded guarantees namespace is populated");
        let index = self.indices.get_mut(&text_ns).unwrap();
        if index.doc_bloom.might_contain(key) {
            index.remove_document(key);
        }
        index.add_document(key.to_vec(), &tokens, doc_len_tokens, facets.clone());
        self.dirty.insert(text_ns.clone());

        let record = TextRecord { text: text.to_string(), facets };
        let record_bytes = encode_text_record(&record);
        engine.put(&text_ns, key, &record_bytes)
    }

    /// Delete a document from the index.
    pub fn delete_document(
        &mut self,
        engine: &mut Engine,
        ns: &[u8],
        key: &[u8],
    ) -> Result<u64, EdgestoreError> {
        assert!(!ns.is_empty(), "ns must not be empty");
        assert!(!key.is_empty(), "key must not be empty");
        let text_ns = text_namespace(ns);
        self.ensure_loaded(engine, &text_ns)?;

        debug_assert!(self.indices.contains_key(&text_ns), "ensure_loaded guarantees namespace is populated");
        let index = self.indices.get_mut(&text_ns).unwrap();
        index.remove_document(key);
        let is_empty = index.total_docs == 0;

        if is_empty {
            self.indices.remove(&text_ns);
            self.dirty.remove(&text_ns);
            engine.delete(&text_ns, TEXT_INDEX_KEY)?;
        } else {
            self.dirty.insert(text_ns.clone());
        }

        engine.delete(&text_ns, key)
    }

    /// Search for the top `results_limit` most relevant documents using BM25 scoring.
    pub fn search(
        &mut self,
        engine: &Engine,
        ns: &[u8],
        query: &str,
        results_limit: usize,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        assert!(!ns.is_empty(), "ns must not be empty");
        self.search_with_options(engine, ns, query, &SearchOptions { limit: results_limit, ..Default::default() })
    }

    /// BM25 search with scan statistics.
    ///
    /// Returns results alongside [`TextSearchStats`] carrying `total_docs_indexed`,
    /// `docs_examined`, and `bytes_scanned`. Useful for quality panels: a result of
    /// `docs_examined == 0` with `total_docs_indexed > 0` means the query terms
    /// genuinely don't appear in the corpus (not "index not built").
    pub fn search_with_stats(
        &mut self,
        engine: &Engine,
        ns: &[u8],
        query: &str,
        results_limit: usize,
    ) -> Result<(Vec<TextSearchResult>, TextSearchStats), EdgestoreError> {
        assert!(!ns.is_empty(), "ns must not be empty");
        if results_limit == 0 {
            return Ok((vec![], TextSearchStats::default()));
        }

        let query_tokens = tokenize(query, self.language);
        if query_tokens.is_empty() {
            return Ok((vec![], TextSearchStats::default()));
        }

        let text_ns = text_namespace(ns);
        self.ensure_loaded(engine, &text_ns)?;

        debug_assert!(self.indices.contains_key(&text_ns), "ensure_loaded guarantees namespace is populated");
        let index = self.indices.get(&text_ns).unwrap();
        Self::search_in_index_with_stats(index, &query_tokens, results_limit)
    }

    /// Search with full options (facets, typo tolerance).
    pub fn search_with_options(
        &mut self,
        engine: &Engine,
        ns: &[u8],
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        assert!(!ns.is_empty(), "ns must not be empty");
        if options.limit == 0 {
            return Ok(vec![]);
        }

        let query_tokens = tokenize(query, self.language);
        if query_tokens.is_empty() {
            return Ok(vec![]);
        }

        let text_ns = text_namespace(ns);
        self.ensure_loaded(engine, &text_ns)?;

        debug_assert!(self.indices.contains_key(&text_ns), "ensure_loaded guarantees namespace is populated");
        let index = self.indices.get(&text_ns).unwrap();
        if index.total_docs == 0 {
            return Ok(vec![]);
        }

        Self::search_in_index(index, &query_tokens, options)
    }

    /// Search and return results with extracted snippets.
    pub fn search_with_snippets(
        &mut self,
        engine: &Engine,
        ns: &[u8],
        query: &str,
        results_limit: usize,
        context_chars: usize,
    ) -> Result<Vec<SnippetResult>, EdgestoreError> {
        assert!(!ns.is_empty(), "ns must not be empty");
        let query_tokens = tokenize(query, self.language);
        if query_tokens.is_empty() || results_limit == 0 {
            return Ok(vec![]);
        }
        let query_terms: std::collections::HashSet<String> =
            query_tokens.iter().map(|t| t.term.clone()).collect();
        let text_ns = text_namespace(ns);
        let base_results = self.search(engine, ns, query, results_limit)?;
        let index_opt = self.indices.get(&text_ns).cloned();

        base_results
            .into_iter()
            .map(|result| {
                let snippets = match &index_opt {
                    Some(index) => Self::build_snippets(
                        index, &result.doc_id, &query_terms, engine, &text_ns, context_chars,
                    )?,
                    None => vec![],
                };
                Ok(SnippetResult { doc_id: result.doc_id, score: result.score, snippets })
            })
            .collect()
    }

    fn build_snippets(
        index: &InvertedIndex,
        doc_id: &[u8],
        query_terms: &std::collections::HashSet<String>,
        engine: &Engine,
        text_ns: &[u8],
        context_chars: usize,
    ) -> Result<Vec<Snippet>, EdgestoreError> {
        assert!(!doc_id.is_empty(), "doc_id must not be empty");
        assert!(!text_ns.is_empty(), "text_ns must not be empty");
        let mut char_positions: Vec<u32> = Vec::new();
        for (term, postings) in &index.postings {
            if !query_terms.contains(term.as_str()) {
                continue;
            }
            if let Some(posting) = postings.iter().find(|p| p.doc_id.as_slice() == doc_id) {
                char_positions.extend_from_slice(&posting.positions);
            }
        }
        if char_positions.is_empty() {
            return Ok(vec![]);
        }
        let raw = match engine.get(text_ns, doc_id)? {
            Some(r) => r,
            None => return Ok(vec![]),
        };
        // None here means the WAL record is corrupt; WAL reconstruction would have
        // skipped it too, so we return no snippets rather than propagating an error.
        let record = match decode_text_record(&raw) {
            Some(r) => r,
            None => return Ok(vec![]),
        };
        let chars: Vec<char> = record.text.chars().collect();
        char_positions.sort_unstable();
        char_positions.dedup();
        debug_assert!(!char_positions.is_empty(), "char_positions must be non-empty after dedup");
        Ok(char_positions
            .iter()
            .filter_map(|&pos| {
                let char_start = pos as usize;
                if char_start >= chars.len() {
                    return None;
                }
                let char_end = chars[char_start..]
                    .iter()
                    .position(|c| c.is_whitespace())
                    .map(|i| char_start + i)
                    .unwrap_or(chars.len());
                let ctx_start = char_start.saturating_sub(context_chars);
                let ctx_end = (char_end + context_chars).min(chars.len());
                let context_text: String = chars[ctx_start..ctx_end].iter().collect();
                let prefix_text: String = chars[ctx_start..char_start].iter().collect();
                let matched_text: String = chars[char_start..char_end].iter().collect();
                Some(Snippet {
                    text: context_text,
                    byte_start: prefix_text.len(),
                    byte_end: prefix_text.len() + matched_text.len(),
                })
            })
            .collect())
    }

    /// Persist dirty in-memory indices to the engine as sidecar entries.
    ///
    /// Only namespaces modified since the last `persist` call are written.
    /// Calling `persist` with no intervening `index_document`/`delete_document`
    /// is a no-op.
    ///
    /// ## Performance
    ///
    /// Call once after a **batch** of `index_document` calls, not after every
    /// individual document. Calling after every document results in O(n²) total
    /// bytes written because each persist serialises the full growing index.
    pub fn persist(&mut self, engine: &mut Engine) -> Result<(), EdgestoreError> {
        if self.dirty.is_empty() {
            return Ok(());
        }
        let to_persist: Vec<(Vec<u8>, Vec<u8>)> = self
            .indices
            .iter()
            .filter(|(ns, _)| self.dirty.contains(*ns))
            .map(|(ns, index)| (ns.clone(), index.serialize()))
            .collect();
        let mut lsns: Vec<(Vec<u8>, u64)> = Vec::with_capacity(to_persist.len());
        for (ns, bytes) in to_persist {
            let lsn = engine.put(&ns, TEXT_INDEX_KEY, &bytes)?;
            lsns.push((ns, lsn));
        }
        for (ns, lsn) in lsns {
            if let Some(index) = self.indices.get_mut(&ns) {
                index.sidecar_lsn = lsn;
            }
            self.dirty.remove(&ns);
        }
        Ok(())
    }

    /// Ensure `self.indices` has an entry for `text_ns`, rebuilding from WAL if needed.
    ///
    /// This is the single cold-start loading point. WAL is always the ground truth —
    /// the sidecar is never loaded here because it may be stale after a crash.
    fn ensure_loaded(&mut self, engine: &Engine, text_ns: &[u8]) -> Result<(), EdgestoreError> {
        assert!(!text_ns.is_empty(), "text_ns must not be empty");
        if !self.indices.contains_key(text_ns) {
            let rebuilt = Self::rebuild_from_engine(engine, text_ns, self.language)?;
            self.indices.insert(text_ns.to_vec(), rebuilt);
        }
        debug_assert!(self.indices.contains_key(text_ns), "namespace must be populated after ensure_loaded");
        Ok(())
    }

    /// Rebuild an [`InvertedIndex`] by scanning raw text records from the engine WAL.
    ///
    /// This is the cold-start ground truth. Skips the `__index__` sidecar key.
    /// The result is inserted into `self.indices` by the callers (not marked dirty —
    /// it reflects what is durable, not an un-persisted change).
    fn rebuild_from_engine(
        engine: &Engine,
        text_ns: &[u8],
        language: Language,
    ) -> Result<InvertedIndex, EdgestoreError> {
        assert!(!text_ns.is_empty(), "text_ns must not be empty");
        let pairs = engine.prefix(text_ns, b"")?;
        let mut index = InvertedIndex::new();
        for (key, value) in &pairs {
            if key.as_slice() == TEXT_INDEX_KEY {
                continue;
            }
            if let Some(record) = decode_text_record(value) {
                let tokens = tokenize(&record.text, language);
                let doc_len_tokens = tokens.len() as u32;
                index.add_document(key.clone(), &tokens, doc_len_tokens, record.facets);
            }
            // decode_text_record returning None means a corrupt WAL record — skipped
            // silently here because WAL reconstruction is best-effort; the document
            // simply won't appear in search results, which is consistent with what
            // would happen if the write never completed.
        }
        debug_assert!(index.total_docs as usize <= pairs.len(), "total_docs cannot exceed WAL pair count");
        Ok(index)
    }

    fn search_in_index(
        index: &InvertedIndex,
        query_tokens: &[crate::text::tokenizer::Token],
        options: &SearchOptions,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        Ok(Self::score_index_impl(index, query_tokens, options)?.0)
    }

    fn search_in_index_with_stats(
        index: &InvertedIndex,
        query_tokens: &[crate::text::tokenizer::Token],
        results_limit: usize,
    ) -> Result<(Vec<TextSearchResult>, TextSearchStats), EdgestoreError> {
        Self::score_index_impl(index, query_tokens, &SearchOptions { limit: results_limit, ..Default::default() })
    }

    /// Expand `search_terms` with typo candidates (edit distance ≤ 1) from the index.
    fn expand_typo_terms(
        index: &InvertedIndex,
        query_tokens: &[crate::text::tokenizer::Token],
        search_terms: &mut Vec<String>,
    ) {
        for token in query_tokens {
            for term in index.postings.keys() {
                if term != &token.term
                    && is_one_edit_away(term, &token.term)
                    && !search_terms.contains(term)
                {
                    search_terms.push(term.clone());
                }
            }
        }
    }

    fn score_index_impl(
        index: &InvertedIndex,
        query_tokens: &[crate::text::tokenizer::Token],
        options: &SearchOptions,
    ) -> Result<(Vec<TextSearchResult>, TextSearchStats), EdgestoreError> {
        assert!(!query_tokens.is_empty(), "query_tokens must not be empty — callers must check");
        assert!(options.limit > 0, "limit must be positive — callers must check");
        let mut search_terms: Vec<String> =
            query_tokens.iter().map(|t| t.term.clone()).collect();

        if options.typo_tolerance {
            Self::expand_typo_terms(index, query_tokens, &mut search_terms);
        }

        let mut doc_scores: HashMap<Vec<u8>, f32> = HashMap::new();
        let mut bytes_scanned: u64 = 0;
        let avg_doc_len = index.avg_doc_len();

        for term in &search_terms {
            if let Some(postings) = index.postings.get(term) {
                let doc_freq = postings.len() as u64;
                bytes_scanned +=
                    postings.len() as u64
                    * std::mem::size_of::<crate::text::index::Posting>() as u64;

                let filtered = if !options.facet_filters.is_empty() {
                    crate::text::facet::filter_by_facets(postings, &options.facet_filters)
                } else {
                    postings.to_vec()
                };

                let is_fuzzy = !query_tokens.iter().any(|t| &t.term == term);
                let weight = if is_fuzzy { 0.5 } else { 1.0 };

                for posting in &filtered {
                    let score = bm25_score(
                        index.total_docs,
                        doc_freq,
                        posting.term_freq,
                        posting.doc_len_tokens,
                        avg_doc_len,
                        crate::text::index::BM25_K1,
                        crate::text::index::BM25_B,
                    ) * weight;
                    *doc_scores.entry(posting.doc_id.clone()).or_insert(0.0) += score;
                }
            }
        }

        let stats = TextSearchStats {
            total_docs_indexed: index.total_docs,
            docs_examined: doc_scores.len() as u64,
            bytes_scanned,
        };

        let mut results: Vec<TextSearchResult> = doc_scores
            .into_iter()
            .map(|(doc_id, score)| TextSearchResult { doc_id, score })
            .collect();
        results.sort_by(|a, b| {
            edgestore::total_cmp_f32(b.score, a.score).then(a.doc_id.cmp(&b.doc_id))
        });
        results.truncate(options.limit);
        debug_assert!(results.len() <= options.limit, "results must not exceed limit after truncation");
        Ok((results, stats))
    }
}
