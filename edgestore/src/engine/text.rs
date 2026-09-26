use std::collections::HashMap;

use super::{Engine, QueryStats, TEXT_INDEX_KEY};
use crate::error::EdgestoreError;
use crate::types::Lsn;
use crate::text::engine::{text_namespace, TextEngine, TextSearchResult};
use crate::text::index::InvertedIndex;
use crate::text::types::{encode_text_record, FacetValue};
use crate::text::tokenizer::tokenize;

impl Engine {
    /// Text search with cost accounting. Returns results + [`QueryStats`].
    ///
    /// `bytes_scanned` reflects the size of the serialized inverted index examined.
    pub fn search_text_with_stats(
        &self,
        ns: &[u8],
        query: &str,
        k: usize,
    ) -> Result<(Vec<crate::text::engine::TextSearchResult>, QueryStats), EdgestoreError> {
        let text_ns = crate::text::engine::text_namespace(ns);
        let index_bytes_size = match self.text_indices.get(&text_ns) {
            Some(idx) => idx.serialize().len() as u64,
            None => match self.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(ref b) => b.len() as u64,
                None => 0,
            },
        };
        let results = self.search_text(ns, query, k)?;
        let stats = QueryStats {
            segments_scanned: if index_bytes_size > 0 { 1 } else { 0 },
            bytes_scanned: index_bytes_size,
            items_examined: results.len() as u64,
        };
        Ok((results, stats))
    }

    /// Text search that returns [`SnippetResult`]s — short context windows around
    /// each matched term — instead of just document keys and scores.
    ///
    /// Requires that the index was written with v3 format (`index_text` called after
    /// upgrading to this version). Documents indexed under v1/v2 format return an
    /// empty `snippets` vec but still appear in results with their BM25 score.
    ///
    /// `context_chars` controls how many characters appear before and after each
    /// match in the snippet. 80 is a reasonable default for agent-facing output.
    pub fn search_text_with_snippets(
        &self,
        ns: &[u8],
        query: &str,
        k: usize,
        context_chars: usize,
    ) -> Result<Vec<crate::text::engine::SnippetResult>, EdgestoreError> {
        use crate::text::types::decode_text_record;
        use crate::text::engine::{Snippet, SnippetResult};

        let text_ns = text_namespace(ns);
        let query_tokens = tokenize(query, self.config.text_language);
        if query_tokens.is_empty() || k == 0 {
            return Ok(vec![]);
        }
        let query_terms: std::collections::HashSet<String> =
            query_tokens.iter().map(|t| t.term.clone()).collect();

        let base_results = self.search_text(ns, query, k)?;

        let index_opt: Option<InvertedIndex> = match self.text_indices.get(&text_ns) {
            Some(idx) => Some(idx.clone()),
            None => match self.get(&text_ns, TEXT_INDEX_KEY)? {
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
                    match self.get(&text_ns, &result.doc_id)? {
                        Some(raw) => {
                            match decode_text_record(&raw) {
                                Some(rec) => {
                                    let text = &rec.text;
                                    let chars: Vec<char> = text.chars().collect();
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
                                            let ctx_start =
                                                char_start.saturating_sub(context_chars);
                                            let ctx_end =
                                                (char_end + context_chars).min(chars.len());
                                            let ctx: String =
                                                chars[ctx_start..ctx_end].iter().collect();
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
                            }
                        }
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
}

impl TextEngine for Engine {
    fn index_text(
        &mut self,
        ns: &[u8],
        key: &[u8],
        text: &str,
        facets: HashMap<String, FacetValue>,
    ) -> Result<Lsn, EdgestoreError> {
        let tokens = tokenize(text, self.config.text_language);
        let doc_len = tokens.len() as u32;
        let text_ns = text_namespace(ns);

        let loaded_index = match self.get(&text_ns, TEXT_INDEX_KEY) {
            Ok(Some(bytes)) => {
                InvertedIndex::deserialize(&bytes).unwrap_or_else(|_| InvertedIndex::new())
            }
            _ => InvertedIndex::new(),
        };

        let index = self
            .text_indices
            .entry(text_ns.clone())
            .or_insert(loaded_index);
        // Skip the O(total index size) removal scan when this doc_id is definitely
        // new (the common case for an append-only workload — see `text::bloom`).
        // Zero false negatives means this is always correct to skip on `false`.
        if index.doc_bloom.might_contain(key) {
            index.remove_document(key);
        }
        index.add_document(key.to_vec(), &tokens, doc_len, facets.clone());

        // Merged index is NOT written here — only on flush() / drop.
        // Raw text record goes through normal WAL → segment path (durable).
        let record = crate::text::types::TextRecord {
            text: text.to_string(),
            facets,
        };
        let record_bytes = encode_text_record(&record);
        self.put(&text_ns, key, &record_bytes)
    }

    fn search_text(
        &self,
        ns: &[u8],
        query: &str,
        k: usize,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        self.search_text_with_options(
            ns,
            query,
            &crate::text::engine::SearchOptions {
                k,
                ..Default::default()
            },
        )
    }

    fn search_text_with_options(
        &self,
        ns: &[u8],
        query: &str,
        options: &crate::text::engine::SearchOptions,
    ) -> Result<Vec<TextSearchResult>, EdgestoreError> {
        if options.k == 0 {
            return Ok(vec![]);
        }

        let query_tokens = tokenize(query, self.config.text_language);
        if query_tokens.is_empty() {
            return Ok(vec![]);
        }

        let text_ns = text_namespace(ns);

        let index = match self.text_indices.get(&text_ns) {
            Some(idx) => idx,
            None => match self.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(bytes) => {
                    let idx = InvertedIndex::deserialize(&bytes)?;
                    if idx.total_docs == 0 {
                        return Ok(vec![]);
                    }
                    return Self::search_in_index(&idx, &query_tokens, options);
                }
                None => return Ok(vec![]),
            },
        };

        if index.total_docs == 0 {
            return Ok(vec![]);
        }

        Self::search_in_index(index, &query_tokens, options)
    }

    fn delete_text(&mut self, ns: &[u8], key: &[u8]) -> Result<Lsn, EdgestoreError> {
        let text_ns = text_namespace(ns);

        let mut index = match self.text_indices.remove(&text_ns) {
            Some(idx) => idx,
            None => match self.get(&text_ns, TEXT_INDEX_KEY)? {
                Some(bytes) => {
                    InvertedIndex::deserialize(&bytes).unwrap_or_else(|_| InvertedIndex::new())
                }
                None => InvertedIndex::new(),
            },
        };

        index.remove_document(key);

        if index.total_docs == 0 {
            self.text_indices.remove(&text_ns);
            self.delete(&text_ns, TEXT_INDEX_KEY)?;
        } else {
            let index_bytes = index.serialize();
            self.put(&text_ns, TEXT_INDEX_KEY, &index_bytes)?;
            self.text_indices.insert(text_ns.clone(), index);
        }

        // Delete the raw text record (durable via WAL)
        self.delete(&text_ns, key)
    }
}
