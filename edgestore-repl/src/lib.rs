//! `edgestore-repl` — HTTP transport layer and pull-only anti-entropy loop.
//!
//! Provides:
//! - `HttpReplicationClient` — implements `ReplicationProtocol` over HTTP + MessagePack
//! - `HttpReplicationServer` — serves pull-only endpoints with `?debug=json` support
//! - `AntiEntropyLoop`       — background thread for pull-only sync with per-peer cursor
//! - `S3RemoteStore` (with `s3` feature) — `RemoteStore` impl using AWS SDK for S3
//!
//! ## What this crate does NOT do
//!
//! `edgestore-repl` is a **transport** crate. It moves segments between nodes or to S3.
//! Cache eviction, tiering policy, and transparent read-through live in `edgestore-tier`.

pub mod anti_entropy;
pub(crate) mod wire;
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
