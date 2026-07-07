//! `ReplicatedEngine` — convenience wrapper for the common primary + replica pattern.
//!
//! Eliminates the three-step boilerplate of wiring an `Engine` with
//! `HttpReplicationServer` and `AntiEntropyLoop`.
//!
//! ## Primary
//!
//! ```rust,no_run
//! use edgestore::EdgestoreConfig;
//! use edgestore_repl::ReplicatedEngine;
//!
//! let primary = ReplicatedEngine::open_primary(
//!     EdgestoreConfig::new("/var/db/primary"),
//!     "0.0.0.0:8900",
//! ).unwrap();
//!
//! primary.engine().lock().unwrap().put(b"products", b"p1", b"data").unwrap();
//! ```
//!
//! ## Replica
//!
//! ```rust,no_run
//! use edgestore::EdgestoreConfig;
//! use edgestore_repl::ReplicatedEngine;
//!
//! let replica = ReplicatedEngine::open_replica(
//!     EdgestoreConfig::new("/var/db/replica"),
//!     "http://primary-host:8900",
//! ).unwrap();
//!
//! // Engine is read-only — write attempts return Err(EdgestoreError::ReadOnly).
//! let val = replica.engine().lock().unwrap().get(b"products", b"p1").unwrap();
//! ```
//!
//! ## Wake-on-flush (reduced replication lag)
//!
//! Primary registers an `on_segment_flushed` callback via `Engine::with_on_segment_flushed`
//! before passing the engine to this wrapper. The callback wakes the anti-entropy loop
//! via a channel instead of waiting for the poll interval.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use edgestore::{EdgestoreConfig, EdgestoreError, Engine};

use crate::anti_entropy::AntiEntropyLoop;
use crate::http_server::HttpReplicationServer;

/// Wraps `Engine` + `HttpReplicationServer` (primary) or `Engine` + `AntiEntropyLoop` (replica).
///
/// Background threads are owned by this struct. When dropped, threads continue
/// running until the process exits — same behavior as calling `start()` manually.
pub struct ReplicatedEngine {
    engine: Arc<Mutex<Engine>>,
    // JoinHandles held to prevent accidental detach; threads are daemon-like (run until exit).
    #[allow(dead_code)]
    handles: Vec<std::thread::JoinHandle<()>>,
    bound_port: Option<u16>,
}

impl ReplicatedEngine {
    /// Open a primary replication node.
    ///
    /// Starts `HttpReplicationServer` on `bind_addr` in a background thread.
    /// The engine is writable. Other nodes point their `AntiEntropyLoop` at this address.
    pub fn open_primary(
        config: EdgestoreConfig,
        bind_addr: &str,
    ) -> Result<ReplicatedEngine, EdgestoreError> {
        let engine = Arc::new(Mutex::new(Engine::open(config)?));
        let server = HttpReplicationServer::new(Arc::clone(&engine));
        let (handle, port) = server.start(bind_addr)?;
        Ok(ReplicatedEngine {
            engine,
            handles: vec![handle],
            bound_port: Some(port),
        })
    }

    /// Open a replica node.
    ///
    /// Opens the engine in read-only mode (`Engine::open_readonly`) and starts an
    /// `AntiEntropyLoop` that pulls from `primary_url`. The engine rejects all write
    /// calls with `Err(EdgestoreError::ReadOnly)`.
    ///
    /// `primary_url` must be the base URL of the primary's `HttpReplicationServer`
    /// (e.g. `"http://host:8900"`).
    pub fn open_replica(
        config: EdgestoreConfig,
        primary_url: &str,
    ) -> Result<ReplicatedEngine, EdgestoreError> {
        let db_path: PathBuf = config.path.clone();
        let engine = Arc::new(Mutex::new(Engine::open_readonly(config)?));
        let peer_id = "primary".to_string();
        let loop_ = AntiEntropyLoop::new(
            Arc::clone(&engine),
            primary_url.to_string(),
            peer_id,
            db_path,
        );
        let handle = loop_.start();
        Ok(ReplicatedEngine {
            engine,
            handles: vec![handle],
            bound_port: None,
        })
    }

    /// Access the underlying `Arc<Mutex<Engine>>` for reads (replica) or reads+writes (primary).
    pub fn engine(&self) -> &Arc<Mutex<Engine>> {
        &self.engine
    }

    /// Port the `HttpReplicationServer` is listening on, or `None` for a replica.
    pub fn bound_port(&self) -> Option<u16> {
        self.bound_port
    }
}
