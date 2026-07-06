/// Small hand-rolled Bloom filter — existence check to avoid O(n) index scans.
pub mod bloom;
/// Text engine trait and helpers.
pub mod engine;
/// Facet filtering.
pub mod facet;
/// Inverted index and BM25 scoring.
pub mod index;
/// Tokenization and stemming.
pub mod tokenizer;
/// Typo tolerance (Levenshtein distance).
pub mod typo;
/// Text record types and encoding.
pub mod types;

pub use engine::{text_namespace, TextEngine, TextSearchResult};
pub use facet::{filter_by_facets, FacetFilter};
pub use index::{bm25_score, score_document, InvertedIndex, Posting};
pub use tokenizer::{tokenize, Token};
pub use typo::{is_one_edit_away, levenshtein};
pub use types::{decode_text_record, encode_text_record, FacetValue, TextRecord};
