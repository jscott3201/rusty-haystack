use std::collections::HashMap;

/// Cached result of a filter evaluation.
#[derive(Debug, Clone)]
struct CachedResult {
    /// Entity IDs matching the filter.
    entity_ids: Vec<String>,
    /// Graph version when this result was cached.
    graph_version: u64,
    /// Sum of connector cache versions when cached.
    connector_versions_sum: u64,
}

/// Per-session cache of filter → entity ID results.
///
/// Results are invalidated when graph_version or connector cache versions change.
/// Uses simple LRU eviction when capacity is exceeded.
pub struct WorkingSetCache {
    entries: HashMap<String, CachedResult>,
    capacity: usize,
    /// Tracks insertion order for LRU eviction.
    order: Vec<String>,
    hits: u64,
    misses: u64,
}

impl WorkingSetCache {
    /// Create a new cache with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            order: Vec::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Look up a cached filter result. Returns None if not cached or stale.
    pub fn get(
        &mut self,
        filter: &str,
        current_graph_version: u64,
        current_connector_versions_sum: u64,
    ) -> Option<&[String]> {
        if let Some(entry) = self.entries.get(filter) {
            if entry.graph_version == current_graph_version
                && entry.connector_versions_sum == current_connector_versions_sum
            {
                self.hits += 1;
                // Move to end of LRU order
                if let Some(pos) = self.order.iter().position(|k| k == filter) {
                    let key = self.order.remove(pos);
                    self.order.push(key);
                }
                return Some(&self.entries.get(filter).unwrap().entity_ids);
            }
            // Stale — remove
            self.entries.remove(filter);
            self.order.retain(|k| k != filter);
        }
        self.misses += 1;
        None
    }

    /// Insert a filter result into the cache.
    pub fn insert(
        &mut self,
        filter: String,
        entity_ids: Vec<String>,
        graph_version: u64,
        connector_versions_sum: u64,
    ) {
        // Evict if at capacity
        while self.entries.len() >= self.capacity && !self.order.is_empty() {
            let oldest = self.order.remove(0);
            self.entries.remove(&oldest);
        }

        self.order.retain(|k| k != &filter);
        self.order.push(filter.clone());
        self.entries.insert(
            filter,
            CachedResult {
                entity_ids,
                graph_version,
                connector_versions_sum,
            },
        );
    }

    /// Clear all cached entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Get cache statistics.
    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_on_same_version() {
        let mut cache = WorkingSetCache::new(16);
        cache.insert("site".into(), vec!["s1".into(), "s2".into()], 1, 10);
        let result = cache.get("site", 1, 10);
        assert_eq!(
            result,
            Some(vec!["s1".to_string(), "s2".to_string()].as_slice())
        );
    }

    #[test]
    fn cache_miss_on_graph_version_change() {
        let mut cache = WorkingSetCache::new(16);
        cache.insert("site".into(), vec!["s1".into()], 1, 10);
        assert!(cache.get("site", 2, 10).is_none());
    }

    #[test]
    fn cache_miss_on_connector_version_change() {
        let mut cache = WorkingSetCache::new(16);
        cache.insert("site".into(), vec!["s1".into()], 1, 10);
        assert!(cache.get("site", 1, 11).is_none());
    }

    #[test]
    fn lru_eviction() {
        let mut cache = WorkingSetCache::new(2);
        cache.insert("a".into(), vec!["1".into()], 1, 0);
        cache.insert("b".into(), vec!["2".into()], 1, 0);
        // Access "a" to make it most-recently-used
        cache.get("a", 1, 0);
        // Insert "c" — should evict "b" (least recently used)
        cache.insert("c".into(), vec!["3".into()], 1, 0);
        assert!(cache.get("b", 1, 0).is_none());
        assert!(cache.get("a", 1, 0).is_some());
        assert!(cache.get("c", 1, 0).is_some());
    }

    #[test]
    fn clear_empties_cache() {
        let mut cache = WorkingSetCache::new(16);
        cache.insert("site".into(), vec!["s1".into()], 1, 0);
        assert!(!cache.is_empty());
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn stats_tracking() {
        let mut cache = WorkingSetCache::new(16);
        cache.insert("site".into(), vec!["s1".into()], 1, 0);
        cache.get("site", 1, 0); // hit
        cache.get("site", 1, 0); // hit
        cache.get("missing", 1, 0); // miss
        let (hits, misses) = cache.stats();
        assert_eq!(hits, 2);
        assert_eq!(misses, 1);
    }
}
