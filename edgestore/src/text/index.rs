use std::collections::HashMap;

use crate::error::EdgestoreError;
use crate::text::bloom::BloomFilter;
use crate::text::types::FacetValue;

/// A posting in the inverted index.
#[derive(Debug, Clone, PartialEq)]
pub struct Posting {
    /// Document key.
    pub doc_id: Vec<u8>,
    /// Term frequency in this document.
    pub term_freq: u32,
    /// Total token count in this document.
    pub doc_len: u32,
    /// Facet values attached to this document.
    pub facets: HashMap<String, FacetValue>,
}

/// In-memory inverted index for a namespace.
#[derive(Debug, Clone, Default)]
pub struct InvertedIndex {
    /// Map from term to postings list.
    pub postings: HashMap<String, Vec<Posting>>,
    /// Total number of documents in the index.
    pub total_docs: u64,
    /// Sum of all document lengths.
    pub total_doc_len: u64,
    /// LSN of the `flush()` / `persist_text_indices()` that wrote this sidecar.
    /// Used for staleness detection on crash recovery. 0 = unknown / treat as stale.
    pub sidecar_lsn: u64,
    /// Existence check to skip `remove_document`'s O(total index size) scan when a
    /// doc_id is definitely new (the common case for an append-only workload) — see
    /// `crate::text::bloom` for why. Deliberately not part of the on-disk format;
    /// rebuilt in `deserialize()` and whenever `bloom_insert()` detects saturation.
    pub doc_bloom: BloomFilter,
}

impl InvertedIndex {
    /// Create a new empty inverted index.
    pub fn new() -> Self {
        InvertedIndex {
            postings: HashMap::new(),
            total_docs: 0,
            total_doc_len: 0,
            sidecar_lsn: 0,
            doc_bloom: BloomFilter::new(),
        }
    }

    /// Rebuilds the (unpersisted) Bloom filter from the current `postings` at the
    /// given `capacity` — not a fixed capacity (see `text::bloom`'s doc comment on
    /// why a fixed capacity degrades silently once exceeded). Used right after
    /// deserializing from disk (the filter isn't part of the wire format) and by
    /// `bloom_insert()` whenever the filter has saturated. O(total postings), paid
    /// once per load/grow rather than once per document indexed — the same
    /// amortized-cost shape as `Vec`'s reallocation on push.
    fn rebuild_bloom_at_capacity(&mut self, capacity: usize) {
        self.doc_bloom =
            BloomFilter::with_capacity(capacity.max(crate::text::bloom::INITIAL_CAPACITY));
        for postings in self.postings.values() {
            for posting in postings {
                self.doc_bloom.insert(&posting.doc_id);
            }
        }
    }

    /// Records `doc_id` as present in the Bloom filter, growing it first if it has
    /// saturated — a saturated filter's false-positive rate climbs fast, silently
    /// reintroducing the O(n) `remove_document` scan for most "new" documents (see
    /// `text::bloom`'s doc). One call encapsulates the whole grow-then-insert policy
    /// so `add_document` doesn't need to know about it.
    fn bloom_insert(&mut self, doc_id: &[u8]) {
        if self.doc_bloom.is_saturated() {
            self.rebuild_bloom_at_capacity(self.doc_bloom.capacity() * 2);
        }
        self.doc_bloom.insert(doc_id);
    }

    /// Remove a document from the index.
    /// Returns true if the document was found and removed.
    pub fn remove_document(&mut self, doc_id: &[u8]) -> bool {
        let mut removed = false;
        let mut doc_len_removed: Option<u64> = None;

        self.postings.retain(|_term, postings| {
            postings.retain(|p| {
                if p.doc_id == doc_id {
                    removed = true;
                    if doc_len_removed.is_none() {
                        doc_len_removed = Some(p.doc_len as u64);
                    }
                    false
                } else {
                    true
                }
            });
            !postings.is_empty()
        });

        if let Some(len) = doc_len_removed {
            self.total_docs -= 1;
            self.total_doc_len -= len;
        }

        removed
    }

    /// Add a document to the index.
    pub fn add_document(
        &mut self,
        doc_id: Vec<u8>,
        tokens: &[crate::text::tokenizer::Token],
        doc_len: u32,
        facets: HashMap<String, FacetValue>,
    ) {
        self.bloom_insert(&doc_id);

        // Count term frequencies
        let mut term_counts: HashMap<String, u32> = HashMap::new();
        for token in tokens {
            *term_counts.entry(token.term.clone()).or_insert(0) += 1;
        }

        for (term, freq) in term_counts {
            let posting = Posting {
                doc_id: doc_id.clone(),
                term_freq: freq,
                doc_len,
                facets: facets.clone(),
            };
            self.postings.entry(term).or_default().push(posting);
        }

        self.total_docs += 1;
        self.total_doc_len += doc_len as u64;
    }

