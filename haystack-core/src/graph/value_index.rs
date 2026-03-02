// Value-level secondary indexes using B-Tree maps.
//
// Provides O(log N + result_size) range queries for comparison-based filters
// (e.g. `temp > 72`, `area == 500`) instead of scanning all entities.

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;

use crate::kinds::Kind;

/// Orderable wrapper around a subset of Kind values that support comparison.
///
/// Only Number and Str are indexed (the most common comparison targets in
/// Haystack filter expressions). Other kinds are silently skipped.
#[derive(Debug, Clone)]
enum OrderableKind {
    Num(OrderedF64),
    Str(String),
}

/// f64 wrapper with total ordering (NaN < everything, then normal f64 order).
#[derive(Debug, Clone, Copy)]
struct OrderedF64(f64);

impl PartialEq for OrderedF64 {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == std::cmp::Ordering::Equal
    }
}
impl Eq for OrderedF64 {}

impl PartialOrd for OrderedF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OrderedF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl PartialEq for OrderableKind {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for OrderableKind {}

impl PartialOrd for OrderableKind {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderableKind {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (OrderableKind::Num(a), OrderableKind::Num(b)) => a.cmp(b),
            (OrderableKind::Str(a), OrderableKind::Str(b)) => a.cmp(b),
            // Numbers sort before strings for cross-type ordering.
            (OrderableKind::Num(_), OrderableKind::Str(_)) => std::cmp::Ordering::Less,
            (OrderableKind::Str(_), OrderableKind::Num(_)) => std::cmp::Ordering::Greater,
        }
    }
}

impl OrderableKind {
    /// Try to convert a Kind value into an OrderableKind for indexing.
    fn from_kind(kind: &Kind) -> Option<Self> {
        match kind {
            Kind::Number(n) => Some(OrderableKind::Num(OrderedF64(n.val))),
            Kind::Str(s) => Some(OrderableKind::Str(s.clone())),
            _ => None,
        }
    }
}

/// A collection of B-Tree indexes keyed by field name.
///
/// Each index maps orderable values to the set of entity IDs that have that
/// value for the given field. Supports efficient range lookups.
pub struct ValueIndex {
    /// field_name → BTreeMap<value, Vec<entity_id>>
    indexes: HashMap<String, BTreeMap<OrderableKind, Vec<usize>>>,
}

impl ValueIndex {
    /// Create an empty value index.
    pub fn new() -> Self {
        Self {
            indexes: HashMap::new(),
        }
    }

    /// Register a field for indexing. Call this before adding entities.
    pub fn index_field(&mut self, field: &str) {
        self.indexes
            .entry(field.to_string())
            .or_insert_with(BTreeMap::new);
    }

    /// Returns the set of indexed field names.
    pub fn indexed_fields(&self) -> impl Iterator<Item = &str> {
        self.indexes.keys().map(|s| s.as_str())
    }

    /// Returns true if a given field has a value index.
    pub fn has_index(&self, field: &str) -> bool {
        self.indexes.contains_key(field)
    }

    /// Add an entity's value to the index for a given field.
    pub fn add(&mut self, entity_id: usize, field: &str, value: &Kind) {
        if let Some(tree) = self.indexes.get_mut(field)
            && let Some(key) = OrderableKind::from_kind(value)
        {
            tree.entry(key).or_insert_with(Vec::new).push(entity_id);
        }
    }

    /// Remove an entity from the index for a given field/value.
    pub fn remove(&mut self, entity_id: usize, field: &str, value: &Kind) {
        if let Some(tree) = self.indexes.get_mut(field)
            && let Some(key) = OrderableKind::from_kind(value)
        {
            if let Some(ids) = tree.get_mut(&key) {
                ids.retain(|&id| id != entity_id);
                if ids.is_empty() {
                    tree.remove(&key);
                }
            }
        }
    }

    /// Look up entity IDs where field == val.
    pub fn eq_lookup(&self, field: &str, val: &Kind) -> Vec<usize> {
        let key = match OrderableKind::from_kind(val) {
            Some(k) => k,
            None => return Vec::new(),
        };
        self.indexes
            .get(field)
            .and_then(|tree| tree.get(&key))
            .cloned()
            .unwrap_or_default()
    }

    /// Look up entity IDs where field != val (all indexed minus exact match).
    pub fn ne_lookup(&self, field: &str, val: &Kind) -> Vec<usize> {
        let key = match OrderableKind::from_kind(val) {
            Some(k) => k,
            None => return Vec::new(),
        };
        let Some(tree) = self.indexes.get(field) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for (k, ids) in tree {
            if k != &key {
                result.extend(ids);
            }
        }
        result
    }

    /// Look up entity IDs where field > val.
    pub fn gt_lookup(&self, field: &str, val: &Kind) -> Vec<usize> {
        let key = match OrderableKind::from_kind(val) {
            Some(k) => k,
            None => return Vec::new(),
        };
        let Some(tree) = self.indexes.get(field) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for (_, ids) in tree.range((Bound::Excluded(key), Bound::Unbounded)) {
            result.extend(ids);
        }
        result
    }

    /// Look up entity IDs where field >= val.
    pub fn ge_lookup(&self, field: &str, val: &Kind) -> Vec<usize> {
        let key = match OrderableKind::from_kind(val) {
            Some(k) => k,
            None => return Vec::new(),
        };
        let Some(tree) = self.indexes.get(field) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for (_, ids) in tree.range((Bound::Included(key), Bound::Unbounded)) {
            result.extend(ids);
        }
        result
    }

