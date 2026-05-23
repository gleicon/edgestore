//! `edgestore-repl` — HTTP transport layer and pull-only anti-entropy loop.
//!
//! Provides:
//! - `HttpReplicationClient` — implements `ReplicationProtocol` over HTTP + MessagePack (D07)
//! - `HttpReplicationServer` — serves 3 pull-only endpoints with `?debug=json` support (D07)
//! - `AntiEntropyLoop`       — background thread for pull-only sync with per-peer cursor (D08)

pub mod anti_entropy;
pub mod http_client;
pub mod http_server;

pub use anti_entropy::AntiEntropyLoop;
pub use http_client::HttpReplicationClient;
pub use http_server::HttpReplicationServer;
