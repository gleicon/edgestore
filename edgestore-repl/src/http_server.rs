//! HTTP replication server exposing 3 pull-only endpoints.
//!
//! Endpoints:
//!   GET /merkle              → MessagePack `{root: Vec<u8>}`
//!   GET /segments            → MessagePack `[{segment_id, segment_hash}]`
//!   GET /segments/{hash_hex} → raw segment bytes (application/octet-stream)
//!
//! All endpoints support `?debug=json` to re-serialize as JSON for human inspection (D07).
//!
//! Engine access is serialized via `Arc<Mutex<Engine>>` — correct for single-writer (D08).

use std::io::Read;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use edgestore::Engine;
use edgestore::EdgestoreError;

/// MessagePack wire struct for GET /merkle response.
#[derive(Serialize, Deserialize)]
struct MerkleResponse {
    root: Vec<u8>,
}

/// MessagePack wire struct for one item in GET /segments response.
#[derive(Serialize, Deserialize)]
struct SegmentEntry {
    segment_id: u64,
    segment_hash: Vec<u8>,
}

/// HTTP replication server wrapping an `Arc<Mutex<Engine>>`.
///
/// Call `start(bind_addr)` to spawn the server loop in a background thread.
pub struct HttpReplicationServer {
    engine: Arc<Mutex<Engine>>,
}

impl HttpReplicationServer {
    /// Create a new server wrapping the provided engine.
    pub fn new(engine: Arc<Mutex<Engine>>) -> Self {
        HttpReplicationServer { engine }
    }

    /// Bind to `bind_addr` and start the request loop in a background thread.
    ///
    /// Returns the `JoinHandle` for the background thread. The server runs until the
    /// process exits (the thread loops forever).
    pub fn start(
        &self,
        bind_addr: &str,
    ) -> Result<std::thread::JoinHandle<()>, EdgestoreError> {
        let server = tiny_http::Server::http(bind_addr)
            .map_err(|e| EdgestoreError::ReplicationError(format!("bind error: {}", e)))?;

        let engine = Arc::clone(&self.engine);

        let handle = std::thread::spawn(move || {
            for request in server.incoming_requests() {
                handle_request(request, &engine);
            }
        });

        Ok(handle)
    }
}

/// Parse path and query string from a request URL.
///
/// Returns `(path_parts, debug_json)` where `path_parts` is the non-empty path
/// components and `debug_json` is `true` when `?debug=json` is present.
fn parse_url(url: &str) -> (Vec<&str>, bool) {
    let (path_part, query_part) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };

    let debug_json = query_part.split('&').any(|kv| kv == "debug=json");

    let parts: Vec<&str> = path_part
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    (parts, debug_json)
}

/// Respond with a MessagePack-serialized value, or JSON if `debug_json` is set.
fn respond_msgpack<T: Serialize>(
    request: tiny_http::Request,
    value: &T,
    debug_json: bool,
) {
    if debug_json {
        match serde_json::to_vec(value) {
            Ok(json_bytes) => {
                let response = tiny_http::Response::from_data(json_bytes)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            "Content-Type",
                            "application/json",
                        )
                        .unwrap(),
                    );
                let _ = request.respond(response);
            }
            Err(e) => {
                respond_error(request, 500, &format!("json serialization error: {}", e));
            }
        }
    } else {
        match rmp_serde::to_vec_named(value) {
            Ok(msgpack_bytes) => {
                let response = tiny_http::Response::from_data(msgpack_bytes)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            "Content-Type",
                            "application/msgpack",
                        )
                        .unwrap(),
                    );
                let _ = request.respond(response);
            }
            Err(e) => {
                respond_error(request, 500, &format!("msgpack serialization error: {}", e));
            }
        }
    }
}

/// Respond with raw bytes and `application/octet-stream`.
fn respond_raw(request: tiny_http::Request, data: Vec<u8>) {
    let response = tiny_http::Response::from_data(data)
        .with_header(
            tiny_http::Header::from_bytes(
                "Content-Type",
                "application/octet-stream",
            )
            .unwrap(),
        );
    let _ = request.respond(response);
}

/// Respond with an error status and plain-text body.
fn respond_error(request: tiny_http::Request, status: u16, msg: &str) {
    let response = tiny_http::Response::from_string(msg.to_string())
        .with_status_code(tiny_http::StatusCode(status));
    let _ = request.respond(response);
}

