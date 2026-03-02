//! Federation manager — coordinates multiple remote connectors for federated queries.

use std::collections::HashMap;
use std::sync::Arc;

use crate::connector::{Connector, ConnectorConfig};
use haystack_core::data::HDict;

/// TOML file structure for federation configuration.
///
/// Example:
/// ```toml
/// [connectors.building-a]
/// name = "Building A"
/// url = "http://building-a:8080/api"
/// username = "federation"
/// password = "s3cret"
/// ```
#[derive(serde::Deserialize)]
struct FederationToml {
    connectors: HashMap<String, ConnectorConfig>,
}

/// Manages multiple remote connectors for federated queries.
pub struct Federation {
    pub connectors: Vec<Arc<Connector>>,
}

impl Federation {
    /// Create a new federation with no connectors.
    pub fn new() -> Self {
        Self {
            connectors: Vec::new(),
        }
    }

    /// Add a connector for a remote Haystack server.
    pub fn add(&mut self, config: ConnectorConfig) {
        self.connectors.push(Arc::new(Connector::new(config)));
    }

    /// Sync a single connector by name, returning the entity count on success.
    pub async fn sync_one(&self, name: &str) -> Result<usize, String> {
        for connector in &self.connectors {
            if connector.config.name == name {
                return connector.sync().await;
            }
        }
        Err(format!("connector not found: {name}"))
    }

    /// Sync all connectors, returning a vec of (name, result) pairs.
    ///
    /// Each result is either `Ok(count)` with the number of entities synced,
    /// or `Err(message)` with the error description.
    pub async fn sync_all(&self) -> Vec<(String, Result<usize, String>)> {
        let mut results = Vec::new();
        for connector in &self.connectors {
            let name = connector.config.name.clone();
            let result = connector.sync().await;
            results.push((name, result));
        }
        results
    }

    /// Returns all cached entities from all connectors, merged into a single vec.
    pub fn all_cached_entities(&self) -> Vec<HDict> {
        let mut all = Vec::new();
        for connector in &self.connectors {
            all.extend(connector.cached_entities());
        }
        all
    }

    /// Filter cached entities across all connectors using bitmap-accelerated queries.
    ///
    /// Each connector uses its own bitmap tag index for fast filtering, then
    /// results are merged up to the given limit. Much faster than linear scan
    /// for tag-based queries over large federated entity sets.
    pub fn filter_cached_entities(
        &self,
        filter_expr: &str,
        limit: usize,
    ) -> Result<Vec<HDict>, String> {
        let effective_limit = if limit == 0 { usize::MAX } else { limit };
        let mut all = Vec::new();
        for connector in &self.connectors {
            if all.len() >= effective_limit {
                break;
            }
            let remaining = effective_limit - all.len();
            let results = connector.filter_cached(filter_expr, remaining)?;
            all.extend(results);
        }
        Ok(all)
    }

    /// Returns the number of connectors.
    pub fn connector_count(&self) -> usize {
        self.connectors.len()
    }

    /// Returns the connector that owns the entity with the given ID, if any.
    pub fn owner_of(&self, id: &str) -> Option<&Arc<Connector>> {
        self.connectors.iter().find(|c| c.owns(id))
    }

    /// Batch read entities by ID across federated connectors.
    ///
    /// Groups IDs by owning connector and fetches each group in a single
    /// indexed lookup (O(1) per ID via `cache_id_map`), avoiding repeated
    /// linear scans. Returns `(found_entities, missing_ids)`.
    pub fn batch_read_by_id<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> (Vec<HDict>, Vec<String>) {
        // Group IDs by connector index.
        let mut groups: HashMap<usize, Vec<&str>> = HashMap::new();
        let mut not_owned: Vec<String> = Vec::new();

        for id in ids {
            let mut found = false;
            for (idx, connector) in self.connectors.iter().enumerate() {
                if connector.owns(id) {
                    groups.entry(idx).or_default().push(id);
                    found = true;
                    break;
                }
            }
            if !found {
                not_owned.push(id.to_string());
            }
        }

        // Fetch each group from its connector in a single pass.
        let mut all_found = Vec::new();
        for (idx, ids) in &groups {
            let (found, mut missing) = self.connectors[*idx].batch_get_cached(ids);
            all_found.extend(found);
            not_owned.append(&mut missing);
        }

        (all_found, not_owned)
    }

    /// Returns `(name, entity_count)` for each connector.
    pub fn status(&self) -> Vec<(String, usize)> {
        self.connectors
            .iter()
            .map(|c| (c.config.name.clone(), c.entity_count()))
            .collect()
    }

