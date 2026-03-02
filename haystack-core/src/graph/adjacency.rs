// Ref adjacency — bidirectional edge tracking for entity references.

use smallvec::SmallVec;
use std::collections::HashMap;

/// Bidirectional ref-edge tracking.
///
/// Maintains forward edges (entity -> targets) and reverse edges
/// (target -> sources) for efficient traversal in both directions.
pub struct RefAdjacency {
    /// entity_id -> [(ref_tag, target_ref_val)]
    forward: HashMap<usize, SmallVec<[(String, String); 4]>>,
    /// target_ref_val -> [(ref_tag, source_entity_id)]
    reverse: HashMap<String, SmallVec<[(String, usize); 4]>>,
}

impl RefAdjacency {
    /// Create an empty adjacency index.
    pub fn new() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        }
    }

    /// Record a ref edge from `entity_id` via `ref_tag` to `target_ref_val`.
    pub fn add(&mut self, entity_id: usize, ref_tag: &str, target_ref_val: &str) {
        self.forward
            .entry(entity_id)
            .or_default()
            .push((ref_tag.to_string(), target_ref_val.to_string()));
        self.reverse
            .entry(target_ref_val.to_string())
            .or_default()
            .push((ref_tag.to_string(), entity_id));
    }

    /// Remove all edges originating from `entity_id`.
    ///
    /// Cleans up both forward and reverse indices.
    pub fn remove(&mut self, entity_id: usize) {
        if let Some(edges) = self.forward.remove(&entity_id) {
            for (ref_tag, target) in edges {
                if let Some(rev) = self.reverse.get_mut(&target) {
                    rev.retain(|(rt, sid)| !(rt == &ref_tag && *sid == entity_id));
                    if rev.is_empty() {
                        self.reverse.remove(&target);
                    }
                }
            }
        }
    }

    /// Get target ref values from an entity, optionally filtered by ref type.
    ///
    /// If `ref_type` is `None`, returns all targets. Otherwise only targets
    /// reachable via the specified ref tag (e.g. "siteRef").
    pub fn targets_from(&self, entity_id: usize, ref_type: Option<&str>) -> Vec<String> {
        match self.forward.get(&entity_id) {
            Some(edges) => edges
                .iter()
                .filter(|(rt, _)| ref_type.is_none_or(|t| rt == t))
                .map(|(_, target)| target.clone())
                .collect(),
            None => Vec::new(),
        }
    }

    /// Get source entity ids that reference `target_ref_val`, optionally
    /// filtered by ref type.
    pub fn sources_to(&self, target_ref_val: &str, ref_type: Option<&str>) -> Vec<usize> {
        match self.reverse.get(target_ref_val) {
            Some(edges) => edges
                .iter()
                .filter(|(rt, _)| ref_type.is_none_or(|t| rt == t))
                .map(|(_, sid)| *sid)
                .collect(),
            None => Vec::new(),
        }
    }
}

impl Default for RefAdjacency {
    fn default() -> Self {
        Self::new()
    }
}

impl RefAdjacency {
    /// Read-only access to the forward edge map (for CSR construction).
    pub fn forward_raw(&self) -> &HashMap<usize, SmallVec<[(String, String); 4]>> {
        &self.forward
    }

    /// Read-only access to the reverse edge map (for CSR construction).
    pub fn reverse_raw(&self) -> &HashMap<String, SmallVec<[(String, usize); 4]>> {
        &self.reverse
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_forward_and_reverse_edges() {
        let mut adj = RefAdjacency::new();
        adj.add(0, "siteRef", "site-1");
        adj.add(0, "equipRef", "equip-1");

        let targets = adj.targets_from(0, None);
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&"site-1".to_string()));
        assert!(targets.contains(&"equip-1".to_string()));

        let sources = adj.sources_to("site-1", None);
        assert_eq!(sources, vec![0]);
    }

    #[test]
    fn remove_entity_edges() {
        let mut adj = RefAdjacency::new();
        adj.add(0, "siteRef", "site-1");
        adj.add(1, "siteRef", "site-1");

        adj.remove(0);

        assert!(adj.targets_from(0, None).is_empty());
        // Entity 1 should still reference site-1.
        let sources = adj.sources_to("site-1", None);
        assert_eq!(sources, vec![1]);
    }

    #[test]
    fn targets_from_with_type_filter() {
        let mut adj = RefAdjacency::new();
        adj.add(0, "siteRef", "site-1");
        adj.add(0, "equipRef", "equip-1");

        let site_targets = adj.targets_from(0, Some("siteRef"));
        assert_eq!(site_targets, vec!["site-1".to_string()]);

        let equip_targets = adj.targets_from(0, Some("equipRef"));
        assert_eq!(equip_targets, vec!["equip-1".to_string()]);

        let none_targets = adj.targets_from(0, Some("spaceRef"));
        assert!(none_targets.is_empty());
    }

    #[test]
    fn targets_from_without_type_filter() {
        let mut adj = RefAdjacency::new();
        adj.add(0, "siteRef", "site-1");
        adj.add(0, "equipRef", "equip-1");

        let all = adj.targets_from(0, None);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn sources_to_with_type_filter() {
        let mut adj = RefAdjacency::new();
        adj.add(0, "siteRef", "site-1");
        adj.add(1, "equipRef", "site-1");

        let site_sources = adj.sources_to("site-1", Some("siteRef"));
        assert_eq!(site_sources, vec![0]);

        let equip_sources = adj.sources_to("site-1", Some("equipRef"));
        assert_eq!(equip_sources, vec![1]);
    }

    #[test]
    fn sources_to_without_type_filter() {
        let mut adj = RefAdjacency::new();
        adj.add(0, "siteRef", "site-1");
        adj.add(1, "equipRef", "site-1");

        let all = adj.sources_to("site-1", None);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn targets_from_nonexistent_entity() {
        let adj = RefAdjacency::new();
        assert!(adj.targets_from(999, None).is_empty());
    }

    #[test]
    fn sources_to_nonexistent_target() {
        let adj = RefAdjacency::new();
        assert!(adj.sources_to("nonexistent", None).is_empty());
    }

    #[test]
    fn remove_nonexistent_entity_is_noop() {
        let mut adj = RefAdjacency::new();
        // Should not panic.
        adj.remove(999);
    }

    #[test]
    fn remove_cleans_up_reverse_entry() {
        let mut adj = RefAdjacency::new();
        adj.add(0, "siteRef", "site-1");

        adj.remove(0);

        // The reverse entry for site-1 should be gone entirely.
        assert!(adj.sources_to("site-1", None).is_empty());
    }
}
