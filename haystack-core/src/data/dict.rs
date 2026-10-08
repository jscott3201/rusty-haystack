// Haystack Dict — a mutable tag dictionary backed by HashMap.

use crate::kinds::{HRef, Kind};
use std::collections::HashMap;
use std::fmt;

/// Haystack Dict — the fundamental entity/row type in Haystack.
///
/// An `HDict` is a mutable dictionary mapping tag names (`String`) to values (`Kind`).
/// Dicts are used as rows in grids, as entity records, and as metadata containers.
#[derive(Clone, Default)]
pub struct HDict {
    tags: HashMap<String, Kind>,
    // A conservative table-scan bound. HashMap::capacity() may decrease on
    // deletion due to tombstones even though its bucket allocation is retained.
    scan_bound: usize,
}

impl HDict {
    /// Create an empty dict.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a dict from a pre-built HashMap. Normalize its table once so an
    /// already-sparse caller map cannot hide retained buckets from metered
    /// traversal. Tag/value identity is unchanged; excess capacity may shrink.
    pub fn from_tags(tags: HashMap<String, Kind>) -> Self {
        // Move into an explicitly fresh table: collecting an IntoIter may reuse
        // its allocation, and shrink_to_fit does not promise a table shape.
        let mut normalized = HashMap::with_capacity(tags.len());
        normalized.extend(tags);
        let scan_bound = bucket_bound(normalized.capacity());
        Self {
            tags: normalized,
            scan_bound,
        }
    }

    pub(crate) fn try_with_capacity(
        capacity: usize,
    ) -> Result<Self, std::collections::TryReserveError> {
        let mut tags = HashMap::new();
        tags.try_reserve(capacity)?;
        let scan_bound = bucket_bound(tags.capacity());
        Ok(Self { tags, scan_bound })
    }

    /// Upper bound for a complete native table traversal, including buckets
    /// retained after deletion. Caller-owned storage is not new output memory.
    pub(crate) fn scan_bound(&self) -> usize {
        self.scan_bound
    }

    /// Returns `true` if the dict contains a tag with the given name.
    pub fn has(&self, name: &str) -> bool {
        self.tags.contains_key(name)
    }

    /// Returns a reference to the value for the given tag name, if present.
    pub fn get(&self, name: &str) -> Option<&Kind> {
        self.tags.get(name)
    }

    /// Returns `true` if the dict does NOT contain a tag with the given name.
    pub fn missing(&self, name: &str) -> bool {
        !self.tags.contains_key(name)
    }

    /// Returns the `id` tag value if it is a Ref, otherwise `None`.
    pub fn id(&self) -> Option<&HRef> {
        match self.tags.get("id") {
            Some(Kind::Ref(r)) => Some(r),
            _ => None,
        }
    }

    /// Returns the display string for this dict.
    ///
    /// Prefers the `dis` tag (if it is a `Str`), then falls back to the
    /// `id` ref's display name.
    pub fn dis(&self) -> Option<&str> {
        if let Some(Kind::Str(s)) = self.tags.get("dis") {
            return Some(s.as_str());
        }
        if let Some(r) = self.id() {
            return r.dis.as_deref();
        }
        None
    }

    /// Returns `true` if the dict has no tags.
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// Returns the number of tags in the dict.
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    /// Set (insert or overwrite) a tag.
    pub fn set(&mut self, name: impl Into<String>, val: Kind) {
        self.tags.insert(name.into(), val);
        self.scan_bound = self.scan_bound.max(bucket_bound(self.tags.capacity()));
    }

    /// Remove a tag by name, returning its value if it was present.
    pub fn remove_tag(&mut self, name: &str) -> Option<Kind> {
        self.tags.remove(name)
    }

    /// Merge another dict into this one.
    ///
    /// Tags from `other` overwrite tags in `self`. If a tag in `other` is
    /// `Kind::Remove`, the corresponding tag in `self` is removed instead.
    pub fn merge(&mut self, other: &HDict) {
        for (k, v) in &other.tags {
            match v {
                Kind::Remove => {
                    self.tags.remove(k);
                }
                _ => {
                    self.set(k.clone(), v.clone());
                }
            }
        }
    }

