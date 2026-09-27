//! Property-based tests for the TextSearchConsistency invariant from edgestore.tla.
//!
//! Mirrors the four-invariant pattern in edgestore/tests/spec_invariants.rs but
//! covers the text-layer invariant that TLC does not model yet:
//!
//!   TextSearchConsistency — every document indexed via `index_document` is
//!   findable via `search`, even after a simulated crash before `persist()`.
//!
//! This cross-verifies the modelled behaviour (D38 in DECISIONS.md) against the
//! real implementation. The three actions that matter:
//!   IndexDoc  — writes raw text record to WAL (durable immediately)
//!   Persist   — writes inverted-index sidecar (optimisation, not required)
//!   Crash     — engine drop + reopen with a fresh TextIndex (no in-memory state)
//!
//! TLA+ companion: edgestore.tla `TextSearchConsistency` scope note.
//! Run: cargo test -p edgestore-text --test spec_invariants

use std::collections::HashMap;
use std::path::PathBuf;

use edgestore::{EdgestoreConfig, Engine};
use edgestore_text::TextIndex;
use proptest::prelude::*;
use tempfile::TempDir;

const NS_A: &[u8] = b"spec_a";
const NS_B: &[u8] = b"spec_b";

#[derive(Clone, Debug)]
enum TextOp {
    IndexDoc(Vec<u8>, Vec<u8>, String),
    Persist,
    Flush,
    Crash,
}

fn arb_ns() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![Just(NS_A.to_vec()), Just(NS_B.to_vec())]
}

fn arb_key() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(b"doc1".to_vec()),
        Just(b"doc2".to_vec()),
        Just(b"doc3".to_vec()),
    ]
}

fn arb_content() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("segment compaction storage engine".to_string()),
        Just("database index replication backup".to_string()),
        Just("search query ranking results".to_string()),
    ]
}

fn arb_text_op() -> impl Strategy<Value = TextOp> {
    prop_oneof![
        5 => (arb_ns(), arb_key(), arb_content()).prop_map(|(ns, k, c)| TextOp::IndexDoc(ns, k, c)),
        2 => Just(TextOp::Persist),
        2 => Just(TextOp::Flush),
        1 => Just(TextOp::Crash),
    ]
}

fn arb_text_ops() -> impl Strategy<Value = Vec<TextOp>> {
    prop::collection::vec(arb_text_op(), 2..15)
}

fn open(path: &PathBuf) -> Engine {
    Engine::open(EdgestoreConfig::new(path)).unwrap()
}

// TextSearchConsistency: every indexed document is findable via search after
// any sequence of index/persist/crash operations.
//
// TLA+ mapping:
//   IndexDoc  ↔ engine.put(__text__{ns}, doc_key, text_bytes) — raw record durable
//   Persist   ↔ engine.put(__text__{ns}, __index__, sidecar) — derived, optional
//   Crash     ↔ Restart action: WAL replayed on reopen, memtable rebuilt
//   search    ↔ TextSearchConsistency invariant check
proptest! {
    #[test]
    fn text_search_consistency(ops in arb_text_ops()) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        let mut engine = open(&path);
        let mut text = TextIndex::new();

        // model: (ns, key) -> content for all docs indexed so far
        let mut model: HashMap<(Vec<u8>, Vec<u8>), String> = HashMap::new();

        for op in &ops {
            match op {
                TextOp::IndexDoc(ns, key, content) => {
                    text.index_document(&mut engine, ns, key, content, HashMap::new()).unwrap();
                    model.insert((ns.clone(), key.clone()), content.clone());
                }
                TextOp::Persist => {
                    text.persist(&mut engine).unwrap();
                }
                TextOp::Flush => {
                    let _ = engine.flush();
                }
                TextOp::Crash => {
                    // Flush WAL so raw records survive reopen (models Engine::drop fsync).
                    // Do NOT call persist() — the sidecar may or may not exist.
                    let _ = engine.flush();
                    drop(engine);
                    engine = open(&path);
                    text = TextIndex::new(); // fresh: no in-memory cache, no sidecar guaranteed
                }
            }
        }

        // Invariant: every doc in the model must be findable via search.
        for ((ns, key), content) in &model {
            // Extract a non-trivial term (len > 4, skips stopwords / short tokens).
            let term = match content.split_whitespace().find(|w| w.len() > 4) {
                Some(t) => t,
                None => continue,
            };

            let results = text.search(&engine, ns, term, 100).unwrap();
            prop_assert!(
                results.iter().any(|r| &r.doc_id == key),
                "TextSearchConsistency violated: doc {:?} in ns {:?} not found for term {:?}",
                String::from_utf8_lossy(key),
                String::from_utf8_lossy(ns),
                term,
            );
        }
    }
}

// TextSearchConsistency with stats: search_with_stats also satisfies the invariant
// and reports total_docs_indexed >= the number of docs the model knows about.
proptest! {
    #[test]
    fn text_search_consistency_with_stats(ops in arb_text_ops()) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        let mut engine = open(&path);
        let mut text = TextIndex::new();
        let mut model: HashMap<(Vec<u8>, Vec<u8>), String> = HashMap::new();

        for op in &ops {
            match op {
                TextOp::IndexDoc(ns, key, content) => {
                    text.index_document(&mut engine, ns, key, content, HashMap::new()).unwrap();
                    model.insert((ns.clone(), key.clone()), content.clone());
                }
                TextOp::Persist => { text.persist(&mut engine).unwrap(); }
                TextOp::Flush => { let _ = engine.flush(); }
                TextOp::Crash => {
                    let _ = engine.flush();
                    drop(engine);
                    engine = open(&path);
                    text = TextIndex::new();
                }
            }
        }

        for ((ns, key), content) in &model {
            let term = match content.split_whitespace().find(|w| w.len() > 4) {
                Some(t) => t,
                None => continue,
            };

            let (results, stats) = text.search_with_stats(&engine, ns, term, 100).unwrap();

            // total_docs_indexed must be at least as many as the model for this namespace.
            let ns_doc_count = model.keys().filter(|(n, _)| n == ns).count() as u64;
            prop_assert!(
                stats.total_docs_indexed >= ns_doc_count,
                "stats.total_docs_indexed ({}) < model doc count ({}) for ns {:?}",
                stats.total_docs_indexed, ns_doc_count, String::from_utf8_lossy(ns),
            );

            prop_assert!(
                results.iter().any(|r| &r.doc_id == key),
                "TextSearchConsistency (stats path) violated: doc {:?} in ns {:?} not found for term {:?}",
                String::from_utf8_lossy(key),
                String::from_utf8_lossy(ns),
                term,
            );
        }
    }
}
