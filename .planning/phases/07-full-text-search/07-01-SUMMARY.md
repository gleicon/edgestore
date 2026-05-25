# Plan 07-01 Summary — Tokenizer + Text Types

## What Was Delivered

- `Token` struct: `term: String`, `position: usize`
- `tokenize(text: &str) -> Vec<Token>`: splits on non-alphanumeric, lowercases, removes stopwords, applies stemming
- Simple English stemmer: strips -ing, -ed, -ies→y, -s plurals, -ly suffixes
- Stopword list: ~100 common English words (the, a, an, is, are, was, were, be, been, being, have, has, had, do, does, did, will, would, could, should, may, might, must, shall, can, need, dare, ought, used, to, of, in, for, on, with, at, by, from, as, into, through, during, before, after, above, below, between, under, and, but, or, yet, so, if, because, although, though, while, where, when, that, which, who, whom, whose, what, this, these, those, such, no, nor, not, only, own, same, each, few, more, most, other, some, very, just, now, then, here, there, up, down, out, off, over, again, further, once)
- `TextRecord`: `text: String`, `facets: HashMap<String, FacetValue>`
- `FacetValue` enum: String, Number(i64), Bool
- `encode_text_record` / `decode_text_record`: binary serialization for KV storage

## Files Changed

- `edgestore/src/text/tokenizer.rs` (new)
- `edgestore/src/text/types.rs` (new)
- `edgestore/src/text/mod.rs` (module declarations)

## Tests

- `test_tokenize_basic`: "Hello world" → ["hello", "world"]
- `test_tokenize_punctuation`: "Hello, world!" → ["hello", "world"]
- `test_tokenize_stopwords`: "The quick brown fox" → ["quick", "brown", "fox"]
- `test_stem_ing`: running → run
- `test_stem_ed`: jumped → jump
- `test_stem_ies`: babies → baby
- `test_stem_s`: cats → cat
- `test_tokenize_empty`: "" → []
- `test_tokenize_positions`: position index increments correctly
- `test_text_record_roundtrip`: encode/decode with all facet types
- `test_text_record_empty_facets`: no facets round-trip
- `test_decode_too_short`: rejects truncated data

## Verification

- cargo build --workspace: ✅
- cargo clippy -D warnings: ✅
- cargo test --workspace: 421 tests pass