    /// Returns a reference to the underlying HashMap.
    pub fn tags(&self) -> &HashMap<String, Kind> {
        &self.tags
    }

    /// Iterate over `(name, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Kind)> {
        self.tags.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Return tags sorted by key name.
    pub fn sorted_tags(&self) -> Vec<(&str, &Kind)> {
        let mut pairs: Vec<_> = self.tags.iter().map(|(k, v)| (k.as_str(), v)).collect();
        pairs.sort_unstable_by_key(|(k, _)| *k);
        pairs
    }

    /// Iterate over tag names.
    pub fn tag_names(&self) -> impl Iterator<Item = &str> {
        self.tags.keys().map(|k| k.as_str())
    }

    /// Collect all tag names into a HashSet.
    pub fn tag_name_set(&self) -> std::collections::HashSet<&str> {
        self.tags.keys().map(|k| k.as_str()).collect()
    }
}

// Rust 1.99 HashMap uses power-of-two buckets with at most 7/8 occupancy.
// Fresh/normalized capacity rounds back to the allocated bucket count. Empty
// maps need no scan; removals never lower the recorded bound.
fn bucket_bound(capacity: usize) -> usize {
    if capacity == 0 {
        0
    } else {
        capacity.checked_next_power_of_two().unwrap_or(usize::MAX)
    }
}

impl fmt::Debug for HDict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HDict").field("tags", &self.tags).finish()
    }
}

impl PartialEq for HDict {
    fn eq(&self, other: &Self) -> bool {
        self.tags == other.tags
    }
}

impl Eq for HDict {}

impl std::hash::Hash for HDict {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.len().hash(state);
        for (name, value) in self.sorted_tags() {
            name.hash(state);
            value.hash(state);
        }
    }
}

