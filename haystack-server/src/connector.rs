//! Connector for fetching entities from a remote Haystack server.

use parking_lot::RwLock;

use haystack_client::HaystackClient;
use haystack_core::data::HDict;
use haystack_core::kinds::{HRef, Kind};

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
}

/// A connector that can fetch entities from a remote Haystack server.
pub struct Connector {
    pub config: ConnectorConfig,
    /// Cached entities from last sync.
    cache: RwLock<Vec<HDict>>,
}

impl Connector {
    /// Create a new connector with an empty cache.
    pub fn new(config: ConnectorConfig) -> Self {
        Self {
            config,
            cache: RwLock::new(Vec::new()),
        }
    }

    /// Connect to the remote server, fetch all entities, apply id prefixing,
    /// and store them in the cache. Returns the count of entities synced.
    pub async fn sync(&self) -> Result<usize, String> {
        let client = HaystackClient::connect(
            &self.config.url,
            &self.config.username,
            &self.config.password,
        )
        .await
        .map_err(|e| format!("connection failed: {e}"))?;

        let grid = client
            .read("*", None)
            .await
            .map_err(|e| format!("read failed: {e}"))?;

        let mut entities: Vec<HDict> = grid.rows.into_iter().collect();

        // Apply id prefix if configured.
        if let Some(ref prefix) = self.config.id_prefix {
            for entity in &mut entities {
                prefix_refs(entity, prefix);
            }
        }

        let count = entities.len();
        *self.cache.write() = entities;
        Ok(count)
    }

    /// Returns a clone of all cached entities.
    pub fn cached_entities(&self) -> Vec<HDict> {
        self.cache.read().clone()
    }

    /// Returns the number of cached entities.
    pub fn entity_count(&self) -> usize {
        self.cache.read().len()
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
}
