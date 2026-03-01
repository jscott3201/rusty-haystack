// DefNamespace -- unified Haystack 4 type system.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::data::HDict;
use crate::kinds::Kind;
use crate::xeto::Spec;

use super::OntologyError;
use super::conjunct::ConjunctIndex;
use super::def::{Def, DefKind};
use super::lib::Lib;
use super::taxonomy::TaxonomyTree;
use super::trio_loader::load_trio;
use super::validation::{FitIssue, ValidationIssue};

/// Tracks how a library was loaded.
#[derive(Debug, Clone)]
pub enum LibSource {
    /// Bundled into the binary at compile time.
    Bundled,
    /// Loaded from Trio text.
    Trio(String),
    /// Loaded from Xeto text.
    Xeto(String),
    /// Loaded from a directory on disk.
    Directory(PathBuf),
}

/// Unified container for Haystack 4 defs.
///
/// Provides resolution, taxonomy queries, structural typing (`fits`),
/// and validation. Loads defs from Trio format.
pub struct DefNamespace {
    /// Symbol -> Def mapping.
    defs: HashMap<String, Def>,
    /// Library name -> Lib mapping.
    libs: HashMap<String, Lib>,
    /// Unified inheritance graph.
    taxonomy: TaxonomyTree,
    /// Conjunct decomposition index.
    conjuncts: ConjunctIndex,
    /// Set of def symbols that have the `mandatory` flag.
    mandatory_defs: HashSet<String>,
    /// Entity type -> tags that apply via tagOn.
    tag_on_index: HashMap<String, Vec<String>>,
    /// Choice def -> subtypes that are options.
    choice_index: HashMap<String, Vec<String>>,
    /// Xeto specs by qualified name (e.g. "ph::Ahu").
    specs: HashMap<String, Spec>,
    /// Library name -> list of spec qnames belonging to that lib.
    spec_libs: HashMap<String, Vec<String>>,
    /// Library name -> how it was loaded.
    lib_sources: HashMap<String, LibSource>,
}

impl DefNamespace {
    /// Create an empty namespace.
    pub fn new() -> Self {
        Self {
            defs: HashMap::new(),
            libs: HashMap::new(),
            taxonomy: TaxonomyTree::new(),
            conjuncts: ConjunctIndex::new(),
            mandatory_defs: HashSet::new(),
            tag_on_index: HashMap::new(),
            choice_index: HashMap::new(),
            specs: HashMap::new(),
            spec_libs: HashMap::new(),
            lib_sources: HashMap::new(),
        }
    }

    /// Load the bundled standard Haystack 4 defs.
    ///
    /// Loads ph, phScience, phIoT, and phIct libraries from the bundled
    /// `defs.trio` file.
    pub fn load_standard() -> Result<Self, OntologyError> {
        let source = include_str!("../../data/defs.trio");
        let mut ns = Self::new();
        let libs = load_trio(source)?;
        for lib in libs {
            let lib_name = lib.name.clone();
            ns.register_lib(lib);
            ns.set_lib_source(&lib_name, LibSource::Bundled);
        }

        // Load bundled Xeto libraries (best-effort).
        // Libraries are returned in dependency order so sequential loading works.
        for bundled in crate::xeto::bundled::bundled_libs() {
            match crate::xeto::loader::load_xeto_source(bundled.source, bundled.name, &ns) {
                Ok((lib, specs)) => {
                    // Only register the Lib if it wasn't already loaded from
                    // Trio — the xeto-produced Lib has an empty defs map and
                    // would overwrite the real one.
                    if !ns.libs().contains_key(lib.name.as_str()) {
                        ns.register_lib(lib);
                    }
                    for spec in specs {
                        ns.register_spec(spec);
                    }
                    ns.set_lib_source(bundled.name, LibSource::Bundled);
                }
                Err(_e) => {
                    // Best-effort: skip libraries that fail to parse.
                    // This is expected for some complex syntax our parser
                    // doesn't yet support.
                }
            }
        }

        Ok(ns)
    }

    /// Load defs from Trio text and register them in this namespace.
    pub fn load_trio_str(&mut self, source: &str) -> Result<Vec<Lib>, OntologyError> {
        let libs = load_trio(source)?;
        for lib in &libs {
            self.register_lib(lib.clone());
        }
        Ok(libs)
    }

