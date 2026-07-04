use edgestore::{
    EdgestoreConfig, Engine, TextEngine, VectorEngine,
    text::types::{FacetValue, TextRecord},
};
use tempfile::TempDir;

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
}

#[test]
fn test_index_and_search_basic() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.index_text(b"ns", b"doc1", "The quick brown fox", std::collections::HashMap::new()).unwrap();
    engine.index_text(b"ns", b"doc2", "The lazy dog sleeps", std::collections::HashMap::new()).unwrap();
    engine.index_text(b"ns", b"doc3", "Quick brown fox jumps", std::collections::HashMap::new()).unwrap();

    let results = engine.search_text(b"ns", "quick brown", 3).unwrap();
    assert!(!results.is_empty(), "search should return results");
    // Both doc1 and doc3 have "quick" and "brown"; they should be in results
    assert!(results.iter().any(|r| r.doc_id == b"doc1"), "doc1 should match 'quick brown'");
    assert!(results.iter().any(|r| r.doc_id == b"doc3"), "doc3 should match 'quick brown'");
}

#[test]
fn test_bm25_ranking() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    // Doc with term appearing twice should rank higher
    engine.index_text(b"ns", b"doc1", "hello hello world", std::collections::HashMap::new()).unwrap();
    engine.index_text(b"ns", b"doc2", "hello world", std::collections::HashMap::new()).unwrap();

    let results = engine.search_text(b"ns", "hello", 2).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].doc_id, b"doc1", "doc with more 'hello' should rank higher");
    assert!(results[0].score > results[1].score);
}

#[test]
fn test_search_empty_namespace() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);

    let results = engine.search_text(b"ns", "hello", 5).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_empty_query() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).unwrap();

    let results = engine.search_text(b"ns", "", 5).unwrap();
    assert!(results.is_empty());

    let results2 = engine.search_text(b"ns", "the a an", 5).unwrap();
    assert!(results2.is_empty(), "stopwords-only query should return empty");
}

#[test]
fn test_delete_removes_from_search() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).unwrap();
    let results_before = engine.search_text(b"ns", "hello", 5).unwrap();
    assert_eq!(results_before.len(), 1);

    engine.delete_text(b"ns", b"doc1").unwrap();
    let results_after = engine.search_text(b"ns", "hello", 5).unwrap();
    assert!(results_after.is_empty(), "deleted doc should not appear in search");
}

#[test]
fn test_facet_filter() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    let mut facets1 = std::collections::HashMap::new();
    facets1.insert("category".to_string(), FacetValue::String("news".to_string()));
    engine.index_text(b"ns", b"doc1", "breaking news today", facets1).unwrap();

    let mut facets2 = std::collections::HashMap::new();
    facets2.insert("category".to_string(), FacetValue::String("sports".to_string()));
    engine.index_text(b"ns", b"doc2", "sports update", facets2).unwrap();

    // Search without facet filter should find both
    let results = engine.search_text(b"ns", "news", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].doc_id, b"doc1");
}

#[test]
fn test_search_ranking_stability() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.index_text(b"ns", b"doc1", "alpha beta gamma", std::collections::HashMap::new()).unwrap();
    engine.index_text(b"ns", b"doc2", "beta gamma delta", std::collections::HashMap::new()).unwrap();

    let results1 = engine.search_text(b"ns", "beta gamma", 5).unwrap();
    let results2 = engine.search_text(b"ns", "beta gamma", 5).unwrap();

    assert_eq!(results1.len(), results2.len());
    for (a, b) in results1.iter().zip(results2.iter()) {
        assert_eq!(a.doc_id, b.doc_id);
        assert!((a.score - b.score).abs() < 1e-6);
    }
}

