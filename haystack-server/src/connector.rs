//! Connector for fetching entities from a remote Haystack server.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;

use haystack_client::HaystackClient;
use haystack_client::transport::http::HttpTransport;
use haystack_client::transport::ws::WsTransport;
use haystack_core::data::{HDict, HGrid};
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
}

/// A connector that can fetch entities from a remote Haystack server.
pub struct Connector {
    pub config: ConnectorConfig,
    /// Cached entities from last sync.
    cache: RwLock<Vec<HDict>>,
    /// Set of entity IDs owned by this connector (populated during sync/update_cache).
    owned_ids: RwLock<HashSet<String>>,
    /// Entity IDs being watched remotely (prefixed IDs).
    remote_watch_ids: RwLock<HashSet<String>>,
    /// Transport protocol (HTTP or WebSocket).
    transport_mode: AtomicU8,
    /// Whether the connector is currently connected (last sync succeeded).
    connected: AtomicBool,
    /// Timestamp of the last successful sync.
    last_sync: RwLock<Option<DateTime<Utc>>>,
    /// Persistent client connection used by sync and background tasks.
    /// Lazily initialized on first sync. Cleared on connection error.
    /// Uses `tokio::sync::RwLock` because the guard is held across `.await`.
    client: tokio::sync::RwLock<Option<ConnectorClient>>,
}

impl Connector {
    /// Create a new connector with an empty cache.
    pub fn new(config: ConnectorConfig) -> Self {
        Self {
            config,
            cache: RwLock::new(Vec::new()),
            owned_ids: RwLock::new(HashSet::new()),
            remote_watch_ids: RwLock::new(HashSet::new()),
            transport_mode: AtomicU8::new(TransportMode::Http as u8),
            connected: AtomicBool::new(false),
            last_sync: RwLock::new(None),
            client: tokio::sync::RwLock::new(None),
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

    /// Connect to the remote server, fetch all entities, apply id prefixing,
    /// and store them in the cache. Returns the count of entities synced.
    ///
    /// Uses the persistent client if available, otherwise establishes a new
    /// connection (WS-first, falling back to HTTP).
    pub async fn sync(&self) -> Result<usize, String> {
        // Ensure we have a connection
        if self.client.read().await.is_none() {
            self.connect_persistent().await?;
        }

        // Use the persistent client to read all entities
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
        Ok(count)
    }

    /// Replace the cached entities and rebuild the owned-ID index.
    ///
    /// Extracts all `id` Ref values from the given entities into a `HashSet`,
    /// then atomically replaces both the entity cache and the ownership set.
    pub fn update_cache(&self, entities: Vec<HDict>) {
        let ids: HashSet<String> = entities
            .iter()
            .filter_map(|e| match e.get("id") {
                Some(Kind::Ref(r)) => Some(r.val.clone()),
                _ => None,
            })
            .collect();
        *self.cache.write() = entities;
        *self.owned_ids.write() = ids;
    }

    /// Returns `true` if this connector owns an entity with the given ID.
    pub fn owns(&self, id: &str) -> bool {
        self.owned_ids.read().contains(id)
    }

    /// Returns a clone of all cached entities.
    pub fn cached_entities(&self) -> Vec<HDict> {
        self.cache.read().clone()
    }

    /// Returns the number of cached entities.
    pub fn entity_count(&self) -> usize {
        self.cache.read().len()
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

    /// Create a fresh HTTP connection to the remote.
    ///
    // TODO: Reuse the persistent client for proxy methods once we have a
    // better abstraction over ConnectorClient that exposes typed ops.
    async fn connect_remote(
        &self,
    ) -> Result<
        HaystackClient<haystack_client::transport::http::HttpTransport>,
        String,
    > {
        HaystackClient::connect(
            &self.config.url,
            &self.config.username,
            &self.config.password,
        )
        .await
        .map_err(|e| format!("connection failed: {e}"))
    }

    /// Proxy a hisRead request to the remote server.
    pub async fn proxy_his_read(
        &self,
        prefixed_id: &str,
        range: &str,
    ) -> Result<HGrid, String> {
        let id = self.strip_id(prefixed_id);
        let client = self.connect_remote().await?;
        client
            .his_read(&id, range)
            .await
            .map_err(|e| format!("hisRead failed: {e}"))
    }

    /// Proxy a pointWrite request to the remote server.
    pub async fn proxy_point_write(
        &self,
        prefixed_id: &str,
        level: u8,
        val: &Kind,
    ) -> Result<HGrid, String> {
        let id = self.strip_id(prefixed_id);
        let client = self.connect_remote().await?;
        client
            .point_write(&id, level, val.clone())
            .await
            .map_err(|e| format!("pointWrite failed: {e}"))
    }

    /// Proxy a hisWrite request to the remote server.
    pub async fn proxy_his_write(
        &self,
        prefixed_id: &str,
        items: Vec<HDict>,
    ) -> Result<HGrid, String> {
        let id = self.strip_id(prefixed_id);
        let client = self.connect_remote().await?;
        client
            .his_write(&id, items)
            .await
            .map_err(|e| format!("hisWrite failed: {e}"))
    }

    /// Proxy an import request for a single entity to the remote server.
    ///
    /// Strips the id prefix from the entity, wraps it in a single-row grid,
    /// and calls the remote `import` op.
    pub async fn proxy_import(&self, entity: &HDict) -> Result<HGrid, String> {
        use crate::connector::strip_prefix_refs;

        let mut stripped = entity.clone();
        if let Some(ref prefix) = self.config.id_prefix {
            strip_prefix_refs(&mut stripped, prefix);
        }

        // Build a single-row grid with columns from the entity.
        let col_names: Vec<String> = stripped.tag_names().map(|s| s.to_string()).collect();
        let cols: Vec<haystack_core::data::HCol> =
            col_names.iter().map(|n| haystack_core::data::HCol::new(n.as_str())).collect();
        let grid = HGrid::from_parts(HDict::new(), cols, vec![stripped]);

        let client = self.connect_remote().await?;
        client
            .call("import", &grid)
            .await
            .map_err(|e| format!("import failed: {e}"))
    }

    /// Proxy an invokeAction request to the remote server.
    pub async fn proxy_invoke_action(
        &self,
        prefixed_id: &str,
        action: &str,
        args: HDict,
    ) -> Result<HGrid, String> {
        let id = self.strip_id(prefixed_id);
        let client = self.connect_remote().await?;
        client
            .invoke_action(&id, action, args)
            .await
            .map_err(|e| format!("invokeAction failed: {e}"))
    }

    /// Spawn a background sync task for this connector.
    ///
    /// The task loops forever, syncing entities at the configured interval.
    /// On error, the persistent client is cleared to force reconnection on
    /// the next iteration.
    pub fn spawn_sync_task(connector: Arc<Connector>) -> tokio::task::JoinHandle<()> {
        let interval_secs = connector.config.effective_sync_interval_secs();
        tokio::spawn(async move {
            loop {
                match connector.sync().await {
                    Ok(count) => {
                        log::debug!(
                            "Synced {} entities from {}",
                            count,
                            connector.config.name
                        );
                    }
                    Err(e) => {
                        log::error!("Sync failed for {}: {}", connector.config.name, e);
                        // Clear the client on error to force reconnection
                        *connector.client.write().await = None;
                        connector.connected.store(false, Ordering::Relaxed);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
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
        assert_eq!(config.client_cert, Some("/etc/certs/client.pem".to_string()));
        assert_eq!(config.client_key, Some("/etc/certs/client-key.pem".to_string()));
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
}