    /// Register a library and all its defs.
    ///
    /// Uses a two-pass approach: first registers all defs (taxonomy,
    /// mandatory, conjuncts, tagOn), then builds the choice index so
    /// that parent defs are guaranteed to exist when checking.
    pub fn register_lib(&mut self, lib: Lib) {
        let defs: Vec<Def> = lib.defs.values().cloned().collect();
        self.libs.insert(lib.name.clone(), lib);

        // Pass 1: register all defs in basic indices
        let mut new_symbols: Vec<String> = Vec::new();
        for def in defs {
            let symbol = def.symbol.clone();
            new_symbols.push(symbol.clone());
            self.register_def_basic(def);
        }

        // Pass 2: build choice index now that all defs exist
        for symbol in &new_symbols {
            self.register_def_choice_index(symbol);
        }
    }

    /// Register a single def in basic indices (taxonomy, mandatory,
    /// conjuncts, tagOn). Does NOT build the choice index.
    fn register_def_basic(&mut self, def: Def) {
        let symbol = def.symbol.clone();

        // Taxonomy
        self.taxonomy.add(&symbol, &def.is_);

        // Mandatory index
        if def.mandatory {
            self.mandatory_defs.insert(symbol.clone());
        }

        // Conjunct index (defs with "-" in name)
        if symbol.contains('-') {
            let parts: Vec<String> = symbol.split('-').map(|s| s.to_string()).collect();
            self.conjuncts.register(&symbol, parts);
        }

        // tagOn index: which tags apply to which entity types
        for target in &def.tag_on {
            self.tag_on_index
                .entry(target.clone())
                .or_default()
                .push(symbol.clone());
        }

        // Add to defs
        self.defs.insert(symbol, def);
    }

    /// Build the choice index entry for a single def.
    ///
    /// Must be called after all defs in the batch are in `self.defs`
    /// so parent lookups succeed regardless of registration order.
    fn register_def_choice_index(&mut self, symbol: &str) {
        let is_ = match self.defs.get(symbol) {
            Some(def) => def.is_.clone(),
            None => return,
        };

        for parent in &is_ {
            if let Some(parent_def) = self.defs.get(parent) {
                if parent_def.kind() == DefKind::Choice {
                    self.choice_index
                        .entry(parent.clone())
                        .or_default()
                        .push(symbol.to_string());
                }
            }
        }
    }

    // -- Resolution --

    /// Look up a def by symbol.
    pub fn get_def(&self, symbol: &str) -> Option<&Def> {
        self.defs.get(symbol)
    }

    /// Resolve a name to a Def. In the future, this will also try Spec lookup.
    pub fn resolve(&self, name: &str) -> Option<&Def> {
        // TODO: Also check specs once Xeto Spec type is integrated
        self.get_def(name)
    }

    // -- Taxonomy --

    /// Check nominal subtype relationship.
    ///
    /// Returns `true` if `name` is a subtype of `supertype` (or equal).
    pub fn is_a(&self, name: &str, supertype: &str) -> bool {
        self.taxonomy.is_subtype(name, supertype)
    }

    /// Direct subtypes of a type.
    pub fn subtypes(&self, name: &str) -> Vec<String> {
        self.taxonomy.subtypes_of(name)
    }

    /// Full supertype chain (transitive, breadth-first).
    pub fn supertypes(&self, name: &str) -> Vec<String> {
        self.taxonomy.supertypes_of(name)
    }

    /// Mandatory marker tags for a type (cached).
    ///
    /// Walks the supertype chain and collects all mandatory markers.
    pub fn mandatory_tags(&self, name: &str) -> HashSet<String> {
        self.taxonomy.mandatory_tags(name, &self.mandatory_defs)
    }

    /// All tags that apply to an entity type via `tagOn`.
    pub fn tags_for(&self, name: &str) -> HashSet<String> {
        let mut tags: HashSet<String> = HashSet::new();
        // Direct tagOn
        if let Some(tag_list) = self.tag_on_index.get(name) {
            tags.extend(tag_list.iter().cloned());
        }
        // Tags from supertypes
        for sup in self.taxonomy.supertypes_of(name) {
            if let Some(tag_list) = self.tag_on_index.get(&sup) {
                tags.extend(tag_list.iter().cloned());
            }
        }
        tags
    }

    /// Decompose a conjunct name into component tags.
    pub fn conjunct_parts(&self, name: &str) -> Option<&[String]> {
        self.conjuncts.decompose(name)
    }

