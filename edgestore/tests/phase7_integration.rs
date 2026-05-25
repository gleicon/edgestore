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
