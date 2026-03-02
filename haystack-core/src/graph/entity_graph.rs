// EntityGraph — in-memory entity store with bitmap indexing and ref adjacency.

use std::collections::HashMap;

use parking_lot::Mutex;
use rayon::prelude::*;

use crate::data::{HCol, HDict, HGrid};
use crate::filter::{matches_with_ns, parse_filter};
use crate::kinds::{HRef, Kind};
use crate::ontology::{DefNamespace, ValidationIssue};

use super::adjacency::RefAdjacency;
use super::bitmap::TagBitmapIndex;
use super::changelog::{DiffOp, GraphDiff};
use super::columnar::ColumnarStore;
use super::csr::CsrAdjacency;
use super::query_planner;
use super::value_index::ValueIndex;

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
    changelog: std::collections::VecDeque<GraphDiff>,
    /// LRU query cache: (filter, version) → matching ref_vals.
    /// Uses Mutex for interior mutability since read_all takes &self.
    query_cache: Mutex<QueryCache>,
    /// Optional B-Tree value indexes for comparison-based filter acceleration.
    value_index: ValueIndex,
    /// CSR snapshot of adjacency for cache-friendly traversal.
    /// Rebuilt lazily via `rebuild_csr()`. `None` until first build.
    csr: Option<CsrAdjacency>,
    /// Version at which the CSR was last rebuilt (for staleness detection).
    csr_version: u64,
    /// Columnar storage for cache-friendly single-tag scans.
    columnar: ColumnarStore,
}

/// Simple fixed-capacity LRU cache for filter query results.
struct QueryCache {
    entries: Vec<QueryCacheEntry>,
    capacity: usize,
}

struct QueryCacheEntry {
    filter: String,
    version: u64,
    ref_vals: Vec<String>,
}

impl QueryCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&mut self, filter: &str, version: u64) -> Option<&[String]> {
        let pos = self
            .entries
            .iter()
            .position(|e| e.version == version && e.filter == filter)?;
        // Move to front (most recently used)
        if pos > 0 {
            let entry = self.entries.remove(pos);
            self.entries.insert(0, entry);
        }
        Some(&self.entries[0].ref_vals)
    }

    fn insert(&mut self, filter: String, version: u64, ref_vals: Vec<String>) {
        // Evict oldest if at capacity
        if self.entries.len() >= self.capacity {
            self.entries.pop();
        }
        self.entries.insert(
            0,
            QueryCacheEntry {
                filter,
                version,
                ref_vals,
            },
        );
    }
}

