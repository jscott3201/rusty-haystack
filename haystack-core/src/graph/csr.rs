// Compressed Sparse Row (CSR) adjacency — read-optimized graph traversal.
//
// Provides cache-friendly, contiguous-memory layout for ref edges.
// Forward edges are sorted by source entity ID; reverse edges by target.
// All edges for a single vertex are contiguous in memory, eliminating
// HashMap/SmallVec pointer-chasing overhead during traversal.
//
// This is a **read-only snapshot** rebuilt from the mutable RefAdjacency.
// It is not updated incrementally — call `rebuild()` after mutations.

use std::collections::HashSet;

/// Read-optimized compressed sparse row adjacency for graph traversal.
///
/// Memory layout for N entities with E total edges:
/// - Forward: `row_offsets[N+1]` + `targets[E]` + `edge_tags[E]`
/// - Reverse: separate CSR indexed by target ref_val
///
/// `refs_from(eid)` is a single slice: `targets[row_offsets[eid]..row_offsets[eid+1]]`
pub struct CsrAdjacency {
    // ── Forward edges (source entity_id → targets) ──
    /// `row_offsets[eid]..row_offsets[eid+1]` is the range in `targets`/`edge_tags`
    /// for edges from entity `eid`. Length = max_entity_id + 2.
    fwd_offsets: Vec<usize>,
    /// Target ref_vals, contiguous per source entity.
    fwd_targets: Vec<String>,
    /// Ref tag names (parallel to `fwd_targets`).
    fwd_tags: Vec<String>,

    // ── Reverse edges (target ref_val → source entity_ids) ──
    /// Sorted unique target ref_vals.
    rev_keys: Vec<String>,
    /// `rev_offsets[i]..rev_offsets[i+1]` is the range in `rev_sources`/`rev_tags`.
    rev_offsets: Vec<usize>,
    /// Source entity IDs, contiguous per target.
    rev_sources: Vec<usize>,
    /// Ref tag names (parallel to `rev_sources`).
    rev_tags: Vec<String>,
}

impl CsrAdjacency {
    /// Build a CSR snapshot from the mutable HashMap-based adjacency.
    pub fn from_ref_adjacency(adj: &super::adjacency::RefAdjacency, max_entity_id: usize) -> Self {
        // ── Forward CSR ──
        let fwd_data = adj.forward_raw();
        let num_rows = max_entity_id + 1;
        let mut fwd_offsets = vec![0usize; num_rows + 1];
        let mut fwd_targets = Vec::new();
        let mut fwd_tags = Vec::new();

        // Count edges per entity.
        for (&eid, edges) in fwd_data {
            if eid < num_rows {
                fwd_offsets[eid + 1] = edges.len();
            }
        }
        // Prefix sum.
        for i in 1..=num_rows {
            fwd_offsets[i] += fwd_offsets[i - 1];
        }
        // Fill edge arrays.
        fwd_targets.resize(fwd_offsets[num_rows], String::new());
        fwd_tags.resize(fwd_offsets[num_rows], String::new());
        // Track current write position per entity.
        let mut write_pos: Vec<usize> = fwd_offsets[..num_rows].to_vec();
        for (&eid, edges) in fwd_data {
            if eid < num_rows {
                for (tag, target) in edges {
                    let pos = write_pos[eid];
                    fwd_targets[pos] = target.clone();
                    fwd_tags[pos] = tag.clone();
                    write_pos[eid] += 1;
                }
            }
        }

        // ── Reverse CSR ──
        let rev_data = adj.reverse_raw();
        // Collect and sort keys for binary search.
        let mut rev_keys: Vec<String> = rev_data.keys().cloned().collect();
        rev_keys.sort();

        let mut rev_offsets = vec![0usize; rev_keys.len() + 1];
        let mut rev_sources = Vec::new();
        let mut rev_tags = Vec::new();

        for (i, key) in rev_keys.iter().enumerate() {
            if let Some(edges) = rev_data.get(key) {
                rev_offsets[i + 1] = rev_offsets[i] + edges.len();
                for (tag, src) in edges {
                    rev_sources.push(*src);
                    rev_tags.push(tag.clone());
                }
            } else {
                rev_offsets[i + 1] = rev_offsets[i];
            }
        }

        Self {
            fwd_offsets,
            fwd_targets,
            fwd_tags,
            rev_keys,
            rev_offsets,
            rev_sources,
            rev_tags,
        }
    }