    /// Look up entity IDs where field < val.
    pub fn lt_lookup(&self, field: &str, val: &Kind) -> Vec<usize> {
        let key = match OrderableKind::from_kind(val) {
            Some(k) => k,
            None => return Vec::new(),
        };
        let Some(tree) = self.indexes.get(field) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for (_, ids) in tree.range((Bound::Unbounded, Bound::Excluded(key))) {
            result.extend(ids);
        }
        result
    }

    /// Look up entity IDs where field <= val.
    pub fn le_lookup(&self, field: &str, val: &Kind) -> Vec<usize> {
        let key = match OrderableKind::from_kind(val) {
            Some(k) => k,
            None => return Vec::new(),
        };
        let Some(tree) = self.indexes.get(field) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for (_, ids) in tree.range((Bound::Unbounded, Bound::Included(key))) {
            result.extend(ids);
        }
        result
    }

    /// Clear all indexes.
    pub fn clear(&mut self) {
        for tree in self.indexes.values_mut() {
            tree.clear();
        }
    }
}

impl Default for ValueIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::Number;

    #[test]
    fn eq_lookup_returns_matching_ids() {
        let mut idx = ValueIndex::new();
        idx.index_field("temp");
        idx.add(0, "temp", &Kind::Number(Number::unitless(72.0)));
        idx.add(1, "temp", &Kind::Number(Number::unitless(68.0)));
        idx.add(2, "temp", &Kind::Number(Number::unitless(72.0)));

        let result = idx.eq_lookup("temp", &Kind::Number(Number::unitless(72.0)));
        assert_eq!(result, vec![0, 2]);
    }

    #[test]
    fn gt_lookup_returns_greater_ids() {
        let mut idx = ValueIndex::new();
        idx.index_field("area");
        idx.add(0, "area", &Kind::Number(Number::unitless(100.0)));
        idx.add(1, "area", &Kind::Number(Number::unitless(500.0)));
        idx.add(2, "area", &Kind::Number(Number::unitless(200.0)));
        idx.add(3, "area", &Kind::Number(Number::unitless(50.0)));

        let result = idx.gt_lookup("area", &Kind::Number(Number::unitless(150.0)));
        assert!(result.contains(&2)); // 200
        assert!(result.contains(&1)); // 500
        assert!(!result.contains(&0)); // 100
        assert!(!result.contains(&3)); // 50
    }

    #[test]
    fn lt_lookup_returns_lesser_ids() {
        let mut idx = ValueIndex::new();
        idx.index_field("area");
        idx.add(0, "area", &Kind::Number(Number::unitless(100.0)));
        idx.add(1, "area", &Kind::Number(Number::unitless(500.0)));
        idx.add(2, "area", &Kind::Number(Number::unitless(200.0)));

        let result = idx.lt_lookup("area", &Kind::Number(Number::unitless(200.0)));
        assert_eq!(result, vec![0]); // 100 < 200
    }

    #[test]
    fn string_index_works() {
        let mut idx = ValueIndex::new();
        idx.index_field("dis");
        idx.add(0, "dis", &Kind::Str("Alpha".to_string()));
        idx.add(1, "dis", &Kind::Str("Beta".to_string()));
        idx.add(2, "dis", &Kind::Str("Alpha".to_string()));

        let result = idx.eq_lookup("dis", &Kind::Str("Alpha".to_string()));
        assert_eq!(result, vec![0, 2]);
    }

    #[test]
    fn unindexed_field_returns_empty() {
        let idx = ValueIndex::new();
        let result = idx.eq_lookup("temp", &Kind::Number(Number::unitless(72.0)));
        assert!(result.is_empty());
    }

    #[test]
    fn remove_entity_from_index() {
        let mut idx = ValueIndex::new();
        idx.index_field("temp");
        idx.add(0, "temp", &Kind::Number(Number::unitless(72.0)));
        idx.add(1, "temp", &Kind::Number(Number::unitless(72.0)));

        idx.remove(0, "temp", &Kind::Number(Number::unitless(72.0)));

        let result = idx.eq_lookup("temp", &Kind::Number(Number::unitless(72.0)));
        assert_eq!(result, vec![1]);
    }

    #[test]
    fn ne_lookup_excludes_matching() {
        let mut idx = ValueIndex::new();
        idx.index_field("status");
        idx.add(0, "status", &Kind::Str("active".to_string()));
        idx.add(1, "status", &Kind::Str("inactive".to_string()));
        idx.add(2, "status", &Kind::Str("active".to_string()));

        let result = idx.ne_lookup("status", &Kind::Str("active".to_string()));
        assert_eq!(result, vec![1]);
    }

    #[test]
    fn ge_and_le_lookups() {
        let mut idx = ValueIndex::new();
        idx.index_field("temp");
        idx.add(0, "temp", &Kind::Number(Number::unitless(70.0)));
        idx.add(1, "temp", &Kind::Number(Number::unitless(72.0)));
        idx.add(2, "temp", &Kind::Number(Number::unitless(74.0)));

        let ge = idx.ge_lookup("temp", &Kind::Number(Number::unitless(72.0)));
        assert!(ge.contains(&1)); // 72
        assert!(ge.contains(&2)); // 74
        assert!(!ge.contains(&0)); // 70

        let le = idx.le_lookup("temp", &Kind::Number(Number::unitless(72.0)));
        assert!(le.contains(&0)); // 70
        assert!(le.contains(&1)); // 72
        assert!(!le.contains(&2)); // 74
    }
}
