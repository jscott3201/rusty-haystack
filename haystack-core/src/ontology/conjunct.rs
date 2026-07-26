// ConjunctIndex -- decomposition of compound tag names.

use std::collections::HashMap;

/// Maps conjunct def names to their component parts.
///
/// A conjunct like `"hot-water"` decomposes into `["hot", "water"]`.
/// Components are the individual marker tags separated by `"-"`.
#[derive(Debug, Clone)]
pub struct ConjunctIndex {
    /// conjunct name -> component tag list
    parts: HashMap<String, Vec<String>>,
    /// case- and hyphen-insensitive name -> the one conjunct that spells it,
    /// or `None` where more than one does. See [`Self::resolve_normalized`].
    normalized: HashMap<String, Option<String>>,
}

/// Lowercase and drop hyphens, so every spelling of one conjunct collapses to
/// a single key: `fuelOil-output`, `FuelOilOutput`, and `fuel-oil-output` all
/// become `fueloiloutput`.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

impl ConjunctIndex {
    /// Create an empty conjunct index.
    pub fn new() -> Self {
        Self {
            parts: HashMap::new(),
            normalized: HashMap::new(),
        }
    }

    /// Register a conjunct decomposition.
    pub fn register(&mut self, conjunct: &str, parts: Vec<String>) {
        self.parts.insert(conjunct.to_string(), parts);
        match self.normalized.entry(normalize(conjunct)) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(Some(conjunct.to_string()));
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                // Re-registering the same conjunct is a rebuild, not a clash.
                if e.get().as_deref() != Some(conjunct) {
                    e.insert(None);
                }
            }
        }
    }

    /// Find the conjunct a case- and hyphen-insensitive name refers to.
    ///
    /// This is what lets a CamelCase spec term reach a conjunct whose own
    /// components are camelCase. `FuelOilOutput` cannot be converted to
    /// `fuelOil-output` by any rule — the capital that was a word boundary and
    /// the capital that was inside a component are the same character — so the
    /// query is answered by lookup rather than by transformation.
    ///
    /// Returns `None` when the name matches no conjunct, and also when it
    /// matches more than one: `a-bc` and `ab-c` both normalize to `abc`, and
    /// picking either would answer a genuinely ambiguous question.
    pub fn resolve_normalized(&self, name: &str) -> Option<&str> {
        self.normalized.get(&normalize(name))?.as_deref()
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

    fn reg(idx: &mut ConjunctIndex, name: &str) {
        idx.register(name, name.split('-').map(str::to_string).collect());
    }

    #[test]
    fn normalized_lookup_ignores_case_and_hyphens() {
        let mut idx = ConjunctIndex::new();
        reg(&mut idx, "elec-meter");

        for spelling in ["elec-meter", "ElecMeter", "elecmeter", "ELEC-METER"] {
            assert_eq!(
                idx.resolve_normalized(spelling),
                Some("elec-meter"),
                "{spelling} should reach elec-meter"
            );
        }
        assert_eq!(idx.resolve_normalized("water-meter"), None);
    }

    #[test]
    fn normalized_lookup_reaches_camel_case_components() {
        // The case a CamelCase-to-kebab conversion cannot serve: `FuelOilOutput`
        // would become `fuel-oil-output`, which is not a def.
        let mut idx = ConjunctIndex::new();
        reg(&mut idx, "fuelOil-output");

        assert_eq!(
            idx.resolve_normalized("FuelOilOutput"),
            Some("fuelOil-output")
        );
        assert_eq!(
            idx.resolve_normalized("fuelOil-output"),
            Some("fuelOil-output")
        );
    }

    #[test]
    fn an_ambiguous_normalized_name_resolves_to_nothing() {
        // `a-bc` and `ab-c` are different defs that share a normalized form.
        // Answering either would be a coin flip presented as a fact.
        let mut idx = ConjunctIndex::new();
        reg(&mut idx, "a-bc");
        assert_eq!(idx.resolve_normalized("ABc"), Some("a-bc"));

        reg(&mut idx, "ab-c");
        assert_eq!(idx.resolve_normalized("ABc"), None);
        assert_eq!(idx.resolve_normalized("abc"), None);

        // Both are still reachable by their exact names.
        assert!(idx.contains("a-bc"));
        assert!(idx.contains("ab-c"));
    }

    #[test]
    fn re_registering_the_same_conjunct_is_not_a_clash() {
        // Index rebuilds replay every def; that must not poison the lookup.
        let mut idx = ConjunctIndex::new();
        reg(&mut idx, "elec-meter");
        reg(&mut idx, "elec-meter");
        assert_eq!(idx.resolve_normalized("ElecMeter"), Some("elec-meter"));
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
