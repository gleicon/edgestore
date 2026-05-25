# Plan 07-02 Summary — Inverted Index Core

## What Was Delivered

- `Posting` struct: `doc_id: Vec<u8>`, `term_freq: u32`, `doc_len: u32`, `facets: HashMap<String, FacetValue>`
- `InvertedIndex` struct: `postings: HashMap<String, Vec<Posting>>`, `total_docs: u64`, `total_doc_len: u64`
- `add_document`: counts term frequencies, creates postings with facet metadata
- `bm25_score`: standard BM25 formula with configurable k1 and b parameters
- `score_document`: aggregates BM25 scores across multiple query tokens
- Binary serialization format: `INVX` magic header, version 1, term→postings mapping
- `serialize() -> Vec<u8>` and `deserialize() -> Result<InvertedIndex, EdgestoreError>`

## Files Changed

- `edgestore/src/text/index.rs` (new)

## Tests

- `test_index_add_document`: verify postings created with correct term frequencies
- `test_bm25_monotonic`: higher term_freq → higher score
- `test_serialize_roundtrip`: preserve all postings after serialize/deserialize
- `test_deserialize_invalid_magic`: rejects bad magic bytes
- `test_score_document`: positive score for matching terms, zero for non-matching

## Verification

- cargo build --workspace: ✅
- cargo clippy -D warnings: ✅
- cargo test --workspace: 421 tests pass
