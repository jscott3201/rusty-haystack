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

/// What a qualified spec term in a filter resolves to.
///
/// A namespace holds two type systems — Haystack 4 defs in the taxonomy and
/// Xeto specs in their own map — and a term like `ph::Ahu` may name either.
/// See [`DefNamespace::resolve_spec_term`].
#[derive(Debug, Clone)]
pub enum SpecTerm<'a> {
    /// A Xeto spec, matched on its exact qualified name.
    Spec(&'a Spec),
    /// A def, carrying the taxonomy symbol the term resolved to. This is not
    /// always the bare name as written: `ph::Ahu` resolves to `ahu`.
    Def(String),
}

/// Unified container for Haystack 4 defs.
///
/// Provides resolution, taxonomy queries, structural typing (`fits`),
/// and validation. Loads defs from Trio format.
///
/// Cloning is a deep copy of every index and is not cheap — a standard
/// namespace holds several hundred defs. Prefer sharing an
/// [`Arc<DefNamespace>`](std::sync::Arc), which is what
/// [`EntityGraph`](crate::graph::EntityGraph) stores. `Clone` exists so an
/// `Arc` can be forked with `Arc::make_mut` when one holder needs to load or
/// unload a library without disturbing the others.
#[derive(Clone)]
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
            if let Some(parent_def) = self.defs.get(parent)
                && parent_def.kind() == DefKind::Choice
            {
                self.choice_index
                    .entry(parent.clone())
                    .or_default()
                    .push(symbol.to_string());
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
    /// `type_name` and its supertypes — a **conformance** question: is this
    /// entity well-formed as a `type_name`?
    ///
    /// That is deliberately not the same question a filter asks. A filter asks
    /// **membership**: is this entity a `type_name`? Use
    /// [`entity_is_a`](Self::entity_is_a) for that. The two differ sharply — an
    /// entity with no markers at all conforms to any type with no mandatory
    /// markers (579 of the 719 standard defs), but is a member of none of them.
    pub fn fits(&self, entity: &HDict, type_name: &str) -> bool {
        // Fail closed on a name this namespace has never seen. `mandatory_tags`
        // returns an empty set for an unregistered name and `.all()` over an
        // empty iterator is vacuously true, so without this guard a misspelled
        // or not-yet-loaded type matches every entity — a typo in a filter would
        // widen the result set to the whole graph instead of narrowing it.
        if !self.has_type(type_name) {
            return false;
        }
        let mandatory = self.mandatory_tags(type_name);
        mandatory.iter().all(|tag| entity.has(tag))
    }

    /// Whether `name` is a def registered in this namespace.
    ///
    /// This is the check [`fits`](Self::fits) uses to reject unknown types, and
    /// the one callers should use to tell "does not fit" apart from "no such
    /// type" — `fits` collapses both to `false`.
    ///
    /// Takes a bare def symbol, not a qualified name. To resolve a `lib::Name`
    /// term use [`resolve_spec_term`](Self::resolve_spec_term), which also
    /// searches Xeto specs.
    pub fn has_type(&self, name: &str) -> bool {
        self.taxonomy.contains(name)
    }

    /// Whether `entity` **is a** `type_name` — the membership question a filter
    /// asks, as opposed to the conformance question [`fits`](Self::fits) asks.
    ///
    /// An entity declares its types with marker tags. It is a `type_name` if it
    /// carries any marker whose def is `type_name` or a subtype of it, so an
    /// entity tagged `ahu` is an `ahu`, an `equip`, and an `entity`.
    ///
    /// This is why filters cannot use `fits`: a def's mandatory-marker set is a
    /// well-formedness rule, not an identity. `mandatory_tags("sensor")` is
    /// empty, so every entity conforms to `sensor`; `mandatory_tags("floor")` is
    /// `{space}`, so a floor that omits the conventional `space` marker does not
    /// conform to its own type. Neither answer is what `ph::Sensor` or
    /// `ph::Floor` means to someone writing a query.
    ///
    /// Conjunct defs (`hot-water`, `elec-meter`) are out of scope: an entity
    /// expresses those through their component markers rather than a literal
    /// conjunct marker, and this does not decompose them. They are unreachable
    /// from a filter anyway — the grammar reads the `-` as an operator.
    pub fn entity_is_a(&self, entity: &HDict, type_name: &str) -> bool {
        if !self.has_type(type_name) {
            return false;
        }
        self.taxonomy.any_is_subtype(
            entity
                .iter()
                .filter(|(_, val)| matches!(val, Kind::Marker))
                .map(|(tag, _)| tag),
            type_name,
        )
    }

    /// Resolve a qualified spec term from a filter, such as `ph::Ahu`,
    /// `ph::ahuZoneDelivery`, or `ph.equips::WaterMeter`.
    ///
    /// A term can name either of the two type systems this namespace holds, and
    /// the spellings differ, so resolution tries each in turn:
    ///
    /// 1. A Xeto spec under its exact qualified name. Specs live in their own
    ///    map keyed by qname and are never registered in the taxonomy, so a
    ///    taxonomy lookup alone cannot see any of them.
    /// 2. A def whose symbol is the bare name exactly as written — def symbols
    ///    are frequently camelCase (`ahuZoneDelivery`), which lowercasing
    ///    destroys.
    /// 3. A def whose symbol is the lowercased bare name. This is the Haystack
    ///    convention that writes `ph::Ahu` for the def `ahu`.
    ///
    /// Only rung 1 looks at the library qualifier. Rungs 2 and 3 match on the
    /// bare name alone, so `totallyMadeUp::Ahu` resolves to the def `ahu` just
    /// as `ph::Ahu` does. That mirrors how the term was reduced before specs
    /// were consulted at all, and Haystack def symbols are globally unique.
    ///
    /// Returns `None` when the term matches nothing, which is what callers use
    /// to reject a filter rather than silently evaluate it as a non-match.
    pub fn resolve_spec_term(&self, term: &str) -> Option<SpecTerm<'_>> {
        if let Some(spec) = self.specs.get(term) {
            return Some(SpecTerm::Spec(spec));
        }
        let bare = term.rsplit("::").next().unwrap_or(term);
        if self.has_type(bare) {
            return Some(SpecTerm::Def(bare.to_string()));
        }
        let lowered = bare.to_lowercase();
        if self.has_type(&lowered) {
            return Some(SpecTerm::Def(lowered));
        }
        None
    }

    /// Whether `entity` matches the qualified spec term `term`, as a filter
    /// means it.
    ///
    /// Dispatches on what the term resolves to. A def is answered by
    /// [`entity_is_a`](Self::entity_is_a) — membership, not conformance, so
    /// `ph::Vav` matches vavs rather than everything carrying `equip`. A Xeto
    /// spec is answered structurally against its slots, which is the only
    /// meaning a spec has.
    ///
    /// An unresolvable term is `false`; callers that can report an error should
    /// call [`resolve_spec_term`](Self::resolve_spec_term) first.
    pub fn fits_spec_term(&self, entity: &HDict, term: &str) -> bool {
        self.fits_spec_term_with(entity, term, None)
    }

    /// As [`fits_spec_term`](Self::fits_spec_term), with a resolver for query slots.
    ///
    /// A Xeto spec can constrain what an entity must be able to *reach* — an AHU
    /// spec whose `vavs` slot queries `airRef+`, say. Those constraints cannot be
    /// evaluated from the entity alone, so without a resolver they are skipped and
    /// the spec matches more than it should. Callers holding a graph should pass
    /// one; `fits_spec_term` remains for callers that have no entity store.
    pub fn fits_spec_term_with(
        &self,
        entity: &HDict,
        term: &str,
        resolver: Option<&crate::xeto::EntityResolver<'_>>,
    ) -> bool {
        match self.resolve_spec_term(term) {
            Some(SpecTerm::Def(name)) => self.entity_is_a(entity, &name),
            Some(SpecTerm::Spec(spec)) => {
                crate::xeto::fitting::explain_against_spec_in(entity, spec, &self.specs, resolver)
                    .is_empty()
            }
            None => false,
        }
    }

    /// Explain why an entity does or does not fit a type.
    ///
    /// Returns a list of `FitIssue` items; empty if entity fits.
    pub fn fits_explain(&self, entity: &HDict, type_name: &str) -> Vec<FitIssue> {
        let mut issues: Vec<FitIssue> = Vec::new();
        // Distinguishing an unknown type from a genuine non-match is the whole
        // point of this method — `fits` reports both as plain `false`.
        if !self.has_type(type_name) {
            issues.push(FitIssue::UnknownType {
                spec: type_name.to_string(),
            });
            return issues;
        }
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

    /// Rebuild all derived indexes (taxonomy, mandatory, conjuncts, tagOn,
    /// choice) from the current `defs` map. Needed after `unload_lib` removes a
    /// library's defs, because those indexes accumulate per-def entries that a
    /// plain removal would otherwise leave stale.
    fn rebuild_derived_indexes(&mut self) {
        // Snapshot the data we need so we can clear and rebuild the indexes
        // without holding an immutable borrow of `self.defs` while mutating.
        let snapshot: Vec<(String, Vec<String>, bool, Vec<String>)> = self
            .defs
            .values()
            .map(|d| {
                (
                    d.symbol.clone(),
                    d.is_.clone(),
                    d.mandatory,
                    d.tag_on.clone(),
                )
            })
            .collect();

        self.taxonomy = TaxonomyTree::new();
        self.conjuncts = ConjunctIndex::new();
        self.mandatory_defs.clear();
        self.tag_on_index.clear();
        self.choice_index.clear();

        for (symbol, is_, mandatory, tag_on) in &snapshot {
            self.taxonomy.add(symbol, is_);
            if *mandatory {
                self.mandatory_defs.insert(symbol.clone());
            }
            if symbol.contains('-') {
                let parts: Vec<String> = symbol.split('-').map(|s| s.to_string()).collect();
                self.conjuncts.register(symbol, parts);
            }
            for target in tag_on {
                self.tag_on_index
                    .entry(target.clone())
                    .or_default()
                    .push(symbol.clone());
            }
        }
        // Choice index needs every def present in `self.defs` (they are).
        for (symbol, ..) in &snapshot {
            self.register_def_choice_index(symbol);
        }
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

        // Rebuild derived indexes (taxonomy, mandatory, conjuncts, tagOn,
        // choice) from the remaining defs so no stale entries from the unloaded
        // library survive. This also resets the mandatory-tag cache.
        self.rebuild_derived_indexes();

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

    /// An entity that fits every *registered* type in the namespace. If `fits`
    /// ever reports a match for an unregistered name, it will do so for this.
    fn fits_everything() -> HDict {
        let mut e = HDict::new();
        e.set("id", Kind::Ref(HRef::from_val("x")));
        e.set("marker", Kind::Marker);
        e.set("entity", Kind::Marker);
        e.set("equip", Kind::Marker);
        e.set("ahu", Kind::Marker);
        e
    }

    #[test]
    fn fits_is_false_for_an_unregistered_type() {
        // `mandatory_tags` returns an empty set for an unknown name and
        // `.all()` over an empty iterator is vacuously true, so this used to
        // report a match for anything at all.
        let ns = build_test_ns();
        let e = fits_everything();
        assert!(ns.fits(&e, "ahu"), "control: a known type still fits");

        assert!(!ns.fits(&e, "bogus"));
        assert!(!ns.fits(&e, "Ahu"), "lookup is case-sensitive");
        assert!(!ns.fits(&e, ""));
        assert!(!ns.fits(&HDict::new(), "bogus"));
    }

    #[test]
    fn fits_is_false_for_every_type_in_an_empty_namespace() {
        let ns = DefNamespace::new();
        assert!(!ns.fits(&fits_everything(), "point"));
        assert!(!ns.fits(&fits_everything(), "ahu"));
    }

    #[test]
    fn has_type_separates_unknown_from_non_matching() {
        let ns = build_test_ns();
        assert!(ns.has_type("ahu"));
        assert!(ns.has_type("point"));
        assert!(!ns.has_type("bogus"));

        // A site does not fit `ahu`, but `ahu` is a real type — `fits` collapses
        // that distinction to `false` and `has_type` is how a caller recovers it.
        let mut site = HDict::new();
        site.set("site", Kind::Marker);
        assert!(!ns.fits(&site, "ahu"));
        assert!(ns.has_type("ahu"));
    }

    #[test]
    fn fits_explain_reports_an_unknown_type() {
        let ns = build_test_ns();
        let issues = ns.fits_explain(&fits_everything(), "bogus");
        assert_eq!(
            issues,
            vec![FitIssue::UnknownType {
                spec: "bogus".to_string()
            }]
        );

        // A registered type the entity does satisfy still explains cleanly.
        assert!(ns.fits_explain(&fits_everything(), "ahu").is_empty());
    }

    #[test]
    fn resolve_spec_term_finds_xeto_specs_by_qname() {
        // Specs live in their own map keyed by qname and are never registered in
        // the taxonomy, so a taxonomy-only lookup saw none of them. Every spec
        // the standard namespace ships must resolve.
        let ns = DefNamespace::load_standard().expect("bundled defs load");
        let specs: Vec<String> = ns.specs(None).iter().map(|s| s.qname.clone()).collect();
        assert!(!specs.is_empty(), "the standard namespace ships specs");

        for qname in &specs {
            assert!(
                matches!(ns.resolve_spec_term(qname), Some(SpecTerm::Spec(_))),
                "{qname} must resolve to its Xeto spec"
            );
        }
    }

    #[test]
    fn resolve_spec_term_preserves_camel_case_def_names() {
        // Def symbols are frequently camelCase. Lowercasing the bare name
        // unconditionally made every one of them unresolvable.
        let ns = DefNamespace::load_standard().expect("bundled defs load");
        let camel: Vec<String> = ns
            .defs()
            .keys()
            .filter(|d| d.chars().any(|c| c.is_uppercase()))
            .cloned()
            .collect();
        assert!(
            !camel.is_empty(),
            "the standard namespace ships camelCase defs"
        );

        for name in &camel {
            let term = format!("ph::{name}");
            match ns.resolve_spec_term(&term) {
                Some(SpecTerm::Def(resolved)) => assert_eq!(&resolved, name),
                other => panic!("{term} resolved to {other:?}, expected the def {name}"),
            }
        }
    }

    #[test]
    fn resolve_spec_term_accepts_the_haystack_capitalised_spelling() {
        // `ph::Ahu` is how Haystack writes a reference to the def `ahu`, so the
        // lowercased fallback has to stay — it just must not come first.
        let ns = build_test_ns();
        match ns.resolve_spec_term("ph::Ahu") {
            Some(SpecTerm::Def(name)) => assert_eq!(name, "ahu"),
            other => panic!("expected def `ahu`, got {other:?}"),
        }
        match ns.resolve_spec_term("ph::ahu") {
            Some(SpecTerm::Def(name)) => assert_eq!(name, "ahu"),
            other => panic!("expected def `ahu`, got {other:?}"),
        }
        assert!(ns.resolve_spec_term("ph::Bogus").is_none());
    }

    #[test]
    fn fits_spec_term_dispatches_on_what_the_term_resolved_to() {
        let ns = DefNamespace::load_standard().expect("bundled defs load");

        let mut point = HDict::new();
        point.set("id", Kind::Ref(HRef::from_val("p1")));
        point.set("point", Kind::Marker);

        assert!(ns.fits_spec_term(&point, "ph::Point"));
        assert!(!ns.fits_spec_term(&point, "ph::Ahu"));
        assert!(!ns.fits_spec_term(&point, "ph::Bogus"));

        // A real Xeto spec goes through structural slot checking rather than
        // def mandatory-marker checking, and must not vacuously accept a point.
        let spec_qname = ns
            .specs(None)
            .iter()
            .map(|s| s.qname.clone())
            .find(|q| q.ends_with("::WaterMeter"));
        if let Some(q) = spec_qname {
            assert!(
                matches!(ns.resolve_spec_term(&q), Some(SpecTerm::Spec(_))),
                "{q} resolves as a spec"
            );
            assert!(!ns.fits_spec_term(&point, &q), "a bare point is not a {q}");
        }
    }

    #[test]
    fn clone_carries_xeto_specs_as_well_as_defs() {
        // A clone that copied only the taxonomy would still answer def-backed
        // terms correctly, so spec lookups are what pin the specs map.
        let ns = DefNamespace::load_standard().expect("bundled defs load");
        let qname = ns
            .specs(None)
            .first()
            .map(|s| s.qname.clone())
            .expect("the standard namespace ships specs");

        let snapshot = ns.clone();
        assert!(
            snapshot.get_spec(&qname).is_some(),
            "specs survive the clone"
        );
        assert!(matches!(
            snapshot.resolve_spec_term(&qname),
            Some(SpecTerm::Spec(_))
        ));
        assert_eq!(snapshot.specs(None).len(), ns.specs(None).len());
    }

    #[test]
    fn clone_is_independent_of_the_original() {
        let mut ns = build_test_ns();
        let snapshot = ns.clone();
        assert_eq!(snapshot.len(), ns.len());
        assert!(snapshot.has_type("ahu"));

        // Warm the taxonomy's memo cache on the clone, then mutate the original.
        assert!(snapshot.fits(&fits_everything(), "ahu"));
        ns.unload_lib("phIoT").expect("phIoT unloads");

        assert!(!ns.has_type("ahu"), "original lost the lib");
        assert!(snapshot.has_type("ahu"), "clone kept it");
        assert!(snapshot.fits(&fits_everything(), "ahu"));
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
    fn unload_lib_purges_taxonomy_and_indexes() {
        fn def(symbol: &str, lib: &str, is_: &[&str]) -> Def {
            Def {
                symbol: symbol.to_string(),
                lib: lib.to_string(),
                is_: is_.iter().map(|s| s.to_string()).collect(),
                tag_on: Vec::new(),
                of: None,
                mandatory: false,
                doc: String::new(),
                tags: HDict::new(),
            }
        }

        let mut ns = DefNamespace::new();
        // Base lib (kept): the "entity" root.
        ns.register_lib(Lib {
            name: "base".to_string(),
            version: "1.0".to_string(),
            doc: String::new(),
            depends: Vec::new(),
            defs: HashMap::from([("entity".to_string(), def("entity", "base", &[]))]),
        });
        // Custom lib (to be unloaded): "thing" is-a entity, plus a conjunct.
        ns.register_lib(Lib {
            name: "custom".to_string(),
            version: "1.0".to_string(),
            doc: String::new(),
            depends: Vec::new(),
            defs: HashMap::from([
                ("thing".to_string(), def("thing", "custom", &["entity"])),
                ("hot-water".to_string(), def("hot-water", "custom", &[])),
            ]),
        });
        ns.set_lib_source("custom", LibSource::Xeto("x".to_string()));

        // Sanity: the taxonomy edge exists before unload.
        assert!(ns.is_a("thing", "entity"));

        ns.unload_lib("custom").unwrap();

        // Regression: the unloaded lib's taxonomy edge must be gone, not stale.
        assert!(!ns.is_a("thing", "entity"));
        // The kept base lib is unaffected.
        assert!(ns.is_a("entity", "entity"));
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
