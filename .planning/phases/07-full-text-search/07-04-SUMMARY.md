# Plan 07-04 Summary — Faceting + Typo Tolerance

## What Was Delivered

- `FacetFilter` struct: `field: String`, `value: FacetValue`
- `filter_by_facets(postings, filters) -> Vec<Posting>`: keeps only postings matching ALL filters
- `levenshtein(a, b) -> usize`: Wagner-Fischer DP algorithm for edit distance
- `is_one_edit_away(a, b) -> bool`: convenience for typo tolerance threshold
- Typo-tolerant search in `search_text_with_options`:
  - For each query term, scans all indexed terms for edit distance ≤ 1
  - Fuzzy matches get 0.5 weight penalty vs exact matches
  - Combined with exact-match scores for final ranking

## Files Changed

- `edgestore/src/text/facet.rs` (new)
- `edgestore/src/text/typo.rs` (new)
- `edgestore/src/engine.rs` (updated search_text_with_options)

## Tests

- `test_filter_by_facets_exact_match`: single facet filter narrows results
- `test_filter_by_facets_empty_filters`: no filters returns all postings
- `test_filter_by_facets_no_match`: non-matching filter returns empty
- `test_filter_multiple_facets`: multiple filters require ALL to match
- `test_levenshtein_identical`: distance to self = 0
- `test_levenshtein_one_substitution`: hello vs hallo = 1
- `test_levenshtein_one_insertion`: hello vs helllo = 1
- `test_levenshtein_one_deletion`: hello vs hell = 1
- `test_one_edit_away`: boolean wrapper works correctly

## Verification

- cargo build --workspace: ✅
- cargo clippy -D warnings: ✅
- cargo test --workspace: 421 tests pass
