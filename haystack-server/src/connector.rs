//! Connector for fetching entities from a remote Haystack server.
//!
//! # Overview
//!
//! A [`Connector`] maintains a persistent connection (HTTP or WebSocket) to a
//! remote Haystack server, periodically syncs entities into a local cache, and
//! proxies write operations (`hisRead`, `hisWrite`, `pointWrite`,
//! `invokeAction`, `import`) to the remote when the target entity is owned by
//! that connector.
//!
//! # Lifecycle
//!
//! 1. **Construction** — [`Connector::new`] creates a connector with an empty
//!    cache from a [`ConnectorConfig`] (parsed from the TOML federation config).
//! 2. **Connection** — on first sync, [`Connector::sync`] calls
//!    `connect_persistent()` which tries WebSocket first, falling back to HTTP.
//! 3. **Sync loop** — [`Connector::spawn_sync_task`] spawns a tokio task that
//!    calls [`Connector::sync`] at an adaptive interval. Sync tries incremental
//!    delta sync via the `changes` op first, falling back to full entity fetch.
//! 4. **Cache** — [`Connector::update_cache`] replaces the cached entity list
//!    and rebuilds the bitmap tag index and owned-ID set for fast lookups.
//! 5. **Proxy** — write ops check [`Connector::owns`] and forward to the remote
//!    server via `proxy_*` methods, stripping/re-adding the ID prefix as needed.
//!
//! # ID Prefixing
//!
//! When `id_prefix` is configured, all Ref values (`id`, `siteRef`, etc.) are
//! prefixed on sync and stripped before proxying. See [`prefix_refs`] and
//! [`strip_prefix_refs`].

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use haystack_client::HaystackClient;
use haystack_client::transport::http::HttpTransport;
use haystack_client::transport::ws::WsTransport;
use haystack_core::data::{HDict, HGrid};
use haystack_core::filter::{FilterNode, matches, parse_filter};
use haystack_core::graph::bitmap::TagBitmapIndex;
use haystack_core::graph::query_planner;
use haystack_core::kinds::{HRef, Kind};

/// Type-erased persistent connection to a remote Haystack server.
///
/// Wraps either an HTTP or WebSocket `HaystackClient`, allowing the connector
/// to hold a single persistent connection regardless of transport.
enum ConnectorClient {
    Http(HaystackClient<HttpTransport>),
    Ws(HaystackClient<WsTransport>),
}

impl ConnectorClient {
    /// Call a Haystack op through the underlying client, returning the
    /// response grid or an error string.
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, String> {
        match self {
            ConnectorClient::Http(c) => c.call(op, req).await.map_err(|e| e.to_string()),
            ConnectorClient::Ws(c) => c.call(op, req).await.map_err(|e| e.to_string()),
        }
    }

    /// Read historical data for a point.
    async fn his_read(&self, id: &str, range: &str) -> Result<HGrid, String> {
        match self {
            ConnectorClient::Http(c) => c.his_read(id, range).await.map_err(|e| e.to_string()),
            ConnectorClient::Ws(c) => c.his_read(id, range).await.map_err(|e| e.to_string()),
        }
    }

    /// Write historical data for a point.
    async fn his_write(&self, id: &str, items: Vec<HDict>) -> Result<HGrid, String> {
        match self {
            ConnectorClient::Http(c) => c.his_write(id, items).await.map_err(|e| e.to_string()),
            ConnectorClient::Ws(c) => c.his_write(id, items).await.map_err(|e| e.to_string()),
        }
    }

    /// Write a value to a writable point.
    async fn point_write(&self, id: &str, level: u8, val: Kind) -> Result<HGrid, String> {
        match self {
            ConnectorClient::Http(c) => c
                .point_write(id, level, val)
                .await
                .map_err(|e| e.to_string()),
            ConnectorClient::Ws(c) => c
                .point_write(id, level, val)
                .await
                .map_err(|e| e.to_string()),
        }
    }

    /// Invoke an action on an entity.
    async fn invoke_action(&self, id: &str, action: &str, args: HDict) -> Result<HGrid, String> {
        match self {
            ConnectorClient::Http(c) => c
                .invoke_action(id, action, args)
                .await
                .map_err(|e| e.to_string()),
            ConnectorClient::Ws(c) => c
                .invoke_action(id, action, args)
                .await
                .map_err(|e| e.to_string()),
        }
    }
}

/// Transport protocol used by a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransportMode {
    Http = 0,
    WebSocket = 1,
}

/// Configuration for a remote Haystack server connection.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ConnectorConfig {
    /// Display name for this connector.
    pub name: String,
    /// Base URL of the remote Haystack API (e.g. "http://remote:8080/api").
    pub url: String,
    /// Username for SCRAM authentication.
    pub username: String,
    /// Password for SCRAM authentication.
    pub password: String,
    /// Optional tag prefix to namespace remote entity IDs (e.g. "remote1-").
    pub id_prefix: Option<String>,
    /// WebSocket URL (e.g. "ws://remote:8080/api/ws"). Derived from url if omitted.
    pub ws_url: Option<String>,
    /// Fallback polling interval in seconds (default: 60).
    pub sync_interval_secs: Option<u64>,
    /// Path to PEM client certificate for mTLS.
    pub client_cert: Option<String>,
    /// Path to PEM client private key for mTLS.
    pub client_key: Option<String>,
    /// Path to PEM CA certificate for server verification.
    pub ca_cert: Option<String>,
    /// Optional domain tag for scoping federated queries (e.g. "site:bldg-a").
    #[serde(default)]
    pub domain: Option<String>,
}

const MAX_DOMAIN_LEN: usize = 256;

impl ConnectorConfig {
    /// Validate configuration fields.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(ref domain) = self.domain {
            if domain.len() > MAX_DOMAIN_LEN {
                return Err(format!(
                    "domain too long: {} chars (max {MAX_DOMAIN_LEN})",
                    domain.len()
                ));
            }
            if !domain
                .chars()
                .all(|c| c.is_alphanumeric() || "-.:_".contains(c))
            {
                return Err(format!("domain contains invalid characters: {domain}"));
            }
        }
        Ok(())
    }
}

