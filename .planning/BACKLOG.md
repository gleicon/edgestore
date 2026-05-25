# EdgeStore Backlog

Deferred features and future enhancements captured during development.

---

## Full-Text Search Enhancements (Phase 7 v2)

### FT-01: Pluggable Tokenizer Architecture

**Status:** Deferred from Phase 7 v1
**Rationale:** Hugging Face `tokenizers` crate adds ~100+ transitive dependencies. v1 uses lightweight English tokenizer to maintain minimal dependency footprint.

**Design:** Add a `Tokenizer` trait that the Engine accepts at initialization:
```rust
pub trait Tokenizer: Send + Sync {
    fn tokenize(&self, text: &str) -> Vec<Token>;
}
```
- Default: `EnglishTokenizer` (current simple implementation)
- Future: `HfTokenizer` wrapping `tokenizers` crate
- Config: accept `.json` tokenizer config path in `EdgestoreConfig`

---

### FT-02: LLM-Compatible Tokenization

**Status:** Deferred from Phase 7 v1
**Rationale:** Requires external `tokenizers` crate (see FT-01).

**Design:**
- Support BPE (GPT-2/3/4, Llama), WordPiece (BERT), Unigram (T5, XLNet)
- Load vocabulary from `.json` config file at Engine initialization
- Token IDs map to compact `u32` internal IDs for storage efficiency
- Enables exact same tokenization as the LLM used for embeddings

---

### FT-03: Bigram/Shingle Inverted Index

**Status:** Deferred from Phase 7 v1
**Rationale:** Adds vocabulary dictionary layer and more complex key encoding. v1 uses direct `term → postings` mapping.

**Design:**
1. **Vocabulary Index:** `vbf:<token_id_1>:<token_id_2>` → `shingle_id: u32`
   - Maps token bigrams to compact internal IDs
   - Saves space in main inverted index
2. **Inverted Index:** `inv:<shingle_id>:<doc_id>` → `term_freq: u32`
   - Uses native prefix scan for fast lookup
3. **Document Index:** `doc:<doc_id>` → `{length, raw_text, embedding}`

**Benefits:**
- Captures phrase-like context (2-gram co-occurrence)
- Better precision for multi-word queries
- Shared vocabulary across namespaces

---

### FT-04: RAG-Optimized Chunk-Level Indexing

**Status:** Deferred from Phase 7 v1
**Rationale:** Requires design decision on chunking strategy (fixed-size vs semantic). Can be implemented as a layer on top of existing text indexing.

**Design:**
- Treat each text chunk (100–500 tokens) as independent `doc_id`
- Store chunk text + embedding vector in same KV record
- BM25 scores measure keyword density within the exact LLM context window
- Combine with HNSW vector search for hybrid lexical+semantic retrieval

---

### FT-05: Multi-Language Tokenization

**Status:** Deferred indefinitely
**Rationale:** Internationalization is a long tail. English-first scope per Phase 7 plan.

**Design:**
- Unicode-aware tokenization (ICU or `unicode-segmentation`)
- Language-specific stemmers (Snowball stemmers)
- CJK bigram/character indexing

---

## Other Backlog Items

### BK-01: SQLite WAL Mode Comparison

**Status:** Not started
**Note:** Evaluate SQLite's WAL mode for comparison with EdgeStore's append-only WAL design.

### BK-02: LZ4 Compression for WAL

**Status:** Implemented in config but not benchmarked
**Note:** `compression_wal: Compression::Lz4` exists in EdgestoreConfig but baseline benchmarks use uncompressed WAL.

---

## Adding Items to This Backlog

When deferring a feature during phase execution:
1. Add entry with status "Deferred from Phase X"
2. Include rationale for deferral
3. Include design sketch for future implementation
4. Reference this file from the phase SUMMARY.md
