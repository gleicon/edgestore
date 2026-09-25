//! Pull-only anti-entropy loop with per-peer cursor persistence.
//!
//! `AntiEntropyLoop` wakes every N seconds, probes the peer's Merkle root, and if
//! diverged pulls all missing segments one by one. Progress is tracked in a per-peer
//! cursor file at `{db_path}/sync/{peer_id}.cursor` (MessagePack format).
//!
//! Cursor fields:
//!   - `last_known_merkle_root` — peer's Merkle root as of last successful sync
//!   - `segments_pending`       — hashes not yet applied (resume after crash)
//!   - `last_attempt_secs`      — unix timestamp of last probe attempt
//!   - `segments_applied_total` — running count of segments applied

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use edgestore::replication::ReplicationProtocol;
use edgestore::{Engine, ImportResult, RemoteStore};

use crate::http_client::HttpReplicationClient;
use crate::wire::hash_to_hex;

/// Per-peer cursor: durable progress state for the anti-entropy loop.
#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct PeerCursor {
    /// Peer's Merkle root from the last completed sync (32 bytes stored as Vec).
    pub last_known_merkle_root: Vec<u8>,
    /// Segment hashes that have been identified as missing but not yet applied.
    pub segments_pending: Vec<Vec<u8>>,
    /// Unix timestamp (seconds) of the last probe attempt.
    pub last_attempt_secs: u64,
    /// Total number of segments applied to date.
    pub segments_applied_total: u64,
    /// Last observed write token from the primary (BtrLog §4.2, arXiv:2606.27051).
    ///
    /// If the peer's reported `wtoken` ever exceeds this value the loop logs a
    /// warning — a monotonic jump signals a primary failover.
    pub last_known_wtoken: u64,
}

/// Background pull-only anti-entropy loop.
///
/// Spawned via `AntiEntropyLoop::start()`. Probes the configured peer every `interval_secs`
/// seconds. If the Merkle roots differ, pulls all missing segments and applies LWW merges
/// via `Engine::import_segment`. Per-peer cursor makes progress durable across crashes.
pub struct AntiEntropyLoop {
    engine: Arc<Mutex<Engine>>,
    peer_url: String,
    peer_id: String,
    db_path: PathBuf,
    /// Probe interval in seconds. Default: 30.
    pub interval_secs: u64,
    /// Optional durable segment backend. When `Some`, each successfully applied
    /// segment is uploaded after import. Upload failure is non-fatal — the
    /// segment is already applied locally.
    remote_store: Option<Arc<dyn RemoteStore>>,
}

impl AntiEntropyLoop {
    /// Create a new anti-entropy loop.
    ///
    /// - `engine`   — shared engine (`Arc<Mutex>`) for replication API access.
    /// - `peer_url` — base URL of the remote peer's `HttpReplicationServer` (e.g. `"http://host:8900"`).
    /// - `peer_id`  — unique identifier for the peer; used as the cursor file name.
    /// - `db_path`  — database directory path; cursor file is written under `{db_path}/sync/`.
    pub fn new(
        engine: Arc<Mutex<Engine>>,
        peer_url: String,
        peer_id: String,
        db_path: PathBuf,
    ) -> Self {
        AntiEntropyLoop {
            engine,
            peer_url,
            peer_id,
            db_path,
            interval_secs: 30,
            remote_store: None,
        }
    }

    /// Attach a `RemoteStore` backend. After each segment is successfully applied, the
    /// loop will call `remote_store.upload(hash, data)`. Upload failures are logged and
    /// ignored — they do not abort the sync loop.
    pub fn with_remote_store(mut self, store: Arc<dyn RemoteStore>) -> Self {
        self.remote_store = Some(store);
        self
    }

    /// Override the probe interval (default: 30 seconds).
    ///
    /// Useful in tests to reduce the time between anti-entropy cycles.
    pub fn with_interval(mut self, secs: u64) -> Self {
        self.interval_secs = secs;
        self
    }

    /// Spawn the anti-entropy loop in a background thread.
    ///
    /// Returns the `JoinHandle` for the background thread. The thread runs until the
    /// process exits.
    pub fn start(self) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(self.interval_secs));
            run_once(
                &self.engine,
                &self.peer_url,
                &self.peer_id,
                &self.db_path,
                self.remote_store.as_deref(),
            );
        })
    }
}