/// A connector that can fetch entities from a remote Haystack server.
///
/// Holds a persistent connection, a local entity cache with bitmap index,
/// Consolidated cache state — entities, ownership set, bitmap index, and ID map
/// bundled under a single lock for atomic reads/updates and consistency.
struct CacheState {
    entities: Vec<Arc<HDict>>,
    owned_ids: HashSet<String>,
    tag_index: TagBitmapIndex,
    id_map: HashMap<String, usize>,
}

impl CacheState {
    fn empty() -> Self {
        Self {
            entities: Vec::new(),
            owned_ids: HashSet::new(),
            tag_index: TagBitmapIndex::new(),
            id_map: HashMap::new(),
        }
    }

    /// Maximum number of tags per entity before the entity is skipped.
    const MAX_ENTITY_TAGS: usize = 1_000;
    /// Maximum entity ID length (bytes) before the entity is skipped.
    const MAX_ENTITY_ID_LEN: usize = 256;

    /// Build a fully indexed cache state from a list of entities.
    /// Entities with an ID exceeding [`MAX_ENTITY_ID_LEN`] or more than
    /// [`MAX_ENTITY_TAGS`] tags are silently skipped.
    fn build(entities: Vec<HDict>) -> Self {
        let mut owned_ids = HashSet::with_capacity(entities.len());
        let mut tag_index = TagBitmapIndex::new();
        let mut id_map = HashMap::with_capacity(entities.len());
        let mut valid_entities = Vec::with_capacity(entities.len());

        for entity in entities {
            // Validate entity: skip oversized IDs or excessive tag counts
            if let Some(Kind::Ref(r)) = entity.get("id") {
                if r.val.len() > Self::MAX_ENTITY_ID_LEN {
                    log::warn!("skipping entity with oversized ID ({} bytes)", r.val.len());
                    continue;
                }
            }
            if entity.len() > Self::MAX_ENTITY_TAGS {
                log::warn!("skipping entity with too many tags ({})", entity.len());
                continue;
            }

            let eid = valid_entities.len();
            if let Some(Kind::Ref(r)) = entity.get("id") {
                owned_ids.insert(r.val.clone());
                id_map.insert(r.val.clone(), eid);
            }
            let tags: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();
            tag_index.add(eid, &tags);
            valid_entities.push(Arc::new(entity));
        }

        Self {
            entities: valid_entities,
            owned_ids,
            tag_index,
            id_map,
        }
    }

    /// Rebuild tag index and id_map from current entities.
    fn rebuild_index(&mut self) {
        self.owned_ids.clear();
        self.tag_index = TagBitmapIndex::new();
        self.id_map.clear();

        for (eid, entity) in self.entities.iter().enumerate() {
            if let Some(Kind::Ref(r)) = entity.get("id") {
                self.owned_ids.insert(r.val.clone());
                self.id_map.insert(r.val.clone(), eid);
            }
            let tags: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();
            self.tag_index.add(eid, &tags);
        }
    }
}

/// and state for adaptive sync and federation proxy operations. See the
/// [module-level documentation](self) for the full lifecycle.
/// Observable state of a federation connector.
#[derive(Debug, Clone)]
pub struct ConnectorState {
    pub name: String,
    pub connected: bool,
    pub cache_version: u64,
    pub entity_count: usize,
    pub last_sync_ts: Option<i64>,
    pub staleness_secs: Option<f64>,
}

pub struct Connector {
    pub config: ConnectorConfig,
    /// Consolidated cache state: entities, ownership index, bitmap tag index, and
    /// ID→position map — all under a single lock for atomic consistency.
    cache_state: RwLock<CacheState>,
    /// Entity IDs being watched remotely (prefixed IDs).
    remote_watch_ids: RwLock<HashSet<String>>,
    /// Transport protocol (HTTP or WebSocket).
    transport_mode: AtomicU8,
    /// Whether the connector is currently connected (last sync succeeded).
    connected: AtomicBool,
    /// Timestamp of the last successful sync.
    last_sync: RwLock<Option<DateTime<Utc>>>,
    /// Remote graph version from last successful sync (for delta sync).
    last_remote_version: RwLock<Option<u64>>,
    /// Persistent client connection used by sync and background tasks.
    /// Lazily initialized on first sync. Cleared on connection error.
    /// Uses `tokio::sync::RwLock` because the guard is held across `.await`.
    client: tokio::sync::RwLock<Option<ConnectorClient>>,
    /// Current adaptive sync interval in seconds (adjusted based on change rate).
    current_interval_secs: AtomicU64,
    /// Number of entities from last sync (for change detection).
    last_entity_count: AtomicU64,
    /// Monotonically increasing version counter, bumped on each cache update.
    cache_version: AtomicU64,
}

impl Connector {
    /// Create a new connector with an empty cache.
    ///
    /// The connector starts disconnected; a persistent connection is
    /// established lazily on the first [`sync`](Self::sync) call.
    pub fn new(config: ConnectorConfig) -> Self {
        let base_interval = config.effective_sync_interval_secs();
        Self {
            config,
            cache_state: RwLock::new(CacheState::empty()),
            remote_watch_ids: RwLock::new(HashSet::new()),
            transport_mode: AtomicU8::new(TransportMode::Http as u8),
            connected: AtomicBool::new(false),
            last_sync: RwLock::new(None),
            last_remote_version: RwLock::new(None),
            client: tokio::sync::RwLock::new(None),
            current_interval_secs: AtomicU64::new(base_interval),
            last_entity_count: AtomicU64::new(0),
            cache_version: AtomicU64::new(0),
        }
    }

