//! WL-inspired structural fingerprinting for entity graph partitioning.
//!
//! Adapts the 1-dimensional Weisfeiler-Leman colour refinement algorithm
//! to Haystack entities. Each entity gets a structural fingerprint based
//! on its tag set and the tag sets of its k-hop neighbors.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use roaring::RoaringBitmap;
use rustc_hash::FxHasher;

use crate::data::HDict;

/// Structural index: maps entities to WL fingerprints and partitions
/// entities by fingerprint for fast structural queries.
pub struct StructuralIndex {
    /// ref_val → fingerprint
    fingerprints: HashMap<String, u64>,
    /// fingerprint → entity IDs (as roaring bitmap)
    partitions: HashMap<u64, RoaringBitmap>,
    /// fingerprint → set of tag names that entities with this fingerprint have
    partition_tags: HashMap<u64, Vec<String>>,
    /// Number of WL refinement rounds (default 2).
    depth: usize,
    /// Whether the index needs full recomputation.
    stale: bool,
}

impl StructuralIndex {
    pub fn new() -> Self {
        Self::with_depth(2)
    }

    pub fn with_depth(depth: usize) -> Self {
        Self {
            fingerprints: HashMap::new(),
            partitions: HashMap::new(),
            partition_tags: HashMap::new(),
            depth,
            stale: true,
        }
    }

    /// Compute a fast non-cryptographic hash.
    fn fx_hash(data: &[u8]) -> u64 {
        let mut hasher = FxHasher::default();
        data.hash(&mut hasher);
        hasher.finish()
    }

    /// Compute round-0 fingerprint: hash of sorted tag names.
    fn round0_fingerprint(entity: &HDict) -> u64 {
        let mut tags: Vec<&str> = entity.tag_names().collect();
        tags.sort_unstable();
        let combined = tags.join("\0");
        Self::fx_hash(combined.as_bytes())
    }

    /// Maximum entities for full WL refinement (with neighbor propagation).
    const MAX_ENTITIES_FULL_WL: usize = 50_000;
    /// Maximum entities for any structural indexing.
    const MAX_ENTITIES_STRUCTURAL: usize = 200_000;

    /// Full recomputation of the structural index.
    pub fn compute(
        &mut self,
        entities: &HashMap<String, HDict>,
        id_map: &HashMap<String, usize>,
        adjacency_targets: impl Fn(&str) -> Vec<String>,
    ) {
        self.fingerprints.clear();
        self.partitions.clear();
        self.partition_tags.clear();

        if entities.is_empty() {
            self.stale = false;
            return;
        }

        // Skip entirely for very large graphs.
        if entities.len() > Self::MAX_ENTITIES_STRUCTURAL {
            self.stale = false;
            return;
        }

        // Adaptive depth: tag-hash only for large graphs.
        let effective_depth = if entities.len() > Self::MAX_ENTITIES_FULL_WL {
            0
        } else {
            self.depth
        };

        // Round 0: hash of sorted tag names per entity.
        let mut current: HashMap<String, u64> = HashMap::new();
        for (ref_val, entity) in entities {
            current.insert(ref_val.clone(), Self::round0_fingerprint(entity));
        }

        // Rounds 1..depth: incorporate neighbor fingerprints.
        for _ in 0..effective_depth {
            let mut next: HashMap<String, u64> = HashMap::new();
            for ref_val in entities.keys() {
                let own_fp = current[ref_val];
                let mut neighbor_fps: Vec<u64> = adjacency_targets(ref_val)
                    .iter()
                    .filter_map(|n| current.get(n).copied())
                    .collect();
                neighbor_fps.sort_unstable();

                // Hash: (own_fp, sorted neighbor fingerprints)
                let mut hasher = FxHasher::default();
                own_fp.hash(&mut hasher);
                for nfp in &neighbor_fps {
                    nfp.hash(&mut hasher);
                }
                next.insert(ref_val.clone(), hasher.finish());
            }
            current = next;
        }

        // Build partitions.
        for (ref_val, fp) in &current {
            self.fingerprints.insert(ref_val.clone(), *fp);
            if let Some(&eid) = id_map.get(ref_val) {
                self.partitions.entry(*fp).or_default().insert(eid as u32);
            }
        }

        // Build partition tag sets (store one representative's tags per partition).
        for (ref_val, fp) in &current {
            if !self.partition_tags.contains_key(fp)
                && let Some(entity) = entities.get(ref_val)
            {
                let mut tags: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();
                tags.sort();
                self.partition_tags.insert(*fp, tags);
            }
        }

        self.stale = false;
    }

    /// Get the fingerprint for an entity.
    pub fn fingerprint(&self, ref_val: &str) -> Option<u64> {
        self.fingerprints.get(ref_val).copied()
    }

    /// Get the entity IDs in a structural partition.
    pub fn partition(&self, fingerprint: u64) -> Option<&RoaringBitmap> {
        self.partitions.get(&fingerprint)
    }

    /// Find all partitions whose entities have ALL the given required tags.
    pub fn partitions_with_tags(&self, required_tags: &[&str]) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for (fp, tags) in &self.partition_tags {
            if required_tags.iter().all(|rt| tags.iter().any(|t| t == rt))
                && let Some(bm) = self.partitions.get(fp)
            {
                result |= bm;
            }
        }
        result
    }

    /// Number of distinct structural partitions.
    pub fn partition_count(&self) -> usize {
        self.partitions.len()
    }

    /// Whether the index needs recomputation.
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Mark the index as needing recomputation.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Get a histogram of fingerprints: fp → count.
    pub fn histogram(&self) -> HashMap<u64, u64> {
        self.partitions
            .iter()
            .map(|(fp, bm)| (*fp, bm.len()))
            .collect()
    }
}

