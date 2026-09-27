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
//! let results = text.search(&engine, b"articles", "storage", 10).unwrap();
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
/// The inverted index is cached in memory and flushed to a sidecar key on [`persist`].
///
/// ## Flush strategy
///
/// Call [`persist`] **once after a batch** of `index_document` calls, not after
/// every individual document. Persisting after every document causes O(n²) total
/// bytes written (each persist serialises the full growing index).
///
/// Raw text records written by `index_document` are durable in the engine WAL
/// immediately; only the search-optimised sidecar needs an explicit persist.
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
    /// [`persist`]: TextIndex::persist
    pub fn index_document(
        &mut self,
        engine: &mut Engine,
        ns: &[u8],
        key: &[u8],
        text: &str,
        facets: HashMap<String, FacetValue>,
    ) -> Result<u64, EdgestoreError> {
        let tokens = tokenize(text, self.language);
        let doc_len = tokens.len() as u32;
        let text_ns = text_namespace(ns);

        let loaded_index = match engine.get(&text_ns, TEXT_INDEX_KEY) {
            Ok(Some(bytes)) => {
                InvertedIndex::deserialize(&bytes).unwrap_or_else(|_| InvertedIndex::new())
            }
            _ => InvertedIndex::new(),
        };

        let index = self.indices.entry(text_ns.clone()).or_insert(loaded_index);
        if index.doc_bloom.might_contain(key) {
            index.remove_document(key);
        }
        index.add_document(key.to_vec(), &tokens, doc_len, facets.clone());
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
        let text_ns = text_namespace(ns);

        let mut index = match self.indices.remove(&text_ns) {
            Some(idx) => idx,
            None => match engine.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(bytes) => {
                    InvertedIndex::deserialize(&bytes).unwrap_or_else(|_| InvertedIndex::new())
                }
                None => InvertedIndex::new(),
            },
        };

        index.remove_document(key);
        self.dirty.insert(text_ns.clone());

        if index.total_docs == 0 {
            self.indices.remove(&text_ns);
            self.dirty.remove(&text_ns);
            engine.delete(&text_ns, TEXT_INDEX_KEY)?;
        } else {
            let index_bytes = index.serialize();
            engine.put(&text_ns, TEXT_INDEX_KEY, &index_bytes)?;
            self.indices.insert(text_ns.clone(), index);
        }

        engine.delete(&text_ns, key)
    }

    /// Search for the k most relevant documents using BM25 scoring.
    pub fn search(
        &self,
        engine: &Engine,
        ns: &[u8],
        query: &str,
        k: usize,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        self.search_with_options(engine, ns, query, &SearchOptions { k, ..Default::default() })
    }

    /// BM25 search with scan statistics.
    ///
    /// Returns results alongside [`TextSearchStats`] carrying `total_docs_indexed`,
    /// `docs_examined`, and `bytes_scanned`. Useful for quality panels: a result of
    /// `docs_examined == 0` with `total_docs_indexed > 0` means the query terms
    /// genuinely don't appear in the corpus (not "index not built").
    pub fn search_with_stats(
        &self,
        engine: &Engine,
        ns: &[u8],
        query: &str,
        k: usize,
    ) -> Result<(Vec<TextSearchResult>, TextSearchStats), EdgestoreError> {
        if k == 0 {
            return Ok((vec![], TextSearchStats::default()));
        }

        let query_tokens = tokenize(query, self.language);
        if query_tokens.is_empty() {
            return Ok((vec![], TextSearchStats::default()));
        }

        let text_ns = text_namespace(ns);

        let index = match self.indices.get(&text_ns) {
            Some(idx) => {
                return Self::search_in_index_with_stats(idx, &query_tokens, k);
            }
            None => match engine.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(bytes) => InvertedIndex::deserialize(&bytes)?,
                None => {
                    return Ok((vec![], TextSearchStats::default()));
                }
            },
        };

        Self::search_in_index_with_stats(&index, &query_tokens, k)
    }

    /// Search with full options (facets, typo tolerance).
    pub fn search_with_options(
        &self,
        engine: &Engine,
        ns: &[u8],
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        if options.k == 0 {
            return Ok(vec![]);
        }

        let query_tokens = tokenize(query, self.language);
        if query_tokens.is_empty() {
            return Ok(vec![]);
        }

        let text_ns = text_namespace(ns);

        let index = match self.indices.get(&text_ns) {
            Some(idx) => idx,
            None => {
                return match engine.get(&text_ns, TEXT_INDEX_KEY)? {
                    Some(bytes) => {
                        let idx = InvertedIndex::deserialize(&bytes)?;
                        if idx.total_docs == 0 {
                            return Ok(vec![]);
                        }
                        Self::search_in_index(&idx, &query_tokens, options)
                    }
                    None => Ok(vec![]),
                };
            }
        };

        if index.total_docs == 0 {
            return Ok(vec![]);
        }

        Self::search_in_index(index, &query_tokens, options)
    }

    /// Search and return results with extracted snippets.
    pub fn search_with_snippets(
        &self,
        engine: &Engine,
        ns: &[u8],
        query: &str,
        k: usize,
        context_chars: usize,
    ) -> Result<Vec<SnippetResult>, EdgestoreError> {
        let text_ns = text_namespace(ns);
        let query_tokens = tokenize(query, self.language);
        if query_tokens.is_empty() || k == 0 {
            return Ok(vec![]);
        }
        let query_terms: std::collections::HashSet<String> =
            query_tokens.iter().map(|t| t.term.clone()).collect();

        let base_results = self.search(engine, ns, query, k)?;

        let index_opt: Option<InvertedIndex> = match self.indices.get(&text_ns) {
            Some(idx) => Some(idx.clone()),
            None => match engine.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(bytes) => InvertedIndex::deserialize(&bytes).ok(),
                None => None,
            },
        };

        let mut out = Vec::with_capacity(base_results.len());
        for result in base_results {
            let snippets = if let Some(ref index) = index_opt {
                let mut byte_positions: Vec<u32> = Vec::new();
                for (term, postings) in &index.postings {
                    if !query_terms.contains(term.as_str()) {
                        continue;
                    }
                    if let Some(posting) = postings.iter().find(|p| p.doc_id == result.doc_id) {
                        byte_positions.extend_from_slice(&posting.positions);
                    }
                }
                if !byte_positions.is_empty() {
                    match engine.get(&text_ns, &result.doc_id)? {
                        Some(raw) => match decode_text_record(&raw) {
                            Some(rec) => {
                                let chars: Vec<char> = rec.text.chars().collect();
                                byte_positions.sort_unstable();
                                byte_positions.dedup();
                                byte_positions
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
                                        let ctx: String = chars[ctx_start..ctx_end].iter().collect();
                                        let prefix: String =
                                            chars[ctx_start..char_start].iter().collect();
                                        let matched: String =
                                            chars[char_start..char_end].iter().collect();
                                        Some(Snippet {
                                            text: ctx,
                                            byte_start: prefix.len(),
                                            byte_end: prefix.len() + matched.len(),
                                        })
                                    })
                                    .collect()
                            }
                            None => vec![],
                        },
                        None => vec![],
                    }
                } else {
                    vec![]
                }
            } else {
                vec![]
            };
            out.push(SnippetResult {
                doc_id: result.doc_id,
                score: result.score,
                snippets,
            });
        }
        Ok(out)
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

    fn search_in_index(
        index: &InvertedIndex,
        query_tokens: &[crate::text::tokenizer::Token],
        options: &SearchOptions,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        let mut search_terms: Vec<String> =
            query_tokens.iter().map(|t| t.term.clone()).collect();

        if options.typo_tolerance {
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

        let mut doc_scores: HashMap<Vec<u8>, f32> = HashMap::new();
        let avg_doc_len = index.avg_doc_len();
        for term in &search_terms {
            if let Some(postings) = index.postings.get(term) {
                let doc_freq = postings.len() as u64;
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
                        posting.doc_len,
                        avg_doc_len,
                        crate::text::index::BM25_K1,
                        crate::text::index::BM25_B,
                    ) * weight;
                    *doc_scores.entry(posting.doc_id.clone()).or_insert(0.0) += score;
                }
            }
        }

        let mut results: Vec<TextSearchResult> = doc_scores
            .into_iter()
            .map(|(doc_id, score)| TextSearchResult { doc_id, score })
            .collect();
        results.sort_by(|a, b| {
            edgestore::total_cmp_f32(b.score, a.score).then(a.doc_id.cmp(&b.doc_id))
        });
        results.truncate(options.k);
        Ok(results)
    }

    fn search_in_index_with_stats(
        index: &InvertedIndex,
        query_tokens: &[crate::text::tokenizer::Token],
        k: usize,
    ) -> Result<(Vec<TextSearchResult>, TextSearchStats), EdgestoreError> {
        let search_terms: Vec<String> =
            query_tokens.iter().map(|t| t.term.clone()).collect();

        let mut doc_scores: HashMap<Vec<u8>, f32> = HashMap::new();
        let mut bytes_scanned: u64 = 0;
        let avg_doc_len = index.avg_doc_len();

        for term in &search_terms {
            if let Some(postings) = index.postings.get(term) {
                let doc_freq = postings.len() as u64;
                bytes_scanned += postings.len() as u64 * std::mem::size_of::<crate::text::index::Posting>() as u64;
                for posting in postings {
                    let score = bm25_score(
                        index.total_docs,
                        doc_freq,
                        posting.term_freq,
                        posting.doc_len,
                        avg_doc_len,
                        crate::text::index::BM25_K1,
                        crate::text::index::BM25_B,
                    );
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
        results.truncate(k);
        Ok((results, stats))
    }
}
