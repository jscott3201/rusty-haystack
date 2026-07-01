//! WebSocket handler and watch subscription manager.
//!
//! This module provides two major components:
//!
//! 1. **`WatchManager`** — a thread-safe subscription registry that manages
//!    watch lifecycles (subscribe, poll, unsubscribe, add/remove IDs).
//!
//! 2. **`ws_handler`** — an Axum WebSocket upgrade endpoint (`GET /api/ws`)
//!    that handles Haystack watch operations over JSON messages.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use parking_lot::RwLock;
use serde_json::{Map, Value};
use uuid::Uuid;

use haystack_core::codecs::json::v3 as json_v3;
use haystack_core::data::HDict;
use haystack_core::graph::SharedGraph;

use crate::auth::AuthUser;
use crate::state::SharedState;

// ---------------------------------------------------------------------------
// Tuning constants
// ---------------------------------------------------------------------------

const MAX_WATCHES: usize = 100;
const MAX_ENTITY_IDS_PER_WATCH: usize = 1_000;
/// Maximum total entity IDs a single user can watch across all watches.
const MAX_TOTAL_WATCHED_IDS: usize = 5_000;
/// Maximum watches a single user can hold at once.
const MAX_WATCHES_PER_USER: usize = 20;

/// Maximum entries in the per-connection encode cache.
const MAX_ENCODE_CACHE_ENTRIES: usize = 50_000;

/// Server-initiated ping interval for liveness detection.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// If no pong is received within this duration after a ping, the connection
/// is considered dead and will be closed.
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

/// mpsc channel capacity for outbound messages.
const CHANNEL_CAPACITY: usize = 64;

/// Number of consecutive `try_send` failures before closing a slow client.
const MAX_SEND_FAILURES: u32 = 3;

// ---------------------------------------------------------------------------
// WebSocket message types
// ---------------------------------------------------------------------------

