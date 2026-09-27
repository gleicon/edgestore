use std::collections::HashMap;
use edgestore::{EdgestoreConfig, Engine};
use edgestore_text::{FacetValue, SearchOptions, TextIndex};
use tempfile::TempDir;

fn open_engine(dir: &TempDir) -> Engine {
    Engine::open(EdgestoreConfig::new(dir.path())).unwrap()
}

#[test]
fn test_index_and_search_basic() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    text.index_document(&mut engine, b"ns", b"doc1", "The quick brown fox", HashMap::new()).unwrap();
    text.index_document(&mut engine, b"ns", b"doc2", "The lazy dog sleeps", HashMap::new()).unwrap();
    text.index_document(&mut engine, b"ns", b"doc3", "Quick brown fox jumps", HashMap::new()).unwrap();

    let results = text.search(&engine, b"ns", "quick brown", 3).unwrap();
    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.doc_id == b"doc1"));
    assert!(results.iter().any(|r| r.doc_id == b"doc3"));
}

#[test]
fn test_bm25_ranking() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    text.index_document(&mut engine, b"ns", b"doc1", "segment segment compaction", HashMap::new()).unwrap();
    text.index_document(&mut engine, b"ns", b"doc2", "segment compaction", HashMap::new()).unwrap();

    let results = text.search(&engine, b"ns", "segment", 2).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].doc_id, b"doc1", "doc with higher term freq should rank first");
    assert!(results[0].score > results[1].score);
}

#[test]
fn test_search_empty_namespace() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let text = TextIndex::new();

    let results = text.search(&engine, b"ns", "segment", 5).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_stopwords_only_query() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    text.index_document(&mut engine, b"ns", b"doc1", "segment compaction", HashMap::new()).unwrap();

    let results = text.search(&engine, b"ns", "", 5).unwrap();
    assert!(results.is_empty());

    let results2 = text.search(&engine, b"ns", "the a an", 5).unwrap();
    assert!(results2.is_empty(), "stopwords-only query should return empty");
}

#[test]
fn test_delete_removes_from_search() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    text.index_document(&mut engine, b"ns", b"doc1", "segment compaction", HashMap::new()).unwrap();
    let results_before = text.search(&engine, b"ns", "segment", 5).unwrap();
    assert_eq!(results_before.len(), 1);

    text.delete_document(&mut engine, b"ns", b"doc1").unwrap();
    let results_after = text.search(&engine, b"ns", "segment", 5).unwrap();
    assert!(results_after.is_empty(), "deleted doc should not appear in search");
}

#[test]
fn test_facet_filter() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    let mut facets1 = HashMap::new();
    facets1.insert("category".to_string(), FacetValue::String("news".to_string()));
    text.index_document(&mut engine, b"ns", b"doc1", "breaking news today", facets1).unwrap();

    let mut facets2 = HashMap::new();
    facets2.insert("category".to_string(), FacetValue::String("sports".to_string()));
    text.index_document(&mut engine, b"ns", b"doc2", "sports update", facets2).unwrap();

    let results = text.search(&engine, b"ns", "news", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].doc_id, b"doc1");
}

#[test]
fn test_search_ranking_stability() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    text.index_document(&mut engine, b"ns", b"doc1", "alpha beta gamma", HashMap::new()).unwrap();
    text.index_document(&mut engine, b"ns", b"doc2", "beta gamma delta", HashMap::new()).unwrap();

    let results1 = text.search(&engine, b"ns", "beta gamma", 5).unwrap();
    let results2 = text.search(&engine, b"ns", "beta gamma", 5).unwrap();

    assert_eq!(results1.len(), results2.len());
    for (a, b) in results1.iter().zip(results2.iter()) {
        assert_eq!(a.doc_id, b.doc_id);
        assert!((a.score - b.score).abs() < 1e-6);
    }
}

