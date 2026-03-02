//! WebSocket handler and watch subscription manager.
//!
//! This module provides two major components:
//!
//! 1. **`WatchManager`** — a thread-safe subscription registry that manages
//!    watch lifecycles (subscribe, poll, unsubscribe, add/remove IDs). Each
//!    watch tracks a set of entity IDs and the graph version at last poll,
//!    enabling efficient change detection.
//!
//! 2. **`ws_handler`** — an Actix-Web WebSocket upgrade endpoint (`GET /api/ws`)
//!    that handles Haystack watch operations over JSON messages. Supports
//!    server-initiated ping/pong liveness, deflate compression for large
//!    payloads, and automatic server-push of graph changes to watching clients.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use parking_lot::RwLock;
use serde_json::{Map, Value};
use uuid::Uuid;

use haystack_core::codecs::json::v3 as json_v3;
use haystack_core::data::HDict;
use haystack_core::graph::SharedGraph;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

const MAX_WATCHES: usize = 100;
const MAX_ENTITY_IDS_PER_WATCH: usize = 10_000;
/// Maximum watches a single user can hold at once.
const MAX_WATCHES_PER_USER: usize = 20;

/// Server-initiated ping interval for liveness detection.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// If no pong is received within this duration after a ping, the connection
/// is considered dead and will be closed.
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

/// mpsc channel capacity for outbound messages.
const CHANNEL_CAPACITY: usize = 64;

/// Number of consecutive `try_send` failures before closing a slow client.
const MAX_SEND_FAILURES: u32 = 3;

/// Minimum payload size (bytes) to consider compressing with deflate.
const COMPRESSION_THRESHOLD: usize = 512;

// ---------------------------------------------------------------------------
// WebSocket message types
// ---------------------------------------------------------------------------

/// Incoming JSON message from a WebSocket client.
#[derive(serde::Deserialize, Debug)]
struct WsRequest {
    op: String,
    #[serde(rename = "reqId")]
    req_id: Option<String>,
    #[serde(rename = "watchDis")]
    #[allow(dead_code)]
    watch_dis: Option<String>,
    #[serde(rename = "watchId")]
    watch_id: Option<String>,
    ids: Option<Vec<String>>,
}

/// Outgoing JSON message sent to a WebSocket client.
#[derive(serde::Serialize, Debug)]
struct WsResponse {
    #[serde(rename = "reqId", skip_serializing_if = "Option::is_none")]
    req_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<Vec<Value>>,
    #[serde(rename = "watchId", skip_serializing_if = "Option::is_none")]
    watch_id: Option<String>,
}

impl WsResponse {
    /// Build an error response, preserving the request ID for correlation.
    fn error(req_id: Option<String>, msg: impl Into<String>) -> Self {
        Self {
            req_id,
            error: Some(msg.into()),
            rows: None,
            watch_id: None,
        }
    }