impl Default for StructuralIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{HRef, Kind};

    fn make_entity(id: &str, tags: &[&str], refs: &[(&str, &str)]) -> HDict {
        let mut e = HDict::new();
        e.set("id", Kind::Ref(HRef::from_val(id)));
        for tag in tags {
            e.set(*tag, Kind::Marker);
        }
        for (tag, target) in refs {
            e.set(*tag, Kind::Ref(HRef::from_val(*target)));
        }
        e
    }

    fn adjacency_fn<'a>(entities: &'a HashMap<String, HDict>) -> impl Fn(&str) -> Vec<String> + 'a {
        move |ref_val: &str| match entities.get(ref_val) {
            Some(e) => e
                .iter()
                .filter_map(|(name, val)| {
                    if name != "id"
                        && let Kind::Ref(r) = val
                    {
                        return Some(r.val.clone());
                    }
                    None
                })
                .collect(),
            None => Vec::new(),
        }
    }

    #[test]
    fn identical_structures_get_same_fingerprint() {
        let mut entities = HashMap::new();
        let mut id_map = HashMap::new();

        entities.insert(
            "vav-1".to_string(),
            make_entity("vav-1", &["vav", "equip"], &[("siteRef", "site-1")]),
        );
        entities.insert(
            "vav-2".to_string(),
            make_entity("vav-2", &["vav", "equip"], &[("siteRef", "site-1")]),
        );
        entities.insert("site-1".to_string(), make_entity("site-1", &["site"], &[]));
        id_map.insert("vav-1".to_string(), 0);
        id_map.insert("vav-2".to_string(), 1);
        id_map.insert("site-1".to_string(), 2);

        let mut si = StructuralIndex::new();
        si.compute(&entities, &id_map, adjacency_fn(&entities));

        assert_eq!(si.fingerprint("vav-1"), si.fingerprint("vav-2"));
        assert_ne!(si.fingerprint("vav-1"), si.fingerprint("site-1"));
        assert_eq!(si.partition_count(), 2);
    }

    #[test]
    fn different_structures_get_different_fingerprints() {
        let mut entities = HashMap::new();
        let mut id_map = HashMap::new();

        entities.insert(
            "sensor-1".to_string(),
            make_entity("sensor-1", &["point", "sensor"], &[]),
        );
        entities.insert(
            "cmd-1".to_string(),
            make_entity("cmd-1", &["point", "cmd"], &[]),
        );
        id_map.insert("sensor-1".to_string(), 0);
        id_map.insert("cmd-1".to_string(), 1);

        let mut si = StructuralIndex::new();
        si.compute(&entities, &id_map, |_| Vec::new());

        assert_ne!(si.fingerprint("sensor-1"), si.fingerprint("cmd-1"));
    }

    #[test]
    fn partitions_with_tags_returns_matching() {
        let mut entities = HashMap::new();
        let mut id_map = HashMap::new();

        entities.insert(
            "s-1".to_string(),
            make_entity("s-1", &["point", "sensor", "temp"], &[]),
        );
        entities.insert(
            "s-2".to_string(),
            make_entity("s-2", &["point", "sensor", "temp"], &[]),
        );
        entities.insert(
            "c-1".to_string(),
            make_entity("c-1", &["point", "cmd"], &[]),
        );
        id_map.insert("s-1".to_string(), 0);
        id_map.insert("s-2".to_string(), 1);
        id_map.insert("c-1".to_string(), 2);

        let mut si = StructuralIndex::new();
        si.compute(&entities, &id_map, |_| Vec::new());

        let result = si.partitions_with_tags(&["point", "sensor"]);
        assert_eq!(result.len(), 2);
        assert!(result.contains(0));
        assert!(result.contains(1));
        assert!(!result.contains(2));
    }

    #[test]
    fn histogram_reflects_partition_sizes() {
        let mut entities = HashMap::new();
        let mut id_map = HashMap::new();

        for i in 0..5 {
            let id = format!("vav-{i}");
            entities.insert(id.clone(), make_entity(&id, &["vav", "equip"], &[]));
            id_map.insert(id, i);
        }
        entities.insert("site-1".to_string(), make_entity("site-1", &["site"], &[]));
        id_map.insert("site-1".to_string(), 5);

        let mut si = StructuralIndex::new();
        si.compute(&entities, &id_map, |_| Vec::new());

        let hist = si.histogram();
        assert_eq!(hist.len(), 2);
        assert!(hist.values().any(|&count| count == 5));
        assert!(hist.values().any(|&count| count == 1));
    }

    #[test]
    fn stale_tracking() {
        let mut si = StructuralIndex::new();
        assert!(si.is_stale());

        si.compute(&HashMap::new(), &HashMap::new(), |_| Vec::new());
        assert!(!si.is_stale());

        si.mark_stale();
        assert!(si.is_stale());
    }

    #[test]
    fn empty_graph_produces_no_partitions() {
        let mut si = StructuralIndex::new();
        si.compute(&HashMap::new(), &HashMap::new(), |_| Vec::new());
        assert_eq!(si.partition_count(), 0);
        assert!(!si.is_stale());
    }
}
