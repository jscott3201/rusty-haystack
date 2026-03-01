// EntityGraph — in-memory entity store with bitmap indexing and ref adjacency.

use std::collections::HashMap;

use crate::data::{HCol, HDict, HGrid};
use crate::filter::{matches_with_ns, parse_filter};
use crate::kinds::{HRef, Kind};
use crate::ontology::{DefNamespace, ValidationIssue};

use super::adjacency::RefAdjacency;
use super::bitmap::TagBitmapIndex;
use super::changelog::{DiffOp, GraphDiff};
use super::query_planner;

/// Errors returned by EntityGraph operations.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("entity missing 'id' tag")]
    MissingId,
    #[error("entity id must be a Ref")]
    InvalidId,
    #[error("entity already exists: {0}")]
    DuplicateRef(String),
    #[error("entity not found: {0}")]
    NotFound(String),
    #[error("filter error: {0}")]
    Filter(String),
}

/// Core entity graph with bitmap tag indexing and bidirectional ref adjacency.
pub struct EntityGraph {
    /// ref_val -> entity dict
    entities: HashMap<String, HDict>,
    /// ref_val -> internal numeric id (for bitmap indexing)
    id_map: HashMap<String, usize>,
    /// internal numeric id -> ref_val
    reverse_id: HashMap<usize, String>,
    /// Next internal id to assign.
    next_id: usize,
    /// Tag bitmap index for fast has/missing queries.
    tag_index: TagBitmapIndex,
    /// Bidirectional ref adjacency for graph traversal.
    adjacency: RefAdjacency,
    /// Optional ontology namespace for spec-aware operations.
    namespace: Option<DefNamespace>,
    /// Monotonic version counter, incremented on every mutation.
    version: u64,
    /// Ordered list of mutations.
    changelog: Vec<GraphDiff>,
}

const MAX_CHANGELOG: usize = 10_000;

impl EntityGraph {
    /// Create an empty entity graph.
    pub fn new() -> Self {
        Self {
            entities: HashMap::new(),
            id_map: HashMap::new(),
            reverse_id: HashMap::new(),
            next_id: 0,
            tag_index: TagBitmapIndex::new(),
            adjacency: RefAdjacency::new(),
            namespace: None,
            version: 0,
            changelog: Vec::new(),
        }
    }

    /// Create an entity graph with an ontology namespace.
    pub fn with_namespace(ns: DefNamespace) -> Self {
        Self {
            namespace: Some(ns),
            ..Self::new()
        }
    }

    // ── CRUD ──

    /// Add an entity to the graph.
    ///
    /// The entity must have an `id` tag that is a `Ref`. Returns the ref
    /// value string on success.
    pub fn add(&mut self, entity: HDict) -> Result<String, GraphError> {
        let ref_val = extract_ref_val(&entity)?;

        if self.entities.contains_key(&ref_val) {
            return Err(GraphError::DuplicateRef(ref_val));
        }

        let eid = self.next_id;
        self.next_id += 1;

        self.id_map.insert(ref_val.clone(), eid);
        self.reverse_id.insert(eid, ref_val.clone());

        // Index before inserting (borrows entity immutably, self mutably).
        self.index_tags(eid, &entity);
        self.index_refs(eid, &entity);

        // Clone for the changelog, then move the entity into the map.
        let entity_for_log = entity.clone();
        self.entities.insert(ref_val.clone(), entity);

        self.version += 1;
        self.push_changelog(GraphDiff {
            version: self.version,
            op: DiffOp::Add,
            ref_val: ref_val.clone(),
            old: None,
            new: Some(entity_for_log),
        });

        Ok(ref_val)
    }

    /// Get a reference to an entity by ref value.
    pub fn get(&self, ref_val: &str) -> Option<&HDict> {
        self.entities.get(ref_val)
    }

