//! `edgestore-repl` — HTTP transport layer and pull-only anti-entropy loop.
//!
//! Provides:
//! - `HttpReplicationClient` — implements `ReplicationProtocol` over HTTP + MessagePack (D07)
//! - `HttpReplicationServer` — serves 3 pull-only endpoints with `?debug=json` support (D07)
//! - `AntiEntropyLoop`       — background thread for pull-only sync with per-peer cursor (D08)
//! - `S3RemoteStore` (with `s3` feature) — `RemoteStore` impl using AWS SDK for S3
//!
//! ## What this crate does NOT do
//!
//! `edgestore-repl` is a **transport** crate. It moves segments between nodes or to S3.
//! It does **not** implement:
//! - Cache eviction / tiering policy (which segments stay local vs. go to S3)
//! - Transparent read-through (automatic `get()` fallback to S3 on miss)
//! - Prefetch or cache warming
//!
//! Those are application-level concerns. You can build them yourself using the
//! `RemoteStore` primitives exported here, or wait for a future `edgestore-tier` crate.
//! See ARCHITECTURE.md in the repo root for the full pattern.

pub mod anti_entropy;
pub mod filesystem_remote_store;
pub mod http_client;
pub mod http_server;
pub mod replicated_engine;

#[cfg(feature = "s3")]
pub mod s3_remote_store;

pub use anti_entropy::{AntiEntropyLoop, PeerCursor};
pub use filesystem_remote_store::FilesystemRemoteStore;
pub use http_client::HttpReplicationClient;
pub use http_server::HttpReplicationServer;
pub use replicated_engine::ReplicatedEngine;

#[cfg(feature = "s3")]
pub use s3_remote_store::S3RemoteStore;
