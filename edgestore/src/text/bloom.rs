//! A small, hand-rolled Bloom filter — no new crate, consistent with this codebase's
//! existing preference for hand-rolling simple approximate structures (see
//! Space-Saving top-K in the Pierre-side rollup module for the same philosophy).
//!
//! `InvertedIndex::add_document` needs to know whether a doc_id was already indexed
//! before deciding to remove its old postings first; a full scan of every posting in
//! every term to answer that is O(total index size) per call. This filter answers
//! "definitely not present" in O(1) with zero false negatives — safe to skip the
//! expensive scan for genuinely-new documents, never skipped for a real re-index or
//! delete (a false positive here only costs an unnecessary scan, never incorrectness).
//!
//! Capacity is not fixed: a filter sized for its actual population would silently
//! degrade (false-positive rate climbing toward saturation) once usage exceeds it.
//! `InvertedIndex` doubles capacity — rebuilding from `postings`, itself the source
//! of truth for which doc_ids exist — whenever this filter saturates. Amortized O(1)
//! per insert over the filter's lifetime, the same analysis that makes `Vec::push`
//! amortized O(1) despite occasional reallocation.
//!
//! Deliberately not persisted as part of `InvertedIndex::serialize()` — a pure
//! in-memory optimization aid, rebuilt in O(total docs) once whenever an index is
//! loaded or grown, far cheaper than paying an O(n) scan on every document insert.
//! Keeping it out of the wire format also means no on-disk compatibility concerns.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash, Hasher};

/// Target false-positive rate used to size a new filter for a given capacity.
/// Independent from `EdgestoreConfig::xor_filter_fpr` (also 0.01 by default) — that
/// setting tunes a different, user-configurable per-segment xor filter; this constant
/// is an internal, non-configurable detail of this Bloom filter. Same value by
/// coincidence (1% is a conventional default for both filter kinds), not by a shared
/// source of truth.
const TARGET_FPR: f64 = 0.01;

/// Starting capacity for a freshly-created index's filter — small, since most
/// `InvertedIndex` instances (one per BM25 time-bucket, bounded by Pierre's FR-11
/// bucket-duration config) never grow large; it doubles from here as needed.
pub const INITIAL_CAPACITY: usize = 4096;

/// Standard Kirsch–Mitzenmacher double hashing: derive `num_hashes` independent
/// probe positions from two 32-bit halves of one 64-bit hash, avoiding the need for
/// `num_hashes` separate hash computations per operation.
///
/// Hashed with a per-instance random seed (`RandomState`, the same mechanism every
/// `HashMap` in this codebase already gets by default), not a fixed one — a fixed
/// seed would let a caller who controls `doc_id`/key values (true for edgestore as a
/// general-purpose library, even though Pierre's own key generation already
/// includes a random suffix) craft keys that collide into the same bit positions,
/// inflating the false-positive rate past its design target and forcing
/// `remove_document`'s expensive scan far more often than intended.
#[derive(Clone)]
pub struct BloomFilter {
    bits: Vec<u64>,
    num_bits: usize,
    num_hashes: u32,
    /// The capacity this filter was sized for — once `count` reaches this, the
    /// false-positive rate starts climbing well past `TARGET_FPR`; the caller
    /// (`InvertedIndex`) should rebuild at a larger capacity.
    capacity: usize,
    count: usize,
    seed: RandomState,
}

impl std::fmt::Debug for BloomFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BloomFilter")
            .field("num_bits", &self.num_bits)
            .field("num_hashes", &self.num_hashes)
            .field("capacity", &self.capacity)
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl Default for BloomFilter {
    fn default() -> Self {
        Self::with_capacity(INITIAL_CAPACITY)
    }
}