    /// Valid options for a choice def.
    pub fn choices(&self, choice_name: &str) -> Vec<String> {
        let choice_def = match self.defs.get(choice_name) {
            Some(d) => d,
            None => return vec![],
        };
        // If choice has 'of' tag, subtypes of that target are options
        if let Some(ref of_target) = choice_def.of {
            return self.taxonomy.all_subtypes(of_target);
        }
        // Otherwise, direct subtypes registered in the choice index
        self.choice_index
            .get(choice_name)
            .cloned()
            .unwrap_or_default()
    }

    // -- Structural Typing --

    /// Check if an entity structurally fits a type.
    ///
    /// Checks whether `entity` has all mandatory markers defined by
    /// `type_name` and its supertypes.
    pub fn fits(&self, entity: &HDict, type_name: &str) -> bool {
        let mandatory = self.mandatory_tags(type_name);
        mandatory.iter().all(|tag| entity.has(tag))
    }

    /// Explain why an entity does or does not fit a type.
    ///
    /// Returns a list of `FitIssue` items; empty if entity fits.
    pub fn fits_explain(&self, entity: &HDict, type_name: &str) -> Vec<FitIssue> {
        let mut issues: Vec<FitIssue> = Vec::new();
        let mandatory = self.mandatory_tags(type_name);
        for tag in &mandatory {
            if entity.missing(tag) {
                issues.push(FitIssue::MissingMarker {
                    tag: tag.clone(),
                    spec: type_name.to_string(),
                });
            }
        }
        issues
    }

    // -- Validation --

    /// Validate a single entity against the namespace.
    ///
    /// Checks that all mandatory markers are present for each type
    /// the entity claims to be (marker tags that are also defs).
    pub fn validate_entity(&self, entity: &HDict) -> Vec<ValidationIssue> {
        let mut issues: Vec<ValidationIssue> = Vec::new();
        let ref_str = entity.id().map(|r| r.val.clone());

        // Find which types this entity claims to be (marker tags that
        // are also known defs)
        let tag_names: Vec<String> = entity.tag_names().map(|s| s.to_string()).collect();
        for tag_name in &tag_names {
            let val = entity.get(tag_name);
            if !matches!(val, Some(Kind::Marker)) {
                continue;
            }
            if !self.defs.contains_key(tag_name.as_str()) {
                continue;
            }
            // Check mandatory markers for this type
            let mandatory = self.mandatory_tags(tag_name);
            for m in &mandatory {
                if entity.missing(m) {
                    issues.push(ValidationIssue {
                        entity: ref_str.clone(),
                        issue_type: "missing_marker".to_string(),
                        detail: format!(
                            "Entity claims '{}' but is missing mandatory marker '{}'",
                            tag_name, m
                        ),
                    });
                }
            }
        }
        issues
    }

    // -- Properties --

    /// Number of registered defs.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Returns true if no defs are registered.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Check if a name is registered as a def.
    pub fn contains(&self, name: &str) -> bool {
        self.defs.contains_key(name)
    }

    /// All registered defs.
    pub fn defs(&self) -> &HashMap<String, Def> {
        &self.defs
    }

    /// All registered libraries.
    pub fn libs(&self) -> &HashMap<String, Lib> {
        &self.libs
    }

    /// Get a reference to the taxonomy tree.
    pub fn taxonomy(&self) -> &TaxonomyTree {
        &self.taxonomy
    }

    // -- Spec Registry --

    /// Register a resolved Spec in the registry.
    pub fn register_spec(&mut self, spec: Spec) {
        let lib = spec.lib.clone();
        let qname = spec.qname.clone();
        self.specs.insert(qname.clone(), spec);
        self.spec_libs.entry(lib).or_default().push(qname);
    }

    /// Look up a Spec by qualified name (e.g. "ph::Ahu").
    pub fn get_spec(&self, qname: &str) -> Option<&Spec> {
        self.specs.get(qname)
    }

    /// List all specs, optionally filtered by library.
    pub fn specs(&self, lib: Option<&str>) -> Vec<&Spec> {
        match lib {
            Some(lib_name) => self
                .spec_libs
                .get(lib_name)
                .map(|qnames| qnames.iter().filter_map(|q| self.specs.get(q)).collect())
                .unwrap_or_default(),
            None => self.specs.values().collect(),
        }
    }

    /// Get the raw specs HashMap (for fitting/effective_slots).
    pub fn specs_map(&self) -> &HashMap<String, Spec> {
        &self.specs
    }

    /// Track the source of a loaded library.
    pub fn set_lib_source(&mut self, lib_name: &str, source: LibSource) {
        self.lib_sources.insert(lib_name.to_string(), source);
    }

