//! EdgeStore — local-first embedded KV + vector database.
//!
//! This crate provides the core synchronous engine. Async wrappers live in
//! `edgestore-tokio`. HTTP transport lives in `edgestore-repl`.
//!
//! # Public API Surface
//!
//! | Type | Item | Description |
//! |------|------|-------------|
//! | Engine | [`Engine`] | Main KV engine (open, get, put, delete, range, prefix, flush, snapshot, compact) |
//! | Config | [`EdgestoreConfig`] | Database configuration |
//! | Error | [`EdgestoreError`] | All error variants |
//! | Transaction | [`Transaction`] | Multi-record atomic batch |
//! | Metrics | [`MetricsSnapshot`] | Point-in-time metrics snapshot |
//! | Vector | [`VectorEngine`] trait, [`VectorRecord`], [`Dtype`], [`Metric`] | Vector storage & search |
//! | Text | [`TextEngine`] trait, [`TextSearchResult`], [`SearchOptions`] | Full-text search |
//! | Snapshot | [`Snapshot`], [`SnapshotRegistry`] | Point-in-time reads |
//! | Replication | [`ReplicationProtocol`], [`HostId`], [`SegmentRef`] | Pull-only sync |
//! | Storage | [`StorageBackend`], [`DefaultStorageBackend`], [`MemoryStorageBackend`] | Pluggable I/O |
//! | Compaction | [`Compactor`], [`CompactionStats`] | Deathtime-cohort compaction |
//! | Merkle | [`RangeMerkleTree`] | Anti-entropy probe |
//! | Immutable | [`ImmutableEngine`], [`InMemorySegmentReader`] | Read-only in-memory engine for serverless / WASM |

#![warn(missing_docs)]

/// Deathtime-cohort compaction engine.
pub mod compactor;
/// Database configuration.
pub mod config;
/// Error types.
pub mod error;
/// FDP (Flexible Data Placement) storage backends.
pub mod fdp_backend;
/// Read-only in-memory engine for serverless / WASM environments (Phase 9).
pub mod immutable;
/// Range-level Merkle tree for anti-entropy probes.
pub mod merkle;
/// Engine metrics and snapshots.
pub mod metrics;
/// Remote segment store abstraction.
pub mod remote_store;
/// Pull-only replication protocol.
pub mod replication;
/// Point-in-time snapshot reads.
pub mod snapshot;
/// Pluggable storage backend trait.
pub mod storage_backend;
/// Full-text search index and engine.
pub mod text;
/// Core types (Lsn, SegmentMeta, Compression, etc.).
pub mod types;

/// Main KV engine.
pub mod engine;
/// Segment manifest (internal).
pub mod manifest;
/// In-memory table trait and BTree implementation.
pub mod memtable;
/// WAL replay recovery (internal).
pub mod recovery;
/// Segment format, reader, writer, and store (internal).
pub mod segment;
/// Multi-record transaction batch.
pub mod transaction;
/// Vector storage, distance, and search (internal modules with public re-exports).
pub mod vector;
/// Write-ahead log format and I/O (internal).
pub mod wal;

pub use compactor::{CompactionStats, Compactor};
pub use config::EdgestoreConfig;
pub use engine::{BudgetedScan, Engine, ImportResult, QueryStats, RangePage, ScanBudget};
pub use error::EdgestoreError;
pub use fdp_backend::{FdpStorageBackend, MockFdpBackend};
pub use immutable::ImmutableEngine;
pub use merkle::RangeMerkleTree;
pub use metrics::MetricsSnapshot;
pub use remote_store::RemoteStore;
pub use replication::{HostId, ReplicationProtocol, SegmentRef};
pub use segment::in_memory::InMemorySegmentReader;
pub use snapshot::{Snapshot, SnapshotRegistry};
pub use storage_backend::{
    DefaultStorageBackend, MemoryStorageBackend, PlacementHint, StorageBackend,
};
pub use text::engine::{
    text_namespace, SearchOptions, Snippet, SnippetResult, TextEngine, TextSearchResult,
};
pub use text::facet::{filter_by_facets, FacetFilter};
pub use text::index::{bm25_score, score_document, InvertedIndex, Posting};
pub use text::tokenizer::{tokenize, Token};
pub use text::types::{decode_text_record, FacetValue};
pub use text::typo::{is_one_edit_away, levenshtein};
pub use transaction::Transaction;
pub use vector::api::{vector_namespace, VectorEngine};
pub use vector::distance::{distance, distance_scalar, total_cmp_f32, Metric};
pub use vector::hnsw::HnswIndex;
pub use vector::search::{vector_page, vector_search, VectorPage, VectorSearchResult};
pub use vector::types::{Dtype, VectorRecord};
