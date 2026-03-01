//! Federation manager — coordinates multiple remote connectors for federated queries.

use crate::connector::{Connector, ConnectorConfig};
use haystack_core::data::HDict;

/// Manages multiple remote connectors for federated queries.
pub struct Federation {
    connectors: Vec<Connector>,
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
        self.connectors.push(Connector::new(config));
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

    /// Returns the number of connectors.
    pub fn connector_count(&self) -> usize {
        self.connectors.len()
    }

    /// Returns `(name, entity_count)` for each connector.
    pub fn status(&self) -> Vec<(String, usize)> {
        self.connectors
            .iter()
            .map(|c| (c.config.name.clone(), c.entity_count()))
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
        });
        assert_eq!(fed.connector_count(), 1);

        fed.add(ConnectorConfig {
            name: "server-2".to_string(),
            url: "http://localhost:8081/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: Some("s2-".to_string()),
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
        });
        fed.add(ConnectorConfig {
            name: "beta".to_string(),
            url: "http://beta:8080/api".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            id_prefix: Some("b-".to_string()),
        });

        let status = fed.status();
        assert_eq!(status.len(), 2);
        assert_eq!(status[0].0, "alpha");
        assert_eq!(status[0].1, 0); // no sync yet
        assert_eq!(status[1].0, "beta");
        assert_eq!(status[1].1, 0);
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
        });
        // No sync performed, so entities are empty.
        assert!(fed.all_cached_entities().is_empty());
    }
}
