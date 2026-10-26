use bbtree::unsync::BBTreeMap;

use crate::types::MemEntry;

/// In-memory ordered table trait. Implementations must be Send + Sync.
///
/// All read methods return owned values. This allows concurrent implementations
/// (e.g. `bbtree::BBTreeMap`) that cannot safely return borrowed references
/// because the underlying node memory may be reclaimed and reused.
pub trait MemTable: Send + Sync {
    /// Insert or overwrite an entry.
    ///
    /// # Panics
    /// Implementations backed by a fallible allocator (e.g. `BBTreeConcurrentMemTable`)
    /// panic on allocation failure. This is a known constraint of the current trait design.
    fn insert(&mut self, key: Vec<u8>, entry: MemEntry);
    /// Retrieve an entry by key (owned clone).
    fn get(&self, key: &[u8]) -> Option<MemEntry>;
    /// Scan entries in [start, end) — returns owned pairs.
    fn range(&self, start: &[u8], end: &[u8]) -> Vec<(Vec<u8>, MemEntry)>;
    /// Scan entries with the given prefix — returns owned pairs.
    fn prefix(&self, prefix: &[u8]) -> Vec<(Vec<u8>, MemEntry)>;
    /// Iterate all entries — returns owned pairs.
    fn iter(&self) -> Vec<(Vec<u8>, MemEntry)>;
    /// Number of entries.
    fn len(&self) -> usize;
    /// Clear all entries.
    fn clear(&mut self);
    /// True if the table is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Default memtable backed by `BTreeMap`.
pub struct BTreeMemTable {
    inner: std::collections::BTreeMap<Vec<u8>, MemEntry>,
}

impl BTreeMemTable {
    /// Create a new empty `BTreeMemTable`.
    pub fn new() -> Self {
        BTreeMemTable {
            inner: std::collections::BTreeMap::new(),
        }
    }
}

impl Default for BTreeMemTable {
    fn default() -> Self {
        Self::new()
    }
}

// Generates MemTable for any backing map that exposes the std BTreeMap interface:
// .insert(k, v), .get(k).cloned(), .range(bounds), .iter(), .len(), .clear().
// Used for BTreeMemTable and BBTreeMemTable (bbtree::unsync), which share identical bodies.
macro_rules! impl_simple_memtable {
    ($name:ident) => {
        impl MemTable for $name {
            fn insert(&mut self, key: Vec<u8>, entry: MemEntry) {
                self.inner.insert(key, entry);
            }
            fn get(&self, key: &[u8]) -> Option<MemEntry> {
                self.inner.get(key).cloned()
            }
            fn range(&self, start: &[u8], end: &[u8]) -> Vec<(Vec<u8>, MemEntry)> {
                use std::ops::Bound;
                self.inner
                    .range((Bound::Included(start.to_vec()), Bound::Excluded(end.to_vec())))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            }
            fn prefix(&self, prefix: &[u8]) -> Vec<(Vec<u8>, MemEntry)> {
                self.inner
                    .range(prefix.to_vec()..)
                    .take_while(|(k, _)| k.starts_with(prefix))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            }
            fn iter(&self) -> Vec<(Vec<u8>, MemEntry)> {
                self.inner.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            }
            fn len(&self) -> usize {
                self.inner.len()
            }
            fn clear(&mut self) {
                self.inner.clear();
            }
        }
    };
}

impl_simple_memtable!(BTreeMemTable);

/// Memtable backed by `bbtree::unsync::BBTreeMap`.
///
/// Drop-in replacement for `BTreeMemTable` with ~5× faster point lookups,
/// lower memory usage, and SIMD-accelerated node search. Uses the same
/// `&mut self` ownership model as std's `BTreeMap` — no API changes required.
pub struct BBTreeMemTable {
    inner: BBTreeMap<Vec<u8>, MemEntry>,
}

impl BBTreeMemTable {
    /// Create a new empty `BBTreeMemTable`.
    pub fn new() -> Self {
        BBTreeMemTable {
            inner: BBTreeMap::new(),
        }
    }
}

impl Default for BBTreeMemTable {
    fn default() -> Self {
        Self::new()
    }
}

impl_simple_memtable!(BBTreeMemTable);

/// Memtable backed by `bbtree::BBTreeMap` — the concurrent B-tree variant.
///
/// Multiple readers can access this table simultaneously without cache coherence
/// traffic because the internal version lock never writes to shared cache lines
/// during reads. Writes still require exclusive `&mut self` access (for WAL/LSN
/// ordering), but concurrent reads in `AsyncEngine::run_read` benefit
/// significantly at high thread counts.
///
/// `get`, `range`, `prefix`, and `iter` all return owned values — the version
/// lock may reclaim and reuse freed nodes, so it is unsound to return `&V`.
pub struct BBTreeConcurrentMemTable {
    inner: bbtree::BBTreeMap<Vec<u8>, MemEntry>,
    count: usize,
}

impl BBTreeConcurrentMemTable {
    /// Create a new empty `BBTreeConcurrentMemTable`.
    pub fn new() -> Self {
        BBTreeConcurrentMemTable {
            inner: bbtree::BBTreeMap::new(),
            count: 0,
        }
    }
}

impl Default for BBTreeConcurrentMemTable {
    fn default() -> Self {
        Self::new()
    }
}

impl MemTable for BBTreeConcurrentMemTable {
    /// Panics on allocator exhaustion — OOM kills the process.
    fn insert(&mut self, key: Vec<u8>, entry: MemEntry) {
        match self.inner.insert(key, entry) {
            Ok(None) => self.count += 1,
            Ok(Some(_)) => {}
            Err(_) => panic!("BBTreeConcurrentMemTable: allocation failed"),
        }
    }