    /// Build a success response with rows and an optional watch ID.
    fn ok(req_id: Option<String>, rows: Vec<Value>, watch_id: Option<String>) -> Self {
        Self {
            req_id,
            error: None,
            rows: Some(rows),
            watch_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Entity encoding helper
// ---------------------------------------------------------------------------

/// Encode an `HDict` entity as a JSON object using the Haystack JSON v3
/// encoding for individual tag values.
fn encode_entity(entity: &HDict) -> Value {
    let mut m = Map::new();
    let mut keys: Vec<&String> = entity.tags().keys().collect();
    keys.sort();
    for k in keys {
        let v = &entity.tags()[k];
        if let Ok(encoded) = json_v3::encode_kind(v) {
            m.insert(k.clone(), encoded);
        }
    }
    Value::Object(m)
}

// ---------------------------------------------------------------------------
// WebSocket op dispatch
// ---------------------------------------------------------------------------

/// Handle a parsed `WsRequest` by dispatching to the appropriate watch op.
///
/// Returns the serialized JSON response string.
fn handle_ws_request(req: &WsRequest, username: &str, state: &AppState) -> String {
    let resp = match req.op.as_str() {
        "watchSub" => handle_watch_sub(req, username, state),
        "watchPoll" => handle_watch_poll(req, username, state),
        "watchUnsub" => handle_watch_unsub(req, username, state),
        other => WsResponse::error(req.req_id.clone(), format!("unknown op: {other}")),
    };
    // Serialization of WsResponse should never fail in practice.
    serde_json::to_string(&resp).unwrap_or_else(|e| {
        let fallback = WsResponse::error(req.req_id.clone(), format!("serialization error: {e}"));
        serde_json::to_string(&fallback).unwrap()
    })
}

/// Handle the `watchSub` op: create a new watch and return the initial
/// state of the subscribed entities.
fn handle_watch_sub(req: &WsRequest, username: &str, state: &AppState) -> WsResponse {
    let ids = match &req.ids {
        Some(ids) if !ids.is_empty() => ids.clone(),
        _ => {
            return WsResponse::error(
                req.req_id.clone(),
                "watchSub requires non-empty 'ids' array",
            );
        }
    };

    // Strip leading '@' from ref strings if present.
    let ids: Vec<String> = ids
        .into_iter()
        .map(|id| id.strip_prefix('@').unwrap_or(&id).to_string())
        .collect();

    let graph_version = state.graph.version();
    let watch_id = match state
        .watches
        .subscribe(username, ids.clone(), graph_version)
    {
        Ok(wid) => wid,
        Err(e) => return WsResponse::error(req.req_id.clone(), e),
    };

    // Resolve initial entity values.
    let rows: Vec<Value> = ids
        .iter()
        .filter_map(|id| state.graph.get(id).map(|e| encode_entity(&e)))
        .collect();

    WsResponse::ok(req.req_id.clone(), rows, Some(watch_id))
}

/// Handle the `watchPoll` op: poll an existing watch for changes.
fn handle_watch_poll(req: &WsRequest, username: &str, state: &AppState) -> WsResponse {
    let watch_id = match &req.watch_id {
        Some(wid) => wid.clone(),
        None => {
            return WsResponse::error(req.req_id.clone(), "watchPoll requires 'watchId'");
        }
    };

    match state.watches.poll(&watch_id, username, &state.graph) {
        Some(changed) => {
            let rows: Vec<Value> = changed.iter().map(encode_entity).collect();
            WsResponse::ok(req.req_id.clone(), rows, Some(watch_id))
        }
        None => WsResponse::error(req.req_id.clone(), format!("watch not found: {watch_id}")),
    }
}

/// Handle the `watchUnsub` op: remove a watch or specific IDs from it.
fn handle_watch_unsub(req: &WsRequest, username: &str, state: &AppState) -> WsResponse {
    let watch_id = match &req.watch_id {
        Some(wid) => wid.clone(),
        None => {
            return WsResponse::error(req.req_id.clone(), "watchUnsub requires 'watchId'");
        }
    };

    // If specific IDs are provided, remove only those; otherwise remove the
    // entire watch.
    if let Some(ids) = &req.ids
        && !ids.is_empty()
    {
        let clean: Vec<String> = ids
            .iter()
            .map(|id| id.strip_prefix('@').unwrap_or(id).to_string())
            .collect();
        if !state.watches.remove_ids(&watch_id, username, &clean) {
            return WsResponse::error(req.req_id.clone(), format!("watch not found: {watch_id}"));
        }
        return WsResponse::ok(req.req_id.clone(), vec![], Some(watch_id));
    }

    if !state.watches.unsubscribe(&watch_id, username) {
        return WsResponse::error(req.req_id.clone(), format!("watch not found: {watch_id}"));
    }
    WsResponse::ok(req.req_id.clone(), vec![], None)
}

/// A single watch subscription.
struct Watch {
    /// Entity IDs being watched (HashSet for O(1) membership tests).
    entity_ids: HashSet<String>,
    /// Graph version at last poll.
    last_version: u64,
    /// Username of the watch owner.
    owner: String,
}

/// Manages watch subscriptions for change polling.
///
/// Watches are keyed by a UUID watch ID and owned by a specific user.
/// The manager enforces global and per-user watch limits, per-watch entity
/// ID limits, and provides an entity encoding cache for efficient
/// WebSocket server-push serialization.
pub struct WatchManager {
    watches: RwLock<HashMap<String, Watch>>,
    /// Cached entity encodings keyed by (ref_val, version) for watch poll.
    encode_cache: RwLock<HashMap<(String, u64), Value>>,
    /// Graph version at which the encode cache was last validated.
    cache_version: RwLock<u64>,
}

impl WatchManager {
    /// Create a new empty WatchManager.
    pub fn new() -> Self {
        Self {
            watches: RwLock::new(HashMap::new()),
            encode_cache: RwLock::new(HashMap::new()),
            cache_version: RwLock::new(0),
        }
    }

    /// Subscribe to changes on a set of entity IDs.
    ///
    /// Returns the watch ID, or an error if a growth cap would be exceeded.
    pub fn subscribe(
        &self,
        username: &str,
        ids: Vec<String>,
        graph_version: u64,
    ) -> Result<String, String> {
        let mut watches = self.watches.write();
        if watches.len() >= MAX_WATCHES {
            return Err("maximum number of watches reached".to_string());
        }
        let user_count = watches.values().filter(|w| w.owner == username).count();
        if user_count >= MAX_WATCHES_PER_USER {
            return Err(format!(
                "user '{}' has reached the maximum of {} watches",
                username, MAX_WATCHES_PER_USER
            ));
        }
        if ids.len() > MAX_ENTITY_IDS_PER_WATCH {
            return Err(format!(
                "too many entity IDs (max {})",
                MAX_ENTITY_IDS_PER_WATCH
            ));
        }
        let watch_id = Uuid::new_v4().to_string();
        let watch = Watch {
            entity_ids: ids.into_iter().collect(),
            last_version: graph_version,
            owner: username.to_string(),
        };
        watches.insert(watch_id.clone(), watch);
        Ok(watch_id)
    }

    /// Poll for changes since the last poll.
    ///
    /// Returns the current state of watched entities that have changed,
    /// or `None` if the watch ID is not found or the caller is not the owner.
    pub fn poll(&self, watch_id: &str, username: &str, graph: &SharedGraph) -> Option<Vec<HDict>> {
        // Acquire the write lock only long enough to read watch state and
        // update last_version.  Graph reads happen outside the lock to
        // avoid holding it during potentially expensive I/O.
        let (entity_ids, last_version) = {
            let mut watches = self.watches.write();
            let watch = watches.get_mut(watch_id)?;
            if watch.owner != username {
                return None;
            }

            let current_version = graph.version();
            if current_version == watch.last_version {
                // No changes
                return Some(Vec::new());
            }

            let ids = watch.entity_ids.clone();
            let last = watch.last_version;
            watch.last_version = current_version;
            (ids, last)
        }; // write lock released here

        // Graph reads happen without the WatchManager write lock held.
        let changes = graph.changes_since(last_version);
        let changed_refs: HashSet<&str> = changes.iter().map(|d| d.ref_val.as_str()).collect();

        Some(
            entity_ids
                .iter()
                .filter(|id| changed_refs.contains(id.as_str()))
                .filter_map(|id| graph.get(id))
                .collect(),
        )
    }

    /// Unsubscribe a watch by ID.
    ///
    /// Returns `true` if the watch existed, was owned by `username`, and was removed.
    pub fn unsubscribe(&self, watch_id: &str, username: &str) -> bool {
        let mut watches = self.watches.write();
        match watches.get(watch_id) {
            Some(watch) if watch.owner == username => {
                watches.remove(watch_id);
                true
            }
            _ => false,
        }
    }

    /// Add entity IDs to an existing watch.
    ///
    /// Returns `true` if the watch exists, is owned by `username`, and
    /// the addition would not exceed the per-watch entity ID limit.
    pub fn add_ids(&self, watch_id: &str, username: &str, ids: Vec<String>) -> bool {
        let mut watches = self.watches.write();
        if let Some(watch) = watches.get_mut(watch_id) {
            if watch.owner != username {
                return false;
            }
            if watch.entity_ids.len() + ids.len() > MAX_ENTITY_IDS_PER_WATCH {
                return false;
            }
            watch.entity_ids.extend(ids);
            true
        } else {
            false
        }
    }

    /// Remove specific entity IDs from an existing watch.
    ///
    /// Returns `true` if the watch exists and is owned by `username`.
    /// If all IDs are removed, the watch remains but with an empty entity set.
    pub fn remove_ids(&self, watch_id: &str, username: &str, ids: &[String]) -> bool {
        let mut watches = self.watches.write();
        if let Some(watch) = watches.get_mut(watch_id) {
            if watch.owner != username {
                return false;
            }
            for id in ids {
                watch.entity_ids.remove(id);
            }
            true
        } else {
            false
        }
    }

    /// Return the list of entity IDs for a given watch.
    ///
    /// Returns `None` if the watch does not exist.
    pub fn get_ids(&self, watch_id: &str) -> Option<Vec<String>> {
        let watches = self.watches.read();
        watches
            .get(watch_id)
            .map(|w| w.entity_ids.iter().cloned().collect())
    }

    /// Return the number of active watches.
    pub fn len(&self) -> usize {
        self.watches.read().len()
    }

    /// Return whether there are no active watches.
    pub fn is_empty(&self) -> bool {
        self.watches.read().is_empty()
    }

    /// Encode an entity using the cache. Returns cached value if the entity
    /// version hasn't changed; otherwise encodes and caches the result.
    pub fn encode_cached(&self, ref_val: &str, graph_version: u64, entity: &HDict) -> Value {
        // Invalidate entire cache when graph version advances beyond what we've seen.
        {
            let mut cv = self.cache_version.write();
            if graph_version > *cv {
                self.encode_cache.write().clear();
                *cv = graph_version;
            }
        }

        let key = (ref_val.to_string(), graph_version);
        if let Some(cached) = self.encode_cache.read().get(&key) {
            return cached.clone();
        }

        let encoded = encode_entity(entity);
        self.encode_cache.write().insert(key, encoded.clone());
        encoded
    }

    /// Get the IDs of all entities watched by any watch, for server-push
    /// change detection.
    pub fn all_watched_ids(&self) -> HashSet<String> {
        let watches = self.watches.read();
        watches
            .values()
            .flat_map(|w| w.entity_ids.iter().cloned())
            .collect()
    }

    /// Find watches that contain any of the given changed ref_vals.
    /// Returns (watch_id, owner, changed_entity_ids) tuples.
    pub fn watches_affected_by(
        &self,
        changed_refs: &HashSet<&str>,
    ) -> Vec<(String, String, Vec<String>)> {
        let watches = self.watches.read();
        let mut affected = Vec::new();
        for (wid, watch) in watches.iter() {
            let matched: Vec<String> = watch
                .entity_ids
                .iter()
                .filter(|id| changed_refs.contains(id.as_str()))
                .cloned()
                .collect();
            if !matched.is_empty() {
                affected.push((wid.clone(), watch.owner.clone(), matched));
            }
        }
        affected
    }
}

impl Default for WatchManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Compression helpers (application-level deflate)
// ---------------------------------------------------------------------------

/// Compress a response string with deflate if it exceeds the threshold.
/// Returns the original text if compression doesn't save space.
fn maybe_compress_response(text: &str) -> WsPayload {
    if text.len() < COMPRESSION_THRESHOLD {
        return WsPayload::Text(text.to_string());
    }
    let compressed = compress_deflate(text.as_bytes());
    if compressed.len() < text.len() {
        WsPayload::Binary(compressed)
    } else {
        WsPayload::Text(text.to_string())
    }
}

fn compress_deflate(data: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_else(|_| data.to_vec())
}

/// Maximum decompressed payload size (10 MB) to prevent zip bomb attacks.
const MAX_DECOMPRESSED_SIZE: u64 = 10 * 1024 * 1024;

fn decompress_deflate(data: &[u8]) -> Result<String, std::io::Error> {
    use flate2::read::DeflateDecoder;
    use std::io::Read;

    let decoder = DeflateDecoder::new(data);
    let mut limited = decoder.take(MAX_DECOMPRESSED_SIZE);
    let mut output = String::new();
    limited.read_to_string(&mut output)?;
    Ok(output)
}

enum WsPayload {
    Text(String),
    Binary(Vec<u8>),
}

// ---------------------------------------------------------------------------
// WebSocket handler
// ---------------------------------------------------------------------------

/// WebSocket upgrade handler for `/api/ws`.
///
/// Upgrades the HTTP connection to a WebSocket and handles Haystack
/// watch operations (watchSub, watchPoll, watchUnsub) over JSON
/// messages.  Each client request may include a `reqId` field which
/// is echoed back in the response for correlation.
///
/// Features:
/// - Server-initiated ping every [`PING_INTERVAL`] for liveness detection
/// - Backpressure: slow clients are disconnected after [`MAX_SEND_FAILURES`]
/// - Deflate compression for large responses (binary frames)
/// - Server-push: graph changes are pushed to watching clients automatically
pub async fn ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    state: web::Data<AppState>,
) -> actix_web::Result<HttpResponse> {
    // Require authenticated user when auth is enabled
    let username = if state.auth.is_enabled() {
        match req.extensions().get::<crate::auth::AuthUser>() {
            Some(u) => u.username.clone(),
            None => {
                return Err(crate::error::HaystackError::new(
                    "authentication required for WebSocket connections",
                    actix_web::http::StatusCode::UNAUTHORIZED,
                )
                .into());
            }
        }
    } else {
        req.extensions()
            .get::<crate::auth::AuthUser>()
            .map(|u| u.username.clone())
            .unwrap_or_else(|| "anonymous".to_string())
    };

    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, stream)?;

    actix_rt::spawn(async move {
        use actix_ws::Message;
        use tokio::sync::mpsc;

        let (tx, mut rx) = mpsc::channel::<WsPayload>(CHANNEL_CAPACITY);

        // Spawn a task to forward messages from the channel to the WS session.
        let mut session_clone = session.clone();
        actix_rt::spawn(async move {
            while let Some(payload) = rx.recv().await {
                let result = match payload {
                    WsPayload::Text(text) => session_clone.text(text).await,
                    WsPayload::Binary(data) => session_clone.binary(data).await,
                };
                if result.is_err() {
                    break;
                }
            }
        });

        // Track connection liveness.
        let mut last_activity = Instant::now();
        let mut ping_interval = tokio::time::interval(PING_INTERVAL);
        ping_interval.tick().await; // consume the immediate first tick
        let mut awaiting_pong = false;
        let mut send_failures: u32 = 0;

        // Track graph version for server-push change detection.
        let mut last_push_version = state.graph.version();

        // Server-push check interval (faster than ping but slower than a busy loop).
        let mut push_interval = tokio::time::interval(Duration::from_millis(500));
        push_interval.tick().await;

        use futures_util::StreamExt as _;

        loop {
            tokio::select! {
                // ── Incoming WS messages ──
                msg = msg_stream.next() => {
                    let Some(Ok(msg)) = msg else { break };
                    last_activity = Instant::now();
                    awaiting_pong = false;

                    match msg {
                        Message::Text(text) => {
                            let response_text = match serde_json::from_str::<WsRequest>(&text) {
                                Ok(ws_req) => handle_ws_request(&ws_req, &username, &state),
                                Err(e) => {
                                    let err = WsResponse::error(None, format!("invalid request: {e}"));
                                    serde_json::to_string(&err).unwrap()
                                }
                            };
                            let payload = maybe_compress_response(&response_text);
                            if tx.try_send(payload).is_err() {
                                send_failures += 1;
                                if send_failures >= MAX_SEND_FAILURES {
                                    log::warn!("closing slow WS client ({})", username);
                                    break;
                                }
                            } else {
                                send_failures = 0;
                            }
                        }
                        Message::Binary(data) => {
                            // Compressed request from client.
                            if let Ok(text) = decompress_deflate(&data) {
                                let response_text = match serde_json::from_str::<WsRequest>(&text) {
                                    Ok(ws_req) => handle_ws_request(&ws_req, &username, &state),
                                    Err(e) => {
                                        let err = WsResponse::error(None, format!("invalid request: {e}"));
                                        serde_json::to_string(&err).unwrap()
                                    }
                                };
                                let payload = maybe_compress_response(&response_text);
                                let _ = tx.try_send(payload);
                            }
                        }
                        Message::Ping(bytes) => {
                            let _ = session.pong(&bytes).await;
                        }
                        Message::Pong(_) => {
                            awaiting_pong = false;
                        }
                        Message::Close(_) => {
                            break;
                        }
                        _ => {}
                    }
                }

                // ── Server-initiated ping for liveness ──
                _ = ping_interval.tick() => {
                    if awaiting_pong && last_activity.elapsed() > PONG_TIMEOUT {
                        log::info!("closing stale WS connection ({}): no pong", username);
                        break;
                    }
                    let _ = session.ping(b"").await;
                    awaiting_pong = true;
                }

                // ── Server-push: check for graph changes ──
                _ = push_interval.tick() => {
                    let current_version = state.graph.version();
                    if current_version > last_push_version {
                        let changes = state.graph.changes_since(last_push_version);
                        let changed_refs: HashSet<&str> =
                            changes.iter().map(|d| d.ref_val.as_str()).collect();

                        let affected = state.watches.watches_affected_by(&changed_refs);
                        for (watch_id, owner, changed_ids) in &affected {
                            if owner != &username {
                                continue;
                            }
                            let rows: Vec<Value> = changed_ids
                                .iter()
                                .filter_map(|id| {
                                    let entity = state.graph.get(id)?;
                                    Some(state.watches.encode_cached(id, current_version, &entity))
                                })
                                .collect();
                            if !rows.is_empty() {
                                let push_msg = serde_json::json!({
                                    "type": "push",
                                    "watchId": watch_id,
                                    "rows": rows,
                                });
                                if let Ok(text) = serde_json::to_string(&push_msg) {
                                    let payload = maybe_compress_response(&text);
                                    let _ = tx.try_send(payload);
                                }
                            }
                        }
                        last_push_version = current_version;
                    }
                }
            }
        }

        let _ = session.close(None).await;
    });

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_core::graph::{EntityGraph, SharedGraph};
    use haystack_core::kinds::{HRef, Kind};

    fn make_graph_with_entity(id: &str) -> SharedGraph {
        let graph = SharedGraph::new(EntityGraph::new());
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val(id)));
        entity.set("site", Kind::Marker);
        entity.set("dis", Kind::Str(format!("Site {id}")));
        graph.add(entity).unwrap();
        graph
    }

    #[test]
    fn subscribe_returns_watch_id() {
        let wm = WatchManager::new();
        let watch_id = wm.subscribe("admin", vec!["site-1".into()], 0).unwrap();
        assert!(!watch_id.is_empty());
    }

    #[test]
    fn poll_no_changes() {
        let graph = make_graph_with_entity("site-1");
        let wm = WatchManager::new();
        let version = graph.version();
        let watch_id = wm
            .subscribe("admin", vec!["site-1".into()], version)
            .unwrap();

        let changes = wm.poll(&watch_id, "admin", &graph).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn poll_with_changes() {
        let graph = make_graph_with_entity("site-1");
        let wm = WatchManager::new();
        let version = graph.version();
        let watch_id = wm
            .subscribe("admin", vec!["site-1".into()], version)
            .unwrap();

        // Modify the entity
        let mut changes = HDict::new();
        changes.set("dis", Kind::Str("Updated".into()));
        graph.update("site-1", changes).unwrap();

        let result = wm.poll(&watch_id, "admin", &graph).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn poll_unknown_watch() {
        let graph = make_graph_with_entity("site-1");
        let wm = WatchManager::new();
        assert!(wm.poll("unknown", "admin", &graph).is_none());
    }

    #[test]
    fn poll_wrong_owner() {
        let graph = make_graph_with_entity("site-1");
        let wm = WatchManager::new();
        let version = graph.version();
        let watch_id = wm
            .subscribe("admin", vec!["site-1".into()], version)
            .unwrap();

        // Different user cannot poll the watch
        assert!(wm.poll(&watch_id, "other-user", &graph).is_none());
    }

    #[test]
    fn unsubscribe_removes_watch() {
        let wm = WatchManager::new();
        let watch_id = wm.subscribe("admin", vec!["site-1".into()], 0).unwrap();
        assert!(wm.unsubscribe(&watch_id, "admin"));
        assert!(!wm.unsubscribe(&watch_id, "admin")); // already removed
    }

    #[test]
    fn unsubscribe_wrong_owner() {
        let wm = WatchManager::new();
        let watch_id = wm.subscribe("admin", vec!["site-1".into()], 0).unwrap();
        // Different user cannot unsubscribe
        assert!(!wm.unsubscribe(&watch_id, "other-user"));
        // Original owner can still unsubscribe
        assert!(wm.unsubscribe(&watch_id, "admin"));
    }

    #[test]
    fn remove_ids_selective() {
        let wm = WatchManager::new();
        let watch_id = wm
            .subscribe(
                "admin",
                vec!["site-1".into(), "site-2".into(), "site-3".into()],
                0,
            )
            .unwrap();

        // Remove only site-2
        assert!(wm.remove_ids(&watch_id, "admin", &["site-2".into()]));

        let remaining = wm.get_ids(&watch_id).unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&"site-1".to_string()));
        assert!(remaining.contains(&"site-3".to_string()));
        assert!(!remaining.contains(&"site-2".to_string()));
    }

    #[test]
    fn remove_ids_nonexistent_watch() {
        let wm = WatchManager::new();
        assert!(!wm.remove_ids("no-such-watch", "admin", &["site-1".into()]));
    }

    #[test]
    fn remove_ids_wrong_owner() {
        let wm = WatchManager::new();
        let watch_id = wm
            .subscribe("admin", vec!["site-1".into(), "site-2".into()], 0)
            .unwrap();

        // Different user cannot remove IDs
        assert!(!wm.remove_ids(&watch_id, "other-user", &["site-1".into()]));

        // IDs remain unchanged
        let remaining = wm.get_ids(&watch_id).unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn remove_ids_leaves_watch_alive() {
        let wm = WatchManager::new();
        let watch_id = wm
            .subscribe("admin", vec!["site-1".into(), "site-2".into()], 0)
            .unwrap();

        // Remove all IDs selectively — watch still exists with empty entity set
        assert!(wm.remove_ids(&watch_id, "admin", &["site-1".into(), "site-2".into()]));

        let remaining = wm.get_ids(&watch_id).unwrap();
        assert!(remaining.is_empty());

        // The watch itself still exists (unsubscribe should succeed)
        assert!(wm.unsubscribe(&watch_id, "admin"));
    }

    #[test]
    fn unsubscribe_full_removal() {
        let wm = WatchManager::new();
        let watch_id = wm
            .subscribe("admin", vec!["site-1".into(), "site-2".into()], 0)
            .unwrap();

        // Full unsubscribe removes the watch entirely
        assert!(wm.unsubscribe(&watch_id, "admin"));

        // Watch no longer exists — get_ids returns None
        assert!(wm.get_ids(&watch_id).is_none());

        // Second unsubscribe returns false
        assert!(!wm.unsubscribe(&watch_id, "admin"));
    }

    #[test]
    fn add_ids_ownership_check() {
        let wm = WatchManager::new();
        let watch_id = wm.subscribe("admin", vec!["site-1".into()], 0).unwrap();

        // Different user cannot add IDs
        assert!(!wm.add_ids(&watch_id, "other-user", vec!["site-2".into()]));

        // Original owner can add IDs
        assert!(wm.add_ids(&watch_id, "admin", vec!["site-2".into()]));

        let ids = wm.get_ids(&watch_id).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"site-1".to_string()));
        assert!(ids.contains(&"site-2".to_string()));
    }

    #[test]
    fn get_ids_returns_none_for_unknown_watch() {
        let wm = WatchManager::new();
        assert!(wm.get_ids("nonexistent").is_none());
    }

    // -----------------------------------------------------------------------
    // WebSocket message format tests
    // -----------------------------------------------------------------------

    #[test]
    fn ws_request_deserialization() {
        let json = r#"{
            "op": "watchSub",
            "reqId": "abc-123",
            "watchDis": "my-watch",
            "ids": ["@ref1", "@ref2"]
        }"#;
        let req: WsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.op, "watchSub");
        assert_eq!(req.req_id.as_deref(), Some("abc-123"));
        assert_eq!(req.watch_dis.as_deref(), Some("my-watch"));
        assert!(req.watch_id.is_none());
        let ids = req.ids.unwrap();
        assert_eq!(ids, vec!["@ref1", "@ref2"]);
    }

    #[test]
    fn ws_request_deserialization_minimal() {
        // Only `op` is required; all other fields are optional.
        let json = r#"{"op": "watchPoll", "watchId": "w-1"}"#;
        let req: WsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.op, "watchPoll");
        assert!(req.req_id.is_none());
        assert!(req.watch_dis.is_none());
        assert_eq!(req.watch_id.as_deref(), Some("w-1"));
        assert!(req.ids.is_none());
    }

    #[test]
    fn ws_response_serialization() {
        let resp = WsResponse::ok(
            Some("r-1".into()),
            vec![serde_json::json!({"id": "r:site-1"})],
            Some("w-1".into()),
        );
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["reqId"], "r-1");
        assert_eq!(json["watchId"], "w-1");
        assert!(json["rows"].is_array());
        assert_eq!(json["rows"][0]["id"], "r:site-1");
        // `error` should be absent (not null) when None
        assert!(json.get("error").is_none());
    }

    #[test]
    fn ws_response_omits_none_fields() {
        let resp = WsResponse::ok(None, vec![], None);
        let json = serde_json::to_value(&resp).unwrap();
        // reqId, error, and watchId should all be absent
        assert!(json.get("reqId").is_none());
        assert!(json.get("error").is_none());
        assert!(json.get("watchId").is_none());
        // rows is present (empty array)
        assert!(json["rows"].is_array());
    }

    #[test]
    fn ws_response_includes_req_id() {
        let resp = WsResponse::error(Some("req-42".into()), "something went wrong");
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["reqId"], "req-42");
        assert_eq!(json["error"], "something went wrong");
        // rows and watchId should be absent on error
        assert!(json.get("rows").is_none());
        assert!(json.get("watchId").is_none());
    }

    #[test]
    fn ws_error_response_format() {
        let resp = WsResponse::error(None, "bad request");
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["error"], "bad request");
        // reqId should be absent when not provided
        assert!(json.get("reqId").is_none());
        // rows and watchId should be absent
        assert!(json.get("rows").is_none());
        assert!(json.get("watchId").is_none());
    }
}