/// Incoming JSON message from a WebSocket client.
#[derive(serde::Deserialize, Debug)]
struct WsRequest {
    op: String,
    #[serde(rename = "reqId")]
    req_id: Option<String>,
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
fn handle_ws_request(req: &WsRequest, username: &str, state: &SharedState) -> String {
    let resp = match req.op.as_str() {
        "watchSub" => handle_watch_sub(req, username, state),
        "watchPoll" => handle_watch_poll(req, username, state),
        "watchUnsub" => handle_watch_unsub(req, username, state),
        other => WsResponse::error(req.req_id.clone(), format!("unknown op: {other}")),
    };
    serde_json::to_string(&resp).unwrap_or_else(|e| {
        let fallback = WsResponse::error(req.req_id.clone(), format!("serialization error: {e}"));
        serde_json::to_string(&fallback).unwrap()
    })
}

fn handle_watch_sub(req: &WsRequest, username: &str, state: &SharedState) -> WsResponse {
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

    let rows: Vec<Value> = ids
        .iter()
        .filter_map(|id| state.graph.get(id).map(|e| encode_entity(&e)))
        .collect();

    WsResponse::ok(req.req_id.clone(), rows, Some(watch_id))
}

fn handle_watch_poll(req: &WsRequest, username: &str, state: &SharedState) -> WsResponse {
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

fn handle_watch_unsub(req: &WsRequest, username: &str, state: &SharedState) -> WsResponse {
    let watch_id = match &req.watch_id {
        Some(wid) => wid.clone(),
        None => {
            return WsResponse::error(req.req_id.clone(), "watchUnsub requires 'watchId'");
        }
    };

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
    /// Entity IDs being watched.
    entity_ids: HashSet<String>,
    /// Graph version at last poll.
    last_version: u64,
    /// Username of the watch owner.
    owner: String,
}

/// Manages watch subscriptions for change polling.
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
        let user_total: usize = watches
            .values()
            .filter(|w| w.owner == username)
            .map(|w| w.entity_ids.len())
            .sum();
        if user_total + ids.len() > MAX_TOTAL_WATCHED_IDS {
            return Err(format!(
                "user '{}' would exceed the maximum of {} total watched IDs",
                username, MAX_TOTAL_WATCHED_IDS
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
    pub fn poll(&self, watch_id: &str, username: &str, graph: &SharedGraph) -> Option<Vec<HDict>> {
        let (entity_ids, last_version) = {
            let mut watches = self.watches.write();
            let watch = watches.get_mut(watch_id)?;
            if watch.owner != username {
                return None;
            }

            let current_version = graph.version();
            if current_version == watch.last_version {
                return Some(Vec::new());
            }

            let ids = watch.entity_ids.clone();
            let last = watch.last_version;
            watch.last_version = current_version;
            (ids, last)
        };

        let changes = match graph.changes_since(last_version) {
            Ok(c) => c,
            Err(_gap) => {
                return Some(entity_ids.iter().filter_map(|id| graph.get(id)).collect());
            }
        };
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
    pub fn add_ids(&self, watch_id: &str, username: &str, ids: Vec<String>) -> bool {
        let mut watches = self.watches.write();

        let (owner_ok, per_watch_ok, user_total) = match watches.get(watch_id) {
            Some(watch) => (
                watch.owner == username,
                watch.entity_ids.len() + ids.len() <= MAX_ENTITY_IDS_PER_WATCH,
                watches
                    .values()
                    .filter(|w| w.owner == username)
                    .map(|w| w.entity_ids.len())
                    .sum::<usize>(),
            ),
            None => return false,
        };

        if !owner_ok || !per_watch_ok {
            return false;
        }
        if user_total + ids.len() > MAX_TOTAL_WATCHED_IDS {
            return false;
        }

        if let Some(watch) = watches.get_mut(watch_id) {
            watch.entity_ids.extend(ids);
        }
        true
    }

    /// Remove specific entity IDs from an existing watch.
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

    /// Remove all watches owned by a given user.
    pub fn remove_by_owner(&self, owner: &str) {
        let mut watches = self.watches.write();
        watches.retain(|_, w| w.owner != owner);
    }

    /// Return the list of entity IDs for a given watch.
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

    /// Encode an entity using the cache.
    pub fn encode_cached(&self, ref_val: &str, graph_version: u64, entity: &HDict) -> Value {
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
        let mut cache = self.encode_cache.write();
        cache.insert(key, encoded.clone());
        if cache.len() > MAX_ENCODE_CACHE_ENTRIES {
            let to_remove = cache.len() / 4;
            let keys: Vec<_> = cache.keys().take(to_remove).cloned().collect();
            for k in keys {
                cache.remove(&k);
            }
        }
        encoded
    }

    /// Get the IDs of all entities watched by any watch.
    pub fn all_watched_ids(&self) -> HashSet<String> {
        let watches = self.watches.read();
        watches
            .values()
            .flat_map(|w| w.entity_ids.iter().cloned())
            .collect()
    }

    /// Find watches that contain any of the given changed ref_vals.
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
// WebSocket handler
// ---------------------------------------------------------------------------

/// WebSocket upgrade handler for `/api/ws`.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
    auth: Option<Extension<AuthUser>>,
) -> Response {
    let username = auth
        .map(|Extension(u)| u.username)
        .unwrap_or_else(|| "anonymous".into());
    ws.on_upgrade(move |socket| handle_socket(socket, username, state))
}

/// Handle a WebSocket connection after upgrade.
async fn handle_socket(socket: WebSocket, username: String, state: SharedState) {
    use tokio::sync::mpsc;

    let (tx, mut rx) = mpsc::channel::<Message>(CHANNEL_CAPACITY);

    // Spawn a task to forward messages from the channel to the WS session.
    use futures_util::{SinkExt, StreamExt};

    let (mut ws_sender, mut ws_receiver) = socket.split();

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
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

    // Server-push check interval.
    let mut push_interval = tokio::time::interval(Duration::from_millis(500));
    push_interval.tick().await;

    loop {
        tokio::select! {
            // Incoming WS messages
            msg = ws_receiver.next() => {
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
                        if tx.try_send(Message::Text(response_text.into())).is_err() {
                            send_failures += 1;
                            if send_failures >= MAX_SEND_FAILURES {
                                log::warn!("closing slow WS client ({})", username);
                                break;
                            }
                        } else {
                            send_failures = 0;
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {
                        awaiting_pong = false;
                    }
                    Message::Close(_) => {
                        break;
                    }
                    _ => {}
                }
            }

            // Server-initiated ping for liveness
            _ = ping_interval.tick() => {
                if awaiting_pong && last_activity.elapsed() > PONG_TIMEOUT {
                    log::info!("closing stale WS connection ({}): no pong", username);
                    break;
                }
                if tx.try_send(Message::Ping(vec![].into())).is_err() {
                    break;
                }
                awaiting_pong = true;
            }

            // Server-push: check for graph changes
            _ = push_interval.tick() => {
                let current_version = state.graph.version();
                if current_version > last_push_version {
                    let changes = match state.graph.changes_since(last_push_version) {
                        Ok(c) => c,
                        Err(_gap) => {
                            last_push_version = current_version;
                            continue;
                        }
                    };
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
                                let _ = tx.try_send(Message::Text(text.into()));
                            }
                        }
                    }
                    last_push_version = current_version;
                }
            }
        }
    }

    // Cleanup: remove all watches owned by this user on disconnect.
    state.watches.remove_by_owner(&username);
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

        assert!(wm.poll(&watch_id, "other-user", &graph).is_none());
    }

    #[test]
    fn unsubscribe_removes_watch() {
        let wm = WatchManager::new();
        let watch_id = wm.subscribe("admin", vec!["site-1".into()], 0).unwrap();
        assert!(wm.unsubscribe(&watch_id, "admin"));
        assert!(!wm.unsubscribe(&watch_id, "admin"));
    }

    #[test]
    fn unsubscribe_wrong_owner() {
        let wm = WatchManager::new();
        let watch_id = wm.subscribe("admin", vec!["site-1".into()], 0).unwrap();
        assert!(!wm.unsubscribe(&watch_id, "other-user"));
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

        assert!(!wm.remove_ids(&watch_id, "other-user", &["site-1".into()]));

        let remaining = wm.get_ids(&watch_id).unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn remove_ids_leaves_watch_alive() {
        let wm = WatchManager::new();
        let watch_id = wm
            .subscribe("admin", vec!["site-1".into(), "site-2".into()], 0)
            .unwrap();

        assert!(wm.remove_ids(&watch_id, "admin", &["site-1".into(), "site-2".into()]));

        let remaining = wm.get_ids(&watch_id).unwrap();
        assert!(remaining.is_empty());

        assert!(wm.unsubscribe(&watch_id, "admin"));
    }

    #[test]
    fn unsubscribe_full_removal() {
        let wm = WatchManager::new();
        let watch_id = wm
            .subscribe("admin", vec!["site-1".into(), "site-2".into()], 0)
            .unwrap();

        assert!(wm.unsubscribe(&watch_id, "admin"));
        assert!(wm.get_ids(&watch_id).is_none());
        assert!(!wm.unsubscribe(&watch_id, "admin"));
    }

    #[test]
    fn add_ids_ownership_check() {
        let wm = WatchManager::new();
        let watch_id = wm.subscribe("admin", vec!["site-1".into()], 0).unwrap();

        assert!(!wm.add_ids(&watch_id, "other-user", vec!["site-2".into()]));
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

    #[test]
    fn ws_request_deserialization() {
        let json = r#"{
            "op": "watchSub",
            "reqId": "abc-123",
            "ids": ["@ref1", "@ref2"]
        }"#;
        let req: WsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.op, "watchSub");
        assert_eq!(req.req_id.as_deref(), Some("abc-123"));
        assert!(req.watch_id.is_none());
        let ids = req.ids.unwrap();
        assert_eq!(ids, vec!["@ref1", "@ref2"]);
    }

    #[test]
    fn ws_request_deserialization_minimal() {
        let json = r#"{"op": "watchPoll", "watchId": "w-1"}"#;
        let req: WsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.op, "watchPoll");
        assert!(req.req_id.is_none());
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
        assert!(json.get("error").is_none());
    }

    #[test]
    fn ws_response_omits_none_fields() {
        let resp = WsResponse::ok(None, vec![], None);
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("reqId").is_none());
        assert!(json.get("error").is_none());
        assert!(json.get("watchId").is_none());
        assert!(json["rows"].is_array());
    }

    #[test]
    fn ws_response_includes_req_id() {
        let resp = WsResponse::error(Some("req-42".into()), "something went wrong");
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["reqId"], "req-42");
        assert_eq!(json["error"], "something went wrong");
        assert!(json.get("rows").is_none());
        assert!(json.get("watchId").is_none());
    }

    #[test]
    fn ws_error_response_format() {
        let resp = WsResponse::error(None, "bad request");
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["error"], "bad request");
        assert!(json.get("reqId").is_none());
        assert!(json.get("rows").is_none());
        assert!(json.get("watchId").is_none());
    }
}