    /// Get the source tracking for a library.
    pub fn lib_source(&self, lib_name: &str) -> Option<&LibSource> {
        self.lib_sources.get(lib_name)
    }

    /// Export a library to Xeto source text.
    pub fn export_lib_xeto(&self, lib_name: &str) -> Result<String, String> {
        let lib = self
            .libs()
            .get(lib_name)
            .ok_or_else(|| format!("library '{}' not found", lib_name))?;
        let specs: Vec<&crate::xeto::Spec> = self.specs(Some(lib_name));
        Ok(crate::xeto::export::export_lib(
            lib_name,
            &lib.version,
            &lib.doc,
            &lib.depends,
            &specs,
        ))
    }

    /// Save a library to a file on disk as Xeto text.
    pub fn save_lib(&self, lib_name: &str, path: &std::path::Path) -> Result<(), String> {
        let xeto_text = self.export_lib_xeto(lib_name)?;
        std::fs::write(path, xeto_text).map_err(|e| format!("failed to write {:?}: {}", path, e))
    }

    /// Load a Xeto library from source text and register all specs.
    pub fn load_xeto_str(
        &mut self,
        source: &str,
        lib_name: &str,
    ) -> Result<Vec<String>, crate::xeto::XetoError> {
        let (lib, specs) = crate::xeto::loader::load_xeto_source(source, lib_name, self)?;
        let qnames: Vec<String> = specs.iter().map(|s| s.qname.clone()).collect();
        self.register_lib(lib);
        for spec in specs {
            self.register_spec(spec);
        }
        self.set_lib_source(lib_name, LibSource::Xeto(source.to_string()));
        Ok(qnames)
    }

    /// Load a Xeto library from a directory of .xeto files.
    pub fn load_xeto_dir(
        &mut self,
        dir: &std::path::Path,
    ) -> Result<(String, Vec<String>), crate::xeto::XetoError> {
        let (name, lib, specs) = crate::xeto::loader::load_xeto_dir(dir, self)?;
        let qnames: Vec<String> = specs.iter().map(|s| s.qname.clone()).collect();
        self.register_lib(lib);
        for spec in specs {
            self.register_spec(spec);
        }
        self.set_lib_source(&name, LibSource::Directory(dir.to_path_buf()));
        Ok((name, qnames))
    }

    /// Unload a library by name. Removes all defs, specs, and taxonomy entries.
    /// Returns Err if another loaded library depends on this one or if it's bundled.
    pub fn unload_lib(&mut self, lib_name: &str) -> Result<(), String> {
        // Check for dependents
        for (name, lib) in &self.libs {
            if name != lib_name && lib.depends.contains(&lib_name.to_string()) {
                return Err(format!(
                    "cannot unload '{}': library '{}' depends on it",
                    lib_name, name
                ));
            }
        }
        // Check if bundled
        if matches!(self.lib_sources.get(lib_name), Some(LibSource::Bundled)) {
            return Err(format!("cannot unload bundled library '{}'", lib_name));
        }

        // Remove specs belonging to this lib
        if let Some(qnames) = self.spec_libs.remove(lib_name) {
            for qname in &qnames {
                self.specs.remove(qname);
            }
        }

        // Remove defs belonging to this lib
        self.defs.retain(|_, def| def.lib != lib_name);

        // Remove from libs registry
        self.libs.remove(lib_name);

        // Remove source tracking
        self.lib_sources.remove(lib_name);

        // Invalidate mandatory tag cache
        self.taxonomy.clear_cache();

        Ok(())
    }
}