    /// Update an existing entity by merging `changes` into it.
    ///
    /// Tags in `changes` overwrite existing tags; `Kind::Remove` tags are
    /// deleted. The `id` tag cannot be changed.
    pub fn update(&mut self, ref_val: &str, changes: HDict) -> Result<(), GraphError> {
        let eid = *self
            .id_map
            .get(ref_val)
            .ok_or_else(|| GraphError::NotFound(ref_val.to_string()))?;

        // Remove the old entity from the map (avoids an extra clone).
        let mut old_entity = self
            .entities
            .remove(ref_val)
            .ok_or_else(|| GraphError::NotFound(ref_val.to_string()))?;

        // Remove old indexing.
        self.remove_indexing(eid, &old_entity);

        // Snapshot old state for changelog, then merge in-place.
        let old_snapshot = old_entity.clone();
        old_entity.merge(&changes);

        // Re-index before re-inserting (entity is a local value, no borrow conflict).
        self.index_tags(eid, &old_entity);
        self.index_refs(eid, &old_entity);

        // Clone for the changelog, then move the updated entity into the map.
        let updated_for_log = old_entity.clone();
        self.entities.insert(ref_val.to_string(), old_entity);

        self.version += 1;
        self.push_changelog(GraphDiff {
            version: self.version,
            op: DiffOp::Update,
            ref_val: ref_val.to_string(),
            old: Some(old_snapshot),
            new: Some(updated_for_log),
        });

        Ok(())
    }

    /// Remove an entity from the graph. Returns the removed entity.
    pub fn remove(&mut self, ref_val: &str) -> Result<HDict, GraphError> {
        let eid = self
            .id_map
            .remove(ref_val)
            .ok_or_else(|| GraphError::NotFound(ref_val.to_string()))?;

        self.reverse_id.remove(&eid);

        let entity = self
            .entities
            .remove(ref_val)
            .ok_or_else(|| GraphError::NotFound(ref_val.to_string()))?;

        self.remove_indexing(eid, &entity);

        self.version += 1;
        self.push_changelog(GraphDiff {
            version: self.version,
            op: DiffOp::Remove,
            ref_val: ref_val.to_string(),
            old: Some(entity.clone()),
            new: None,
        });

        Ok(entity)
    }

    // ── Query ──

    /// Run a filter expression and return matching entities as a grid.
    pub fn read(&self, filter_expr: &str, limit: usize) -> Result<HGrid, GraphError> {
        let results = self.read_all(filter_expr, limit)?;

        if results.is_empty() {
            return Ok(HGrid::new());
        }

        // Collect all unique column names.
        let mut col_set: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for entity in &results {
            for name in entity.tag_names() {
                if seen.insert(name.to_string()) {
                    col_set.push(name.to_string());
                }
            }
        }
        col_set.sort();
        let cols: Vec<HCol> = col_set.iter().map(|n| HCol::new(n.as_str())).collect();
        let rows: Vec<HDict> = results.into_iter().cloned().collect();

        Ok(HGrid::from_parts(HDict::new(), cols, rows))
    }

    /// Run a filter expression and return matching entities as references.
    pub fn read_all(&self, filter_expr: &str, limit: usize) -> Result<Vec<&HDict>, GraphError> {
        let ast = parse_filter(filter_expr).map_err(|e| GraphError::Filter(e.to_string()))?;
        let effective_limit = if limit == 0 { usize::MAX } else { limit };

        // Phase 1: bitmap acceleration.
        let max_id = self.next_id;
        let candidates = query_planner::bitmap_candidates(&ast, &self.tag_index, max_id);

        // Phase 2: full filter evaluation.
        let resolver = |r: &HRef| -> Option<HDict> { self.entities.get(&r.val).cloned() };
        let ns = self.namespace.as_ref();

        let mut results = Vec::new();

        if let Some(ref bitmap) = candidates {
            // Evaluate only candidate entities.
            for eid in TagBitmapIndex::iter_set_bits(bitmap) {
                if results.len() >= effective_limit {
                    break;
                }
                if let Some(ref_val) = self.reverse_id.get(&eid) {
                    if let Some(entity) = self.entities.get(ref_val) {
                        if matches_with_ns(&ast, entity, Some(&resolver), ns) {
                            results.push(entity);
                        }
                    }
                }
            }
        } else {
            // No bitmap optimization possible; scan all entities.
            for entity in self.entities.values() {
                if results.len() >= effective_limit {
                    break;
                }
                if matches_with_ns(&ast, entity, Some(&resolver), ns) {
                    results.push(entity);
                }
            }
        }

        Ok(results)
    }

    // ── Ref traversal ──

    /// Get ref values that the given entity points to.
    pub fn refs_from(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        match self.id_map.get(ref_val) {
            Some(&eid) => self.adjacency.targets_from(eid, ref_type),
            None => Vec::new(),
        }
    }

    /// Get ref values of entities that point to the given entity.
    pub fn refs_to(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        self.adjacency
            .sources_to(ref_val, ref_type)
            .iter()
            .filter_map(|eid| self.reverse_id.get(eid).cloned())
            .collect()
    }