/// Execute one anti-entropy probe-and-pull cycle.
fn run_once(
    engine: &Arc<Mutex<Engine>>,
    peer_url: &str,
    peer_id: &str,
    db_path: &Path,
    remote_store: Option<&dyn RemoteStore>,
) {
    let cursor_path = cursor_file_path(db_path, peer_id);
    let mut cursor = load_cursor(&cursor_path);

    cursor.last_attempt_secs = now_secs();
    if let Err(e) = flush_cursor(&cursor, &cursor_path) {
        eprintln!("[anti_entropy] cursor flush error: {}", e);
    }

    let client = HttpReplicationClient::new(peer_url);

    // Fetch watermark to detect primary failovers (BtrLog §4.2, arXiv:2606.27051).
    // Non-fatal: peers without /watermark return InvalidOperation.
    if let Ok(wm) = client.watermark() {
        if wm.wtoken > cursor.last_known_wtoken {
            if cursor.last_known_wtoken > 0 {
                eprintln!(
                    "[anti_entropy] peer {} write token jumped {} → {} — primary failover detected",
                    peer_id, cursor.last_known_wtoken, wm.wtoken
                );
            }
            cursor.last_known_wtoken = wm.wtoken;
            if let Err(e) = flush_cursor(&cursor, &cursor_path) {
                eprintln!("[anti_entropy] cursor flush (wtoken) error: {}", e);
            }
        }
    }

    let peer_root = match client.merkle_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[anti_entropy] peer {} merkle_root error: {}", peer_id, e);
            return;
        }
    };

    let in_sync = {
        match engine.lock() {
            Ok(eng) => match eng.compare_merkle(&peer_root) {
                Ok(same) => same,
                Err(e) => {
                    eprintln!("[anti_entropy] compare_merkle error: {}", e);
                    return;
                }
            },
            Err(_) => {
                eprintln!("[anti_entropy] engine lock poisoned");
                return;
            }
        }
    };

    if in_sync {
        // Roots match — update cursor and skip expensive manifest diff.
        cursor.last_known_merkle_root = peer_root.to_vec();
        if let Err(e) = flush_cursor(&cursor, &cursor_path) {
            eprintln!("[anti_entropy] cursor flush (in-sync) error: {}", e);
        }
        return;
    }

    let peer_segments = match client.list_segments() {
        Ok(segs) => segs,
        Err(e) => {
            eprintln!("[anti_entropy] peer {} list_segments error: {}", peer_id, e);
            return;
        }
    };

    let missing: Vec<[u8; 32]> = {
        match engine.lock() {
            Ok(eng) => eng.missing_segments(&peer_segments),
            Err(_) => {
                eprintln!("[anti_entropy] engine lock poisoned (missing_segments)");
                return;
            }
        }
    };

    cursor.segments_pending = missing.iter().map(|h| h.to_vec()).collect();
    if let Err(e) = flush_cursor(&cursor, &cursor_path) {
        eprintln!("[anti_entropy] cursor flush (pending) error: {}", e);
    }

    let pending_hashes: Vec<Vec<u8>> = cursor.segments_pending.clone();
    for hash_vec in &pending_hashes {
        if hash_vec.len() != 32 {
            eprintln!(
                "[anti_entropy] skipping malformed hash (len={})",
                hash_vec.len()
            );
            continue;
        }

        let mut hash = [0u8; 32];
        hash.copy_from_slice(hash_vec);

        // Download segment from peer.
        let data = match client.fetch_segment(&hash) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[anti_entropy] fetch_segment error: {}", e);
                continue;
            }
        };

        // Import segment into local engine (includes BLAKE3 verification + LWW merge).
        let result = {
            match engine.lock() {
                Ok(mut eng) => eng.import_segment(&data, &hash),
                Err(_) => {
                    eprintln!("[anti_entropy] engine lock poisoned (import_segment)");
                    continue;
                }
            }
        };

        match result {
            Ok(ImportResult::Applied {
                keys_written,
                keys_skipped,
            }) => {
                // Remove from pending and update total.
                cursor.segments_pending.retain(|h| h != hash_vec);
                cursor.segments_applied_total += 1;
                if let Err(e) = flush_cursor(&cursor, &cursor_path) {
                    eprintln!("[anti_entropy] cursor flush (applied) error: {}", e);
                }
                eprintln!(
                    "[anti_entropy] applied segment {}: {} written, {} skipped",
                    hash_to_hex(&hash),
                    keys_written,
                    keys_skipped
                );

                // Uses upload_if_absent so re-archiving on retry never double-writes.
                if let Some(rs) = remote_store {
                    if let Err(e) = rs.upload_if_absent(&hash, &data) {
                        eprintln!(
                            "[anti_entropy] remote_store upload warning for {}: {}",
                            hash_to_hex(&hash),
                            e
                        );
                    }
                }
            }
            Ok(ImportResult::Skipped) => {
                // Already present — remove from pending.
                cursor.segments_pending.retain(|h| h != hash_vec);
                cursor.segments_applied_total += 1;
                if let Err(e) = flush_cursor(&cursor, &cursor_path) {
                    eprintln!("[anti_entropy] cursor flush (skipped) error: {}", e);
                }
            }
            Ok(ImportResult::HashMismatch) => {
                // Do NOT remove from pending — will retry next cycle.
                eprintln!(
                    "[anti_entropy] BLAKE3 mismatch for segment {} — will retry",
                    hash_to_hex(&hash)
                );
            }
            Err(e) => {
                eprintln!("[anti_entropy] import_segment error: {}", e);
                // Leave in pending — will retry next cycle.
            }
        }
    }

    cursor.last_known_merkle_root = peer_root.to_vec();
    if let Err(e) = flush_cursor(&cursor, &cursor_path) {
        eprintln!("[anti_entropy] cursor flush (final) error: {}", e);
    }
}

/// Compute the cursor file path for a given peer.
fn cursor_file_path(db_path: &Path, peer_id: &str) -> PathBuf {
    db_path.join("sync").join(format!("{}.cursor", peer_id))
}

/// Load cursor from disk, returning a default on parse failure or missing file.
///
/// Corrupt cursor is treated as empty to avoid blocking sync on bad state.
fn load_cursor(cursor_path: &Path) -> PeerCursor {
    match std::fs::File::open(cursor_path) {
        Ok(file) => rmp_serde::from_read(file).unwrap_or_default(),
        Err(_) => PeerCursor::default(),
    }
}

/// Flush cursor atomically: write to `.tmp`, then rename to final path.
///
/// Atomic write prevents corrupt cursor state on crash mid-write.
fn flush_cursor(cursor: &PeerCursor, cursor_path: &Path) -> Result<(), std::io::Error> {
    // Ensure the sync/ directory exists.
    if let Some(parent) = cursor_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp_path = cursor_path.with_extension("cursor.tmp");

    let bytes = rmp_serde::to_vec(cursor).map_err(|e| std::io::Error::other(e.to_string()))?;

    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, cursor_path)?;

    Ok(())
}

/// Return current Unix timestamp in seconds.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