    /// Get target ref values from an entity, optionally filtered by ref type.
    ///
    /// Returns a contiguous slice view (no allocation for unfiltered case).
    pub fn targets_from(&self, entity_id: usize, ref_type: Option<&str>) -> Vec<String> {
        if entity_id + 1 >= self.fwd_offsets.len() {
            return Vec::new();
        }
        let start = self.fwd_offsets[entity_id];
        let end = self.fwd_offsets[entity_id + 1];

        match ref_type {
            None => self.fwd_targets[start..end].to_vec(),
            Some(rt) => (start..end)
                .filter(|&i| self.fwd_tags[i] == rt)
                .map(|i| self.fwd_targets[i].clone())
                .collect(),
        }
    }

    /// Get source entity ids that reference `target_ref_val`, optionally
    /// filtered by ref type.
    pub fn sources_to(&self, target_ref_val: &str, ref_type: Option<&str>) -> Vec<usize> {
        let idx = match self
            .rev_keys
            .binary_search_by(|k| k.as_str().cmp(target_ref_val))
        {
            Ok(i) => i,
            Err(_) => return Vec::new(),
        };
        let start = self.rev_offsets[idx];
        let end = self.rev_offsets[idx + 1];

        match ref_type {
            None => self.rev_sources[start..end].to_vec(),
            Some(rt) => (start..end)
                .filter(|&i| self.rev_tags[i] == rt)
                .map(|i| self.rev_sources[i])
                .collect(),
        }
    }

    /// Get targets from an entity, overlaying a patch buffer.
    pub fn targets_from_patched(
        &self,
        entity_id: usize,
        ref_type: Option<&str>,
        patch: &CsrPatch,
    ) -> Vec<String> {
        // If entity was removed/invalidated in patch, skip base CSR for this entity.
        let mut results = if patch.removed_entities.contains(&entity_id) {
            Vec::new()
        } else {
            self.targets_from(entity_id, ref_type)
        };

        // Add patch edges for this entity.
        for (eid, tag, target) in &patch.added_edges {
            if *eid == entity_id && ref_type.is_none_or(|rt| tag == rt) {
                results.push(target.clone());
            }
        }
        results
    }

    /// Get source entity IDs that reference target, overlaying a patch buffer.
    pub fn sources_to_patched(
        &self,
        target_ref_val: &str,
        ref_type: Option<&str>,
        patch: &CsrPatch,
    ) -> Vec<usize> {
        // Base CSR sources, excluding removed entities.
        let mut results: Vec<usize> = self
            .sources_to(target_ref_val, ref_type)
            .into_iter()
            .filter(|eid| !patch.removed_entities.contains(eid))
            .collect();

        // Add patch edges that target this ref_val.
        for (eid, tag, target) in &patch.added_edges {
            if target == target_ref_val
                && ref_type.is_none_or(|rt| tag == rt)
                && !results.contains(eid)
            {
                results.push(*eid);
            }
        }
        results
    }

    /// Total number of forward edges stored.
    pub fn edge_count(&self) -> usize {
        self.fwd_targets.len()
    }
}

/// Patch buffer for incremental CSR updates. Overlaid on the base CSR during queries.
#[derive(Debug, Default)]
pub struct CsrPatch {
    /// Added forward edges: (entity_id, ref_tag, target_ref_val)
    added_edges: Vec<(usize, String, String)>,
    /// Entity IDs whose forward edges have been removed/invalidated.
    removed_entities: HashSet<usize>,
}