    /// Attempt to connect to the remote server. Tries WebSocket first,
    /// falls back to HTTP.
    async fn connect_persistent(&self) -> Result<(), String> {
        // Try WebSocket first
        let ws_url = self.config.effective_ws_url();
        match HaystackClient::connect_ws(
            &self.config.url,
            &ws_url,
            &self.config.username,
            &self.config.password,
        )
        .await
        {
            Ok(ws_client) => {
                *self.client.write().await = Some(ConnectorClient::Ws(ws_client));
                self.transport_mode
                    .store(TransportMode::WebSocket as u8, Ordering::Relaxed);
                self.connected.store(true, Ordering::Relaxed);
                log::info!("Connected to {} via WebSocket", self.config.name);
                return Ok(());
            }
            Err(e) => {
                log::warn!(
                    "WS connection to {} failed: {}, trying HTTP",
                    self.config.name,
                    e
                );
            }
        }

        // Fall back to HTTP
        match HaystackClient::connect(
            &self.config.url,
            &self.config.username,
            &self.config.password,
        )
        .await
        {
            Ok(http_client) => {
                *self.client.write().await = Some(ConnectorClient::Http(http_client));
                self.transport_mode
                    .store(TransportMode::Http as u8, Ordering::Relaxed);
                self.connected.store(true, Ordering::Relaxed);
                log::info!("Connected to {} via HTTP", self.config.name);
                Ok(())
            }
            Err(e) => {
                self.connected.store(false, Ordering::Relaxed);
                Err(format!("connection failed: {e}"))
            }
        }
    }

    /// Connect to the remote server, fetch entities, and update cache.
    ///
    /// Tries incremental delta sync first (via the `changes` op) if we have a
    /// known remote version. Falls back to full sync on first connect, when the
    /// remote doesn't support `changes`, or when the version gap is too large.
    ///
    /// Returns the number of cached entities after sync, or an error string.
    pub async fn sync(&self) -> Result<usize, String> {
        // Ensure we have a connection
        if self.client.read().await.is_none() {
            self.connect_persistent().await?;
        }

        // Attempt delta sync if we have a previous version.
        let maybe_ver = *self.last_remote_version.read();
        if let Some(last_ver) = maybe_ver {
            match self.try_delta_sync(last_ver).await {
                Ok(count) => return Ok(count),
                Err(e) => {
                    log::debug!(
                        "Delta sync failed for {}, falling back to full: {e}",
                        self.config.name,
                    );
                }
            }
        }

        // Full sync: read all entities
        self.full_sync().await
    }

    /// Full sync: fetch all entities from remote and replace cache.
    async fn full_sync(&self) -> Result<usize, String> {
        let grid = {
            let client = self.client.read().await;
            let client = client.as_ref().ok_or("not connected")?;
            client.call("read", &build_read_all_grid()).await
        };

        let grid = grid.map_err(|e| {
            self.connected.store(false, Ordering::Relaxed);
            format!("read failed: {e}")
        })?;

        let mut entities: Vec<HDict> = grid.rows.into_iter().collect();

        // Apply id prefix if configured.
        if let Some(ref prefix) = self.config.id_prefix {
            for entity in &mut entities {
                prefix_refs(entity, prefix);
            }
        }

        let count = entities.len();
        self.update_cache(entities);
        *self.last_sync.write() = Some(Utc::now());
        self.connected.store(true, Ordering::Relaxed);

        // Probe remote version for next delta sync attempt.
        self.probe_remote_version().await;

        Ok(count)
    }

    /// Try to discover the remote graph version by calling `changes` with a
    /// high version number. The response meta contains `curVer` which we store
    /// for future delta syncs. Failures are silently ignored (remote may not
    /// support the `changes` op).
    async fn probe_remote_version(&self) {
        let grid = {
            let client = self.client.read().await;
            let Some(client) = client.as_ref() else {
                return;
            };
            client.call("changes", &build_changes_grid(u64::MAX)).await
        };
        if let Ok(grid) = grid
            && let Some(Kind::Number(n)) = grid.meta.get("curVer")
        {
            *self.last_remote_version.write() = Some(n.val as u64);
        }
    }

    /// Attempt incremental delta sync using the `changes` op.
    ///
    /// Sends the last known remote version to the server. The server returns
    /// only the diffs since that version. We apply them incrementally to our
    /// cache, avoiding a full entity set transfer.
    async fn try_delta_sync(&self, since_version: u64) -> Result<usize, String> {
        let changes_grid = build_changes_grid(since_version);
        let grid = {
            let client = self.client.read().await;
            let client = client.as_ref().ok_or("not connected")?;
            client.call("changes", &changes_grid).await
        }
        .map_err(|e| format!("changes op failed: {e}"))?;

        // If the remote returned an error grid, fall back.
        if grid.is_err() {
            return Err("remote returned error grid for changes op".to_string());
        }

        // Extract current version from response meta.
        let cur_ver = grid
            .meta
            .get("curVer")
            .and_then(|k| {
                if let Kind::Number(n) = k {
                    Some(n.val as u64)
                } else {
                    None
                }
            })
            .ok_or("changes response missing curVer in meta")?;

        // No changes — nothing to do.
        if grid.rows.is_empty() {
            *self.last_remote_version.write() = Some(cur_ver);
            *self.last_sync.write() = Some(Utc::now());
            return Ok(self.cache_state.read().entities.len());
        }

        // Apply diffs in-place under a single write lock.
        let mut state = self.cache_state.write();
        let prefix = self.config.id_prefix.as_deref();

        for row in &grid.rows {
            let op = match row.get("op") {
                Some(Kind::Str(s)) => s.as_str(),
                _ => continue,
            };
            let ref_val = match row.get("ref") {
                Some(Kind::Str(s)) => s.clone(),
                _ => continue,
            };

            const MAX_ENTITY_TAGS: usize = 1_000;
            const MAX_ENTITY_ID_LEN: usize = 256;

            if ref_val.len() > MAX_ENTITY_ID_LEN {
                log::warn!("skipping entity with oversized id: {} bytes", ref_val.len());
                continue;
            }

            match op {
                "add" | "update" => {
                    if let Some(Kind::Dict(entity_box)) = row.get("entity") {
                        let mut entity: HDict = (**entity_box).clone();
                        if entity.len() > MAX_ENTITY_TAGS {
                            log::warn!("skipping oversized entity with {} tags", entity.len());
                            continue;
                        }
                        if let Some(pfx) = prefix {
                            prefix_refs(&mut entity, pfx);
                        }
                        let entity_id = entity.get("id").and_then(|k| {
                            if let Kind::Ref(r) = k {
                                Some(r.val.clone())
                            } else {
                                None
                            }
                        });

                        if let Some(ref eid) = entity_id {
                            if let Some(&idx) = state.id_map.get(eid.as_str()) {
                                // Update in place.
                                state.entities[idx] = Arc::new(entity);
                            } else {
                                // Add new entity.
                                let idx = state.entities.len();
                                state.id_map.insert(eid.clone(), idx);
                                state.entities.push(Arc::new(entity));
                            }
                        }
                    }
                }
                "remove" => {
                    let prefixed_ref = match prefix {
                        Some(pfx) => format!("{pfx}{ref_val}"),
                        None => ref_val,
                    };
                    if let Some(&idx) = state.id_map.get(prefixed_ref.as_str()) {
                        // Swap-remove and fix up the id_map.
                        let last_idx = state.entities.len() - 1;
                        if idx != last_idx {
                            let last_id = state.entities[last_idx].get("id").and_then(|k| {
                                if let Kind::Ref(r) = k {
                                    Some(r.val.clone())
                                } else {
                                    None
                                }
                            });
                            state.entities.swap(idx, last_idx);
                            if let Some(lid) = last_id {
                                state.id_map.insert(lid, idx);
                            }
                        }
                        state.entities.pop();
                        state.id_map.remove(prefixed_ref.as_str());
                    }
                }
                _ => {}
            }
        }

        // Rebuild index after in-place mutations.
        state.rebuild_index();

        let count = state.entities.len();
        drop(state);

        self.cache_version.fetch_add(1, Ordering::Relaxed);
        *self.last_remote_version.write() = Some(cur_ver);
        *self.last_sync.write() = Some(Utc::now());
        self.connected.store(true, Ordering::Relaxed);
        Ok(count)
    }

