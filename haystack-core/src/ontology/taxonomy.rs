// TaxonomyTree -- unified type hierarchy for Haystack 4 defs.

use std::collections::{HashMap, HashSet, VecDeque};
use parking_lot::RwLock;

/// Unified inheritance graph for Haystack 4 defs.
///
/// Supports multiple inheritance (a def can have multiple supertypes via
/// the `is` tag). Pre-computes mandatory marker sets on first access
/// for fast `fits()` evaluation.
pub struct TaxonomyTree {
    /// child -> parent symbols
    parents: HashMap<String, Vec<String>>,
    /// parent -> child symbols
    children: HashMap<String, Vec<String>>,
    /// Cached mandatory tag sets per type (RwLock for thread safety)
    mandatory_cache: RwLock<HashMap<String, HashSet<String>>>,
}

impl TaxonomyTree {
    /// Create an empty taxonomy tree.
    pub fn new() -> Self {
        Self {
            parents: HashMap::new(),
            children: HashMap::new(),
            mandatory_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Register a type with its supertypes.
    pub fn add(&mut self, name: &str, supertypes: &[String]) {
        self.parents.entry(name.to_string()).or_default();
        for parent in supertypes {
            let parents_list = self.parents.entry(name.to_string()).or_default();
            if !parents_list.contains(parent) {
                parents_list.push(parent.clone());
            }
            let children_list = self.children.entry(parent.clone()).or_default();
            if !children_list.contains(&name.to_string()) {
                children_list.push(name.to_string());
            }
            // Ensure parent exists in parents map
            self.parents.entry(parent.clone()).or_default();
        }
        // Invalidate cache
        self.mandatory_cache.write().clear();
    }

    /// Check if `child` is a subtype of `parent`.
    ///
    /// Uses BFS up the parent chain. Returns `true` if `child`
    /// equals `parent` or if `parent` is an ancestor.
    pub fn is_subtype(&self, child: &str, parent: &str) -> bool {
        if child == parent {
            return true;
        }
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        queue.push_back(child);

        while let Some(current) = queue.pop_front() {
            if !visited.insert(current) {
                continue;
            }
            if let Some(parents) = self.parents.get(current) {
                for p in parents {
                    if p == parent {
                        return true;
                    }
                    queue.push_back(p.as_str());
                }
            }
        }
        false
    }

    /// Full ancestor chain (transitive, breadth-first).
    ///
    /// Returns all ancestors of `name`, nearest first.
    pub fn supertypes_of(&self, name: &str) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        if let Some(parents) = self.parents.get(name) {
            for p in parents {
                queue.push_back(p.clone());
            }
        }

        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            result.push(current.clone());
            if let Some(parents) = self.parents.get(&current) {
                for p in parents {
                    queue.push_back(p.clone());
                }
            }
        }
        result
    }

    /// Direct children of a type.
    pub fn subtypes_of(&self, name: &str) -> Vec<String> {
        self.children
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// All descendants (transitive, breadth-first).
    pub fn all_subtypes(&self, name: &str) -> Vec<String> {
        let mut result: Vec<String> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        if let Some(children) = self.children.get(name) {
            for c in children {
                queue.push_back(c.clone());
            }
        }

        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            result.push(current.clone());
            if let Some(children) = self.children.get(&current) {
                for c in children {
                    queue.push_back(c.clone());
                }
            }
        }
        result
    }

    /// Get mandatory marker tags for a type.
    ///
    /// Walks the supertype chain and collects all types that are
    /// marked as mandatory. Results are cached.
    pub fn mandatory_tags(
        &self,
        name: &str,
        mandatory_defs: &HashSet<String>,
    ) -> HashSet<String> {
        if let Some(cached) = self.mandatory_cache.read().get(name) {
            return cached.clone();
        }

        let mut tags: HashSet<String> = HashSet::new();

        // Include self if mandatory
        if mandatory_defs.contains(name) {
            tags.insert(name.to_string());
        }

        // Walk supertypes
        let supertypes = self.supertypes_of(name);
        for sup in &supertypes {
            if mandatory_defs.contains(sup) {
                tags.insert(sup.clone());
            }
        }

        self.mandatory_cache.write().insert(name.to_string(), tags.clone());
        tags
    }

    /// Check if a type is registered in the taxonomy.
    pub fn contains(&self, name: &str) -> bool {
        self.parents.contains_key(name)
    }

    /// Number of registered types.
    pub fn len(&self) -> usize {
        self.parents.len()
    }

    /// Returns true if no types are registered.
    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }

    /// Clear the mandatory tag cache (used after library unload).
    pub fn clear_cache(&self) {
        self.mandatory_cache.write().clear();
    }
}