    /// Parse a TOML string into a `Federation`, adding each connector defined
    /// under `[connectors.<key>]`.
    pub fn from_toml_str(toml_str: &str) -> Result<Self, String> {
        let parsed: FederationToml =
            toml::from_str(toml_str).map_err(|e| format!("invalid federation TOML: {e}"))?;
        let mut fed = Self::new();
        for (_key, config) in parsed.connectors {
            fed.add(config);
        }
        Ok(fed)
    }

    /// Read a TOML file from disk and parse it into a `Federation`.
    pub fn from_toml_file(path: &str) -> Result<Self, String> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
        Self::from_toml_str(&contents)
    }

    /// Start background sync tasks for all connectors.
    ///
    /// Each connector gets its own tokio task that loops at its configured
    /// sync interval, reconnecting automatically on failure.
    /// Returns the join handles (they run until the server shuts down).
    pub fn start_background_sync(&self) -> Vec<tokio::task::JoinHandle<()>> {
        self.connectors
            .iter()
            .map(|c| Connector::spawn_sync_task(Arc::clone(c)))
            .collect()
    }
}

impl Default for Federation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_core::kinds::{HRef, Kind};

    #[test]
    fn federation_new_empty() {
        let fed = Federation::new();
        assert_eq!(fed.connector_count(), 0);
        assert!(fed.all_cached_entities().is_empty());
        assert!(fed.status().is_empty());
    }

    #[test]
    fn federation_add_connector() {
        let mut fed = Federation::new();
        assert_eq!(fed.connector_count(), 0);

        fed.add(ConnectorConfig {
            name: "server-1".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
        });
        assert_eq!(fed.connector_count(), 1);

        fed.add(ConnectorConfig {
            name: "server-2".to_string(),
            url: "http://localhost:8081/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: Some("s2-".to_string()),
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
        });
        assert_eq!(fed.connector_count(), 2);
    }

    #[test]
    fn federation_status_empty() {
        let fed = Federation::new();
        let status = fed.status();
        assert!(status.is_empty());
    }

    #[test]
    fn federation_status_with_connectors() {
        let mut fed = Federation::new();
        fed.add(ConnectorConfig {
            name: "alpha".to_string(),
            url: "http://alpha:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
        });
        fed.add(ConnectorConfig {
            name: "beta".to_string(),
            url: "http://beta:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: Some("b-".to_string()),
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
        });

        let status = fed.status();
        assert_eq!(status.len(), 2);
        assert_eq!(status[0].0, "alpha");
        assert_eq!(status[0].1, 0); // no sync yet
        assert_eq!(status[1].0, "beta");
        assert_eq!(status[1].1, 0);
    }

    #[test]
    fn federation_owner_of_returns_correct_connector() {
        let mut fed = Federation::new();
        fed.add(ConnectorConfig {
            name: "alpha".to_string(),
            url: "http://alpha:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: Some("a-".to_string()),
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
        });

        // Simulate cache population for alpha
        fed.connectors[0].update_cache(vec![{
            let mut d = HDict::new();
            d.set("id", Kind::Ref(HRef::from_val("a-site-1")));
            d
        }]);

        assert!(fed.owner_of("a-site-1").is_some());
        assert_eq!(fed.owner_of("a-site-1").unwrap().config.name, "alpha");
        assert!(fed.owner_of("unknown-1").is_none());
    }

    #[test]
    fn federation_from_toml_str() {
        let toml = r#"
[connectors.building-a]
name = "Building A"
url = "http://building-a:8080/api"
username = "federation"
password = "s3cret"
id_prefix = "bldg-a-"
sync_interval_secs = 30

[connectors.building-b]
name = "Building B"
url = "https://building-b:8443/api"
username = "federation"
password = "s3cret"
id_prefix = "bldg-b-"
client_cert = "/etc/certs/federation.pem"
client_key = "/etc/certs/federation-key.pem"
ca_cert = "/etc/certs/ca.pem"
"#;
        let fed = Federation::from_toml_str(toml).unwrap();
        assert_eq!(fed.connector_count(), 2);
        let status = fed.status();
        let names: Vec<&str> = status.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"Building A"));
        assert!(names.contains(&"Building B"));
    }

    #[test]
    fn federation_from_toml_str_empty() {
        let toml = "[connectors]\n";
        let fed = Federation::from_toml_str(toml).unwrap();
        assert_eq!(fed.connector_count(), 0);
    }

    #[test]
    fn federation_from_toml_str_invalid() {
        let toml = "not valid toml {{{}";
        assert!(Federation::from_toml_str(toml).is_err());
    }

    #[test]
    fn federation_all_cached_entities_empty() {
        let mut fed = Federation::new();
        fed.add(ConnectorConfig {
            name: "server".to_string(),
            url: "http://localhost:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: None,
            ws_url: None,
            sync_interval_secs: None,
            client_cert: None,
            client_key: None,
            ca_cert: None,
        });
        // No sync performed, so entities are empty.
        assert!(fed.all_cached_entities().is_empty());
    }
}