impl Default for DefNamespace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::HRef;

    /// Build a small namespace for testing without loading defs.trio.
    fn build_test_ns() -> DefNamespace {
        let trio = "\
def:^marker
doc:\"Marker type\"
is:[^marker]
lib:^lib:ph
---
def:^entity
doc:\"Top-level entity\"
is:[^marker]
lib:^lib:ph
---
def:^equip
doc:\"Equipment\"
is:[^entity]
lib:^lib:phIoT
mandatory
---
def:^point
doc:\"Data point\"
is:[^entity]
lib:^lib:phIoT
---
def:^ahu
doc:\"Air Handling Unit\"
is:[^equip]
lib:^lib:phIoT
mandatory
---
def:^meter
doc:\"Meter\"
is:[^equip]
lib:^lib:phIoT
---
def:^hot-water
doc:\"Hot water\"
is:[^marker]
lib:^lib:phIoT
---
def:^site
doc:\"A site\"
is:[^entity]
lib:^lib:ph
---
def:^ahuZoneDelivery
doc:\"AHU zone delivery choice\"
is:[^choice]
lib:^lib:phIoT
tagOn:[^ahu]
---
def:^directZone
doc:\"Direct zone\"
is:[^ahuZoneDelivery]
lib:^lib:phIoT
---
def:^lib:ph
doc:\"Project Haystack core\"
is:[^lib]
lib:^lib:ph
version:\"4.0.0\"
---
def:^lib:phIoT
doc:\"Project Haystack IoT\"
is:[^lib]
lib:^lib:phIoT
version:\"4.0.0\"
depends:[^lib:ph]
";
        let mut ns = DefNamespace::new();
        let libs = load_trio(trio).unwrap();
        for lib in libs {
            ns.register_lib(lib);
        }
        ns
    }

    #[test]
    fn new_namespace_is_empty() {
        let ns = DefNamespace::new();
        assert!(ns.is_empty());
        assert_eq!(ns.len(), 0);
    }

    #[test]
    fn register_and_get_def() {
        let ns = build_test_ns();
        assert!(ns.contains("ahu"));
        assert!(ns.contains("equip"));
        assert!(!ns.contains("nonexistent"));

        let ahu = ns.get_def("ahu").unwrap();
        assert_eq!(ahu.symbol, "ahu");
        assert_eq!(ahu.is_, vec!["equip"]);
    }

    #[test]
    fn is_a_direct_parent() {
        let ns = build_test_ns();
        assert!(ns.is_a("ahu", "equip"));
    }

    #[test]
    fn is_a_ancestor() {
        let ns = build_test_ns();
        assert!(ns.is_a("ahu", "entity"));
        assert!(ns.is_a("ahu", "marker"));
    }

    #[test]
    fn is_a_self() {
        let ns = build_test_ns();
        assert!(ns.is_a("ahu", "ahu"));
    }

    #[test]
    fn is_a_false_for_unrelated() {
        let ns = build_test_ns();
        assert!(!ns.is_a("ahu", "point"));
    }

    #[test]
    fn subtypes_direct() {
        let ns = build_test_ns();
        let mut subs = ns.subtypes("equip");
        subs.sort();
        assert_eq!(subs, vec!["ahu", "meter"]);
    }

    #[test]
    fn supertypes_chain() {
        let ns = build_test_ns();
        let supers = ns.supertypes("ahu");
        // BFS: equip, then entity (via equip), then marker (via entity)
        assert_eq!(supers, vec!["equip", "entity", "marker"]);
    }

    #[test]
    fn mandatory_tags_for_ahu() {
        let ns = build_test_ns();
        let tags = ns.mandatory_tags("ahu");
        assert!(tags.contains("ahu"));
        assert!(tags.contains("equip"));
        // entity and marker are NOT mandatory in our test data
        assert!(!tags.contains("entity"));
    }

    #[test]
    fn conjunct_parts_decompose() {
        let ns = build_test_ns();
        let parts = ns.conjunct_parts("hot-water").unwrap();
        assert_eq!(parts, &["hot", "water"]);
    }

    #[test]
    fn conjunct_parts_unknown() {
        let ns = build_test_ns();
        assert!(ns.conjunct_parts("site").is_none());
    }

    #[test]
    fn fits_with_valid_entity() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);

        assert!(ns.fits(&entity, "ahu"));
    }

    #[test]
    fn fits_missing_mandatory() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        // Missing "equip" marker

        assert!(!ns.fits(&entity, "ahu"));
    }

    #[test]
    fn fits_explain_missing_marker() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        // Missing "equip" marker

        let issues = ns.fits_explain(&entity, "ahu");
        assert!(!issues.is_empty());

        let has_equip_issue = issues.iter().any(|i| {
            matches!(i, FitIssue::MissingMarker { tag, spec }
                if tag == "equip" && spec == "ahu")
        });
        assert!(has_equip_issue);
    }

    #[test]
    fn fits_explain_no_issues_when_valid() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);

        let issues = ns.fits_explain(&entity, "ahu");
        assert!(issues.is_empty());
    }

    #[test]
    fn validate_entity_finds_missing_markers() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        // Missing "equip" marker required by ahu

        let issues = ns.validate_entity(&entity);
        assert!(!issues.is_empty());

        let has_issue = issues
            .iter()
            .any(|i| i.issue_type == "missing_marker" && i.detail.contains("equip"));
        assert!(has_issue);
    }

    #[test]
    fn validate_entity_no_issues_for_valid() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);

        let issues = ns.validate_entity(&entity);
        assert!(issues.is_empty());
    }

    #[test]
    fn tags_for_entity_type() {
        let ns = build_test_ns();
        let tags = ns.tags_for("ahu");
        // ahuZoneDelivery has tagOn=[ahu]
        assert!(tags.contains("ahuZoneDelivery"));
    }

    #[test]
    fn choices_from_index() {
        let ns = build_test_ns();
        let options = ns.choices("ahuZoneDelivery");
        assert!(options.contains(&"directZone".to_string()));
    }

    #[test]
    fn libs_registered() {
        let ns = build_test_ns();
        assert!(ns.libs().contains_key("ph"));
        assert!(ns.libs().contains_key("phIoT"));
    }

    #[test]
    fn def_count() {
        let ns = build_test_ns();
        // 12 defs: marker, entity, equip, point, ahu, meter, hot-water,
        // site, ahuZoneDelivery, directZone, lib:ph, lib:phIoT
        assert_eq!(ns.len(), 12);
    }

    // -- Spec Registry Tests --

    #[test]
    fn register_and_get_spec() {
        let mut ns = DefNamespace::new();
        let spec = crate::xeto::Spec::new("test::Foo", "test", "Foo");
        ns.register_spec(spec);
        assert!(ns.get_spec("test::Foo").is_some());
        assert!(ns.get_spec("test::Bar").is_none());
    }

    #[test]
    fn specs_filtered_by_lib() {
        let mut ns = DefNamespace::new();
        ns.register_spec(crate::xeto::Spec::new("test::Foo", "test", "Foo"));
        ns.register_spec(crate::xeto::Spec::new("test::Bar", "test", "Bar"));
        ns.register_spec(crate::xeto::Spec::new("other::Baz", "other", "Baz"));
        assert_eq!(ns.specs(Some("test")).len(), 2);
        assert_eq!(ns.specs(Some("other")).len(), 1);
        assert_eq!(ns.specs(None).len(), 3);
    }

    #[test]
    fn unload_lib_removes_specs() {
        let mut ns = DefNamespace::new();
        ns.register_spec(crate::xeto::Spec::new("test::Foo", "test", "Foo"));
        ns.set_lib_source("test", LibSource::Xeto("...".into()));
        ns.register_lib(crate::ontology::Lib {
            name: "test".into(),
            version: "1.0".into(),
            doc: String::new(),
            depends: vec![],
            defs: std::collections::HashMap::new(),
        });
        assert!(ns.unload_lib("test").is_ok());
        assert!(ns.get_spec("test::Foo").is_none());
        assert!(ns.specs(Some("test")).is_empty());
    }

    #[test]
    fn unload_bundled_fails() {
        let mut ns = DefNamespace::new();
        ns.set_lib_source("sys", LibSource::Bundled);
        assert!(ns.unload_lib("sys").is_err());
    }

    #[test]
    fn unload_with_dependent_fails() {
        let mut ns = DefNamespace::new();
        ns.register_lib(crate::ontology::Lib {
            name: "base".into(),
            version: "1.0".into(),
            doc: String::new(),
            depends: vec![],
            defs: std::collections::HashMap::new(),
        });
        ns.register_lib(crate::ontology::Lib {
            name: "child".into(),
            version: "1.0".into(),
            doc: String::new(),
            depends: vec!["base".into()],
            defs: std::collections::HashMap::new(),
        });
        ns.set_lib_source("base", LibSource::Xeto("...".into()));
        assert!(ns.unload_lib("base").is_err());
    }

    #[test]
    fn load_standard_includes_xeto_specs() {
        let ns = DefNamespace::load_standard().unwrap();
        // Should have some xeto specs loaded (even if not all parse)
        let spec_count = ns.specs(None).len();
        println!("loaded {} xeto specs from bundled libraries", spec_count);
        assert!(spec_count > 0, "should have loaded some xeto specs");
    }

    #[test]
    fn bundled_libs_cannot_be_unloaded() {
        let mut ns = DefNamespace::load_standard().unwrap();
        // Any lib that was loaded should be marked as Bundled and cannot be unloaded.
        // Try unloading "ph" which is loaded from defs.trio
        assert!(ns.unload_lib("ph").is_err());
    }
}