    /// Replace the cached entities and rebuild the owned-ID index and bitmap tag index.
    ///
    /// Extracts all `id` Ref values from the given entities into a `HashSet`,
    /// builds a bitmap tag index for fast filtered reads, then atomically
    /// replaces the cache, ownership set, and index.
    pub fn update_cache(&self, entities: Vec<HDict>) {
        *self.cache_state.write() = CacheState::build(entities);
        self.cache_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns `true` if this connector owns an entity with the given ID.
    pub fn owns(&self, id: &str) -> bool {
        self.cache_state.read().owned_ids.contains(id)
    }

    /// Look up a single cached entity by ID using the O(1) id_map index.
    pub fn get_cached_entity(&self, id: &str) -> Option<Arc<HDict>> {
        let state = self.cache_state.read();
        state
            .id_map
            .get(id)
            .and_then(|&idx| state.entities.get(idx))
            .map(Arc::clone)
    }

    /// Look up multiple cached entities by ID in a single pass.
    /// Returns found entities and missing IDs.
    pub fn batch_get_cached(&self, ids: &[&str]) -> (Vec<Arc<HDict>>, Vec<String>) {
        let state = self.cache_state.read();
        let mut found = Vec::with_capacity(ids.len());
        let mut missing = Vec::new();
        for &id in ids {
            if let Some(&idx) = state.id_map.get(id) {
                if let Some(entity) = state.entities.get(idx) {
                    found.push(Arc::clone(entity));
                } else {
                    missing.push(id.to_string());
                }
            } else {
                missing.push(id.to_string());
            }
        }
        (found, missing)
    }

    /// Returns Arc-wrapped references to all cached entities (cheap pointer copies).
    pub fn cached_entities(&self) -> Vec<Arc<HDict>> {
        self.cache_state.read().entities.clone()
    }

    /// Returns the number of cached entities.
    pub fn entity_count(&self) -> usize {
        self.cache_state.read().entities.len()
    }

    /// Filter cached entities using the bitmap tag index for acceleration.
    ///
    /// Returns matching entities up to `limit` (0 = unlimited). Uses the same
    /// two-phase approach as EntityGraph: bitmap candidates → full filter eval.
    pub fn filter_cached(&self, filter_expr: &str, limit: usize) -> Result<Vec<HDict>, String> {
        let ast = parse_filter(filter_expr).map_err(|e| format!("filter error: {e}"))?;
        Ok(self
            .filter_cached_with_ast(&ast, limit)
            .into_iter()
            .map(|arc| Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone()))
            .collect())
    }

    /// Filter cached entities using a pre-parsed filter AST.
    ///
    /// Avoids redundant parsing when the same filter is applied across
    /// multiple connectors (e.g. federated reads). Returns Arc-wrapped
    /// references for cheap cloning in federation pipelines.
    pub fn filter_cached_with_ast(&self, ast: &FilterNode, limit: usize) -> Vec<Arc<HDict>> {
        let effective_limit = if limit == 0 { usize::MAX } else { limit };

        let state = self.cache_state.read();
        let max_id = state.entities.len();

        let candidates = query_planner::bitmap_candidates(ast, &state.tag_index, max_id);

        let mut results = Vec::with_capacity(effective_limit.min(max_id));

        if let Some(ref bitmap) = candidates {
            for eid in TagBitmapIndex::iter_set_bits(bitmap) {
                if results.len() >= effective_limit {
                    break;
                }
                if let Some(entity) = state.entities.get(eid)
                    && matches(ast, entity, None)
                {
                    results.push(Arc::clone(entity));
                }
            }
        } else {
            for entity in state.entities.iter() {
                if results.len() >= effective_limit {
                    break;
                }
                if matches(ast, entity, None) {
                    results.push(Arc::clone(entity));
                }
            }
        }

        results
    }

    /// Add a federated entity ID to the remote watch set.
    pub fn add_remote_watch(&self, prefixed_id: &str) {
        self.remote_watch_ids
            .write()
            .insert(prefixed_id.to_string());
    }

    /// Remove a federated entity ID from the remote watch set.
    pub fn remove_remote_watch(&self, prefixed_id: &str) {
        self.remote_watch_ids.write().remove(prefixed_id);
    }