    // ── Spec-aware ──

    /// Find all entities that structurally fit a spec/type name.
    ///
    /// Requires a namespace to be set. Returns empty if no namespace.
    pub fn entities_fitting(&self, spec_name: &str) -> Vec<&HDict> {
        match &self.namespace {
            Some(ns) => self
                .entities
                .values()
                .filter(|e| ns.fits(e, spec_name))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Validate all entities against the namespace and check for dangling refs.
    ///
    /// Returns empty if no namespace is set and no dangling refs exist.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues: Vec<ValidationIssue> = match &self.namespace {
            Some(ns) => self
                .entities
                .values()
                .flat_map(|e| ns.validate_entity(e))
                .collect(),
            None => Vec::new(),
        };

        // Check for dangling refs: Ref values (except `id`) that point to
        // entities not present in the graph.
        for entity in self.entities.values() {
            let entity_ref = entity.id().map(|r| r.val.as_str());
            for (name, val) in entity.iter() {
                if name == "id" {
                    continue;
                }
                if let Kind::Ref(r) = val {
                    if !self.entities.contains_key(&r.val) {
                        issues.push(ValidationIssue {
                            entity: entity_ref.map(|s| s.to_string()),
                            issue_type: "dangling_ref".to_string(),
                            detail: format!(
                                "tag '{}' references '{}' which does not exist in the graph",
                                name, r.val
                            ),
                        });
                    }
                }
            }
        }

        issues
    }

    // ── Serialization ──

    /// Convert matching entities to a grid.
    ///
    /// If `filter_expr` is empty, exports all entities.
    /// Otherwise, delegates to `read`.
    pub fn to_grid(&self, filter_expr: &str) -> Result<HGrid, GraphError> {
        if filter_expr.is_empty() {
            let entities: Vec<&HDict> = self.entities.values().collect();
            return Ok(Self::entities_to_grid(&entities));
        }
        self.read(filter_expr, 0)
    }

    /// Build a grid from a slice of entity references.
    fn entities_to_grid(entities: &[&HDict]) -> HGrid {
        if entities.is_empty() {
            return HGrid::new();
        }

        let mut col_set: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for entity in entities {
            for name in entity.tag_names() {
                if seen.insert(name.to_string()) {
                    col_set.push(name.to_string());
                }
            }
        }
        col_set.sort();
        let cols: Vec<HCol> = col_set.iter().map(|n| HCol::new(n.as_str())).collect();
        let rows: Vec<HDict> = entities.iter().map(|e| (*e).clone()).collect();

        HGrid::from_parts(HDict::new(), cols, rows)
    }

    /// Build an EntityGraph from a grid.
    ///
    /// Rows without a valid `id` Ref tag are silently skipped.
    pub fn from_grid(grid: &HGrid, namespace: Option<DefNamespace>) -> Result<Self, GraphError> {
        let mut graph = match namespace {
            Some(ns) => Self::with_namespace(ns),
            None => Self::new(),
        };
        for row in &grid.rows {
            if row.id().is_some() {
                graph.add(row.clone())?;
            }
        }
        Ok(graph)
    }

    // ── Change tracking ──

    /// Get changelog entries since a given version.
    pub fn changes_since(&self, version: u64) -> &[GraphDiff] {
        match self
            .changelog
            .binary_search_by_key(&(version + 1), |d| d.version)
        {
            Ok(idx) => &self.changelog[idx..],
            Err(idx) => &self.changelog[idx..],
        }
    }

    /// Current graph version (monotonically increasing).
    pub fn version(&self) -> u64 {
        self.version
    }

    // ── Container ──

    /// Number of entities in the graph.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Returns `true` if the graph has no entities.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Returns `true` if an entity with the given ref value exists.
    pub fn contains(&self, ref_val: &str) -> bool {
        self.entities.contains_key(ref_val)
    }

    // ── Internal indexing ──

    /// Add tag bitmap entries for an entity.
    fn index_tags(&mut self, entity_id: usize, entity: &HDict) {
        let tags: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();
        self.tag_index.add(entity_id, &tags);
    }

    /// Add ref adjacency entries for an entity.
    fn index_refs(&mut self, entity_id: usize, entity: &HDict) {
        for (name, val) in entity.iter() {
            if let Kind::Ref(r) = val {
                // Skip the "id" tag — it is the entity's own identity,
                // not a reference edge.
                if name != "id" {
                    self.adjacency.add(entity_id, name, &r.val);
                }
            }
        }
    }