/// Dispatch a single HTTP request.
fn handle_request(mut request: tiny_http::Request, engine: &Arc<Mutex<Engine>>) {
    // Drain the request body to avoid blocking the client.
    let mut _body = Vec::new();
    let _ = request.as_reader().read_to_end(&mut _body);

    let method = request.method().clone();
    let url = request.url().to_string();

    let (parts, debug_json) = parse_url(&url);

    match (method, parts.as_slice()) {
        (tiny_http::Method::Get, ["merkle"]) => {
            // GET /merkle — return Merkle root as MessagePack.
            match engine.lock() {
                Ok(eng) => match eng.range_merkle_root() {
                    Ok(root) => {
                        let resp = MerkleResponse { root: root.to_vec() };
                        respond_msgpack(request, &resp, debug_json);
                    }
                    Err(e) => {
                        respond_error(request, 500, &format!("merkle root error: {}", e));
                    }
                },
                Err(_) => {
                    respond_error(request, 500, "engine lock poisoned");
                }
            }
        }

        (tiny_http::Method::Get, ["segments"]) => {
            // GET /segments — return full manifest as MessagePack.
            match engine.lock() {
                Ok(eng) => match eng.export_manifest() {
                    Ok(refs) => {
                        let entries: Vec<SegmentEntry> = refs
                            .into_iter()
                            .map(|sr| SegmentEntry {
                                segment_id: sr.segment_id,
                                segment_hash: sr.segment_hash.to_vec(),
                            })
                            .collect();
                        respond_msgpack(request, &entries, debug_json);
                    }
                    Err(e) => {
                        respond_error(request, 500, &format!("export manifest error: {}", e));
                    }
                },
                Err(_) => {
                    respond_error(request, 500, "engine lock poisoned");
                }
            }
        }

        (tiny_http::Method::Get, ["segments", hash_hex]) => {
            // GET /segments/{hash_hex} — return raw segment bytes.
            // Parse 64-char hex string into 32 bytes.
            let hash_hex = *hash_hex;
            if hash_hex.len() != 64 {
                respond_error(request, 400, "hash_hex must be 64 hex characters");
                return;
            }

            // Decode hex.
            let mut hash_bytes = [0u8; 32];
            let mut valid = true;
            for i in 0..32 {
                match u8::from_str_radix(&hash_hex[i * 2..i * 2 + 2], 16) {
                    Ok(b) => hash_bytes[i] = b,
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if !valid {
                respond_error(request, 400, "invalid hex encoding in hash");
                return;
            }

            // Determine the segment file path from the engine's db path.
            // The canonical segment filename is segment-{id:08}.dat, but we need to find it
            // by content hash. Look up via export_manifest to find the segment_id, then
            // read the canonical .dat file. If not found, fall back to {hash_hex}.dat.
            let dat_bytes = {
                let eng_guard = match engine.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        respond_error(request, 500, "engine lock poisoned");
                        return;
                    }
                };

                // Find the segment_id for this hash.
                let manifest = match eng_guard.export_manifest() {
                    Ok(m) => m,
                    Err(e) => {
                        respond_error(request, 500, &format!("manifest error: {}", e));
                        return;
                    }
                };

                let segment_id = manifest
                    .iter()
                    .find(|sr| sr.segment_hash == hash_bytes)
                    .map(|sr| sr.segment_id);

                let base_path = eng_guard.db_path().to_path_buf();

                drop(eng_guard); // release lock before file I/O

                match segment_id {
                    Some(sid) => {
                        // Read the canonical segment-{id:08}.dat file.
                        let dat_path = base_path.join(format!("segment-{:08}.dat", sid));
                        match std::fs::read(&dat_path) {
                            Ok(bytes) => bytes,
                            Err(_) => {
                                // Fall back to hash-named file.
                                let fallback = base_path.join(format!("{}.dat", hash_hex));
                                match std::fs::read(&fallback) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        respond_error(
                                            request,
                                            404,
                                            &format!("segment not found: {}", e),
                                        );
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        // Hash not in manifest; try direct hash-named file.
                        let dat_path = base_path.join(format!("{}.dat", hash_hex));
                        match std::fs::read(&dat_path) {
                            Ok(b) => b,
                            Err(_) => {
                                respond_error(request, 404, "segment not found");
                                return;
                            }
                        }
                    }
                }
            };

            respond_raw(request, dat_bytes);
        }

        _ => {
            respond_error(request, 404, "not found");
        }
    }
}