    /// Returns the number of entity IDs being watched remotely.
    pub fn remote_watch_count(&self) -> usize {
        self.remote_watch_ids.read().len()
    }

    /// Returns the current transport mode.
    pub fn transport_mode(&self) -> TransportMode {
        match self.transport_mode.load(Ordering::Relaxed) {
            1 => TransportMode::WebSocket,
            _ => TransportMode::Http,
        }
    }

    /// Returns whether the connector is currently connected (last sync succeeded).
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Returns the timestamp of the last successful sync, if any.
    pub fn last_sync_time(&self) -> Option<DateTime<Utc>> {
        *self.last_sync.read()
    }

    /// Returns the current cache version counter.
    pub fn cache_version(&self) -> u64 {
        self.cache_version.load(Ordering::Relaxed)
    }

    /// Returns an observable snapshot of this connector's state.
    pub fn state(&self) -> ConnectorState {
        let now = Utc::now();
        let last_sync = self.last_sync_time();
        ConnectorState {
            name: self.config.name.clone(),
            connected: self.is_connected(),
            cache_version: self.cache_version(),
            entity_count: self.entity_count(),
            last_sync_ts: last_sync.map(|ts| ts.timestamp()),
            staleness_secs: last_sync.map(|ts| (now - ts).num_milliseconds() as f64 / 1000.0),
        }
    }

    /// Strip the id_prefix from an entity ID.
    fn strip_id(&self, prefixed_id: &str) -> String {
        if let Some(ref prefix) = self.config.id_prefix {
            prefixed_id
                .strip_prefix(prefix.as_str())
                .unwrap_or(prefixed_id)
                .to_string()
        } else {
            prefixed_id.to_string()
        }
    }

    /// Ensure the persistent client is connected, establishing a new
    /// connection if needed. Returns an error if connection fails.
    async fn ensure_connected(&self) -> Result<(), String> {
        if self.client.read().await.is_none() {
            self.connect_persistent().await?;
        }
        Ok(())
    }

    /// Handle a failed proxy operation by clearing the client to force
    /// reconnection on the next call.
    async fn on_proxy_error(&self, op_name: &str, e: String) -> String {
        *self.client.write().await = None;
        self.connected.store(false, Ordering::Relaxed);
        format!("{op_name} failed: {e}")
    }

    /// Proxy a hisRead request to the remote server.
    ///
    /// Strips the ID prefix, calls `hisRead` on the remote, and returns the
    /// response grid. Clears the client on connection error.
    pub async fn proxy_his_read(&self, prefixed_id: &str, range: &str) -> Result<HGrid, String> {
        self.ensure_connected().await?;
        let id = self.strip_id(prefixed_id);
        let guard = self.client.read().await;
        let client = guard.as_ref().ok_or("not connected")?;
        match client.his_read(&id, range).await {
            Ok(grid) => Ok(grid),
            Err(e) => {
                drop(guard);
                Err(self.on_proxy_error("hisRead", e).await)
            }
        }
    }

    /// Proxy a pointWrite request to the remote server.
    ///
    /// Strips the ID prefix, calls `pointWrite` on the remote with the given
    /// level and value. Clears the client on connection error.
    pub async fn proxy_point_write(
        &self,
        prefixed_id: &str,
        level: u8,
        val: &Kind,
    ) -> Result<HGrid, String> {
        self.ensure_connected().await?;
        let id = self.strip_id(prefixed_id);
        let val = val.clone();
        let guard = self.client.read().await;
        let client = guard.as_ref().ok_or("not connected")?;
        match client.point_write(&id, level, val).await {
            Ok(grid) => Ok(grid),
            Err(e) => {
                drop(guard);
                Err(self.on_proxy_error("pointWrite", e).await)
            }
        }
    }

    /// Proxy a hisWrite request to the remote server.
    ///
    /// Strips the ID prefix, calls `hisWrite` on the remote with the given
    /// time-series rows. Clears the client on connection error.
    pub async fn proxy_his_write(
        &self,
        prefixed_id: &str,
        items: Vec<HDict>,
    ) -> Result<HGrid, String> {
        self.ensure_connected().await?;
        let id = self.strip_id(prefixed_id);
        let guard = self.client.read().await;
        let client = guard.as_ref().ok_or("not connected")?;
        match client.his_write(&id, items).await {
            Ok(grid) => Ok(grid),
            Err(e) => {
                drop(guard);
                Err(self.on_proxy_error("hisWrite", e).await)
            }
        }
    }

    /// Proxy an import request for a single entity to the remote server.
    ///
    /// Strips the id prefix from the entity, wraps it in a single-row grid,
    /// and calls the remote `import` op.
    pub async fn proxy_import(&self, entity: &HDict) -> Result<HGrid, String> {
        self.ensure_connected().await?;

        let mut stripped = entity.clone();
        if let Some(ref prefix) = self.config.id_prefix {
            strip_prefix_refs(&mut stripped, prefix);
        }

        let col_names: Vec<String> = stripped.tag_names().map(|s| s.to_string()).collect();
        let cols: Vec<haystack_core::data::HCol> = col_names
            .iter()
            .map(|n| haystack_core::data::HCol::new(n.as_str()))
            .collect();
        let grid = HGrid::from_parts(HDict::new(), cols, vec![stripped]);

        let guard = self.client.read().await;
        let client = guard.as_ref().ok_or("not connected")?;
        match client.call("import", &grid).await {
            Ok(grid) => Ok(grid),
            Err(e) => {
                drop(guard);
                Err(self.on_proxy_error("import", e).await)
            }
        }
    }

    /// Proxy an invokeAction request to the remote server.
    ///
    /// Strips the ID prefix, calls `invokeAction` on the remote with the given
    /// action name and arguments dict. Clears the client on connection error.
    pub async fn proxy_invoke_action(
        &self,
        prefixed_id: &str,
        action: &str,
        args: HDict,
    ) -> Result<HGrid, String> {
        self.ensure_connected().await?;
        let id = self.strip_id(prefixed_id);
        let action = action.to_string();
        let guard = self.client.read().await;
        let client = guard.as_ref().ok_or("not connected")?;
        match client.invoke_action(&id, &action, args).await {
            Ok(grid) => Ok(grid),
            Err(e) => {
                drop(guard);
                Err(self.on_proxy_error("invokeAction", e).await)
            }
        }
    }