    /// Average document length (tokens). Returns 0.0 if no documents.
    pub fn avg_doc_len(&self) -> f32 {
        if self.total_docs == 0 {
            0.0
        } else {
            self.total_doc_len as f32 / self.total_docs as f32
        }
    }

    /// Serialize to bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"INVX");
        buf.extend_from_slice(&2u16.to_le_bytes()); // version 2: includes sidecar_lsn
        buf.extend_from_slice(&self.sidecar_lsn.to_le_bytes());
        buf.extend_from_slice(&self.total_docs.to_le_bytes());
        buf.extend_from_slice(&self.total_doc_len.to_le_bytes());
        buf.extend_from_slice(&(self.postings.len() as u32).to_le_bytes());

        for (term, postings) in &self.postings {
            let term_bytes = term.as_bytes();
            buf.extend_from_slice(&(term_bytes.len() as u16).to_le_bytes());
            buf.extend_from_slice(term_bytes);
            buf.extend_from_slice(&(postings.len() as u32).to_le_bytes());
            for p in postings {
                buf.extend_from_slice(&(p.doc_id.len() as u16).to_le_bytes());
                buf.extend_from_slice(&p.doc_id);
                buf.extend_from_slice(&p.term_freq.to_le_bytes());
                buf.extend_from_slice(&p.doc_len.to_le_bytes());
                buf.extend_from_slice(&(p.facets.len() as u16).to_le_bytes());
                for (k, v) in &p.facets {
                    let k_bytes = k.as_bytes();
                    buf.extend_from_slice(&(k_bytes.len() as u16).to_le_bytes());
                    buf.extend_from_slice(k_bytes);
                    match v {
                        FacetValue::String(s) => {
                            buf.push(0u8);
                            let s_bytes = s.as_bytes();
                            buf.extend_from_slice(&(s_bytes.len() as u16).to_le_bytes());
                            buf.extend_from_slice(s_bytes);
                        }
                        FacetValue::Number(n) => {
                            buf.push(1u8);
                            buf.extend_from_slice(&n.to_le_bytes());
                        }
                        FacetValue::Bool(b) => {
                            buf.push(2u8);
                            buf.push(if *b { 1u8 } else { 0u8 });
                        }
                    }
                }
            }
        }