impl CsrPatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new forward edge.
    pub fn add_edge(&mut self, eid: usize, tag: &str, target: &str) {
        self.added_edges
            .push((eid, tag.to_string(), target.to_string()));
    }

    /// Record that an entity's edges are invalidated (removed or changed).
    pub fn remove_entity(&mut self, eid: usize) {
        self.removed_entities.insert(eid);
    }

    /// Total number of patch operations.
    pub fn len(&self) -> usize {
        self.added_edges.len() + self.removed_entities.len()
    }

    /// Whether the patch is empty.
    pub fn is_empty(&self) -> bool {
        self.added_edges.is_empty() && self.removed_entities.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::adjacency::RefAdjacency;

    fn build_test_adjacency() -> (RefAdjacency, usize) {
        let mut adj = RefAdjacency::new();
        // equip-1 (eid=1) -> site-1 via siteRef
        adj.add(1, "siteRef", "site-1");
        // point-1 (eid=2) -> equip-1 via equipRef, site-1 via siteRef
        adj.add(2, "equipRef", "equip-1");
        adj.add(2, "siteRef", "site-1");
        // point-2 (eid=3) -> equip-1 via equipRef
        adj.add(3, "equipRef", "equip-1");
        (adj, 4) // max_entity_id = 3, so pass 4 for size
    }

    #[test]
    fn csr_targets_from_all() {
        let (adj, max) = build_test_adjacency();
        let csr = CsrAdjacency::from_ref_adjacency(&adj, max);

        let targets = csr.targets_from(2, None);
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&"equip-1".to_string()));
        assert!(targets.contains(&"site-1".to_string()));
    }

    #[test]
    fn csr_targets_from_filtered() {
        let (adj, max) = build_test_adjacency();
        let csr = CsrAdjacency::from_ref_adjacency(&adj, max);

        let targets = csr.targets_from(2, Some("siteRef"));
        assert_eq!(targets, vec!["site-1".to_string()]);
    }

    #[test]
    fn csr_sources_to_all() {
        let (adj, max) = build_test_adjacency();
        let csr = CsrAdjacency::from_ref_adjacency(&adj, max);

        let mut sources = csr.sources_to("site-1", None);
        sources.sort();
        assert_eq!(sources, vec![1, 2]);
    }

    #[test]
    fn csr_sources_to_filtered() {
        let (adj, max) = build_test_adjacency();
        let csr = CsrAdjacency::from_ref_adjacency(&adj, max);

        let sources = csr.sources_to("equip-1", Some("equipRef"));
        assert_eq!(sources.len(), 2);
        assert!(sources.contains(&2));
        assert!(sources.contains(&3));
    }

    #[test]
    fn csr_nonexistent_entity() {
        let (adj, max) = build_test_adjacency();
        let csr = CsrAdjacency::from_ref_adjacency(&adj, max);
        assert!(csr.targets_from(99, None).is_empty());
    }

    #[test]
    fn csr_nonexistent_target() {
        let (adj, max) = build_test_adjacency();
        let csr = CsrAdjacency::from_ref_adjacency(&adj, max);
        assert!(csr.sources_to("nonexistent", None).is_empty());
    }

    #[test]
    fn csr_edge_count() {
        let (adj, max) = build_test_adjacency();
        let csr = CsrAdjacency::from_ref_adjacency(&adj, max);
        assert_eq!(csr.edge_count(), 4); // 1 + 2 + 1
    }

    #[test]
    fn csr_empty_graph() {
        let adj = RefAdjacency::new();
        let csr = CsrAdjacency::from_ref_adjacency(&adj, 0);
        assert!(csr.targets_from(0, None).is_empty());
        assert!(csr.sources_to("anything", None).is_empty());
        assert_eq!(csr.edge_count(), 0);
    }
}