    /// Spawn a background sync task for this connector.
    ///
    /// The task loops forever, syncing entities at the configured interval.
    /// On error, the persistent client is cleared to force reconnection on
    /// the next iteration.
    pub fn spawn_sync_task(connector: Arc<Connector>) -> tokio::task::JoinHandle<()> {
        let base_interval = connector.config.effective_sync_interval_secs();
        let min_interval = base_interval / 2;
        let max_interval = base_interval * 5;

        tokio::spawn(async move {
            loop {
                let prev_count = connector.last_entity_count.load(Ordering::Relaxed);

                match connector.sync().await {
                    Ok(count) => {
                        log::debug!("Synced {} entities from {}", count, connector.config.name);

                        // Adaptive interval: increase when no changes, reset when changes detected.
                        let current = connector.current_interval_secs.load(Ordering::Relaxed);
                        let new_interval = if count as u64 == prev_count && prev_count > 0 {
                            // No change — slow down (increase by 50%, capped at max).
                            (current + current / 2).min(max_interval)
                        } else {
                            // Changes detected — reset to base interval.
                            base_interval
                        };
                        connector
                            .current_interval_secs
                            .store(new_interval, Ordering::Relaxed);
                        connector
                            .last_entity_count
                            .store(count as u64, Ordering::Relaxed);
                    }
                    Err(e) => {
                        log::error!("Sync failed for {}: {}", connector.config.name, e);
                        // Clear the client on error to force reconnection
                        *connector.client.write().await = None;
                        connector.connected.store(false, Ordering::Relaxed);
                        // On error, use base interval for retry.
                        connector
                            .current_interval_secs
                            .store(base_interval, Ordering::Relaxed);
                    }
                }

                let sleep_secs = connector
                    .current_interval_secs
                    .load(Ordering::Relaxed)
                    .max(min_interval);
                tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
            }
        })
    }
}

/// Build a `read` request grid with filter `"*"` (all entities).
fn build_read_all_grid() -> HGrid {
    use haystack_core::data::HCol;
    let mut row = HDict::new();
    row.set("filter", Kind::Str("*".to_string()));
    HGrid::from_parts(HDict::new(), vec![HCol::new("filter")], vec![row])
}

/// Build a request grid for the `changes` op with the given version.
fn build_changes_grid(since_version: u64) -> HGrid {
    use haystack_core::data::HCol;
    use haystack_core::kinds::Number;
    let mut row = HDict::new();
    row.set(
        "version",
        Kind::Number(Number::unitless(since_version as f64)),
    );
    HGrid::from_parts(HDict::new(), vec![HCol::new("version")], vec![row])
}

impl ConnectorConfig {
    /// Returns the WebSocket URL. If `ws_url` is set, returns it directly.
    /// Otherwise derives from `url` by replacing `http://` with `ws://`
    /// or `https://` with `wss://` and appending `/ws`.
    pub fn effective_ws_url(&self) -> String {
        if let Some(ref ws) = self.ws_url {
            return ws.clone();
        }
        let ws = if self.url.starts_with("https://") {
            self.url.replacen("https://", "wss://", 1)
        } else {
            self.url.replacen("http://", "ws://", 1)
        };
        format!("{ws}/ws")
    }

    /// Returns the sync interval in seconds. Defaults to 60 if not set.
    pub fn effective_sync_interval_secs(&self) -> u64 {
        self.sync_interval_secs.unwrap_or(60)
    }
}

/// Prefix all Ref values in an entity dict.
///
/// Prefixes the `id` tag and any tag whose name ends with `Ref`
/// (e.g. `siteRef`, `equipRef`, `floorRef`, `spaceRef`).
pub fn prefix_refs(entity: &mut HDict, prefix: &str) {
    let tag_names: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();

    for name in &tag_names {
        let should_prefix = name == "id" || name.ends_with("Ref");
        if !should_prefix {
            continue;
        }

        if let Some(Kind::Ref(r)) = entity.get(name) {
            let new_val = format!("{}{}", prefix, r.val);
            let new_ref = HRef::new(new_val, r.dis.clone());
            entity.set(name.as_str(), Kind::Ref(new_ref));
        }
    }
}

/// Strip a prefix from all Ref values in an entity dict.
///
/// The inverse of [`prefix_refs`]. Strips the given prefix from the `id` tag
/// and any tag whose name ends with `Ref`, but only if the Ref value actually
/// starts with the prefix. Preserves `dis` metadata on Refs.
pub fn strip_prefix_refs(entity: &mut HDict, prefix: &str) {
    let tag_names: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();

    for name in &tag_names {
        let should_strip = name == "id" || name.ends_with("Ref");
        if !should_strip {
            continue;
        }

        if let Some(Kind::Ref(r)) = entity.get(name)
            && let Some(stripped) = r.val.strip_prefix(prefix)
        {
            let new_ref = HRef::new(stripped.to_string(), r.dis.clone());
            entity.set(name.as_str(), Kind::Ref(new_ref));
        }
    }
}

/// Encode an entity grid to HBF binary for efficient federation sync.
///
/// Provides a compact binary representation that is significantly smaller
/// than Zinc or JSON for bulk entity transfer.
pub fn encode_sync_payload(grid: &HGrid) -> Result<Vec<u8>, String> {
    haystack_core::codecs::encode_grid_binary(grid)
}