        buf
    }

    /// Deserialize from bytes.
    pub fn deserialize(bytes: &[u8]) -> Result<Self, EdgestoreError> {
        if bytes.len() < 6 {
            return Err(EdgestoreError::CorruptData(
                "inverted index: truncated header".to_string(),
            ));
        }
        let mut pos = 0usize;

        macro_rules! read {
            ($n:expr) => {{
                if bytes.len() < pos + $n {
                    return Err(EdgestoreError::CorruptData(
                        "inverted index: truncated".to_string(),
                    ));
                }
                let slice = &bytes[pos..pos + $n];
                pos += $n;
                slice
            }};
        }

        let magic = read!(4);
        if magic != b"INVX" {
            return Err(EdgestoreError::CorruptData(
                "inverted index: invalid magic".to_string(),
            ));
        }
        let version = u16::from_le_bytes(read!(2).try_into().unwrap());
        let sidecar_lsn = match version {
            1 => 0, // v1 had no sidecar_lsn field
            2 => u64::from_le_bytes(read!(8).try_into().unwrap()),
            _ => {
                return Err(EdgestoreError::CorruptData(format!(
                    "inverted index: unsupported version {}",
                    version
                )))
            }
        };
        let total_docs = u64::from_le_bytes(read!(8).try_into().unwrap());
        let total_doc_len = u64::from_le_bytes(read!(8).try_into().unwrap());
        let term_count = u32::from_le_bytes(read!(4).try_into().unwrap()) as usize;

        let mut postings = HashMap::with_capacity(term_count);
        for _ in 0..term_count {
            let term_len = u16::from_le_bytes(read!(2).try_into().unwrap()) as usize;
            let term = String::from_utf8(read!(term_len).to_vec()).map_err(|_| {
                EdgestoreError::CorruptData("inverted index: invalid utf8 term".to_string())
            })?;
            let posting_count = u32::from_le_bytes(read!(4).try_into().unwrap()) as usize;
            let mut posting_vec = Vec::with_capacity(posting_count);
            for _ in 0..posting_count {
                let id_len = u16::from_le_bytes(read!(2).try_into().unwrap()) as usize;
                let doc_id = read!(id_len).to_vec();
                let term_freq = u32::from_le_bytes(read!(4).try_into().unwrap());
                let doc_len = u32::from_le_bytes(read!(4).try_into().unwrap());
                let facet_count = u16::from_le_bytes(read!(2).try_into().unwrap()) as usize;
                let mut facets = HashMap::with_capacity(facet_count);
                for _ in 0..facet_count {
                    let k_len = u16::from_le_bytes(read!(2).try_into().unwrap()) as usize;
                    let k = String::from_utf8(read!(k_len).to_vec()).map_err(|_| {
                        EdgestoreError::CorruptData("inverted index: invalid facet key".to_string())
                    })?;
                    let tag = read!(1)[0];
                    let v = match tag {
                        0 => {
                            let s_len = u16::from_le_bytes(read!(2).try_into().unwrap()) as usize;
                            let s = String::from_utf8(read!(s_len).to_vec()).map_err(|_| {
                                EdgestoreError::CorruptData(
                                    "inverted index: invalid facet string".to_string(),
                                )
                            })?;
                            FacetValue::String(s)
                        }
                        1 => {
                            let n = i64::from_le_bytes(read!(8).try_into().unwrap());
                            FacetValue::Number(n)
                        }
                        2 => {
                            let b = read!(1)[0] != 0;
                            FacetValue::Bool(b)
                        }
                        _ => {
                            return Err(EdgestoreError::CorruptData(
                                "inverted index: unknown facet tag".to_string(),
                            ))
                        }
                    };
                    facets.insert(k, v);
                }
                posting_vec.push(Posting {
                    doc_id,
                    term_freq,
                    doc_len,
                    facets,
                });
            }
            postings.insert(term, posting_vec);
        }

        let mut index = InvertedIndex {
            postings,
            total_docs,
            total_doc_len,
            sidecar_lsn,
            doc_bloom: BloomFilter::new(),
        };
        let capacity = total_docs as usize;
        index.rebuild_bloom_at_capacity(capacity);
        Ok(index)
    }
}

/// Default BM25 parameters used by EdgeStore.
pub const BM25_K1: f32 = 1.2;
/// BM25 length normalization parameter.
pub const BM25_B: f32 = 0.75;

/// Compute BM25 score for a single term-document pair.
///
/// k1 and b are the standard BM25 parameters.
pub fn bm25_score(
    total_docs: u64,
    doc_freq: u64,
    term_freq: u32,
    doc_len: u32,
    avg_doc_len: f32,
    k1: f32,
    b: f32,
) -> f32 {
    if total_docs == 0 || avg_doc_len == 0.0 {
        return 0.0;
    }

    let idf = (((total_docs as f32 - doc_freq as f32 + 0.5) / (doc_freq as f32 + 0.5)) + 1.0).ln();
    let tf_component = (term_freq as f32 * (k1 + 1.0))
        / (term_freq as f32 + k1 * (1.0 - b + b * (doc_len as f32 / avg_doc_len)));

    idf * tf_component
}

