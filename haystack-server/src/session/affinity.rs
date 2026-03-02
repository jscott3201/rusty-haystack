use std::collections::HashMap;
use std::time::Instant;

/// Tracks per-session connector hit counts for reranking optimization.
pub struct ConnectorAffinity {
    /// Connector name → hit count since last rerank.
    hits: HashMap<String, u64>,
    /// Ranked connector names (most-accessed first).
    ranked: Vec<String>,
    /// Last rerank timestamp.
    last_rerank: Instant,
    /// Rerank interval in seconds.
    rerank_interval_secs: u64,
    /// Cache of entity_id → owning connector name for fast hisRead routing.
    ownership_cache: HashMap<String, String>,
}

impl ConnectorAffinity {
    pub fn new() -> Self {
        Self {
            hits: HashMap::new(),
            ranked: Vec::new(),
            last_rerank: Instant::now(),
            rerank_interval_secs: 60,
            ownership_cache: HashMap::new(),
        }
    }

    /// Record a hit on a connector.
    pub fn record_hit(&mut self, connector: &str) {
        *self.hits.entry(connector.to_string()).or_insert(0) += 1;
        self.maybe_rerank();
    }

    /// Record entity ownership for fast routing.
    pub fn record_ownership(&mut self, entity_id: &str, connector: &str) {
        self.ownership_cache
            .insert(entity_id.to_string(), connector.to_string());
    }

    /// Get the ranked connector order (most-accessed first).
    pub fn ranked_connectors(&self) -> &[String] {
        &self.ranked
    }

    /// Look up which connector owns an entity (cached).
    pub fn owner_of(&self, entity_id: &str) -> Option<&str> {
        self.ownership_cache.get(entity_id).map(|s| s.as_str())
    }

    fn maybe_rerank(&mut self) {
        if self.last_rerank.elapsed().as_secs() >= self.rerank_interval_secs {
            self.rerank();
        }
    }

    fn rerank(&mut self) {
        let mut pairs: Vec<_> = self.hits.iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(a.1));
        self.ranked = pairs.into_iter().map(|(k, _)| k.clone()).collect();
        self.hits.clear();
        self.last_rerank = Instant::now();
    }
}

impl Default for ConnectorAffinity {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_hit_and_ownership() {
        let mut aff = ConnectorAffinity::new();
        aff.record_hit("conn_a");
        aff.record_hit("conn_b");
        aff.record_ownership("entity1", "conn_a");
        assert_eq!(aff.owner_of("entity1"), Some("conn_a"));
        assert_eq!(aff.owner_of("missing"), None);
    }

    #[test]
    fn rerank_orders_by_hits() {
        let mut aff = ConnectorAffinity::new();
        // Accumulate hits without triggering auto-rerank
        aff.record_hit("low");
        aff.record_hit("high");
        aff.record_hit("high");
        aff.record_hit("high");

        // Force a rerank
        aff.rerank();

        let ranked = aff.ranked_connectors();
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0], "high");
        assert_eq!(ranked[1], "low");
    }

    #[test]
    fn ownership_cache_overwrites() {
        let mut aff = ConnectorAffinity::new();
        aff.record_ownership("e1", "conn_a");
        assert_eq!(aff.owner_of("e1"), Some("conn_a"));
        aff.record_ownership("e1", "conn_b");
        assert_eq!(aff.owner_of("e1"), Some("conn_b"));
    }

    #[test]
    fn default_creates_empty() {
        let aff = ConnectorAffinity::default();
        assert!(aff.ranked_connectors().is_empty());
        assert_eq!(aff.owner_of("any"), None);
    }
}