    fn get(&self, key: &[u8]) -> Option<MemEntry> {
        self.inner.get_cloned(key)
    }

    fn range(&self, start: &[u8], end: &[u8]) -> Vec<(Vec<u8>, MemEntry)> {
        use std::ops::Bound;
        let mut results = Vec::new();
        self.inner.scan::<[u8], ()>(
            (Bound::Included(start), Bound::Excluded(end)),
            |k, v| {
                results.push((k.clone(), v.clone()));
                std::ops::ControlFlow::Continue(())
            },
        );
        results
    }

    fn prefix(&self, prefix: &[u8]) -> Vec<(Vec<u8>, MemEntry)> {
        use std::ops::Bound;
        let mut results = Vec::new();
        self.inner.scan::<[u8], ()>(
            (Bound::Included(prefix), Bound::Unbounded),
            |k, v| {
                if !k.starts_with(prefix) {
                    return std::ops::ControlFlow::Break(());
                }
                results.push((k.clone(), v.clone()));
                std::ops::ControlFlow::Continue(())
            },
        );
        results
    }

    fn iter(&self) -> Vec<(Vec<u8>, MemEntry)> {
        use std::ops::Bound;
        let mut results = Vec::new();
        self.inner.scan::<[u8], ()>(
            (Bound::Unbounded, Bound::Unbounded),
            |k, v| {
                results.push((k.clone(), v.clone()));
                std::ops::ControlFlow::Continue(())
            },
        );
        results
    }

    fn len(&self) -> usize {
        self.count
    }

    fn clear(&mut self) {
        self.inner = bbtree::BBTreeMap::new();
        self.count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{encode_key, Operation};

    fn make_entry(lsn: u64, value: &[u8]) -> MemEntry {
        MemEntry {
            key: vec![],
            value: Some(value.to_vec()),
            op: Operation::Put,
            lsn,
            timestamp: 0,
            ttl: 0,
        }
    }

    fn assert_object_safe(_: Box<dyn MemTable>) {}

    #[test]
    fn test_object_safe() {
        let m: Box<dyn MemTable> = Box::new(BTreeMemTable::new());
        assert_object_safe(m);
    }

    #[test]
    fn test_insert_and_get() {
        let mut m = BTreeMemTable::new();
        let key = encode_key(b"ns", b"k1");
        let entry = make_entry(1, b"val1");
        m.insert(key.clone(), entry);
        assert!(m.get(&key).is_some());
        assert_eq!(m.get(&key).unwrap().value, Some(b"val1".to_vec()));
    }

    #[test]
    fn test_overwrite_lsn() {
        let mut m = BTreeMemTable::new();
        let key = encode_key(b"ns", b"k1");
        m.insert(key.clone(), make_entry(1, b"v1"));
        m.insert(key.clone(), make_entry(2, b"v2"));
        let entry = m.get(&key).unwrap();
        assert_eq!(entry.lsn, 2);
        assert_eq!(entry.value, Some(b"v2".to_vec()));
    }

    #[test]
    fn test_len() {
        let mut m = BTreeMemTable::new();
        for i in 0..10u8 {
            m.insert(encode_key(b"ns", &[i]), make_entry(i as u64, &[i]));
        }
        assert_eq!(m.len(), 10);
    }

    #[test]
    fn test_namespace_isolation() {
        let mut m = BTreeMemTable::new();
        // Insert keys in ns_a and ns_b with same raw key
        m.insert(encode_key(b"ns_a", b"key1"), make_entry(1, b"va1"));
        m.insert(encode_key(b"ns_a", b"key2"), make_entry(2, b"va2"));
        m.insert(encode_key(b"ns_b", b"key1"), make_entry(3, b"vb1"));

        // prefix for ns_a: encode_key(b"ns_a", b"") = {0x00, 0x04, n, s, _, a}
        let ns_a_prefix = encode_key(b"ns_a", b"");
        let ns_a_results = m.prefix(&ns_a_prefix);
        assert_eq!(ns_a_results.len(), 2);
        for (_, entry) in &ns_a_results {
            assert!(
                entry.value != Some(b"vb1".to_vec()),
                "ns_b key leaked into ns_a prefix scan"
            );
        }

        let ns_b_prefix = encode_key(b"ns_b", b"");
        let ns_b_results = m.prefix(&ns_b_prefix);
        assert_eq!(ns_b_results.len(), 1);
    }

    #[test]
    fn test_range_isolation() {
        let mut m = BTreeMemTable::new();
        m.insert(encode_key(b"a", b"k1"), make_entry(1, b"va1"));
        m.insert(encode_key(b"a", b"k2"), make_entry(2, b"va2"));
        m.insert(encode_key(b"b", b"k1"), make_entry(3, b"vb1"));

        // Range bounded to namespace "a"
        let start = encode_key(b"a", b"");
        let end = encode_key(b"b", b"");
        let results = m.range(&start, &end);
        // Should return only ns_a keys
        for (_, entry) in &results {
            assert!(
                entry.value != Some(b"vb1".to_vec()),
                "ns_b key appeared in ns_a range"
            );
        }
    }
}
