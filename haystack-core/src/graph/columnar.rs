// Columnar entity storage — struct-of-arrays layout for cache-friendly scans.
//
// Provides an auxiliary column-oriented view of entity data, indexed by
// entity numeric ID (same IDs used in bitmap/value indexes). This enables
// SIMD-friendly, cache-line-efficient iteration over a single tag across
// all entities — ideal for scan-heavy filter queries.
//
// This is a **supplementary** store; the authoritative data remains in the
// HashMap<String, HDict>. Columns are lazily populated and rebuilt on demand.

use std::collections::HashMap;

use crate::kinds::Kind;

/// Column-oriented storage for entity tags, indexed by entity ID.
///
/// Each "column" is a `Vec<Option<Kind>>` where index `i` corresponds to
/// entity ID `i`. Missing entities or missing tags for that entity are `None`.
///
/// Benefits:
/// - Sequential memory access when scanning a single tag across all entities
/// - Cache-line prefetching works optimally (no pointer chasing through HDict)
/// - Natural alignment with bitmap indexes (same entity ID space)
pub struct ColumnarStore {
    /// Tag name → column of values indexed by entity ID.
    columns: HashMap<String, Vec<Option<Kind>>>,
    /// Set of tag names that are actively tracked as columns.
    tracked_tags: Vec<String>,
    /// Allocated capacity (max entity ID + 1).
    capacity: usize,
}

impl ColumnarStore {
    pub fn new() -> Self {
        Self {
            columns: HashMap::new(),
            tracked_tags: Vec::new(),
            capacity: 0,
        }
    }

    /// Register a tag to be tracked as a column. Must call `rebuild()` after
    /// registering new tags to populate from existing entities.
    pub fn track_tag(&mut self, tag: &str) {
        if !self.tracked_tags.iter().any(|t| t == tag) {
            self.tracked_tags.push(tag.to_string());
            self.columns
                .insert(tag.to_string(), vec![None; self.capacity]);
        }
    }

    /// Returns true if the given tag is tracked as a column.
    pub fn is_tracked(&self, tag: &str) -> bool {
        self.columns.contains_key(tag)
    }

    /// Set the value for entity `eid` at the given tag column.
    pub fn set(&mut self, eid: usize, tag: &str, value: &Kind) {
        if let Some(col) = self.columns.get_mut(tag) {
            if eid >= col.len() {
                col.resize(eid + 1, None);
                if eid >= self.capacity {
                    self.capacity = eid + 1;
                }
            }
            col[eid] = Some(value.clone());
        }
    }

    /// Clear the value for entity `eid` at the given tag column.
    pub fn clear_entity(&mut self, eid: usize) {
        for col in self.columns.values_mut() {
            if eid < col.len() {
                col[eid] = None;
            }
        }
    }

    /// Ensure all columns can hold at least `new_capacity` entries.
    pub fn ensure_capacity(&mut self, new_capacity: usize) {
        if new_capacity > self.capacity {
            for col in self.columns.values_mut() {
                col.resize(new_capacity, None);
            }
            self.capacity = new_capacity;
        }
    }

    /// Get a column slice for a tracked tag. Returns None if the tag is not tracked.
    pub fn column(&self, tag: &str) -> Option<&[Option<Kind>]> {
        self.columns.get(tag).map(|c| c.as_slice())
    }

    /// Get the value for a specific entity and tag.
    pub fn get(&self, eid: usize, tag: &str) -> Option<&Kind> {
        self.columns.get(tag)?.get(eid).and_then(|opt| opt.as_ref())
    }

