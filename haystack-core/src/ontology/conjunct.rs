// ConjunctIndex -- decomposition of compound tag names.

use std::collections::HashMap;

/// Maps conjunct def names to their component parts.
///
/// A conjunct like `"hot-water"` decomposes into `["hot", "water"]`.
/// Components are the individual marker tags separated by `"-"`.
pub struct ConjunctIndex {
    /// conjunct name -> component tag list
    parts: HashMap<String, Vec<String>>,
}

impl ConjunctIndex {
    /// Create an empty conjunct index.
    pub fn new() -> Self {
        Self {
            parts: HashMap::new(),
        }
    }

    /// Register a conjunct decomposition.
    pub fn register(&mut self, conjunct: &str, parts: Vec<String>) {
        self.parts.insert(conjunct.to_string(), parts);
    }

    /// Get component tags for a conjunct.
    ///
    /// Returns `None` if not a registered conjunct.
    pub fn decompose(&self, name: &str) -> Option<&[String]> {
        self.parts.get(name).map(|v| v.as_slice())
    }

    /// Check if a name is a registered conjunct.
    pub fn contains(&self, name: &str) -> bool {
        self.parts.contains_key(name)
    }

    /// Number of registered conjuncts.
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// Returns true if no conjuncts are registered.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

impl Default for ConjunctIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_decompose() {
        let mut idx = ConjunctIndex::new();
        idx.register("hot-water", vec!["hot".to_string(), "water".to_string()]);

        let parts = idx.decompose("hot-water").unwrap();
        assert_eq!(parts, &["hot", "water"]);
    }

    #[test]
    fn contains_check() {
        let mut idx = ConjunctIndex::new();
        idx.register("hot-water", vec!["hot".to_string(), "water".to_string()]);

        assert!(idx.contains("hot-water"));
        assert!(!idx.contains("cold-water"));
    }

    #[test]
    fn unknown_returns_none() {
        let idx = ConjunctIndex::new();
        assert!(idx.decompose("nonexistent").is_none());
    }

    #[test]
    fn len_and_empty() {
        let mut idx = ConjunctIndex::new();
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);

        idx.register("hot-water", vec!["hot".to_string(), "water".to_string()]);
        assert!(!idx.is_empty());
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn three_part_conjunct() {
        let mut idx = ConjunctIndex::new();
        idx.register(
            "ac-elec-meter",
            vec!["ac".to_string(), "elec".to_string(), "meter".to_string()],
        );

        let parts = idx.decompose("ac-elec-meter").unwrap();
        assert_eq!(parts, &["ac", "elec", "meter"]);
    }
}