#[test]
fn test_reindex_updates_terms() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    text.index_document(&mut engine, b"ns", b"doc1", "segment compaction", HashMap::new()).unwrap();
    let results = text.search(&engine, b"ns", "segment", 5).unwrap();
    assert_eq!(results.len(), 1);

    text.index_document(&mut engine, b"ns", b"doc1", "foo bar", HashMap::new()).unwrap();
    let old = text.search(&engine, b"ns", "segment", 5).unwrap();
    assert!(old.is_empty(), "old terms should be gone after re-index");

    let new = text.search(&engine, b"ns", "foo", 5).unwrap();
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].doc_id, b"doc1");
}

#[test]
fn test_namespace_isolation() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    text.index_document(&mut engine, b"ns1", b"doc1", "segment compaction", HashMap::new()).unwrap();
    text.index_document(&mut engine, b"ns2", b"doc1", "foo bar", HashMap::new()).unwrap();

    let r1 = text.search(&engine, b"ns1", "segment", 5).unwrap();
    assert_eq!(r1.len(), 1);

    let r2 = text.search(&engine, b"ns2", "segment", 5).unwrap();
    assert!(r2.is_empty(), "ns2 should not see ns1 terms");

    let r3 = text.search(&engine, b"ns2", "foo", 5).unwrap();
    assert_eq!(r3.len(), 1);
}

#[test]
fn test_incremental_index_many_docs() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    for i in 0..100 {
        let content = format!("document number {} contains quick brown fox", i);
        let key = format!("doc{:04}", i);
        text.index_document(&mut engine, b"ns", key.as_bytes(), &content, HashMap::new()).unwrap();
    }

    let results = text.search(&engine, b"ns", "quick brown", 200).unwrap();
    assert_eq!(results.len(), 100);

    for i in (0..100).step_by(2) {
        let key = format!("doc{:04}", i);
        text.delete_document(&mut engine, b"ns", key.as_bytes()).unwrap();
    }

    let after = text.search(&engine, b"ns", "quick brown", 200).unwrap();
    assert_eq!(after.len(), 50);
}

#[test]
fn test_cold_cache_search_after_persist() {
    let dir = TempDir::new().unwrap();

    // Phase 1: index, persist, flush
    {
        let mut engine = open_engine(&dir);
        let mut text = TextIndex::new();
        text.index_document(&mut engine, b"ns", b"doc1", "segment compaction", HashMap::new()).unwrap();
        text.index_document(&mut engine, b"ns", b"doc2", "segment database", HashMap::new()).unwrap();
        text.persist(&mut engine).unwrap();
        engine.flush().unwrap();
    }

    // Phase 2: cold TextIndex reads sidecar from disk
    {
        let engine = open_engine(&dir);
        let text = TextIndex::new();
        let results = text.search(&engine, b"ns", "segment", 5).unwrap();
        assert_eq!(results.len(), 2, "cold search must find docs via disk sidecar");
    }
}

#[test]
fn test_delete_from_cold_cache() {
    let dir = TempDir::new().unwrap();

    {
        let mut engine = open_engine(&dir);
        let mut text = TextIndex::new();
        text.index_document(&mut engine, b"ns", b"doc1", "segment compaction", HashMap::new()).unwrap();
        text.persist(&mut engine).unwrap();
        engine.flush().unwrap();
    }

    {
        let mut engine = open_engine(&dir);
        let mut text = TextIndex::new();
        let before = text.search(&engine, b"ns", "segment", 5).unwrap();
        assert_eq!(before.len(), 1);

        text.delete_document(&mut engine, b"ns", b"doc1").unwrap();
        let after = text.search(&engine, b"ns", "segment", 5).unwrap();
        assert!(after.is_empty(), "delete from cold cache should work correctly");
    }
}

#[test]
fn test_typo_tolerance() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    text.index_document(&mut engine, b"ns", b"doc1", "segment compaction", HashMap::new()).unwrap();
    text.index_document(&mut engine, b"ns", b"doc2", "segmnt database", HashMap::new()).unwrap();

    let results = text.search_with_options(&engine, b"ns", "segment", &SearchOptions {
        k: 5,
        typo_tolerance: true,
        facet_filters: vec![],
    }).unwrap();
    assert!(results.iter().any(|r| r.doc_id == b"doc1"), "exact match should appear");
    assert!(results.iter().any(|r| r.doc_id == b"doc2"), "'segmnt' ~ 'segment' with typo tolerance");
}