impl Default for TaxonomyTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tree() -> TaxonomyTree {
        let mut tree = TaxonomyTree::new();
        // marker
        //   ├─ entity
        //   │    ├─ equip
        //   │    │    ├─ ahu
        //   │    │    └─ meter
        //   │    └─ point
        //   └─ val
        tree.add("marker", &[]);
        tree.add("entity", &["marker".to_string()]);
        tree.add("equip", &["entity".to_string()]);
        tree.add("ahu", &["equip".to_string()]);
        tree.add("meter", &["equip".to_string()]);
        tree.add("point", &["entity".to_string()]);
        tree.add("val", &["marker".to_string()]);
        tree
    }

    #[test]
    fn is_subtype_self() {
        let tree = build_tree();
        assert!(tree.is_subtype("ahu", "ahu"));
    }

    #[test]
    fn is_subtype_direct_parent() {
        let tree = build_tree();
        assert!(tree.is_subtype("ahu", "equip"));
    }

    #[test]
    fn is_subtype_ancestor() {
        let tree = build_tree();
        assert!(tree.is_subtype("ahu", "entity"));
        assert!(tree.is_subtype("ahu", "marker"));
    }

    #[test]
    fn is_subtype_false_for_unrelated() {
        let tree = build_tree();
        assert!(!tree.is_subtype("ahu", "point"));
        assert!(!tree.is_subtype("ahu", "val"));
    }

    #[test]
    fn is_subtype_false_for_child() {
        let tree = build_tree();
        // equip is NOT a subtype of ahu (it's the other way around)
        assert!(!tree.is_subtype("equip", "ahu"));
    }

    #[test]
    fn supertypes_of_bfs_order() {
        let tree = build_tree();
        let supers = tree.supertypes_of("ahu");
        // BFS: equip first, then entity (from equip), then marker (from entity)
        assert_eq!(supers, vec!["equip", "entity", "marker"]);
    }

    #[test]
    fn supertypes_of_root() {
        let tree = build_tree();
        let supers = tree.supertypes_of("marker");
        assert!(supers.is_empty());
    }

    #[test]
    fn subtypes_of_direct_children() {
        let tree = build_tree();
        let mut children = tree.subtypes_of("equip");
        children.sort();
        assert_eq!(children, vec!["ahu", "meter"]);
    }

    #[test]
    fn subtypes_of_leaf() {
        let tree = build_tree();
        let children = tree.subtypes_of("ahu");
        assert!(children.is_empty());
    }

    #[test]
    fn all_subtypes_full_tree() {
        let tree = build_tree();
        let mut descendants = tree.all_subtypes("entity");
        descendants.sort();
        assert_eq!(descendants, vec!["ahu", "equip", "meter", "point"]);
    }

    #[test]
    fn all_subtypes_leaf() {
        let tree = build_tree();
        let descendants = tree.all_subtypes("ahu");
        assert!(descendants.is_empty());
    }

    #[test]
    fn mandatory_tags_basic() {
        let tree = build_tree();
        let mut mandatory_defs = HashSet::new();
        mandatory_defs.insert("equip".to_string());
        mandatory_defs.insert("entity".to_string());

        let tags = tree.mandatory_tags("ahu", &mandatory_defs);
        assert!(tags.contains("equip"));
        assert!(tags.contains("entity"));
        assert!(!tags.contains("marker"));
        assert!(!tags.contains("ahu"));
    }

    #[test]
    fn mandatory_tags_self_mandatory() {
        let tree = build_tree();
        let mut mandatory_defs = HashSet::new();
        mandatory_defs.insert("ahu".to_string());

        let tags = tree.mandatory_tags("ahu", &mandatory_defs);
        assert!(tags.contains("ahu"));
    }

    #[test]
    fn mandatory_tags_caching() {
        let tree = build_tree();
        let mandatory_defs: HashSet<String> = ["equip".to_string()].into();

        let tags1 = tree.mandatory_tags("ahu", &mandatory_defs);
        let tags2 = tree.mandatory_tags("ahu", &mandatory_defs);
        assert_eq!(tags1, tags2);
    }

    #[test]
    fn contains_and_len() {
        let tree = build_tree();
        assert!(tree.contains("ahu"));
        assert!(tree.contains("marker"));
        assert!(!tree.contains("nonexistent"));
        assert_eq!(tree.len(), 7);
    }

    #[test]
    fn empty_tree() {
        let tree = TaxonomyTree::new();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn multiple_inheritance() {
        let mut tree = TaxonomyTree::new();
        tree.add("a", &[]);
        tree.add("b", &[]);
        tree.add("c", &["a".to_string(), "b".to_string()]);

        assert!(tree.is_subtype("c", "a"));
        assert!(tree.is_subtype("c", "b"));

        let supers = tree.supertypes_of("c");
        assert_eq!(supers.len(), 2);
        assert!(supers.contains(&"a".to_string()));
        assert!(supers.contains(&"b".to_string()));
    }
}