/// Decode an entity grid from HBF binary received during federation sync.
pub fn decode_sync_payload(bytes: &[u8]) -> Result<HGrid, String> {
    haystack_core::codecs::decode_grid_binary(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_core::kinds::HRef;

    #[test]
    fn connector_new_empty_cache() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        let connector = Connector::new(config);
        assert_eq!(connector.entity_count(), 0);
        assert!(connector.cached_entities().is_empty());
    }

    #[test]
    fn connector_config_deserialization() {
        let json = r#"{
            "name": "Remote Server",
            "url": "http://remote:8080/api",
            "username": "admin",
            "password": "secret",
            "id_prefix": "r1-"
        }"#;
        let config: ConnectorConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "Remote Server");
        assert_eq!(config.url, "http://remote:8080/api");
        assert_eq!(config.username, "admin");
        assert_eq!(config.password, "secret");
        assert_eq!(config.id_prefix, Some("r1-".to_string()));
    }

    #[test]
    fn connector_config_deserialization_without_prefix() {
        let json = r#"{
            "name": "Remote",
            "url": "http://remote:8080/api",
            "username": "admin",
            "password": "secret"
        }"#;
        let config: ConnectorConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.id_prefix, None);
    }

    #[test]
    fn id_prefix_application() {
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("site-1")));
        entity.set("dis", Kind::Str("Main Site".to_string()));
        entity.set("site", Kind::Marker);
        entity.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
        entity.set("equipRef", Kind::Ref(HRef::from_val("equip-1")));
        entity.set(
            "floorRef",
            Kind::Ref(HRef::new("floor-1", Some("Floor 1".to_string()))),
        );

        prefix_refs(&mut entity, "r1-");

        // id should be prefixed
        match entity.get("id") {
            Some(Kind::Ref(r)) => assert_eq!(r.val, "r1-site-1"),
            other => panic!("expected Ref, got {other:?}"),
        }

        // siteRef should be prefixed
        match entity.get("siteRef") {
            Some(Kind::Ref(r)) => assert_eq!(r.val, "r1-site-1"),
            other => panic!("expected Ref, got {other:?}"),
        }

        // equipRef should be prefixed
        match entity.get("equipRef") {
            Some(Kind::Ref(r)) => assert_eq!(r.val, "r1-equip-1"),
            other => panic!("expected Ref, got {other:?}"),
        }

        // floorRef should be prefixed, preserving dis
        match entity.get("floorRef") {
            Some(Kind::Ref(r)) => {
                assert_eq!(r.val, "r1-floor-1");
                assert_eq!(r.dis, Some("Floor 1".to_string()));
            }
            other => panic!("expected Ref, got {other:?}"),
        }

        // Non-ref tags should not be changed
        assert_eq!(entity.get("dis"), Some(&Kind::Str("Main Site".to_string())));
        assert_eq!(entity.get("site"), Some(&Kind::Marker));
    }

    #[test]
    fn id_prefix_skips_non_ref_values() {
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("point-1")));
        // A tag ending in "Ref" but whose value is not actually a Ref
        entity.set("customRef", Kind::Str("not-a-ref".to_string()));

        prefix_refs(&mut entity, "p-");

        // id should be prefixed
        match entity.get("id") {
            Some(Kind::Ref(r)) => assert_eq!(r.val, "p-point-1"),
            other => panic!("expected Ref, got {other:?}"),
        }

        // customRef is a Str, not a Ref, so it should be unchanged
        assert_eq!(
            entity.get("customRef"),
            Some(&Kind::Str("not-a-ref".to_string()))
        );
    }

    #[test]
    fn connector_config_deserialization_full() {
        let json = r#"{
            "name": "Full Config",
            "url": "https://remote:8443/api",
            "username": "admin",
            "password": "secret",
            "id_prefix": "r1-",
            "ws_url": "wss://remote:8443/api/ws",
            "sync_interval_secs": 30,
            "client_cert": "/etc/certs/client.pem",
            "client_key": "/etc/certs/client-key.pem",
            "ca_cert": "/etc/certs/ca.pem"
        }"#;
        let config: ConnectorConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.ws_url, Some("wss://remote:8443/api/ws".to_string()));
        assert_eq!(config.sync_interval_secs, Some(30));
        assert_eq!(
            config.client_cert,
            Some("/etc/certs/client.pem".to_string())
        );
        assert_eq!(
            config.client_key,
            Some("/etc/certs/client-key.pem".to_string())
        );
        assert_eq!(config.ca_cert, Some("/etc/certs/ca.pem".to_string()));
    }

    #[test]
    fn strip_prefix_refs_reverses_prefix() {
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("r1-site-1")));
        entity.set("dis", Kind::Str("Main Site".to_string()));
        entity.set("site", Kind::Marker);
        entity.set("siteRef", Kind::Ref(HRef::from_val("r1-site-1")));
        entity.set("equipRef", Kind::Ref(HRef::from_val("r1-equip-1")));

        strip_prefix_refs(&mut entity, "r1-");

        match entity.get("id") {
            Some(Kind::Ref(r)) => assert_eq!(r.val, "site-1"),
            other => panic!("expected Ref, got {other:?}"),
        }
        match entity.get("siteRef") {
            Some(Kind::Ref(r)) => assert_eq!(r.val, "site-1"),
            other => panic!("expected Ref, got {other:?}"),
        }
        match entity.get("equipRef") {
            Some(Kind::Ref(r)) => assert_eq!(r.val, "equip-1"),
            other => panic!("expected Ref, got {other:?}"),
        }
        assert_eq!(entity.get("dis"), Some(&Kind::Str("Main Site".to_string())));
    }

    #[test]
    fn strip_prefix_refs_ignores_non_matching() {
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("other-site-1")));

        strip_prefix_refs(&mut entity, "r1-");

        match entity.get("id") {
            Some(Kind::Ref(r)) => assert_eq!(r.val, "other-site-1"),
            other => panic!("expected Ref, got {other:?}"),
        }
    }

    #[test]
    fn derive_ws_url_from_http() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://remote:8080/api".to_string(),
            username: "u".to_string(),
            password: "p".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        assert_eq!(config.effective_ws_url(), "ws://remote:8080/api/ws");
    }

    #[test]
    fn derive_ws_url_from_https() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "https://remote:8443/api".to_string(),
            username: "u".to_string(),
            password: "p".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        assert_eq!(config.effective_ws_url(), "wss://remote:8443/api/ws");
    }

    #[test]
    fn explicit_ws_url_overrides_derived() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://remote:8080/api".to_string(),
            username: "u".to_string(),
            password: "p".to_string(),
            id_prefix: None,
            ws_url: Some("ws://custom:9999/ws".to_string()),
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        assert_eq!(config.effective_ws_url(), "ws://custom:9999/ws");
    }

    #[test]
    fn connector_tracks_entity_ids_in_ownership() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: Some("t-".to_string()),
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        let connector = Connector::new(config);
        assert!(!connector.owns("t-site-1"));

        // Simulate populating cache
        {
            let mut entity = HDict::new();
            entity.set("id", Kind::Ref(HRef::from_val("t-site-1")));
            connector.update_cache(vec![entity]);
        }

        assert!(connector.owns("t-site-1"));
        assert!(!connector.owns("other-1"));
    }

    #[test]
    fn connector_new_defaults_transport_and_connected() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        let connector = Connector::new(config);
        assert_eq!(connector.transport_mode(), TransportMode::Http);
        assert!(!connector.is_connected());
        assert!(connector.last_sync_time().is_none());
    }

    #[test]
    fn connector_config_new_fields_default_to_none() {
        let json = r#"{
            "name": "Minimal",
            "url": "http://remote:8080/api",
            "username": "user",
            "password": "pass"
        }"#;
        let config: ConnectorConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.ws_url, None);
        assert_eq!(config.sync_interval_secs, None);
        assert_eq!(config.client_cert, None);
        assert_eq!(config.client_key, None);
        assert_eq!(config.ca_cert, None);
    }

    #[test]
    fn remote_watch_add_and_remove() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: Some("r-".to_string()),
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        let connector = Connector::new(config);
        assert_eq!(connector.remote_watch_count(), 0);

        connector.add_remote_watch("r-site-1");
        assert_eq!(connector.remote_watch_count(), 1);

        connector.add_remote_watch("r-equip-2");
        assert_eq!(connector.remote_watch_count(), 2);

        // Duplicate add is idempotent (HashSet).
        connector.add_remote_watch("r-site-1");
        assert_eq!(connector.remote_watch_count(), 2);

        connector.remove_remote_watch("r-site-1");
        assert_eq!(connector.remote_watch_count(), 1);

        // Removing a non-existent ID is a no-op.
        connector.remove_remote_watch("r-nonexistent");
        assert_eq!(connector.remote_watch_count(), 1);

        connector.remove_remote_watch("r-equip-2");
        assert_eq!(connector.remote_watch_count(), 0);
    }

    fn make_test_entities() -> Vec<HDict> {
        let mut site = HDict::new();
        site.set("id", Kind::Ref(HRef::from_val("site-1")));
        site.set("site", Kind::Marker);
        site.set("dis", Kind::Str("Main Site".into()));

        let mut equip = HDict::new();
        equip.set("id", Kind::Ref(HRef::from_val("equip-1")));
        equip.set("equip", Kind::Marker);
        equip.set("siteRef", Kind::Ref(HRef::from_val("site-1")));

        let mut point = HDict::new();
        point.set("id", Kind::Ref(HRef::from_val("point-1")));
        point.set("point", Kind::Marker);
        point.set("sensor", Kind::Marker);
        point.set("equipRef", Kind::Ref(HRef::from_val("equip-1")));

        vec![site, equip, point]
    }

    #[test]
    fn filter_cached_returns_matching_entities() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        let connector = Connector::new(config);
        connector.update_cache(make_test_entities());

        // Filter for site
        let results = connector.filter_cached("site", 0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].get("id"),
            Some(&Kind::Ref(HRef::from_val("site-1")))
        );

        // Filter for equip
        let results = connector.filter_cached("equip", 0).unwrap();
        assert_eq!(results.len(), 1);

        // Filter for point and sensor
        let results = connector.filter_cached("point and sensor", 0).unwrap();
        assert_eq!(results.len(), 1);

        // Filter for something that doesn't exist
        let results = connector.filter_cached("ahu", 0).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn filter_cached_respects_limit() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        let connector = Connector::new(config);

        // Add multiple points
        let mut entities = Vec::new();
        for i in 0..10 {
            let mut p = HDict::new();
            p.set("id", Kind::Ref(HRef::from_val(format!("point-{i}"))));
            p.set("point", Kind::Marker);
            entities.push(p);
        }
        connector.update_cache(entities);

        let results = connector.filter_cached("point", 3).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn filter_cached_or_query() {
        let config = ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        };
        let connector = Connector::new(config);
        connector.update_cache(make_test_entities());

        // site or equip should return 2
        let results = connector.filter_cached("site or equip", 0).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn cache_version_starts_at_zero() {
        let connector = Connector::new(ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        });
        assert_eq!(connector.cache_version(), 0);
    }

    #[test]
    fn cache_version_increments_on_update() {
        let connector = Connector::new(ConnectorConfig {
            name: "test".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        });
        assert_eq!(connector.cache_version(), 0);

        connector.update_cache(vec![]);
        assert_eq!(connector.cache_version(), 1);

        let mut e = HDict::new();
        e.set("id", Kind::Ref(HRef::from_val("p-1")));
        connector.update_cache(vec![e]);
        assert_eq!(connector.cache_version(), 2);
    }

    #[test]
    fn connector_state_populated() {
        let connector = Connector::new(ConnectorConfig {
            name: "alpha".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
            domain: None,
        });

        let st = connector.state();
        assert_eq!(st.name, "alpha");
        assert!(!st.connected);
        assert_eq!(st.cache_version, 0);
        assert_eq!(st.entity_count, 0);
        assert!(st.last_sync_ts.is_none());
        assert!(st.staleness_secs.is_none());

        // After cache update, version and count change
        let mut e = HDict::new();
        e.set("id", Kind::Ref(HRef::from_val("s-1")));
        e.set("site", Kind::Marker);
        connector.update_cache(vec![e]);

        let st = connector.state();
        assert_eq!(st.cache_version, 1);
        assert_eq!(st.entity_count, 1);
    }
}