    /// Scan a column and return entity IDs where the predicate matches.
    /// This is the primary performance advantage: sequential memory access.
    pub fn scan_column<F>(&self, tag: &str, predicate: F) -> Vec<usize>
    where
        F: Fn(&Kind) -> bool,
    {
        match self.columns.get(tag) {
            Some(col) => col
                .iter()
                .enumerate()
                .filter_map(|(eid, val)| val.as_ref().filter(|v| predicate(v)).map(|_| eid))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Number of tracked columns.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Current capacity (max entity IDs).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Clear all column data (keeps tracked tags registered).
    pub fn clear(&mut self) {
        for col in self.columns.values_mut() {
            col.fill(None);
        }
    }

    /// Tracked tag names.
    pub fn tracked_tags(&self) -> &[String] {
        &self.tracked_tags
    }
}

impl Default for ColumnarStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::Number;

    #[test]
    fn track_and_set_values() {
        let mut store = ColumnarStore::new();
        store.track_tag("temp");
        store.ensure_capacity(3);

        store.set(0, "temp", &Kind::Number(Number::unitless(72.0)));
        store.set(2, "temp", &Kind::Number(Number::unitless(68.5)));

        assert!(store.get(0, "temp").is_some());
        assert!(store.get(1, "temp").is_none()); // Entity 1 has no temp
        assert!(store.get(2, "temp").is_some());
    }

    #[test]
    fn scan_column_numeric() {
        let mut store = ColumnarStore::new();
        store.track_tag("temp");
        store.ensure_capacity(5);

        store.set(0, "temp", &Kind::Number(Number::unitless(72.0)));
        store.set(1, "temp", &Kind::Number(Number::unitless(68.5)));
        store.set(2, "temp", &Kind::Number(Number::unitless(75.0)));
        store.set(3, "temp", &Kind::Number(Number::unitless(65.0)));

        let above_70: Vec<usize> = store.scan_column("temp", |k| match k {
            Kind::Number(n) => n.val > 70.0,
            _ => false,
        });
        assert_eq!(above_70, vec![0, 2]); // 72.0 and 75.0
    }

    #[test]
    fn scan_column_string() {
        let mut store = ColumnarStore::new();
        store.track_tag("dis");
        store.ensure_capacity(3);

        store.set(0, "dis", &Kind::Str("Building A".to_string()));
        store.set(1, "dis", &Kind::Str("Building B".to_string()));
        store.set(2, "dis", &Kind::Str("AHU-1".to_string()));

        let buildings: Vec<usize> = store.scan_column("dis", |k| match k {
            Kind::Str(s) => s.starts_with("Building"),
            _ => false,
        });
        assert_eq!(buildings, vec![0, 1]);
    }

    #[test]
    fn clear_entity() {
        let mut store = ColumnarStore::new();
        store.track_tag("temp");
        store.track_tag("dis");
        store.ensure_capacity(2);

        store.set(0, "temp", &Kind::Number(Number::unitless(72.0)));
        store.set(0, "dis", &Kind::Str("Sensor 1".to_string()));

        store.clear_entity(0);
        assert!(store.get(0, "temp").is_none());
        assert!(store.get(0, "dis").is_none());
    }

    #[test]
    fn untracked_tag_ignored() {
        let mut store = ColumnarStore::new();
        store.track_tag("temp");

        store.set(0, "humidity", &Kind::Number(Number::unitless(50.0)));
        assert!(store.get(0, "humidity").is_none());
        assert!(!store.is_tracked("humidity"));
    }

    #[test]
    fn auto_extend_capacity() {
        let mut store = ColumnarStore::new();
        store.track_tag("temp");

        // Set entity beyond initial capacity — should auto-extend.
        store.set(100, "temp", &Kind::Number(Number::unitless(72.0)));
        assert!(store.get(100, "temp").is_some());
        assert!(store.capacity() >= 101);
    }

    #[test]
    fn column_returns_slice() {
        let mut store = ColumnarStore::new();
        store.track_tag("temp");
        store.ensure_capacity(3);
        store.set(1, "temp", &Kind::Number(Number::unitless(72.0)));

        let col = store.column("temp").unwrap();
        assert_eq!(col.len(), 3);
        assert!(col[0].is_none());
        assert!(col[1].is_some());
        assert!(col[2].is_none());
    }

    #[test]
    fn scan_empty_column() {
        let mut store = ColumnarStore::new();
        store.track_tag("temp");
        let results = store.scan_column("temp", |_| true);
        assert!(results.is_empty());
    }

    #[test]
    fn scan_untracked_column() {
        let store = ColumnarStore::new();
        let results = store.scan_column("nonexistent", |_| true);
        assert!(results.is_empty());
    }
}
