// EntityGraph — in-memory entity store with bitmap indexing and ref adjacency.

use std::collections::HashMap;

use indexmap::IndexMap;
use parking_lot::Mutex;
use rayon::prelude::*;

use crate::data::{HCol, HDict, HGrid};
use crate::filter::{FilterNode, matches_with_ns, parse_filter};
use crate::kinds::{HRef, Kind};
use crate::ontology::{DefNamespace, ValidationIssue};

use super::adjacency::RefAdjacency;
use super::bitmap::TagBitmapIndex;
use super::changelog::{ChangelogGap, DiffOp, GraphDiff};
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
    #[error("entity ID space exhausted (max {MAX_ENTITY_ID})")]
    IdExhausted,
}

/// Maximum entity ID — constrained by RoaringBitmap (u32) and snapshot format.
const MAX_ENTITY_ID: usize = u32::MAX as usize;

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
    /// Freelist of recycled entity IDs from removed entities.
    free_ids: Vec<usize>,
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
    /// Maximum number of changelog entries retained.
    changelog_capacity: usize,
    /// Lowest version still present in the changelog (0 = no evictions yet).
    floor_version: u64,
    /// LRU query cache: (filter, version) → matching ref_vals.
    /// Uses Mutex for interior mutability since read_all takes &self.
    query_cache: Mutex<QueryCache>,
    /// Parsed filter AST cache: filter_string → AST (version-independent).
    ast_cache: Mutex<HashMap<String, FilterNode>>,
    /// Optional B-Tree value indexes for comparison-based filter acceleration.
    value_index: ValueIndex,
    /// CSR snapshot of adjacency for cache-friendly traversal.
    /// Rebuilt lazily via `rebuild_csr()`. `None` until first build.
    csr: Option<CsrAdjacency>,
    /// Version at which the CSR was last rebuilt (for staleness detection).
    csr_version: u64,
    /// Patch buffer for incremental CSR updates.
    csr_patch: super::csr::CsrPatch,
    /// Columnar storage for cache-friendly single-tag scans.
    columnar: ColumnarStore,
    /// WL structural fingerprint index.
    structural: super::structural::StructuralIndex,
}

/// Fixed-capacity LRU cache for filter query results using IndexMap for O(1) ops.
struct QueryCache {
    /// (filter, version) → matching ref_vals. Most-recently-used at the back.
    entries: IndexMap<(String, u64), Vec<String>>,
    capacity: usize,
}

impl QueryCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: IndexMap::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&mut self, filter: &str, version: u64) -> Option<&[String]> {
        // Move to back (most recently used) on access.
        let key = (filter.to_string(), version);
        let idx = self.entries.get_index_of(&key)?;
        self.entries.move_index(idx, self.entries.len() - 1);
        self.entries.get(&key).map(|v| v.as_slice())
    }

    fn insert(&mut self, filter: String, version: u64, ref_vals: Vec<String>) {
        if self.entries.len() >= self.capacity {
            // First try purging stale entries from older versions.
            self.purge_stale(version);
        }
        if self.entries.len() >= self.capacity {
            // Still at capacity — evict least recently used (front).
            self.entries.shift_remove_index(0);
        }
        self.entries.insert((filter, version), ref_vals);
    }

    /// Remove all entries whose version is older than `min_version`.
    fn purge_stale(&mut self, min_version: u64) {
        self.entries
            .retain(|(_filter, version), _| *version >= min_version);
    }
}

/// Compute query cache capacity based on entity count.
/// Scales with graph size but bounded: min 256, max 1024.
fn query_cache_capacity_for(entity_count: usize) -> usize {
    (entity_count / 100).clamp(256, 1024)
}

const DEFAULT_QUERY_CACHE_CAPACITY: usize = 256;

/// Common Haystack fields to auto-index for O(log N) value lookups.
/// Number of CSR patch ops before triggering a full rebuild.
const CSR_PATCH_THRESHOLD: usize = 1000;

const AUTO_INDEX_FIELDS: &[&str] = &[
    "siteRef", "equipRef", "dis", "curVal", "area", "geoCity", "kind", "unit",
];

impl EntityGraph {
    /// Create an empty entity graph with standard Haystack fields auto-indexed
    /// and default changelog capacity (50,000).
    pub fn new() -> Self {
        Self::with_changelog_capacity(super::changelog::DEFAULT_CHANGELOG_CAPACITY)
    }