impl BloomFilter {
    /// Creates an empty filter sized for `capacity` items at `TARGET_FPR`, with a
    /// fresh random hash seed.
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        // m = -(n ln p) / (ln 2)^2 ; k = (m/n) ln 2 — standard optimal sizing.
        let m =
            (-(capacity as f64) * TARGET_FPR.ln() / std::f64::consts::LN_2.powi(2)).ceil() as usize;
        let num_words = m.div_ceil(64).max(1);
        let num_bits = num_words * 64;
        let num_hashes = (((num_bits as f64 / capacity as f64) * std::f64::consts::LN_2).round()
            as u32)
            .clamp(1, 30);
        BloomFilter {
            bits: vec![0u64; num_words],
            num_bits,
            num_hashes,
            capacity,
            count: 0,
            seed: RandomState::new(),
        }
    }

    /// Creates an empty filter with the default starting capacity.
    pub fn new() -> Self {
        Self::default()
    }

    /// The capacity this filter was sized for — pass `capacity() * 2` (or more) to
    /// `with_capacity` when rebuilding after `is_saturated()`.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Whether this filter has reached (or passed) its designed capacity — false
    /// positives climb rapidly past this point. The caller should rebuild larger.
    pub fn is_saturated(&self) -> bool {
        self.count >= self.capacity
    }

    fn hash_pair(&self, item: &[u8]) -> (u64, u64) {
        #[allow(clippy::manual_hash_one)]
        let combined = {
            let mut h1 = self.seed.build_hasher();
            item.hash(&mut h1);
            h1.finish()
        };
        // Split into two independent-enough 32-bit halves for double hashing.
        (combined >> 32, combined & 0xFFFF_FFFF)
    }

    /// Takes `h1`/`h2`/`num_bits`/`num_hashes` explicitly (not `&self`) so `insert`
    /// can compute positions from copied-out values while holding `&mut self.bits` —
    /// avoids an allocation that collecting a `&self`-borrowing iterator into a `Vec`
    /// would need. The `hash_pair` call that produces `h1`/`h2` still needs `&self`
    /// (for the seed), but that borrow ends as soon as it returns the owned values.
    fn positions(
        h1: u64,
        h2: u64,
        num_bits: usize,
        num_hashes: u32,
    ) -> impl Iterator<Item = usize> {
        (0..num_hashes)
            .map(move |i| (h1.wrapping_add((i as u64).wrapping_mul(h2)) as usize) % num_bits)
    }

    /// Records `item` as present.
    pub fn insert(&mut self, item: &[u8]) {
        let (h1, h2) = self.hash_pair(item);
        for pos in Self::positions(h1, h2, self.num_bits, self.num_hashes) {
            self.bits[pos / 64] |= 1 << (pos % 64);
        }
        self.count += 1;
    }

    /// `false` means the item is **definitely not present** — safe to skip an
    /// expensive existence-dependent operation. `true` means it *might* be present
    /// (could be a false positive) — must fall back to a real check.
    pub fn might_contain(&self, item: &[u8]) -> bool {
        let (h1, h2) = self.hash_pair(item);
        Self::positions(h1, h2, self.num_bits, self.num_hashes)
            .all(|pos| self.bits[pos / 64] & (1 << (pos % 64)) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_false_negative_for_inserted_items() {
        let mut filter = BloomFilter::with_capacity(10_000);
        let items: Vec<Vec<u8>> = (0..10_000)
            .map(|i| format!("doc-{i}").into_bytes())
            .collect();
        for item in &items {
            filter.insert(item);
        }
        for item in &items {
            assert!(
                filter.might_contain(item),
                "inserted item must never be reported as absent"
            );
        }
    }

    #[test]
    fn false_positive_rate_is_low_at_designed_capacity() {
        let mut filter = BloomFilter::with_capacity(200_000);
        let inserted: Vec<Vec<u8>> = (0..200_000)
            .map(|i| format!("doc-{i}").into_bytes())
            .collect();
        for item in &inserted {
            filter.insert(item);
        }

        let mut false_positives = 0;
        let probes = 10_000;
        for i in 200_000..200_000 + probes {
            let candidate = format!("doc-{i}").into_bytes();
            if filter.might_contain(&candidate) {
                false_positives += 1;
            }
        }
        let fpr = false_positives as f64 / probes as f64;
        assert!(
            fpr < 0.05,
            "false positive rate {fpr} should be well under 5% at designed capacity (target ~1%)"
        );
    }

    #[test]
    fn false_positive_rate_stays_low_even_far_past_a_small_capacity_if_rebuilt_larger() {
        // Simulates what InvertedIndex does: detect saturation, rebuild bigger.
        let mut capacity = 1_000;
        let mut filter = BloomFilter::with_capacity(capacity);
        let mut inserted: Vec<Vec<u8>> = Vec::new();
        for i in 0..50_000usize {
            if filter.is_saturated() {
                capacity *= 2;
                let mut bigger = BloomFilter::with_capacity(capacity);
                for item in &inserted {
                    bigger.insert(item);
                }
                filter = bigger;
            }
            let item = format!("doc-{i}").into_bytes();
            filter.insert(&item);
            inserted.push(item);
        }

        for item in &inserted {
            assert!(filter.might_contain(item));
        }

        let mut false_positives = 0;
        let probes = 10_000;
        for i in 50_000..50_000 + probes {
            if filter.might_contain(format!("doc-{i}").into_bytes().as_slice()) {
                false_positives += 1;
            }
        }
        let fpr = false_positives as f64 / probes as f64;
        assert!(fpr < 0.05, "false positive rate {fpr} should stay low after growing past the original small capacity");
    }

    #[test]
    fn empty_filter_reports_nothing_present() {
        let filter = BloomFilter::new();
        assert!(!filter.might_contain(b"anything"));
    }

    #[test]
    fn is_saturated_flips_once_capacity_is_reached() {
        let mut filter = BloomFilter::with_capacity(10);
        assert!(!filter.is_saturated());
        for i in 0..10 {
            filter.insert(format!("doc-{i}").as_bytes());
        }
        assert!(filter.is_saturated());
    }
}