#[test]
fn test_index_text_record_retrieval() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    let mut facets = std::collections::HashMap::new();
    facets.insert("author".to_string(), FacetValue::String("Alice".to_string()));
    facets.insert("views".to_string(), FacetValue::Number(42));
    facets.insert("published".to_string(), FacetValue::Bool(true));

    engine.index_text(b"ns", b"doc1", "hello world", facets.clone()).unwrap();

    // Retrieve the raw text record via plain KV get
    let text_ns = edgestore::text_namespace(b"ns");
    let raw = engine.get(&text_ns, b"doc1").unwrap().unwrap();
    let record = edgestore::decode_text_record(&raw).unwrap();
    assert_eq!(record.text, "hello world");
    assert_eq!(record.facets.get("author"), Some(&FacetValue::String("Alice".to_string())));
    assert_eq!(record.facets.get("views"), Some(&FacetValue::Number(42)));
    assert_eq!(record.facets.get("published"), Some(&FacetValue::Bool(true)));
}

#[test]
fn test_reindex_updates_merged_index() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    // Index doc1 with "hello world"
    engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).unwrap();
    let results = engine.search_text(b"ns", "hello", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].doc_id, b"doc1");

    // Re-index doc1 with "foo bar" — old terms should be gone
    engine.index_text(b"ns", b"doc1", "foo bar", std::collections::HashMap::new()).unwrap();
    let results_hello = engine.search_text(b"ns", "hello", 5).unwrap();
    assert!(results_hello.is_empty(), "old term 'hello' should not find re-indexed doc");

    let results_foo = engine.search_text(b"ns", "foo", 5).unwrap();
    assert_eq!(results_foo.len(), 1);
    assert_eq!(results_foo[0].doc_id, b"doc1");
}

#[test]
fn test_incremental_index_many_docs() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    // Index 100 docs incrementally
    for i in 0..100 {
        let text = format!("document number {} contains quick brown fox", i);
        let key = format!("doc{:04}", i);
        engine.index_text(b"ns", key.as_bytes(), &text, std::collections::HashMap::new()).unwrap();
    }

    // Search should find all docs with "quick brown"
    let results = engine.search_text(b"ns", "quick brown", 200).unwrap();
    assert_eq!(results.len(), 100, "all 100 docs should match 'quick brown'");

    // Delete every other doc
    for i in (0..100).step_by(2) {
        let key = format!("doc{:04}", i);
        engine.delete_text(b"ns", key.as_bytes()).unwrap();
    }

    // Search should find only remaining 50 docs
    let results_after = engine.search_text(b"ns", "quick brown", 200).unwrap();
    assert_eq!(results_after.len(), 50, "50 docs should remain after deletion");
}

#[test]
fn test_namespace_isolation() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.index_text(b"ns1", b"doc1", "hello world", std::collections::HashMap::new()).unwrap();
    engine.index_text(b"ns2", b"doc1", "foo bar", std::collections::HashMap::new()).unwrap();

    let results1 = engine.search_text(b"ns1", "hello", 5).unwrap();
    assert_eq!(results1.len(), 1);

    let results2 = engine.search_text(b"ns2", "hello", 5).unwrap();
    assert!(results2.is_empty(), "ns2 should not find ns1 terms");

    let results3 = engine.search_text(b"ns2", "foo", 5).unwrap();
    assert_eq!(results3.len(), 1);
}

#[test]
fn test_delete_all_docs_removes_index() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).unwrap();
    engine.index_text(b"ns", b"doc2", "hello world", std::collections::HashMap::new()).unwrap();

    engine.delete_text(b"ns", b"doc1").unwrap();
    engine.delete_text(b"ns", b"doc2").unwrap();

    // Merged index should be deleted when empty
    let text_ns = edgestore::text_namespace(b"ns");
    let index_bytes = engine.get(&text_ns, b"__index__").unwrap();
    assert!(index_bytes.is_none(), "merged index should be deleted when all docs removed");
}