    /// Remove all index entries for an entity.
    fn remove_indexing(&mut self, entity_id: usize, entity: &HDict) {
        let tags: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();
        self.tag_index.remove(entity_id, &tags);
        self.adjacency.remove(entity_id);
    }

    /// Append a diff to the changelog, capping it at [`MAX_CHANGELOG`] entries.
    fn push_changelog(&mut self, diff: GraphDiff) {
        self.changelog.push(diff);
        if self.changelog.len() > MAX_CHANGELOG {
            self.changelog.drain(..self.changelog.len() - MAX_CHANGELOG);
        }
    }
}

impl Default for EntityGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the ref value string from an entity's `id` tag.
fn extract_ref_val(entity: &HDict) -> Result<String, GraphError> {
    match entity.get("id") {
        Some(Kind::Ref(r)) => Ok(r.val.clone()),
        Some(_) => Err(GraphError::InvalidId),
        None => Err(GraphError::MissingId),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::Number;

    fn make_site(id: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str(format!("Site {id}")));
        d.set("area", Kind::Number(Number::new(4500.0, Some("ft\u{00b2}".into()))));
        d
    }

    fn make_equip(id: &str, site_ref: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("equip", Kind::Marker);
        d.set("dis", Kind::Str(format!("Equip {id}")));
        d.set("siteRef", Kind::Ref(HRef::from_val(site_ref)));
        d
    }

    fn make_point(id: &str, equip_ref: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("point", Kind::Marker);
        d.set("sensor", Kind::Marker);
        d.set("temp", Kind::Marker);
        d.set("dis", Kind::Str(format!("Point {id}")));
        d.set("equipRef", Kind::Ref(HRef::from_val(equip_ref)));
        d.set("curVal", Kind::Number(Number::new(72.5, Some("\u{00b0}F".into()))));
        d
    }

    // ── Add tests ──

    #[test]
    fn add_entity_with_valid_id() {
        let mut g = EntityGraph::new();
        let result = g.add(make_site("site-1"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "site-1");
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn add_entity_missing_id_fails() {
        let mut g = EntityGraph::new();
        let entity = HDict::new();
        let err = g.add(entity).unwrap_err();
        assert!(matches!(err, GraphError::MissingId));
    }

    #[test]
    fn add_entity_non_ref_id_fails() {
        let mut g = EntityGraph::new();
        let mut entity = HDict::new();
        entity.set("id", Kind::Str("not-a-ref".into()));
        let err = g.add(entity).unwrap_err();
        assert!(matches!(err, GraphError::InvalidId));
    }

    #[test]
    fn add_duplicate_ref_fails() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        let err = g.add(make_site("site-1")).unwrap_err();
        assert!(matches!(err, GraphError::DuplicateRef(_)));
    }

    // ── Get tests ──

    #[test]
    fn get_existing_entity() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        let entity = g.get("site-1").unwrap();
        assert!(entity.has("site"));
        assert_eq!(entity.get("dis"), Some(&Kind::Str("Site site-1".into())));
    }

    #[test]
    fn get_missing_entity_returns_none() {
        let g = EntityGraph::new();
        assert!(g.get("nonexistent").is_none());
    }

    // ── Update tests ──

    #[test]
    fn update_merges_changes() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();

        let mut changes = HDict::new();
        changes.set("dis", Kind::Str("Updated Site".into()));
        changes.set("geoCity", Kind::Str("Richmond".into()));
        g.update("site-1", changes).unwrap();

        let entity = g.get("site-1").unwrap();
        assert_eq!(entity.get("dis"), Some(&Kind::Str("Updated Site".into())));
        assert_eq!(
            entity.get("geoCity"),
            Some(&Kind::Str("Richmond".into()))
        );
        assert!(entity.has("site")); // unchanged
    }

    #[test]
    fn update_missing_entity_fails() {
        let mut g = EntityGraph::new();
        let err = g.update("nonexistent", HDict::new()).unwrap_err();
        assert!(matches!(err, GraphError::NotFound(_)));
    }

    // ── Remove tests ──

    #[test]
    fn remove_entity() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        let removed = g.remove("site-1").unwrap();
        assert!(removed.has("site"));
        assert!(g.get("site-1").is_none());
        assert_eq!(g.len(), 0);
    }

    #[test]
    fn remove_missing_entity_fails() {
        let mut g = EntityGraph::new();
        let err = g.remove("nonexistent").unwrap_err();
        assert!(matches!(err, GraphError::NotFound(_)));
    }

    // ── Version / changelog tests ──

    #[test]
    fn version_increments_on_mutations() {
        let mut g = EntityGraph::new();
        assert_eq!(g.version(), 0);

        g.add(make_site("site-1")).unwrap();
        assert_eq!(g.version(), 1);

        g.update("site-1", HDict::new()).unwrap();
        assert_eq!(g.version(), 2);

        g.remove("site-1").unwrap();
        assert_eq!(g.version(), 3);
    }

    #[test]
    fn changelog_records_add_update_remove() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.update("site-1", HDict::new()).unwrap();
        g.remove("site-1").unwrap();

        let changes = g.changes_since(0);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].op, DiffOp::Add);
        assert_eq!(changes[0].ref_val, "site-1");
        assert!(changes[0].old.is_none());
        assert!(changes[0].new.is_some());

        assert_eq!(changes[1].op, DiffOp::Update);
        assert!(changes[1].old.is_some());
        assert!(changes[1].new.is_some());

        assert_eq!(changes[2].op, DiffOp::Remove);
        assert!(changes[2].old.is_some());
        assert!(changes[2].new.is_none());
    }

    #[test]
    fn changes_since_returns_subset() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap(); // v1
        g.add(make_site("site-2")).unwrap(); // v2
        g.add(make_site("site-3")).unwrap(); // v3

        let since_v2 = g.changes_since(2);
        assert_eq!(since_v2.len(), 1);
        assert_eq!(since_v2[0].ref_val, "site-3");
    }

    // ── Container tests ──

    #[test]
    fn contains_check() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        assert!(g.contains("site-1"));
        assert!(!g.contains("site-2"));
    }

    #[test]
    fn len_and_is_empty() {
        let mut g = EntityGraph::new();
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);

        g.add(make_site("site-1")).unwrap();
        assert!(!g.is_empty());
        assert_eq!(g.len(), 1);
    }

    // ── Query tests ──

    #[test]
    fn read_with_simple_has_filter() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();

        let results = g.read_all("site", 0).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].has("site"));
    }

    #[test]
    fn read_with_comparison_filter() {
        let mut g = EntityGraph::new();
        g.add(make_point("pt-1", "equip-1")).unwrap();

        let results = g.read_all("curVal > 70\u{00b0}F", 0).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn read_with_and_filter() {
        let mut g = EntityGraph::new();
        g.add(make_point("pt-1", "equip-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();

        let results = g.read_all("point and sensor", 0).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn read_with_or_filter() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();

        let results = g.read_all("site or equip", 0).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn read_limit_parameter_works() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_site("site-2")).unwrap();
        g.add(make_site("site-3")).unwrap();

        let results = g.read_all("site", 2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn read_returns_grid() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_site("site-2")).unwrap();

        let grid = g.read("site", 0).unwrap();
        assert_eq!(grid.len(), 2);
        assert!(grid.col("site").is_some());
        assert!(grid.col("id").is_some());
    }

    #[test]
    fn read_invalid_filter() {
        let g = EntityGraph::new();
        let err = g.read("!!!", 0).unwrap_err();
        assert!(matches!(err, GraphError::Filter(_)));
    }

    // ── Ref traversal tests ──

    #[test]
    fn refs_from_returns_targets() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();

        let targets = g.refs_from("equip-1", None);
        assert_eq!(targets, vec!["site-1".to_string()]);
    }

    #[test]
    fn refs_to_returns_sources() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();
        g.add(make_equip("equip-2", "site-1")).unwrap();

        let mut sources = g.refs_to("site-1", None);
        sources.sort();
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn type_filtered_ref_queries() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();

        let targets = g.refs_from("equip-1", Some("siteRef"));
        assert_eq!(targets, vec!["site-1".to_string()]);

        let targets = g.refs_from("equip-1", Some("equipRef"));
        assert!(targets.is_empty());
    }

    #[test]
    fn refs_from_nonexistent_entity() {
        let g = EntityGraph::new();
        assert!(g.refs_from("nonexistent", None).is_empty());
    }

    #[test]
    fn refs_to_nonexistent_entity() {
        let g = EntityGraph::new();
        assert!(g.refs_to("nonexistent", None).is_empty());
    }

    // ── Serialization tests ──

    #[test]
    fn from_grid_round_trip() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();

        let grid = g.to_grid("site or equip").unwrap();
        assert_eq!(grid.len(), 2);

        let g2 = EntityGraph::from_grid(&grid, None).unwrap();
        assert_eq!(g2.len(), 2);
        assert!(g2.contains("site-1"));
        assert!(g2.contains("equip-1"));
    }

    #[test]
    fn to_grid_empty_result() {
        let g = EntityGraph::new();
        let grid = g.to_grid("site").unwrap();
        assert!(grid.is_empty());
    }

    // ── Update re-indexes correctly ──

    #[test]
    fn update_reindexes_tags() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();

        // Should find the site with "site" filter.
        assert_eq!(g.read_all("site", 0).unwrap().len(), 1);

        // Remove the "site" marker via update.
        let mut changes = HDict::new();
        changes.set("site", Kind::Remove);
        g.update("site-1", changes).unwrap();

        // Should no longer match "site" filter.
        assert_eq!(g.read_all("site", 0).unwrap().len(), 0);
    }

    #[test]
    fn update_reindexes_refs() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_site("site-2")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();

        // Initially equip-1 points to site-1.
        assert_eq!(g.refs_from("equip-1", None), vec!["site-1".to_string()]);

        // Move equip-1 to site-2.
        let mut changes = HDict::new();
        changes.set("siteRef", Kind::Ref(HRef::from_val("site-2")));
        g.update("equip-1", changes).unwrap();

        assert_eq!(g.refs_from("equip-1", None), vec!["site-2".to_string()]);
        assert!(g.refs_to("site-1", None).is_empty());
    }

    // ── Dangling ref validation ──

    #[test]
    fn validate_detects_dangling_refs() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        // equip-1 has siteRef pointing to "site-1" (exists) — no issue
        g.add(make_equip("equip-1", "site-1")).unwrap();
        // equip-2 has siteRef pointing to "site-999" (does not exist) — dangling
        g.add(make_equip("equip-2", "site-999")).unwrap();

        let issues = g.validate();
        assert!(!issues.is_empty());

        let dangling: Vec<_> = issues
            .iter()
            .filter(|i| i.issue_type == "dangling_ref")
            .collect();
        assert_eq!(dangling.len(), 1);
        assert_eq!(dangling[0].entity.as_deref(), Some("equip-2"));
        assert!(dangling[0].detail.contains("site-999"));
        assert!(dangling[0].detail.contains("siteRef"));
    }

    // ── Empty filter exports all ──

    #[test]
    fn to_grid_empty_filter_exports_all() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();
        g.add(make_point("pt-1", "equip-1")).unwrap();

        let grid = g.to_grid("").unwrap();
        assert_eq!(grid.len(), 3);
        assert!(grid.col("id").is_some());
    }

    // ── from_grid skips rows without id ──

    #[test]
    fn changelog_bounded_to_max_size() {
        let mut graph = EntityGraph::new();
        // Add more entities than MAX_CHANGELOG
        for i in 0..12_000 {
            let mut d = HDict::new();
            d.set("id", Kind::Ref(HRef::from_val(&format!("e{i}"))));
            d.set("dis", Kind::Str(format!("Entity {i}")));
            graph.add(d).unwrap();
        }
        // Changelog should be capped
        assert!(graph.changes_since(0).len() <= 10_000);
        // Latest changes should still be present
        assert!(graph.changes_since(11_999).len() <= 1);
    }

    #[test]
    fn from_grid_skips_rows_without_id() {
        let cols = vec![HCol::new("id"), HCol::new("dis"), HCol::new("site")];

        let mut row_with_id = HDict::new();
        row_with_id.set("id", Kind::Ref(HRef::from_val("site-1")));
        row_with_id.set("site", Kind::Marker);
        row_with_id.set("dis", Kind::Str("Has ID".into()));

        // Row with string id (not a Ref) — should be skipped.
        let mut row_bad_id = HDict::new();
        row_bad_id.set("id", Kind::Str("not-a-ref".into()));
        row_bad_id.set("dis", Kind::Str("Bad ID".into()));

        // Row with no id at all — should be skipped.
        let mut row_no_id = HDict::new();
        row_no_id.set("dis", Kind::Str("No ID".into()));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row_with_id, row_bad_id, row_no_id]);
        let g = EntityGraph::from_grid(&grid, None).unwrap();

        assert_eq!(g.len(), 1);
        assert!(g.contains("site-1"));
    }
}