    /// Create an empty entity graph with a custom changelog capacity.
    pub fn with_changelog_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1); // Ensure at least 1 entry
        let mut value_index = ValueIndex::new();
        for field in AUTO_INDEX_FIELDS {
            value_index.index_field(field);
        }
        Self {
            entities: HashMap::new(),
            id_map: HashMap::new(),
            reverse_id: HashMap::new(),
            next_id: 0,
            free_ids: Vec::new(),
            tag_index: TagBitmapIndex::new(),
            adjacency: RefAdjacency::new(),
            namespace: None,
            version: 0,
            changelog: std::collections::VecDeque::new(),
            changelog_capacity: capacity,
            floor_version: 0,
            query_cache: Mutex::new(QueryCache::new(DEFAULT_QUERY_CACHE_CAPACITY)),
            ast_cache: Mutex::new(HashMap::new()),
            value_index,
            csr: None,
            csr_version: 0,
            csr_patch: super::csr::CsrPatch::new(),
            columnar: ColumnarStore::new(),
            structural: super::structural::StructuralIndex::new(),
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

        let eid = if let Some(recycled) = self.free_ids.pop() {
            recycled
        } else {
            if self.next_id > MAX_ENTITY_ID {
                return Err(GraphError::IdExhausted);
            }
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).ok_or(GraphError::InvalidId)?;
            id
        };

        self.id_map.insert(ref_val.clone(), eid);
        self.reverse_id.insert(eid, ref_val.clone());

        // Index before inserting (borrows entity immutably, self mutably).
        self.index_tags(eid, &entity);
        self.index_refs(eid, &entity);

        // Clone for the changelog, then move the entity into the map.
        let entity_for_log = entity.clone();
        self.entities.insert(ref_val.clone(), entity);

        self.version += 1;
        // Patch CSR instead of invalidating.
        if self.csr.is_some() {
            if let Some(entity) = self.entities.get(&ref_val) {
                for (name, val) in entity.iter() {
                    if let Kind::Ref(r) = val
                        && name != "id"
                    {
                        self.csr_patch.add_edge(eid, name, &r.val);
                    }
                }
            }
            if self.csr_patch.len() > CSR_PATCH_THRESHOLD {
                self.rebuild_csr();
                self.csr_patch = super::csr::CsrPatch::new();
            }
        }
        self.push_changelog(GraphDiff {
            version: self.version,
            timestamp: 0,
            op: DiffOp::Add,
            ref_val: ref_val.clone(),
            old: None,
            new: Some(entity_for_log),
            changed_tags: None,
            previous_tags: None,
        });

        // Resize query cache if entity count crossed a threshold.
        let target_cap = query_cache_capacity_for(self.entities.len());
        let mut cache = self.query_cache.lock();
        if cache.capacity < target_cap {
            cache.capacity = target_cap;
        }

        self.structural.mark_stale();
        Ok(ref_val)
    }

    /// Add an entity without changelog tracking or version bump.
    ///
    /// Used for bulk restore from snapshots. Caller must call `finalize_bulk()`
    /// after all bulk adds to rebuild CSR and set version.
    pub fn add_bulk(&mut self, entity: HDict) -> Result<String, GraphError> {
        let ref_val = extract_ref_val(&entity)?;
        if self.entities.contains_key(&ref_val) {
            return Err(GraphError::DuplicateRef(ref_val));
        }

        let eid = if let Some(recycled) = self.free_ids.pop() {
            recycled
        } else {
            if self.next_id > MAX_ENTITY_ID {
                return Err(GraphError::IdExhausted);
            }
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).ok_or(GraphError::InvalidId)?;
            id
        };

        self.id_map.insert(ref_val.clone(), eid);
        self.reverse_id.insert(eid, ref_val.clone());
        self.index_tags(eid, &entity);
        self.index_refs(eid, &entity);
        self.entities.insert(ref_val.clone(), entity);

        Ok(ref_val)
    }

    /// Finalize bulk load: rebuild CSR and set version.
    pub fn finalize_bulk(&mut self, target_version: u64) {
        self.version = target_version;
        self.rebuild_csr();
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

        let mut old_entity = self
            .entities
            .remove(ref_val)
            .ok_or_else(|| GraphError::NotFound(ref_val.to_string()))?;

        // Compute delta for changelog before mutating.
        let mut prev_tags = HDict::new();
        let mut changed = HDict::new();
        for (key, new_val) in changes.iter() {
            if let Some(old_val) = old_entity.get(key) {
                prev_tags.set(key, old_val.clone());
            }
            changed.set(key, new_val.clone());
        }

        // Clone old for delta comparison, then merge.
        let old_snapshot = old_entity.clone();
        old_entity.merge(&changes);

        // Delta indexing: only update what changed.
        self.update_tags_delta(eid, &old_snapshot, &old_entity);

        // Re-index refs only if ref edges changed.
        if Self::refs_changed(&old_snapshot, &old_entity) {
            self.adjacency.remove(eid);
            self.index_refs(eid, &old_entity);
            if self.csr.is_some() {
                self.csr_patch.remove_entity(eid);
                for (name, val) in old_entity.iter() {
                    if let Kind::Ref(r) = val
                        && name != "id"
                    {
                        self.csr_patch.add_edge(eid, name, &r.val);
                    }
                }
                if self.csr_patch.len() > CSR_PATCH_THRESHOLD {
                    self.rebuild_csr();
                    self.csr_patch = super::csr::CsrPatch::new();
                }
            }
        }

        self.entities.insert(ref_val.to_string(), old_entity);

        self.version += 1;
        self.push_changelog(GraphDiff {
            version: self.version,
            timestamp: 0,
            op: DiffOp::Update,
            ref_val: ref_val.to_string(),
            old: None,
            new: None,
            changed_tags: Some(changed),
            previous_tags: Some(prev_tags),
        });

        // Invalidate query cache.
        self.query_cache.lock().entries.clear();

        self.structural.mark_stale();
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
        self.free_ids.push(eid);

        self.version += 1;
        if self.csr.is_some() {
            self.csr_patch.remove_entity(eid);
            if self.csr_patch.len() > CSR_PATCH_THRESHOLD {
                self.rebuild_csr();
                self.csr_patch = super::csr::CsrPatch::new();
            }
        }
        self.push_changelog(GraphDiff {
            version: self.version,
            timestamp: 0,
            op: DiffOp::Remove,
            ref_val: ref_val.to_string(),
            old: Some(entity.clone()),
            new: None,
            changed_tags: None,
            previous_tags: None,
        });

        self.structural.mark_stale();
        Ok(entity)
    }

    // ── Structural Index ──

    /// Get the structural index if it has been computed and is current.
    /// Returns `None` if stale — call `recompute_structural()` under a
    /// write lock first.
    pub fn structural_index(&self) -> Option<&super::structural::StructuralIndex> {
        if self.structural.is_stale() {
            None
        } else {
            Some(&self.structural)
        }
    }

    /// Force recomputation of structural fingerprints.
    pub fn recompute_structural(&mut self) {
        let entities = &self.entities;
        let id_map = &self.id_map;
        let adjacency = &self.adjacency;
        self.structural
            .compute(entities, id_map, |ref_val| match id_map.get(ref_val) {
                Some(&eid) => adjacency.targets_from(eid, None),
                None => Vec::new(),
            });
    }

    // ── Query ──

    /// Run a filter expression and return matching entities as a grid.
    pub fn read(&self, filter_expr: &str, limit: usize) -> Result<HGrid, GraphError> {
        let results = self.read_all(filter_expr, limit)?;

        if results.is_empty() {
            return Ok(HGrid::new());
        }

        // Collect all unique column names.
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(results.len().min(64));
        for entity in &results {
            for name in entity.tag_names() {
                seen.insert(name.to_string());
            }
        }
        let mut col_set: Vec<String> = seen.into_iter().collect();
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

        // Use cached AST or parse and cache it (ASTs are version-independent).
        let ast = {
            let mut ast_cache = self.ast_cache.lock();
            if let Some(cached) = ast_cache.get(filter_expr) {
                cached.clone()
            } else {
                let parsed =
                    parse_filter(filter_expr).map_err(|e| GraphError::Filter(e.to_string()))?;
                ast_cache.insert(filter_expr.to_string(), parsed.clone());
                parsed
            }
        };

        // Phase 1: bitmap acceleration (with value index + adjacency).
        let max_id = self.next_id;
        let candidates = query_planner::bitmap_candidates_with_values(
            &ast,
            &self.tag_index,
            &self.value_index,
            &self.adjacency,
            max_id,
        );

        // Phase 2: full filter evaluation.
        let resolver = |r: &HRef| -> Option<&HDict> { self.entities.get(&r.val) };
        let ns = self.namespace.as_ref();

        // Use parallel evaluation only for large unlimited queries where rayon
        // overhead is worthwhile. Bounded queries always use sequential + early exit.
        const PARALLEL_THRESHOLD: usize = 500;
        let use_parallel = effective_limit == usize::MAX;

        let mut results: Vec<&HDict>;

        if let Some(ref bitmap) = candidates {
            let candidate_ids: Vec<usize> = TagBitmapIndex::iter_set_bits(bitmap).collect();

            if candidate_ids.len() >= PARALLEL_THRESHOLD && use_parallel {
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

            if entity_count >= PARALLEL_THRESHOLD && use_parallel {
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
                    csr.targets_from_patched(eid, ref_type, &self.csr_patch)
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
            csr.sources_to_patched(ref_val, ref_type, &self.csr_patch)
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
        self.csr_patch = super::csr::CsrPatch::new();
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

        let start_eid = match self.id_map.get(ref_val) {
            Some(&eid) => eid,
            None => return (Vec::new(), Vec::new()),
        };

        let mut visited: HashSet<usize> = HashSet::new();
        let mut queue: VecDeque<(usize, usize)> = VecDeque::new();
        let mut result_entities: Vec<&HDict> = Vec::with_capacity(16);
        let mut result_edges: Vec<(String, String, String)> = Vec::with_capacity(16);

        visited.insert(start_eid);
        queue.push_back((start_eid, 0));

        if let Some(entity) = self.entities.get(ref_val) {
            result_entities.push(entity);
        }

        while let Some((current_eid, depth)) = queue.pop_front() {
            if depth >= hops {
                continue;
            }
            let current_ref = match self.reverse_id.get(&current_eid) {
                Some(s) => s.as_str(),
                None => continue,
            };

            // Traverse forward edges
            if let Some(fwd) = self.adjacency.forward_raw().get(&current_eid) {
                for (ref_tag, target) in fwd {
                    if let Some(types) = ref_types
                        && !types.iter().any(|t| t == ref_tag)
                    {
                        continue;
                    }
                    result_edges.push((current_ref.to_string(), ref_tag.clone(), target.clone()));
                    if let Some(&target_eid) = self.id_map.get(target.as_str())
                        && visited.insert(target_eid)
                    {
                        if let Some(entity) = self.entities.get(target.as_str()) {
                            result_entities.push(entity);
                        }
                        queue.push_back((target_eid, depth + 1));
                    }
                }
            }
            // Traverse reverse edges
            if let Some(rev) = self.adjacency.reverse_raw().get(current_ref) {
                for (ref_tag, source_eid) in rev {
                    if let Some(types) = ref_types
                        && !types.iter().any(|t| t == ref_tag)
                    {
                        continue;
                    }
                    if let Some(source_ref) = self.reverse_id.get(source_eid) {
                        result_edges.push((
                            source_ref.clone(),
                            ref_tag.clone(),
                            current_ref.to_string(),
                        ));
                        if visited.insert(*source_eid) {
                            if let Some(entity) = self.entities.get(source_ref.as_str()) {
                                result_entities.push(entity);
                            }
                            queue.push_back((*source_eid, depth + 1));
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
        let from_eid = match self.id_map.get(from) {
            Some(&eid) => eid,
            None => return Vec::new(),
        };
        let to_eid = match self.id_map.get(to) {
            Some(&eid) => eid,
            None => return Vec::new(),
        };

        // parent map: child_eid -> parent_eid (usize::MAX = root sentinel)
        let mut parents: StdHashMap<usize, usize> = StdHashMap::new();
        let mut queue: VecDeque<usize> = VecDeque::new();
        parents.insert(from_eid, usize::MAX);
        queue.push_back(from_eid);

        while let Some(current_eid) = queue.pop_front() {
            let current_ref = match self.reverse_id.get(&current_eid) {
                Some(s) => s.as_str(),
                None => continue,
            };

            // Forward edges
            if let Some(fwd) = self.adjacency.forward_raw().get(&current_eid) {
                for (_, target) in fwd {
                    if let Some(&target_eid) = self.id_map.get(target.as_str())
                        && let std::collections::hash_map::Entry::Vacant(e) =
                            parents.entry(target_eid)
                    {
                        e.insert(current_eid);
                        if target_eid == to_eid {
                            return Self::reconstruct_path_usize(
                                &parents,
                                to_eid,
                                &self.reverse_id,
                            );
                        }
                        queue.push_back(target_eid);
                    }
                }
            }
            // Reverse edges
            if let Some(rev) = self.adjacency.reverse_raw().get(current_ref) {
                for (_, source_eid) in rev {
                    if !parents.contains_key(source_eid) {
                        parents.insert(*source_eid, current_eid);
                        if *source_eid == to_eid {
                            return Self::reconstruct_path_usize(
                                &parents,
                                to_eid,
                                &self.reverse_id,
                            );
                        }
                        queue.push_back(*source_eid);
                    }
                }
            }
        }

        Vec::new() // No path found
    }

    /// Reconstruct path from usize-based BFS parent map.
    fn reconstruct_path_usize(
        parents: &std::collections::HashMap<usize, usize>,
        to_eid: usize,
        reverse_id: &HashMap<usize, String>,
    ) -> Vec<String> {
        let mut path_eids = vec![to_eid];
        let mut current = to_eid;
        while let Some(&parent) = parents.get(&current) {
            if parent == usize::MAX {
                break;
            }
            path_eids.push(parent);
            current = parent;
        }
        path_eids.reverse();
        path_eids
            .iter()
            .filter_map(|eid| reverse_id.get(eid).cloned())
            .collect()
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

    // ── Haystack Hierarchy Helpers ──

    /// Walk a chain of ref tags starting from an entity.
    ///
    /// For example, `ref_chain("point-1", &["equipRef", "siteRef"])` follows
    /// `point-1` → its `equipRef` → that entity's `siteRef`, returning the
    /// ordered path of resolved entities (excluding the starting entity).
    pub fn ref_chain(&self, ref_val: &str, ref_tags: &[&str]) -> Vec<&HDict> {
        let mut result = Vec::with_capacity(ref_tags.len());
        let mut current = ref_val.to_string();
        for tag in ref_tags {
            let entity = match self.entities.get(&current) {
                Some(e) => e,
                None => break,
            };
            match entity.get(tag) {
                Some(Kind::Ref(r)) => {
                    current = r.val.clone();
                    if let Some(target) = self.entities.get(&current) {
                        result.push(target);
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        result
    }

    /// Resolve the site for any entity by walking `equipRef` → `siteRef`.
    ///
    /// If the entity itself has a `site` marker, returns it directly.
    /// Otherwise walks the standard Haystack ref chain.
    pub fn site_for(&self, ref_val: &str) -> Option<&HDict> {
        let entity = self.entities.get(ref_val)?;
        // If the entity is itself a site, return it.
        if entity.has("site") {
            return Some(entity);
        }
        // Check direct siteRef.
        if let Some(Kind::Ref(r)) = entity.get("siteRef") {
            return self.entities.get(&r.val);
        }
        // Walk equipRef → siteRef.
        if let Some(Kind::Ref(r)) = entity.get("equipRef")
            && let Some(equip) = self.entities.get(&r.val)
            && let Some(Kind::Ref(sr)) = equip.get("siteRef")
        {
            return self.entities.get(&sr.val);
        }
        None
    }

    /// All direct children: entities with any ref tag pointing to this entity.
    pub fn children(&self, ref_val: &str) -> Vec<&HDict> {
        self.refs_to(ref_val, None)
            .iter()
            .filter_map(|r| self.entities.get(r))
            .collect()
    }

    /// All points for an equip — children with the `point` marker.
    ///
    /// Optionally filter further with a filter expression.
    pub fn equip_points(&self, equip_ref: &str, filter: Option<&str>) -> Vec<&HDict> {
        let points: Vec<&HDict> = self
            .children(equip_ref)
            .into_iter()
            .filter(|e| e.has("point"))
            .collect();
        match filter {
            Some(expr) => {
                let ast = match crate::filter::parse_filter(expr) {
                    Ok(ast) => ast,
                    Err(_) => return points,
                };
                points
                    .into_iter()
                    .filter(|e| crate::filter::matches(&ast, e, None))
                    .collect()
            }
            None => points,
        }
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
    ///
    /// Returns `Err(ChangelogGap)` if the requested version has been evicted
    /// from the changelog, signalling the subscriber must do a full resync.
    pub fn changes_since(&self, version: u64) -> Result<Vec<&GraphDiff>, ChangelogGap> {
        let target = version + 1;
        // If the floor has advanced past the requested version, the subscriber
        // has fallen behind and missed entries.
        if self.floor_version > 0 && version < self.floor_version {
            return Err(ChangelogGap {
                subscriber_version: version,
                floor_version: self.floor_version,
            });
        }
        // Binary search: versions are monotonically increasing in the VecDeque.
        // partition_point finds the first entry where version >= target.
        let start = self.changelog.partition_point(|d| d.version < target);
        Ok(self.changelog.iter().skip(start).collect())
    }

    /// The lowest version still retained in the changelog.
    ///
    /// Returns 0 if no entries have been evicted.
    pub fn floor_version(&self) -> u64 {
        self.floor_version
    }

    /// The configured changelog capacity.
    pub fn changelog_capacity(&self) -> usize {
        self.changelog_capacity
    }

    /// Current query cache capacity.
    pub fn query_cache_capacity(&self) -> usize {
        self.query_cache.lock().capacity
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

    /// Update only the changed tags in the tag bitmap index.
    fn update_tags_delta(&mut self, entity_id: usize, old: &HDict, new: &HDict) {
        let old_tags: std::collections::HashSet<&str> = old.tag_names().collect();
        let new_tags: std::collections::HashSet<&str> = new.tag_names().collect();

        // Tags removed: clear bits.
        let removed: Vec<String> = old_tags
            .difference(&new_tags)
            .map(|s| s.to_string())
            .collect();
        if !removed.is_empty() {
            self.tag_index.remove(entity_id, &removed);
        }

        // Tags added: set bits.
        let added: Vec<String> = new_tags
            .difference(&old_tags)
            .map(|s| s.to_string())
            .collect();
        if !added.is_empty() {
            self.tag_index.add(entity_id, &added);
        }

        // Update value indexes for changed fields only.
        for (name, new_val) in new.iter() {
            if self.value_index.has_index(name) {
                if let Some(old_val) = old.get(name) {
                    if old_val != new_val {
                        self.value_index.remove(entity_id, name, old_val);
                        self.value_index.add(entity_id, name, new_val);
                    }
                } else {
                    self.value_index.add(entity_id, name, new_val);
                }
            }
            if self.columnar.is_tracked(name) {
                self.columnar.set(entity_id, name, new_val);
            }
        }

        // Remove value indexes for removed fields.
        for name in &removed {
            if self.value_index.has_index(name)
                && let Some(old_val) = old.get(name.as_str())
            {
                self.value_index.remove(entity_id, name, old_val);
            }
        }
    }

    /// Check if ref edges changed between old and new entity.
    fn refs_changed(old: &HDict, new: &HDict) -> bool {
        for (name, val) in new.iter() {
            if name != "id"
                && let Kind::Ref(_) = val
                && old.get(name) != Some(val)
            {
                return true;
            }
        }
        // Check for removed refs.
        for (name, val) in old.iter() {
            if name != "id"
                && let Kind::Ref(_) = val
                && new.get(name).is_none()
            {
                return true;
            }
        }
        false
    }

    /// Build a full hierarchy subtree as a structured tree.
    /// `root` is the entity ref, `max_depth` limits recursion (0 = root only).
    pub fn hierarchy_tree(&self, root: &str, max_depth: usize) -> Option<HierarchyNode> {
        let entity = self.entities.get(root)?.clone();
        Some(self.build_subtree(root, &entity, 0, max_depth))
    }

    fn build_subtree(
        &self,
        ref_val: &str,
        entity: &HDict,
        depth: usize,
        max_depth: usize,
    ) -> HierarchyNode {
        let children = if depth < max_depth {
            self.children(ref_val)
                .into_iter()
                .filter_map(|child| {
                    let child_id = child.id()?.val.clone();
                    Some(self.build_subtree(&child_id, child, depth + 1, max_depth))
                })
                .collect()
        } else {
            Vec::new()
        };
        HierarchyNode {
            entity: entity.clone(),
            children,
            depth,
        }
    }

    /// Determine the most specific entity type from its markers.
    ///
    /// Returns the most specific marker tag that identifies the entity type.
    /// E.g., an entity with `equip` + `ahu` markers returns `"ahu"` (most specific).
    pub fn classify(&self, ref_val: &str) -> Option<String> {
        let entity = self.entities.get(ref_val)?;
        classify_entity(entity)
    }

    /// Append a diff to the changelog, capping at the configured capacity.
    fn push_changelog(&mut self, mut diff: GraphDiff) {
        diff.timestamp = GraphDiff::now_nanos();
        self.changelog.push_back(diff);
        while self.changelog.len() > self.changelog_capacity {
            if let Some(evicted) = self.changelog.pop_front() {
                self.floor_version = evicted.version;
            }
        }
    }
}

impl Default for EntityGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// A node in a hierarchy tree produced by [`EntityGraph::hierarchy_tree`].
#[derive(Debug, Clone)]
pub struct HierarchyNode {
    pub entity: HDict,
    pub children: Vec<HierarchyNode>,
    pub depth: usize,
}

/// Extract the ref value string from an entity's `id` tag.
fn extract_ref_val(entity: &HDict) -> Result<String, GraphError> {
    match entity.get("id") {
        Some(Kind::Ref(r)) => Ok(r.val.clone()),
        Some(_) => Err(GraphError::InvalidId),
        None => Err(GraphError::MissingId),
    }
}

/// Priority-ordered list of marker tags from most specific to least specific.
/// The first match wins.
const CLASSIFY_PRIORITY: &[&str] = &[
    // Point subtypes
    "sensor", "cmd", "sp", // Equipment subtypes
    "ahu", "vav", "boiler", "chiller", "meter", // Base categories
    "point", "equip", // Space types
    "room", "floor", "zone", "space", // Site
    "site",  // Other well-known
    "weather", "device", "network",
];

/// Classify an entity by returning the most specific recognized marker tag.
fn classify_entity(entity: &HDict) -> Option<String> {
    for &tag in CLASSIFY_PRIORITY {
        if entity.has(tag) {
            return Some(tag.to_string());
        }
    }
    None
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

    #[test]
    fn id_freelist_recycles_removed_ids() {
        let mut g = EntityGraph::new();

        // Add 3 entities: IDs 0, 1, 2
        for i in 0..3 {
            let mut e = HDict::new();
            e.set("id", Kind::Ref(HRef::from_val(format!("e-{i}"))));
            g.add(e).unwrap();
        }

        // Remove entity 1 (frees ID 1)
        g.remove("e-1").unwrap();

        // Add a new entity — should reuse ID 1, not allocate ID 3
        let mut e = HDict::new();
        e.set("id", Kind::Ref(HRef::from_val("e-new")));
        g.add(e).unwrap();

        // Graph should have 3 entities and next_id should still be 3
        assert_eq!(g.len(), 3);
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

        let changes = g.changes_since(0).unwrap();
        assert_eq!(changes.len(), 3);

        // Add: has new, no old, no deltas.
        assert_eq!(changes[0].op, DiffOp::Add);
        assert_eq!(changes[0].ref_val, "site-1");
        assert!(changes[0].old.is_none());
        assert!(changes[0].new.is_some());
        assert!(changes[0].changed_tags.is_none());

        // Update: has deltas, no old/new.
        assert_eq!(changes[1].op, DiffOp::Update);
        assert!(changes[1].old.is_none());
        assert!(changes[1].new.is_none());
        assert!(changes[1].changed_tags.is_some());
        assert!(changes[1].previous_tags.is_some());

        // Remove: has old, no new, no deltas.
        assert_eq!(changes[2].op, DiffOp::Remove);
        assert!(changes[2].old.is_some());
        assert!(changes[2].new.is_none());
        assert!(changes[2].changed_tags.is_none());
    }

    #[test]
    fn changes_since_returns_subset() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap(); // v1
        g.add(make_site("site-2")).unwrap(); // v2
        g.add(make_site("site-3")).unwrap(); // v3

        let since_v2 = g.changes_since(2).unwrap();
        assert_eq!(since_v2.len(), 1);
        assert_eq!(since_v2[0].ref_val, "site-3");
    }

    #[test]
    fn configurable_changelog_capacity() {
        let mut g = EntityGraph::with_changelog_capacity(3);
        assert_eq!(g.changelog_capacity(), 3);

        // Add 5 entities — first 2 should be evicted from changelog.
        for i in 0..5 {
            g.add(make_site(&format!("site-{i}"))).unwrap();
        }

        assert_eq!(g.version(), 5);
        assert_eq!(g.floor_version(), 2); // v1 and v2 evicted

        // Can still get changes from v2 onwards.
        let changes = g.changes_since(2).unwrap();
        assert_eq!(changes.len(), 3); // v3, v4, v5

        // Requesting from v1 (evicted) should return ChangelogGap.
        let gap = g.changes_since(1).unwrap_err();
        assert_eq!(gap.subscriber_version, 1);
        assert_eq!(gap.floor_version, 2);
    }

    #[test]
    fn changelog_gap_on_version_zero_after_eviction() {
        let mut g = EntityGraph::with_changelog_capacity(2);
        for i in 0..4 {
            g.add(make_site(&format!("site-{i}"))).unwrap();
        }

        // Requesting since v0 after evictions should return gap.
        let gap = g.changes_since(0).unwrap_err();
        assert_eq!(gap.subscriber_version, 0);
        assert!(gap.floor_version > 0);
    }

    #[test]
    fn no_gap_when_capacity_sufficient() {
        let mut g = EntityGraph::with_changelog_capacity(100);
        for i in 0..50 {
            g.add(make_site(&format!("site-{i}"))).unwrap();
        }
        assert_eq!(g.floor_version(), 0);
        let changes = g.changes_since(0).unwrap();
        assert_eq!(changes.len(), 50);
    }

    #[test]
    fn changelog_entries_have_timestamps() {
        let mut g = EntityGraph::new();
        g.add(make_site("site-1")).unwrap();
        g.update("site-1", HDict::new()).unwrap();
        g.remove("site-1").unwrap();

        let changes = g.changes_since(0).unwrap();
        for diff in &changes {
            assert!(diff.timestamp > 0, "timestamp should be positive");
        }
        // Timestamps should be non-decreasing.
        for pair in changes.windows(2) {
            assert!(pair[1].timestamp >= pair[0].timestamp);
        }
    }

    #[test]
    fn update_diff_carries_delta_tags() {
        let mut g = EntityGraph::new();
        let mut site = HDict::new();
        site.set("id", Kind::Ref(HRef::from_val("site-1")));
        site.set("site", Kind::Marker);
        site.set("dis", Kind::Str("Original".into()));
        site.set("area", Kind::Number(Number::unitless(1000.0)));
        g.add(site).unwrap();

        let mut changes = HDict::new();
        changes.set("dis", Kind::Str("Updated".into()));
        g.update("site-1", changes).unwrap();

        let diffs = g.changes_since(1).unwrap(); // skip the Add
        assert_eq!(diffs.len(), 1);
        let diff = &diffs[0];
        assert_eq!(diff.op, DiffOp::Update);

        // old/new should be None for Update (delta only).
        assert!(diff.old.is_none());
        assert!(diff.new.is_none());

        // changed_tags has the new value.
        let ct = diff.changed_tags.as_ref().unwrap();
        assert_eq!(ct.get("dis"), Some(&Kind::Str("Updated".into())));
        assert!(ct.get("area").is_none()); // unchanged tag not included

        // previous_tags has the old value.
        let pt = diff.previous_tags.as_ref().unwrap();
        assert_eq!(pt.get("dis"), Some(&Kind::Str("Original".into())));
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

    #[test]
    fn query_cache_capacity_scales_with_entity_count() {
        let mut g = EntityGraph::new();
        // Default cache should start at 256
        assert_eq!(g.query_cache_capacity(), 256);
        for i in 0..500 {
            let mut e = HDict::new();
            e.set("id", Kind::Ref(HRef::from_val(format!("e-{i}"))));
            e.set("site", Kind::Marker);
            g.add(e).unwrap();
        }
        // For 500 entities: (500/100).clamp(256, 1024) = 256 (still minimum)
        assert!(g.query_cache_capacity() >= 256);
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

    #[test]
    fn csr_survives_single_mutation() {
        let mut g = EntityGraph::new();
        let mut site = HDict::new();
        site.set("id", Kind::Ref(HRef::from_val("site-1")));
        site.set("site", Kind::Marker);
        g.add(site).unwrap();

        let mut equip = HDict::new();
        equip.set("id", Kind::Ref(HRef::from_val("equip-1")));
        equip.set("equip", Kind::Marker);
        equip.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
        g.add(equip).unwrap();

        g.rebuild_csr();

        // Mutate: add a new entity (should NOT destroy CSR)
        let mut point = HDict::new();
        point.set("id", Kind::Ref(HRef::from_val("point-1")));
        point.set("point", Kind::Marker);
        point.set("equipRef", Kind::Ref(HRef::from_val("equip-1")));
        g.add(point).unwrap();

        // Forward refs should still work (CSR + patch overlay)
        let refs = g.refs_from("equip-1", Some("siteRef"));
        assert_eq!(refs, vec!["site-1".to_string()]);

        // New entity's refs should be found via patch
        let refs = g.refs_from("point-1", Some("equipRef"));
        assert_eq!(refs, vec!["equip-1".to_string()]);
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

    #[test]
    fn update_delta_indexing_preserves_unchanged_tags() {
        let mut g = EntityGraph::new();
        let mut e = HDict::new();
        e.set("id", Kind::Ref(HRef::from_val("p-1")));
        e.set("point", Kind::Marker);
        e.set("sensor", Kind::Marker);
        e.set("curVal", Kind::Number(Number::unitless(72.0)));
        e.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
        g.add(e).unwrap();

        // Update only curVal — point, sensor, siteRef should remain indexed.
        let mut changes = HDict::new();
        changes.set("curVal", Kind::Number(Number::unitless(75.0)));
        g.update("p-1", changes).unwrap();

        // Verify tag bitmap still has point and sensor.
        let results = g.read_all("point and sensor", 0).unwrap();
        assert_eq!(results.len(), 1);

        // Verify ref adjacency still works.
        let refs = g.refs_from("p-1", Some("siteRef"));
        assert_eq!(refs, vec!["site-1".to_string()]);

        // Verify value index has the new curVal.
        let results = g.read_all("curVal >= 74", 0).unwrap();
        assert_eq!(results.len(), 1);
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
        // Use a small capacity to test bounding without 50K iterations.
        let mut graph = EntityGraph::with_changelog_capacity(100);
        for i in 0..200 {
            let mut d = HDict::new();
            d.set("id", Kind::Ref(HRef::from_val(format!("e{i}"))));
            d.set("dis", Kind::Str(format!("Entity {i}")));
            graph.add(d).unwrap();
        }
        // Changelog should be capped at capacity.
        // Requesting since floor_version should succeed.
        let floor = graph.floor_version();
        assert!(floor > 0);
        let changes = graph.changes_since(floor).unwrap();
        assert!(changes.len() <= 100);
        // Latest changes should still be present.
        assert!(graph.changes_since(199).unwrap().len() <= 1);
        // Old versions should return gap.
        assert!(graph.changes_since(0).is_err());
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

    // ── Haystack Hierarchy Helper tests ──

    #[test]
    fn ref_chain_walks_equip_to_site() {
        let g = build_hierarchy_graph();
        // p1 → equipRef=e1 → siteRef=s1
        let chain = g.ref_chain("p1", &["equipRef", "siteRef"]);
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id().unwrap().val, "e1");
        assert_eq!(chain[1].id().unwrap().val, "s1");
    }

    #[test]
    fn ref_chain_stops_on_missing_tag() {
        let g = build_hierarchy_graph();
        // e1 has siteRef but no spaceRef — should return just the site.
        let chain = g.ref_chain("e1", &["siteRef", "spaceRef"]);
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id().unwrap().val, "s1");
    }

    #[test]
    fn ref_chain_empty_for_nonexistent() {
        let g = build_hierarchy_graph();
        let chain = g.ref_chain("nonexistent", &["equipRef"]);
        assert!(chain.is_empty());
    }

    #[test]
    fn site_for_returns_site_itself() {
        let g = build_hierarchy_graph();
        let site = g.site_for("s1").unwrap();
        assert_eq!(site.id().unwrap().val, "s1");
    }

    #[test]
    fn site_for_walks_from_point() {
        let g = build_hierarchy_graph();
        // p1 → equipRef=e1 → siteRef=s1
        let site = g.site_for("p1").unwrap();
        assert_eq!(site.id().unwrap().val, "s1");
    }

    #[test]
    fn site_for_walks_from_equip() {
        let g = build_hierarchy_graph();
        let site = g.site_for("e1").unwrap();
        assert_eq!(site.id().unwrap().val, "s1");
    }

    #[test]
    fn children_returns_direct_refs() {
        let g = build_hierarchy_graph();
        let kids = g.children("s1");
        // e1 and e2 reference s1 via siteRef.
        let ids: Vec<&str> = kids.iter().map(|e| e.id().unwrap().val.as_str()).collect();
        assert!(ids.contains(&"e1"));
        assert!(ids.contains(&"e2"));
    }

    #[test]
    fn equip_points_returns_points_only() {
        let g = build_hierarchy_graph();
        let points = g.equip_points("e1", None);
        assert_eq!(points.len(), 2); // p1, p2
        for p in &points {
            assert!(p.has("point"));
        }
    }

    #[test]
    fn equip_points_with_filter() {
        let mut g = build_hierarchy_graph();
        // Existing points already have temp marker. Add one with flow instead.
        let mut pf = HDict::new();
        pf.set("id", Kind::Ref(HRef::from_val("pf")));
        pf.set("point", Kind::Marker);
        pf.set("flow", Kind::Marker);
        pf.set("equipRef", Kind::Ref(HRef::from_val("e1")));
        g.add(pf).unwrap();

        let temp_points = g.equip_points("e1", Some("temp"));
        // Only p1 and p2 have temp (the existing ones).
        assert_eq!(temp_points.len(), 2);
        assert!(temp_points.iter().all(|p| p.has("temp")));
    }

    // ── Hierarchy tree tests ──

    #[test]
    fn hierarchy_tree_from_site() {
        let g = build_hierarchy_graph();
        let tree = g.hierarchy_tree("s1", 10).unwrap();
        assert_eq!(tree.depth, 0);
        assert_eq!(tree.entity.id().unwrap().val, "s1");
        // s1 has children e1, e2
        assert_eq!(tree.children.len(), 2);
        let child_ids: Vec<String> = tree
            .children
            .iter()
            .map(|c| c.entity.id().unwrap().val.clone())
            .collect();
        assert!(child_ids.contains(&"e1".to_string()));
        assert!(child_ids.contains(&"e2".to_string()));
        // e1 has children p1, p2
        let e1_node = tree
            .children
            .iter()
            .find(|c| c.entity.id().unwrap().val == "e1")
            .unwrap();
        assert_eq!(e1_node.children.len(), 2);
        let point_ids: Vec<String> = e1_node
            .children
            .iter()
            .map(|c| c.entity.id().unwrap().val.clone())
            .collect();
        assert!(point_ids.contains(&"p1".to_string()));
        assert!(point_ids.contains(&"p2".to_string()));
    }

    #[test]
    fn hierarchy_tree_max_depth() {
        let g = build_hierarchy_graph();
        // depth 0 = root only
        let tree = g.hierarchy_tree("s1", 0).unwrap();
        assert!(tree.children.is_empty());
        // depth 1 = root + direct children
        let tree = g.hierarchy_tree("s1", 1).unwrap();
        assert_eq!(tree.children.len(), 2);
        assert!(tree.children.iter().all(|c| c.children.is_empty()));
    }

    #[test]
    fn hierarchy_tree_missing_root() {
        let g = build_hierarchy_graph();
        assert!(g.hierarchy_tree("nonexistent", 10).is_none());
    }

    // ── Classify tests ──

    #[test]
    fn classify_site() {
        let g = build_hierarchy_graph();
        assert_eq!(g.classify("s1").unwrap(), "site");
    }

    #[test]
    fn classify_equip() {
        let mut g = EntityGraph::new();
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        d.set("equip", Kind::Marker);
        d.set("ahu", Kind::Marker);
        g.add(d).unwrap();
        assert_eq!(g.classify("ahu-1").unwrap(), "ahu");
    }

    #[test]
    fn classify_point() {
        let g = build_hierarchy_graph();
        // Points have point + sensor + temp markers; sensor is most specific.
        assert_eq!(g.classify("p1").unwrap(), "sensor");
    }

    #[test]
    fn classify_unknown() {
        let mut g = EntityGraph::new();
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val("x1")));
        d.set("custom", Kind::Marker);
        g.add(d).unwrap();
        assert!(g.classify("x1").is_none());
    }

    #[test]
    fn changes_since_binary_search_equivalence() {
        let mut g = EntityGraph::new();
        for i in 0..100 {
            let mut e = HDict::new();
            e.set("id", Kind::Ref(HRef::from_val(format!("e-{i}"))));
            e.set("site", Kind::Marker);
            g.add(e).unwrap();
        }
        // After 100 adds, version is 100.
        // changes_since(50) should return versions 51..=100 (50 entries).
        let changes = g.changes_since(50).unwrap();
        assert_eq!(changes.len(), 50);
        assert_eq!(changes.first().unwrap().version, 51);
        assert_eq!(changes.last().unwrap().version, 100);
    }

    #[test]
    fn add_bulk_skips_changelog() {
        let mut g = EntityGraph::new();
        for i in 0..10 {
            let mut e = HDict::new();
            e.set("id", Kind::Ref(HRef::from_val(format!("e-{i}"))));
            e.set("site", Kind::Marker);
            g.add_bulk(e).unwrap();
        }
        assert_eq!(g.len(), 10);
        assert_eq!(g.version(), 0); // version not bumped during bulk load
        assert!(g.changes_since(0).unwrap().is_empty()); // no changelog entries
    }
}