#[test]
fn test_language_portuguese() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::with_language(edgestore_text::Language::PortugueseBrazilian);

    // "os" is a Portuguese stopword; "gatos" should be stemmed
    text.index_document(&mut engine, b"ns", b"doc1", "os gatos correm rapido", HashMap::new()).unwrap();
    let results = text.search(&engine, b"ns", "gatos", 5).unwrap();
    // Portuguese stemmer stems "gatos" → "gat"; query "gatos" also stems → "gat"
    assert!(!results.is_empty(), "Portuguese search should find stemmed terms");
}

#[test]
fn test_reindex_with_facets() {
    let dir = TempDir::new().unwrap();
    let mut engine = open_engine(&dir);
    let mut text = TextIndex::new();

    let mut f1 = HashMap::new();
    f1.insert("category".to_string(), FacetValue::String("news".to_string()));
    text.index_document(&mut engine, b"ns", b"doc1", "breaking news today", f1).unwrap();

    let mut f2 = HashMap::new();
    f2.insert("category".to_string(), FacetValue::String("sports".to_string()));
    text.index_document(&mut engine, b"ns", b"doc1", "sports update today", f2).unwrap();

    let r1 = text.search(&engine, b"ns", "breaking", 5).unwrap();
    assert!(r1.is_empty(), "old text should not match after re-index");

    let r2 = text.search(&engine, b"ns", "sports", 5).unwrap();
    assert_eq!(r2.len(), 1);
    assert_eq!(r2[0].doc_id, b"doc1");
}

#[test]
fn test_wal_reconstruction_without_sidecar() {
    // Simulates the post-crash scenario: raw text records are durable in the WAL
    // but persist() was never called, so no sidecar exists on cold start.
    let dir = TempDir::new().unwrap();

    // Phase 1: index docs, flush WAL, but do NOT call persist()
    {
        let mut engine = open_engine(&dir);
        let mut text = TextIndex::new();
        text.index_document(&mut engine, b"ns", b"doc1", "segment compaction storage", HashMap::new()).unwrap();
        text.index_document(&mut engine, b"ns", b"doc2", "database segment index", HashMap::new()).unwrap();
        text.index_document(&mut engine, b"ns", b"doc3", "replication backup", HashMap::new()).unwrap();
        // Flush WAL to segments so records survive engine reopen — but no sidecar written.
        engine.flush().unwrap();
    }

    // Phase 2: cold start — no in-memory index, no sidecar.
    // WAL reconstruction must find docs from raw records.
    {
        let engine = open_engine(&dir);
        let text = TextIndex::new();

        let results = text.search(&engine, b"ns", "segment", 5).unwrap();
        assert_eq!(results.len(), 2, "WAL reconstruction must find docs without sidecar");
        assert!(results.iter().any(|r| r.doc_id == b"doc1"));
        assert!(results.iter().any(|r| r.doc_id == b"doc2"));

        let no_match = text.search(&engine, b"ns", "replication", 5).unwrap();
        assert_eq!(no_match.len(), 1);
        assert_eq!(no_match[0].doc_id, b"doc3");
    }
}

#[test]
fn test_search_with_stats_wal_reconstruction() {
    // search_with_stats must also work via WAL reconstruction.
    let dir = TempDir::new().unwrap();

    {
        let mut engine = open_engine(&dir);
        let mut text = TextIndex::new();
        text.index_document(&mut engine, b"ns", b"doc1", "segment compaction", HashMap::new()).unwrap();
        text.index_document(&mut engine, b"ns", b"doc2", "segment database", HashMap::new()).unwrap();
        engine.flush().unwrap();
        // No persist() — no sidecar
    }

    {
        let engine = open_engine(&dir);
        let text = TextIndex::new();
        let (results, stats) = text.search_with_stats(&engine, b"ns", "segment", 5).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(stats.total_docs_indexed, 2);
        assert!(stats.docs_examined > 0);
    }
}