const MAX_CHANGELOG: usize = 10_000;
const QUERY_CACHE_CAPACITY: usize = 256;

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
            changelog: std::collections::VecDeque::new(),
            query_cache: Mutex::new(QueryCache::new(QUERY_CACHE_CAPACITY)),
            value_index: ValueIndex::new(),
            csr: None,
            csr_version: 0,
            columnar: ColumnarStore::new(),
        }
    }

    /// Create an entity graph with an ontology namespace.
    pub fn with_namespace(ns: DefNamespace) -> Self {
        Self {
            namespace: Some(ns),
            ..Self::new()
        }
    }

    // ── Value Indexes ──

    /// Register a field for B-Tree value indexing. Enables O(log N) range
    /// queries (e.g. `temp > 72`) for this field. Must be called before
    /// entities are added, or followed by `rebuild_value_index` for existing data.
    pub fn index_field(&mut self, field: &str) {
        self.value_index.index_field(field);
    }

    /// Rebuild the value index for all indexed fields from the current entities.
    pub fn rebuild_value_index(&mut self) {
        self.value_index.clear();
        for (ref_val, entity) in &self.entities {
            if let Some(&eid) = self.id_map.get(ref_val.as_str()) {
                for (name, val) in entity.iter() {
                    if self.value_index.has_index(name) {
                        self.value_index.add(eid, name, val);
                    }
                }
            }
        }
    }

    /// Returns a reference to the value index (for use by the query planner).
    pub fn value_index(&self) -> &ValueIndex {
        &self.value_index
    }

    // ── Columnar Storage ──

    /// Register a tag for columnar storage. Enables cache-friendly sequential
    /// scans for this tag. Must be called before entities are added, or followed
    /// by `rebuild_columnar` for existing data.
    pub fn track_column(&mut self, tag: &str) {
        self.columnar.track_tag(tag);
    }

    /// Rebuild all tracked columnar data from current entities.
    pub fn rebuild_columnar(&mut self) {
        self.columnar.clear();
        self.columnar.ensure_capacity(self.next_id);
        for (ref_val, entity) in &self.entities {
            if let Some(&eid) = self.id_map.get(ref_val.as_str()) {
                for (name, val) in entity.iter() {
                    if self.columnar.is_tracked(name) {
                        self.columnar.set(eid, name, val);
                    }
                }
            }
        }
    }

    /// Returns a reference to the columnar store for direct column scans.
    pub fn columnar(&self) -> &ColumnarStore {
        &self.columnar
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
        self.next_id = self.next_id.checked_add(1).ok_or(GraphError::InvalidId)?;

        self.id_map.insert(ref_val.clone(), eid);
        self.reverse_id.insert(eid, ref_val.clone());

        // Index before inserting (borrows entity immutably, self mutably).
        self.index_tags(eid, &entity);
        self.index_refs(eid, &entity);

        // Clone for the changelog, then move the entity into the map.
        let entity_for_log = entity.clone();
        self.entities.insert(ref_val.clone(), entity);

        self.version += 1;
        self.csr = None; // Invalidate CSR snapshot on mutation.
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
        self.csr = None; // Invalidate CSR snapshot on mutation.
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
        self.csr = None; // Invalidate CSR snapshot on mutation.
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
        let effective_limit = if limit == 0 { usize::MAX } else { limit };

        // Check query cache (version-keyed, so mutations auto-invalidate).
        {
            let mut cache = self.query_cache.lock();
            if let Some(cached_refs) = cache.get(filter_expr, self.version) {
                let mut results = Vec::new();
                for rv in cached_refs {
                    if results.len() >= effective_limit {
                        break;
                    }
                    if let Some(entity) = self.entities.get(rv) {
                        results.push(entity);
                    }
                }
                return Ok(results);
            }
        }

        let ast = parse_filter(filter_expr).map_err(|e| GraphError::Filter(e.to_string()))?;

        // Phase 1: bitmap acceleration (with value index enhancement).
        let max_id = self.next_id;
        let candidates = query_planner::bitmap_candidates_with_values(
            &ast,
            &self.tag_index,
            &self.value_index,
            max_id,
        );

        // Phase 2: full filter evaluation.
        let resolver = |r: &HRef| -> Option<&HDict> { self.entities.get(&r.val) };
        let ns = self.namespace.as_ref();

        /// Threshold for parallel evaluation — below this, sequential is faster.
        const PARALLEL_THRESHOLD: usize = 500;

        let mut results: Vec<&HDict>;

        if let Some(ref bitmap) = candidates {
            let candidate_ids: Vec<usize> = TagBitmapIndex::iter_set_bits(bitmap).collect();

            if candidate_ids.len() >= PARALLEL_THRESHOLD && effective_limit == usize::MAX {
                // Parallel path for large, unlimited queries.
                results = candidate_ids
                    .par_iter()
                    .filter_map(|&eid| {
                        let ref_val = self.reverse_id.get(&eid)?;
                        let entity = self.entities.get(ref_val)?;
                        if matches_with_ns(&ast, entity, Some(&resolver), ns) {
                            Some(entity)
                        } else {
                            None
                        }
                    })
                    .collect();
            } else {
                // Sequential path for small sets or limited queries.
                results = Vec::new();
                for eid in TagBitmapIndex::iter_set_bits(bitmap) {
                    if results.len() >= effective_limit {
                        break;
                    }
                    if let Some(ref_val) = self.reverse_id.get(&eid)
                        && let Some(entity) = self.entities.get(ref_val)
                        && matches_with_ns(&ast, entity, Some(&resolver), ns)
                    {
                        results.push(entity);
                    }
                }
            }
        } else {
            let entity_count = self.entities.len();

            if entity_count >= PARALLEL_THRESHOLD && effective_limit == usize::MAX {
                // Parallel full scan for large, unlimited queries.
                results = self
                    .entities
                    .par_iter()
                    .filter_map(|(_, entity)| {
                        if matches_with_ns(&ast, entity, Some(&resolver), ns) {
                            Some(entity)
                        } else {
                            None
                        }
                    })
                    .collect();
            } else {
                // Sequential full scan.
                results = Vec::new();
                for entity in self.entities.values() {
                    if results.len() >= effective_limit {
                        break;
                    }
                    if matches_with_ns(&ast, entity, Some(&resolver), ns) {
                        results.push(entity);
                    }
                }
            }
        }

        // Apply limit to parallel results.
        if results.len() > effective_limit {
            results.truncate(effective_limit);
        }

        // Populate cache with result ref_vals (only for unlimited queries to
        // avoid caching partial results that depend on limit).
        if limit == 0 {
            let ref_vals: Vec<String> = results
                .iter()
                .filter_map(|e| {
                    e.get("id").and_then(|k| match k {
                        Kind::Ref(r) => Some(r.val.clone()),
                        _ => None,
                    })
                })
                .collect();
            let mut cache = self.query_cache.lock();
            cache.insert(filter_expr.to_string(), self.version, ref_vals);
        }

        Ok(results)
    }

    // ── Ref traversal ──

    /// Get ref values that the given entity points to.
    pub fn refs_from(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        match self.id_map.get(ref_val) {
            Some(&eid) => {
                if let Some(csr) = &self.csr {
                    csr.targets_from(eid, ref_type)
                } else {
                    self.adjacency.targets_from(eid, ref_type)
                }
            }
            None => Vec::new(),
        }
    }

    /// Get ref values of entities that point to the given entity.
    pub fn refs_to(&self, ref_val: &str, ref_type: Option<&str>) -> Vec<String> {
        if let Some(csr) = &self.csr {
            csr.sources_to(ref_val, ref_type)
                .iter()
                .filter_map(|eid| self.reverse_id.get(eid).cloned())
                .collect()
        } else {
            self.adjacency
                .sources_to(ref_val, ref_type)
                .iter()
                .filter_map(|eid| self.reverse_id.get(eid).cloned())
                .collect()
        }
    }

    /// Rebuild the CSR snapshot from the current adjacency.
    /// Should be called after a batch of mutations (e.g., import, sync).
    pub fn rebuild_csr(&mut self) {
        let max_id = if self.next_id > 0 { self.next_id } else { 0 };
        self.csr = Some(CsrAdjacency::from_ref_adjacency(&self.adjacency, max_id));
        self.csr_version = self.version;
    }

    /// Returns true if the CSR snapshot is stale (version mismatch).
    pub fn csr_is_stale(&self) -> bool {
        match &self.csr {
            Some(_) => self.csr_version != self.version,
            None => true,
        }
    }

    // ── Graph traversal ──

    /// Return all edges in the graph as `(source_ref, ref_tag, target_ref)` tuples.
    pub fn all_edges(&self) -> Vec<(String, String, String)> {
        let mut edges = Vec::new();
        for (&eid, ref_val) in &self.reverse_id {
            if let Some(fwd) = self.adjacency.forward_raw().get(&eid) {
                for (ref_tag, target) in fwd {
                    edges.push((ref_val.clone(), ref_tag.clone(), target.clone()));
                }
            }
        }
        edges
    }

    /// BFS neighborhood: return entities and edges within `hops` of `ref_val`.
    ///
    /// `ref_types` optionally restricts which ref tags are traversed.
    /// Returns `(entities, edges)` where edges are `(source, ref_tag, target)`.
    pub fn neighbors(
        &self,
        ref_val: &str,
        hops: usize,
        ref_types: Option<&[&str]>,
    ) -> (Vec<&HDict>, Vec<(String, String, String)>) {
        use std::collections::{HashSet, VecDeque};

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut result_entities: Vec<&HDict> = Vec::new();
        let mut result_edges: Vec<(String, String, String)> = Vec::new();

        visited.insert(ref_val.to_string());
        queue.push_back((ref_val.to_string(), 0));

        if let Some(entity) = self.entities.get(ref_val) {
            result_entities.push(entity);
        }

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= hops {
                continue;
            }
            // Traverse forward edges
            if let Some(&eid) = self.id_map.get(&current)
                && let Some(fwd) = self.adjacency.forward_raw().get(&eid)
            {
                for (ref_tag, target) in fwd {
                    if let Some(types) = ref_types
                        && !types.iter().any(|t| t == ref_tag)
                    {
                        continue;
                    }
                    result_edges.push((current.clone(), ref_tag.clone(), target.clone()));
                    if visited.insert(target.clone()) {
                        if let Some(entity) = self.entities.get(target.as_str()) {
                            result_entities.push(entity);
                        }
                        queue.push_back((target.clone(), depth + 1));
                    }
                }
            }
            // Traverse reverse edges
            if let Some(rev) = self.adjacency.reverse_raw().get(&current) {
                for (ref_tag, source_eid) in rev {
                    if let Some(types) = ref_types
                        && !types.iter().any(|t| t == ref_tag)
                    {
                        continue;
                    }
                    if let Some(source_ref) = self.reverse_id.get(source_eid) {
                        result_edges.push((source_ref.clone(), ref_tag.clone(), current.clone()));
                        if visited.insert(source_ref.clone()) {
                            if let Some(entity) = self.entities.get(source_ref.as_str()) {
                                result_entities.push(entity);
                            }
                            queue.push_back((source_ref.clone(), depth + 1));
                        }
                    }
                }
            }
        }

        result_entities.sort_by(|a, b| {
            let a_id = a.id().map(|r| r.val.as_str()).unwrap_or("");
            let b_id = b.id().map(|r| r.val.as_str()).unwrap_or("");
            a_id.cmp(b_id)
        });
        result_edges.sort();
        // Deduplicate edges (reverse traversal can produce duplicates)
        result_edges.dedup();

        (result_entities, result_edges)
    }

    /// BFS shortest path from `from` to `to`. Returns ordered ref_vals, or
    /// empty vec if no path exists.
    pub fn shortest_path(&self, from: &str, to: &str) -> Vec<String> {
        use std::collections::{HashMap as StdHashMap, VecDeque};

        if from == to {
            return vec![from.to_string()];
        }
        if !self.entities.contains_key(from) || !self.entities.contains_key(to) {
            return Vec::new();
        }

        let mut visited: StdHashMap<String, String> = StdHashMap::new(); // child -> parent
        let mut queue: VecDeque<String> = VecDeque::new();
        visited.insert(from.to_string(), String::new());
        queue.push_back(from.to_string());

        while let Some(current) = queue.pop_front() {
            // Forward edges
            if let Some(&eid) = self.id_map.get(&current)
                && let Some(fwd) = self.adjacency.forward_raw().get(&eid)
            {
                for (_, target) in fwd {
                    if !visited.contains_key(target) {
                        visited.insert(target.clone(), current.clone());
                        if target == to {
                            return Self::reconstruct_path(&visited, to);
                        }
                        queue.push_back(target.clone());
                    }
                }
            }
            // Reverse edges
            if let Some(rev) = self.adjacency.reverse_raw().get(&current) {
                for (_, source_eid) in rev {
                    if let Some(source_ref) = self.reverse_id.get(source_eid)
                        && !visited.contains_key(source_ref)
                    {
                        visited.insert(source_ref.clone(), current.clone());
                        if source_ref == to {
                            return Self::reconstruct_path(&visited, to);
                        }
                        queue.push_back(source_ref.clone());
                    }
                }
            }
        }

        Vec::new() // No path found
    }

    /// Reconstruct path from BFS parent map.
    fn reconstruct_path(
        parents: &std::collections::HashMap<String, String>,
        to: &str,
    ) -> Vec<String> {
        let mut path = vec![to.to_string()];
        let mut current = to.to_string();
        while let Some(parent) = parents.get(&current) {
            if parent.is_empty() {
                break;
            }
            path.push(parent.clone());
            current = parent.clone();
        }
        path.reverse();
        path
    }

    /// Return the subtree rooted at `root` up to `max_depth` levels.
    ///
    /// Walks reverse refs (children referencing parent). Returns entities
    /// paired with their depth from root. Root is depth 0.
    pub fn subtree(&self, root: &str, max_depth: usize) -> Vec<(&HDict, usize)> {
        use std::collections::{HashSet, VecDeque};

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut results: Vec<(&HDict, usize)> = Vec::new();

        visited.insert(root.to_string());
        queue.push_back((root.to_string(), 0));

        if let Some(entity) = self.entities.get(root) {
            results.push((entity, 0));
        } else {
            return Vec::new();
        }

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            // Children = entities that reference current (reverse refs)
            let child_refs = self.refs_to(&current, None);
            for child_ref in child_refs {
                if visited.insert(child_ref.clone())
                    && let Some(entity) = self.entities.get(&child_ref)
                {
                    results.push((entity, depth + 1));
                    queue.push_back((child_ref, depth + 1));
                }
            }
        }

        results
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
                if let Kind::Ref(r) = val
                    && !self.entities.contains_key(&r.val)
                {
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
        // Bulk import: build CSR once after all adds.
        graph.rebuild_csr();
        Ok(graph)
    }

    // ── Change tracking ──

    /// Get changelog entries since a given version.
    pub fn changes_since(&self, version: u64) -> Vec<&GraphDiff> {
        let target = version + 1;
        // VecDeque may not be contiguous, so collect matching entries.
        self.changelog
            .iter()
            .filter(|d| d.version >= target)
            .collect()
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

    /// Returns references to all entities in the graph.
    pub fn all(&self) -> Vec<&HDict> {
        self.entities.values().collect()
    }

    // ── Internal indexing ──

    /// Add tag bitmap entries for an entity.
    fn index_tags(&mut self, entity_id: usize, entity: &HDict) {
        let tags: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();
        self.tag_index.add(entity_id, &tags);

        // Update value indexes for any indexed fields present on this entity.
        for (name, val) in entity.iter() {
            if self.value_index.has_index(name) {
                self.value_index.add(entity_id, name, val);
            }
            // Update columnar store for tracked tags.
            if self.columnar.is_tracked(name) {
                self.columnar.set(entity_id, name, val);
            }
        }
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

        // Remove from value indexes.
        for (name, val) in entity.iter() {
            if self.value_index.has_index(name) {
                self.value_index.remove(entity_id, name, val);
            }
        }

        // Clear columnar data for this entity.
        self.columnar.clear_entity(entity_id);
    }

    /// Append a diff to the changelog, capping it at [`MAX_CHANGELOG`] entries.
    fn push_changelog(&mut self, diff: GraphDiff) {
        self.changelog.push_back(diff);
        while self.changelog.len() > MAX_CHANGELOG {
            self.changelog.pop_front();
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
        d.set(
            "area",
            Kind::Number(Number::new(4500.0, Some("ft\u{00b2}".into()))),
        );
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
        d.set(
            "curVal",
            Kind::Number(Number::new(72.5, Some("\u{00b0}F".into()))),
        );
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
        assert_eq!(entity.get("geoCity"), Some(&Kind::Str("Richmond".into())));
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

    #[test]
    fn query_cache_returns_same_results() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.add(make_equip("equip-1", "site-1")).unwrap();
        g.add(make_point("pt-1", "equip-1")).unwrap();

        // First call populates cache
        let results1 = g.read_all("site", 0).unwrap();
        assert_eq!(results1.len(), 1);

        // Second call should hit cache and return same results
        let results2 = g.read_all("site", 0).unwrap();
        assert_eq!(results2.len(), 1);
        assert_eq!(results1[0].get("id"), results2[0].get("id"));
    }

    #[test]
    fn query_cache_invalidated_by_mutation() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();

        let results = g.read_all("site", 0).unwrap();
        assert_eq!(results.len(), 1);

        // Add another site — cache should be invalidated by version bump
        g.add(make_site("site-2")).unwrap();

        let results = g.read_all("site", 0).unwrap();
        assert_eq!(results.len(), 2);
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
            d.set("id", Kind::Ref(HRef::from_val(format!("e{i}"))));
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

    // ── Graph traversal method tests ──

    fn build_hierarchy_graph() -> EntityGraph {
        let mut g = EntityGraph::new();
        g.add(make_site("s1")).unwrap();
        g.add(make_site("s2")).unwrap();
        g.add(make_equip("e1", "s1")).unwrap();
        g.add(make_equip("e2", "s1")).unwrap();
        g.add(make_equip("e3", "s2")).unwrap();
        g.add(make_point("p1", "e1")).unwrap();
        g.add(make_point("p2", "e1")).unwrap();
        g.add(make_point("p3", "e2")).unwrap();
        g
    }

    #[test]
    fn all_edges_returns_all_ref_relationships() {
        let g = build_hierarchy_graph();
        let edges = g.all_edges();
        // e1->s1 (siteRef), e2->s1, e3->s2, p1->e1 (equipRef), p2->e1, p3->e2
        assert_eq!(edges.len(), 6);
        assert!(
            edges
                .iter()
                .any(|(s, t, d)| s == "e1" && t == "siteRef" && d == "s1")
        );
        assert!(
            edges
                .iter()
                .any(|(s, t, d)| s == "p1" && t == "equipRef" && d == "e1")
        );
    }

    #[test]
    fn all_edges_empty_graph() {
        let g = EntityGraph::new();
        assert!(g.all_edges().is_empty());
    }

    #[test]
    fn neighbors_one_hop() {
        let g = build_hierarchy_graph();
        let (entities, edges) = g.neighbors("e1", 1, None);
        // e1 + s1 (forward via siteRef) + p1, p2 (reverse from equipRef)
        let ids: Vec<String> = entities
            .iter()
            .filter_map(|e| e.id().map(|r| r.val.clone()))
            .collect();
        assert!(ids.contains(&"e1".to_string()));
        assert!(ids.contains(&"s1".to_string()));
        assert!(ids.contains(&"p1".to_string()));
        assert!(ids.contains(&"p2".to_string()));
        assert!(!edges.is_empty());
    }

    #[test]
    fn neighbors_with_ref_type_filter() {
        let g = build_hierarchy_graph();
        let (entities, edges) = g.neighbors("e1", 1, Some(&["siteRef"]));
        // Only forward siteRef edge: e1->s1
        let ids: Vec<String> = entities
            .iter()
            .filter_map(|e| e.id().map(|r| r.val.clone()))
            .collect();
        assert!(ids.contains(&"e1".to_string()));
        assert!(ids.contains(&"s1".to_string()));
        // Should not include p1/p2 (those connect via equipRef)
        assert!(!ids.contains(&"p1".to_string()));
        // Edges should only contain siteRef
        assert!(edges.iter().all(|(_, tag, _)| tag == "siteRef"));
    }

    #[test]
    fn neighbors_zero_hops() {
        let g = build_hierarchy_graph();
        let (entities, edges) = g.neighbors("e1", 0, None);
        assert_eq!(entities.len(), 1);
        assert!(edges.is_empty());
    }

    #[test]
    fn neighbors_nonexistent_entity() {
        let g = build_hierarchy_graph();
        let (entities, _) = g.neighbors("nonexistent", 1, None);
        assert!(entities.is_empty());
    }

    #[test]
    fn shortest_path_direct() {
        let g = build_hierarchy_graph();
        let path = g.shortest_path("e1", "s1");
        assert_eq!(path, vec!["e1".to_string(), "s1".to_string()]);
    }

    #[test]
    fn shortest_path_two_hops() {
        let g = build_hierarchy_graph();
        let path = g.shortest_path("p1", "s1");
        // p1 -> e1 -> s1
        assert_eq!(path.len(), 3);
        assert_eq!(path[0], "p1");
        assert_eq!(path[2], "s1");
    }

    #[test]
    fn shortest_path_same_node() {
        let g = build_hierarchy_graph();
        let path = g.shortest_path("s1", "s1");
        assert_eq!(path, vec!["s1".to_string()]);
    }

    #[test]
    fn shortest_path_no_connection() {
        // s1 and s2 are not connected to each other directly
        let g = build_hierarchy_graph();
        let path = g.shortest_path("s1", "s2");
        // They are disconnected (no edges between them)
        assert!(path.is_empty());
    }

    #[test]
    fn shortest_path_nonexistent() {
        let g = build_hierarchy_graph();
        let path = g.shortest_path("s1", "nonexistent");
        assert!(path.is_empty());
    }

    #[test]
    fn subtree_from_site() {
        let g = build_hierarchy_graph();
        let tree = g.subtree("s1", 10);
        // s1 (depth 0), e1 (1), e2 (1), p1 (2), p2 (2), p3 (2)
        assert_eq!(tree.len(), 6);
        // Root is at depth 0
        assert_eq!(tree[0].0.id().unwrap().val, "s1");
        assert_eq!(tree[0].1, 0);
        // Equips at depth 1
        let depth_1: Vec<_> = tree.iter().filter(|(_, d)| *d == 1).collect();
        assert_eq!(depth_1.len(), 2);
        // Points at depth 2
        let depth_2: Vec<_> = tree.iter().filter(|(_, d)| *d == 2).collect();
        assert_eq!(depth_2.len(), 3);
    }

    #[test]
    fn subtree_max_depth_1() {
        let g = build_hierarchy_graph();
        let tree = g.subtree("s1", 1);
        // s1 (depth 0), e1 (1), e2 (1) — no points (depth 2)
        assert_eq!(tree.len(), 3);
    }

    #[test]
    fn subtree_nonexistent_root() {
        let g = build_hierarchy_graph();
        let tree = g.subtree("nonexistent", 10);
        assert!(tree.is_empty());
    }

    #[test]
    fn subtree_leaf_node() {
        let g = build_hierarchy_graph();
        let tree = g.subtree("p1", 10);
        // p1 has no children referencing it
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].0.id().unwrap().val, "p1");
    }
}