/// Score a document against query tokens using BM25.
pub fn score_document(
    index: &InvertedIndex,
    doc_id: &[u8],
    query_tokens: &[crate::text::tokenizer::Token],
) -> f32 {
    let avg_doc_len = index.avg_doc_len();
    let mut score = 0.0f32;

    for token in query_tokens {
        if let Some(postings) = index.postings.get(&token.term) {
            let doc_freq = postings.len() as u64;
            if let Some(posting) = postings.iter().find(|p| p.doc_id == doc_id) {
                score += bm25_score(
                    index.total_docs,
                    doc_freq,
                    posting.term_freq,
                    posting.doc_len,
                    avg_doc_len,
                    BM25_K1,
                    BM25_B,
                );
            }
        }
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::tokenizer::Token;

    #[test]
    fn test_index_add_document() {
        let mut index = InvertedIndex::new();
        let tokens = vec![
            Token {
                term: "hello".to_string(),
                position: 0,
            },
            Token {
                term: "world".to_string(),
                position: 1,
            },
            Token {
                term: "hello".to_string(),
                position: 2,
            },
        ];
        index.add_document(vec![1], &tokens, 3, HashMap::new());

        assert_eq!(index.total_docs, 1);
        let hello_postings = index.postings.get("hello").unwrap();
        assert_eq!(hello_postings.len(), 1);
        assert_eq!(hello_postings[0].term_freq, 2);
        assert_eq!(hello_postings[0].doc_len, 3);
    }

    #[test]
    fn test_bm25_monotonic() {
        let s1 = bm25_score(100, 10, 1, 100, 100.0, 1.2, 0.75);
        let s2 = bm25_score(100, 10, 5, 100, 100.0, 1.2, 0.75);
        assert!(s2 > s1, "higher term_freq should yield higher score");
    }

    #[test]
    fn test_serialize_roundtrip() {
        let mut index = InvertedIndex::new();
        let tokens = vec![
            Token {
                term: "hello".to_string(),
                position: 0,
            },
            Token {
                term: "world".to_string(),
                position: 1,
            },
        ];
        index.add_document(vec![1], &tokens, 2, HashMap::new());

        let bytes = index.serialize();
        let decoded = InvertedIndex::deserialize(&bytes).unwrap();

        assert_eq!(decoded.total_docs, index.total_docs);
        assert_eq!(decoded.total_doc_len, index.total_doc_len);
        assert_eq!(decoded.postings.len(), index.postings.len());
        assert_eq!(decoded.postings.get("hello").unwrap()[0].term_freq, 1);
    }

    #[test]
    fn test_deserialize_invalid_magic() {
        let result = InvertedIndex::deserialize(b"XXXX");
        assert!(result.is_err());
    }

    #[test]
    fn test_score_document() {
        let mut index = InvertedIndex::new();
        let tokens = vec![
            Token {
                term: "hello".to_string(),
                position: 0,
            },
            Token {
                term: "world".to_string(),
                position: 1,
            },
        ];
        index.add_document(vec![1], &tokens, 2, HashMap::new());

        let query = vec![Token {
            term: "hello".to_string(),
            position: 0,
        }];
        let score = score_document(&index, &[1], &query);
        assert!(score > 0.0);

        let query2 = vec![Token {
            term: "nonexistent".to_string(),
            position: 0,
        }];
        let score2 = score_document(&index, &[1], &query2);
        assert_eq!(score2, 0.0);
    }

    #[test]
    fn test_remove_document() {
        let mut index = InvertedIndex::new();
        let tokens1 = vec![
            Token {
                term: "hello".to_string(),
                position: 0,
            },
            Token {
                term: "world".to_string(),
                position: 1,
            },
        ];
        let tokens2 = vec![
            Token {
                term: "hello".to_string(),
                position: 0,
            },
            Token {
                term: "foo".to_string(),
                position: 1,
            },
        ];
        index.add_document(vec![1], &tokens1, 2, HashMap::new());
        index.add_document(vec![2], &tokens2, 2, HashMap::new());

        assert_eq!(index.total_docs, 2);
        assert_eq!(index.postings.get("hello").unwrap().len(), 2);
        assert_eq!(index.postings.get("world").unwrap().len(), 1);
        assert_eq!(index.postings.get("foo").unwrap().len(), 1);

        // Remove doc 1
        assert!(index.remove_document(&[1]));
        assert_eq!(index.total_docs, 1);
        assert_eq!(index.total_doc_len, 2);
        assert_eq!(index.postings.get("hello").unwrap().len(), 1);
        assert!(index.postings.get("world").is_none()); // term removed when empty
        assert_eq!(index.postings.get("foo").unwrap().len(), 1);

        // Removing same doc again returns false
        assert!(!index.remove_document(&[1]));

        // Remove doc 2, index should be empty
        assert!(index.remove_document(&[2]));
        assert_eq!(index.total_docs, 0);
        assert_eq!(index.total_doc_len, 0);
        assert!(index.postings.is_empty());
    }

    #[test]
    fn test_remove_document_updates_avg_len() {
        let mut index = InvertedIndex::new();
        index.add_document(
            vec![1],
            &[Token {
                term: "a".to_string(),
                position: 0,
            }],
            10,
            HashMap::new(),
        );
        index.add_document(
            vec![2],
            &[Token {
                term: "a".to_string(),
                position: 0,
            }],
            20,
            HashMap::new(),
        );
        assert_eq!(index.avg_doc_len(), 15.0);

        index.remove_document(&[1]);
        assert_eq!(index.avg_doc_len(), 20.0);
    }
}