impl fmt::Display for HDict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HDict({{")?;
        let mut first = true;
        // Sort keys for deterministic output
        let mut keys: Vec<&String> = self.tags.keys().collect();
        keys.sort();
        for k in keys {
            let v = &self.tags[k];
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "{k}: {v}")?;
            first = false;
        }
        write!(f, "}})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::Number;

    #[test]
    fn empty_dict() {
        let d = HDict::new();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
        assert!(d.missing("anything"));
        assert!(!d.has("anything"));
        assert_eq!(d.get("anything"), None);
    }

    #[test]
    fn set_get_has_missing() {
        let mut d = HDict::new();
        d.set("site", Kind::Marker);
        d.set("area", Kind::Number(Number::unitless(4500.0)));
        d.set("dis", Kind::Str("Main Site".into()));

        assert!(d.has("site"));
        assert!(!d.missing("site"));
        assert_eq!(d.get("site"), Some(&Kind::Marker));

        assert!(d.has("area"));
        assert_eq!(d.get("area"), Some(&Kind::Number(Number::unitless(4500.0))));

        assert!(d.has("dis"));
        assert_eq!(d.get("dis"), Some(&Kind::Str("Main Site".into())));

        assert!(d.missing("nonexistent"));
        assert_eq!(d.get("nonexistent"), None);

        assert_eq!(d.len(), 3);
        assert!(!d.is_empty());
    }

    #[test]
    fn id_with_ref() {
        let mut d = HDict::new();
        let r = HRef::new("site-1", Some("Main Site".into()));
        d.set("id", Kind::Ref(r));

        let id = d.id().unwrap();
        assert_eq!(id.val, "site-1");
        assert_eq!(id.dis, Some("Main Site".into()));
    }

    #[test]
    fn id_with_non_ref() {
        let mut d = HDict::new();
        d.set("id", Kind::Str("not-a-ref".into()));
        assert!(d.id().is_none());
    }

    #[test]
    fn id_missing() {
        let d = HDict::new();
        assert!(d.id().is_none());
    }

    #[test]
    fn dis_from_dis_tag() {
        let mut d = HDict::new();
        d.set("dis", Kind::Str("My Building".into()));
        d.set("id", Kind::Ref(HRef::new("b-1", Some("Ref Dis".into()))));

        // dis tag takes priority over id ref dis
        assert_eq!(d.dis(), Some("My Building"));
    }

    #[test]
    fn dis_from_ref_fallback() {
        let mut d = HDict::new();
        d.set(
            "id",
            Kind::Ref(HRef::new("b-1", Some("Ref Display".into()))),
        );

        // No dis tag, falls back to ref dis
        assert_eq!(d.dis(), Some("Ref Display"));
    }

    #[test]
    fn dis_from_ref_without_dis() {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val("b-1")));

        // No dis tag, ref has no dis either
        assert_eq!(d.dis(), None);
    }

    #[test]
    fn dis_missing_entirely() {
        let d = HDict::new();
        assert_eq!(d.dis(), None);
    }

    #[test]
    fn dis_non_str_dis_tag() {
        let mut d = HDict::new();
        // dis tag exists but is not a Str
        d.set("dis", Kind::Number(Number::unitless(42.0)));
        assert_eq!(d.dis(), None);
    }

    #[test]
    fn merge_updates_and_adds() {
        let mut base = HDict::new();
        base.set("site", Kind::Marker);
        base.set("area", Kind::Number(Number::unitless(1000.0)));

        let mut update = HDict::new();
        update.set("area", Kind::Number(Number::unitless(2000.0)));
        update.set("geoCity", Kind::Str("Richmond".into()));

        base.merge(&update);

        assert_eq!(base.get("site"), Some(&Kind::Marker));
        assert_eq!(
            base.get("area"),
            Some(&Kind::Number(Number::unitless(2000.0)))
        );
        assert_eq!(base.get("geoCity"), Some(&Kind::Str("Richmond".into())));
        assert_eq!(base.len(), 3);
    }

    #[test]
    fn merge_with_remove() {
        let mut base = HDict::new();
        base.set("site", Kind::Marker);
        base.set("area", Kind::Number(Number::unitless(1000.0)));
        base.set("dis", Kind::Str("Old Name".into()));

        let mut update = HDict::new();
        update.set("area", Kind::Remove); // should remove area
        update.set("dis", Kind::Str("New Name".into())); // should overwrite dis

        base.merge(&update);

        assert!(base.has("site"));
        assert!(base.missing("area")); // removed
        assert_eq!(base.get("dis"), Some(&Kind::Str("New Name".into())));
        assert_eq!(base.len(), 2);
    }

    #[test]
    fn remove_tag() {
        let mut d = HDict::new();
        d.set("a", Kind::Marker);
        d.set("b", Kind::Str("hello".into()));

        let removed = d.remove_tag("a");
        assert_eq!(removed, Some(Kind::Marker));
        assert!(d.missing("a"));
        assert_eq!(d.len(), 1);

        let not_found = d.remove_tag("nonexistent");
        assert_eq!(not_found, None);
    }

    #[test]
    fn from_tags() {
        let mut map = HashMap::new();
        map.insert("site".to_string(), Kind::Marker);
        map.insert("dis".to_string(), Kind::Str("Test".into()));

        let d = HDict::from_tags(map);
        assert_eq!(d.len(), 2);
        assert!(d.has("site"));
        assert!(d.has("dis"));
    }

    #[test]
    fn tag_iteration() {
        let mut d = HDict::new();
        d.set("a", Kind::Marker);
        d.set("b", Kind::Str("hello".into()));
        d.set("c", Kind::Number(Number::unitless(3.0)));

        let pairs: Vec<(&str, &Kind)> = d.iter().collect();
        assert_eq!(pairs.len(), 3);

        // Check all tags are present (order not guaranteed)
        let names: std::collections::HashSet<&str> = d.tag_names().collect();
        assert!(names.contains("a"));
        assert!(names.contains("b"));
        assert!(names.contains("c"));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn tag_name_set() {
        let mut d = HDict::new();
        d.set("alpha", Kind::Marker);
        d.set("beta", Kind::Marker);

        let set = d.tag_name_set();
        assert_eq!(set.len(), 2);
        assert!(set.contains("alpha"));
        assert!(set.contains("beta"));
    }

    #[test]
    fn equality() {
        let mut a = HDict::new();
        a.set("x", Kind::Number(Number::unitless(1.0)));
        a.set("y", Kind::Str("hi".into()));

        let mut b = HDict::new();
        b.set("x", Kind::Number(Number::unitless(1.0)));
        b.set("y", Kind::Str("hi".into()));

        assert_eq!(a, b);
    }

    #[test]
    fn inequality_different_values() {
        let mut a = HDict::new();
        a.set("x", Kind::Number(Number::unitless(1.0)));

        let mut b = HDict::new();
        b.set("x", Kind::Number(Number::unitless(2.0)));

        assert_ne!(a, b);
    }

    #[test]
    fn inequality_different_keys() {
        let mut a = HDict::new();
        a.set("x", Kind::Marker);

        let mut b = HDict::new();
        b.set("y", Kind::Marker);

        assert_ne!(a, b);
    }

    #[test]
    fn display_empty() {
        let d = HDict::new();
        assert_eq!(d.to_string(), "HDict({})");
    }

    #[test]
    fn display_with_tags() {
        let mut d = HDict::new();
        d.set("site", Kind::Marker);

        let s = d.to_string();
        assert!(s.starts_with("HDict({"));
        assert!(s.ends_with("})"));
        assert!(s.contains("site"));
    }

    #[test]
    fn overwrite_existing_tag() {
        let mut d = HDict::new();
        d.set("val", Kind::Number(Number::unitless(1.0)));
        d.set("val", Kind::Number(Number::unitless(2.0)));

        assert_eq!(d.len(), 1);
        assert_eq!(d.get("val"), Some(&Kind::Number(Number::unitless(2.0))));
    }

    #[test]
    fn default_is_empty() {
        let d = HDict::default();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn tags_returns_inner_map() {
        let mut d = HDict::new();
        d.set("a", Kind::Marker);
        let tags = d.tags();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags.get("a"), Some(&Kind::Marker));
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    #[test]
    fn scan_bound_survives_deletion_growth_clone_and_merge() {
        let mut dict = HDict::try_with_capacity(128).unwrap();
        let capacity = dict.tags().capacity();
        for index in 0..capacity {
            dict.set(index.to_string(), Kind::Bool(true));
        }
        let bound = dict.scan_bound();
        let names: Vec<_> = dict.tag_names().map(str::to_owned).collect();
        for name in &names[1..] {
            dict.remove_tag(name);
        }
        assert_eq!(dict.scan_bound(), bound);
        assert_eq!(dict.clone().scan_bound(), bound);
        assert_eq!(dict.len(), 1);
        let mut equivalent = HDict::new();
        equivalent.set(&names[0], Kind::Bool(true));
        assert_eq!(dict, equivalent);
        assert_eq!(format!("{dict:?}"), format!("{equivalent:?}"));
        use std::hash::{Hash, Hasher};
        let mut original_hash = std::collections::hash_map::DefaultHasher::new();
        let mut equivalent_hash = std::collections::hash_map::DefaultHasher::new();
        dict.hash(&mut original_hash);
        equivalent.hash(&mut equivalent_hash);
        assert_eq!(original_hash.finish(), equivalent_hash.finish());
        let mut changes = HDict::new();
        for index in 0..(capacity * 4) {
            changes.set(format!("new{index}"), Kind::Bool(false));
        }
        dict.merge(&changes);
        assert!(dict.scan_bound() >= bucket_bound(dict.tags().capacity()));
        let mut remove = HDict::new();
        for name in dict.tag_names() {
            remove.set(name, Kind::Remove);
        }
        let grown_bound = dict.scan_bound();
        dict.merge(&remove);
        assert_eq!(dict.scan_bound(), grown_bound);
        assert!(dict.is_empty());
    }
    #[test]
    fn incoming_sparse_map_is_normalized_without_identity_loss() {
        let mut tags = HashMap::with_capacity(16_384);
        let capacity = tags.capacity();
        for index in 0..capacity {
            tags.insert(index.to_string(), Kind::Int(index as i64));
        }
        tags.retain(|name, _| name == "42");
        let dict = HDict::from_tags(tags);
        assert_eq!(dict.len(), 1);
        assert_eq!(dict.get("42"), Some(&Kind::Int(42)));
        assert_eq!(dict.scan_bound(), 4);
        assert_eq!(dict.tags().capacity(), 3);
    }
}
