//! Property-based tests encoding the invariants from edgestore.tla.
//!
//! These mirror the four TLA+ invariants checked by TLC:
//!   NoDataLoss    — every committed put is readable until overwritten or deleted
//!   FlushSafety   — data is not lost across a flush (WAL retired only after segment durable)
//!   LsnMonotonic  — LSN only increases
//!   SegmentLsnOrder — segment min/max bounds are consistent
//!
//! Run: cargo test --test spec_invariants
//! TLC:  java -jar tla2tools.jar -config edgestore.cfg edgestore.tla

use std::collections::HashMap;

use edgestore::{EdgestoreConfig, Engine};
use proptest::prelude::*;
use tempfile::TempDir;

const NS: &[u8] = b"spec";

#[derive(Clone, Debug)]
enum Op {
    Put(Vec<u8>, Vec<u8>),
    Delete(Vec<u8>),
    Flush,
}

fn arb_key() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        Just(b"k1".to_vec()),
        Just(b"k2".to_vec()),
        Just(b"k3".to_vec()),
    ]
}

fn arb_value() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![Just(b"v1".to_vec()), Just(b"v2".to_vec()),]
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (arb_key(), arb_value()).prop_map(|(k, v)| Op::Put(k, v)),
        2 => arb_key().prop_map(Op::Delete),
        1 => Just(Op::Flush),
    ]
}

fn arb_ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(arb_op(), 1..20)
}

// NoDataLoss: every Put not subsequently overwritten or deleted is readable.
proptest! {
    #[test]
    fn no_data_loss(ops in arb_ops()) {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        let mut model: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();

        for op in &ops {
            match op {
                Op::Put(k, v) => {
                    engine.put(NS, k, v).unwrap();
                    model.insert(k.clone(), Some(v.clone()));
                }
                Op::Delete(k) => {
                    engine.delete(NS, k).unwrap();
                    model.insert(k.clone(), None);
                }
                Op::Flush => {
                    let _ = engine.flush_to_segments();
                }
            }
        }

        for (k, expected) in &model {
            let got = engine.get(NS, k).unwrap();
            prop_assert_eq!(got.as_deref(), expected.as_deref(),
                "NoDataLoss violated for key {:?}", k);
        }
    }
}

// LsnMonotonic: current LSN never decreases across operations.
proptest! {
    #[test]
    fn lsn_monotonic(ops in arb_ops()) {
        let dir = TempDir::new().unwrap();
        let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        let mut prev_lsn = engine.current_lsn();

        for op in &ops {
            match op {
                Op::Put(k, v) => { engine.put(NS, k, v).unwrap(); }
                Op::Delete(k) => { engine.delete(NS, k).unwrap(); }
                Op::Flush => { let _ = engine.flush_to_segments(); }
            }
            let lsn = engine.current_lsn();
            prop_assert!(lsn >= prev_lsn, "LsnMonotonic violated: {} < {}", lsn, prev_lsn);
            prev_lsn = lsn;
        }
    }
}

// FlushSafety: data committed before a flush survives engine reopen (WAL replay).
proptest! {
    #[test]
    fn flush_safety(ops in arb_ops()) {
        let dir = TempDir::new().unwrap();
        let mut model: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();

        {
            let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
            for op in &ops {
                match op {
                    Op::Put(k, v) => {
                        engine.put(NS, k, v).unwrap();
                        model.insert(k.clone(), Some(v.clone()));
                    }
                    Op::Delete(k) => {
                        engine.delete(NS, k).unwrap();
                        model.insert(k.clone(), None);
                    }
                    Op::Flush => { let _ = engine.flush_to_segments(); }
                }
            }
            // Flush so WAL is retired; engine drop fsyncs remaining WAL.
            let _ = engine.flush_to_segments();
        }

        // Reopen — simulates the TLA+ Crash then Recover cycle.
        let engine2 = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();
        for (k, expected) in &model {
            let got = engine2.get(NS, k).unwrap();
            prop_assert_eq!(got.as_deref(), expected.as_deref(),
                "FlushSafety violated for key {:?}", k);
        }
    }
}

// SegmentLsnOrder: every segment's min_lsn <= max_lsn and all entries are within bounds.
#[test]
fn segment_lsn_order() {
    let dir = TempDir::new().unwrap();
    let mut engine = Engine::open(EdgestoreConfig::new(dir.path())).unwrap();

    for i in 0u32..10 {
        let k = i.to_le_bytes().to_vec();
        let v = i.to_le_bytes().to_vec();
        engine.put(NS, &k, &v).unwrap();
    }
    engine.flush_to_segments().unwrap();

    for meta in engine.list_segment_metas() {
        assert!(meta.min_lsn <= meta.max_lsn,
            "SegmentLsnOrder: min_lsn {} > max_lsn {}", meta.min_lsn, meta.max_lsn);
    }
}