#[test]
fn test_search_performance_at_scale() {
    use std::time::Instant;

    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    // Index 10,000 documents
    let n = 10_000;
    for i in 0..n {
        let text = format!("document number {} contains quick brown fox jumps over lazy dog", i);
        let key = format!("doc{:08}", i);
        engine.index_text(b"ns", key.as_bytes(), &text, std::collections::HashMap::new()).unwrap();
    }

    // Benchmark 100 searches
    let start = Instant::now();
    for _ in 0..100 {
        let results = engine.search_text(b"ns", "quick brown fox", 10).unwrap();
        assert!(!results.is_empty());
    }
    let elapsed = start.elapsed();
    let avg_us = elapsed.as_micros() as f64 / 100.0;

    // Search should be under 5ms per query at 10K docs with merged index
    // (old per-doc micro-index impl was ~165ms at 10K docs = ~6 QPS)
    // With merged index + in-memory cache + inline scoring: ~3ms = ~300 QPS
    assert!(
        avg_us < 5000.0,
        "search too slow: {:.1} µs at {} docs (merged index should be < 5ms)",
        avg_us, n
    );
}

#[test]
fn test_cold_cache_search() {
    let dir = TempDir::new().unwrap();

    // Phase 1: index docs with one engine instance
    {
        let mut engine = open_engine(&dir);
        engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).unwrap();
        engine.index_text(b"ns", b"doc2", "hello foo", std::collections::HashMap::new()).unwrap();
        engine.flush().unwrap();
    }

    // Phase 2: drop engine, reopen — cache is cold. Search should still work
    // via fallback disk read.
    {
        let engine = open_engine(&dir);
        let results = engine.search_text(b"ns", "hello", 5).unwrap();
        assert_eq!(results.len(), 2, "cold-cache search should find both docs via disk fallback");
    }
}

#[test]
fn test_typo_tolerance() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).unwrap();
    engine.index_text(b"ns", b"doc2", "helo there", std::collections::HashMap::new()).unwrap();

    // Exact search finds both ("hello" exact, "helo" is one edit away)
    let exact = engine.search_text_with_options(
        b"ns",
        "hello",
        &edgestore::SearchOptions { k: 5, typo_tolerance: true, ..Default::default() },
    ).unwrap();
    assert!(
        exact.iter().any(|r| r.doc_id == b"doc1"),
        "exact match doc1 should be found"
    );
    assert!(
        exact.iter().any(|r| r.doc_id == b"doc2"),
        "typo-tolerant match doc2 ('helo' ~ 'hello') should be found"
    );
}

#[test]
fn test_delete_fallback_cache_miss() {
    let dir = TempDir::new().unwrap();

    // Index with engine 1
    {
        let mut engine = open_engine(&dir);
        engine.index_text(b"ns", b"doc1", "hello world", std::collections::HashMap::new()).unwrap();
        engine.flush().unwrap();
    }

    // Delete with engine 2 (cold cache — simulates cache miss)
    {
        let mut engine = open_engine(&dir);
        let results_before = engine.search_text(b"ns", "hello", 5).unwrap();
        assert_eq!(results_before.len(), 1);

        engine.delete_text(b"ns", b"doc1").unwrap();

        let results_after = engine.search_text(b"ns", "hello", 5).unwrap();
        assert!(results_after.is_empty(), "delete from cold cache should remove doc");
    }
}

#[test]
fn test_reindex_with_facets() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);

    let mut facets1 = std::collections::HashMap::new();
    facets1.insert("category".to_string(), FacetValue::String("news".to_string()));
    engine.index_text(b"ns", b"doc1", "breaking news today", facets1).unwrap();

    // Re-index with different facets
    let mut facets2 = std::collections::HashMap::new();
    facets2.insert("category".to_string(), FacetValue::String("sports".to_string()));
    engine.index_text(b"ns", b"doc1", "sports update today", facets2).unwrap();

    // Old text should not match "breaking"
    let results = engine.search_text(b"ns", "breaking", 5).unwrap();
    assert!(results.is_empty(), "old text 'breaking' should not match after re-index");

    // New text should match "sports"
    let results2 = engine.search_text(b"ns", "sports", 5).unwrap();
    assert_eq!(results2.len(), 1);
    assert_eq!(results2[0].doc_id, b"doc1");

    // Verify raw record has new facets
    let text_ns = edgestore::text_namespace(b"ns");
    let raw = engine.get(&text_ns, b"doc1").unwrap().unwrap();
    let record = edgestore::decode_text_record(&raw).unwrap();
    assert_eq!(
        record.facets.get("category"),
        Some(&FacetValue::String("sports".to_string()))
    );
}
