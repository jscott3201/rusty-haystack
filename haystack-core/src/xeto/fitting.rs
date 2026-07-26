// Xeto structural type fitting -- checks whether an entity fits a Xeto spec.

use std::collections::{HashMap, HashSet};

use crate::data::HDict;
use crate::kinds::{HRef, Kind};
use crate::ontology::validation::FitIssue;
use crate::ontology::{DefNamespace, SpecTerm};

use super::spec::Spec;

/// Entity resolver function type for query evaluation.
/// Given a ref, returns the entity dict if it exists.
/// Resolves a ref to the entity it points at, for query-slot traversal.
///
/// Borrowed rather than owned, and deliberately so: `EntityGraph` already builds
/// exactly this closure over its own entity map, and an owned signature meant the
/// two could not be connected without cloning a dict per lookup. Faced with that,
/// the filter path passed `None` — and `None` is indistinguishable from "no
/// resolver available", so query slots went unchecked and specs over-matched
/// (issue #22). Converging on the borrowed form makes threading the real resolver
/// the only thing that compiles.
pub type EntityResolver<'a> = dyn Fn(&HRef) -> Option<&'a HDict> + 'a;

/// Resolves the entities that point *at* a ref through a given tag.
///
/// The mirror of [`EntityResolver`], and a genuinely different capability: a
/// forward resolver answers "what does this entity point at", which the entity
/// carries in its own tags. An inverse query asks "which entities point at me",
/// which the entity cannot know. Answering it needs an index over the whole
/// store — `EntityGraph` already maintains one, as `refs_to`.
///
/// Called as `inverse(target, tag)`, returning every entity whose `tag` is a ref
/// equal to `target`.
pub type InverseResolver<'a> = dyn Fn(&HRef, &str) -> Vec<&'a HDict> + 'a;

/// What query-slot evaluation may ask of the surrounding entity store.
///
/// Grouped rather than passed as separate parameters because the two travel
/// together through every layer between a filter and a query slot, and because
/// the set is not closed — `of:` checking wanted a third capability the week
/// after the second was added.
///
/// `inverse` is optional on purpose: a caller holding only a map of entities can
/// answer forward traversal but not reverse, and should say so rather than
/// silently degrade. A spec with an inverse query slot evaluated without an
/// inverse resolver reports [`FitIssue::ConstraintViolation`] rather than
/// passing, so an unanswerable constraint fails closed.
#[derive(Clone, Copy)]
pub struct QueryContext<'a> {
    /// Follows a ref to the entity it points at.
    pub forward: &'a EntityResolver<'a>,
    /// Finds the entities pointing at a ref, when the caller can answer that.
    pub inverse: Option<&'a InverseResolver<'a>>,
}

impl<'a> QueryContext<'a> {
    /// A context that can traverse forward only.
    pub fn forward_only(forward: &'a EntityResolver<'a>) -> Self {
        Self {
            forward,
            inverse: None,
        }
    }

    /// A context that can traverse in both directions.
    pub fn new(forward: &'a EntityResolver<'a>, inverse: &'a InverseResolver<'a>) -> Self {
        Self {
            forward,
            inverse: Some(inverse),
        }
    }
}

/// Check whether an entity structurally fits a Xeto spec.
///
/// This performs three levels of validation:
/// 1. **Mandatory markers**: all non-maybe marker slots must be present
/// 2. **Slot type checking**: typed slots must have matching value types
/// 3. **Query evaluation**: traverses entity refs when a resolver is provided
///
/// If `spec_qname` is not found in the namespace, this delegates to
/// `DefNamespace::fits` for traditional def-based fitting.
pub fn fits(
    entity: &HDict,
    spec_qname: &str,
    ns: &DefNamespace,
    ctx: Option<QueryContext<'_>>,
) -> bool {
    fits_explain(entity, spec_qname, ns, ctx).is_empty()
}

/// Explain why an entity does or does not fit a Xeto spec.
///
/// Returns a list of `FitIssue` items; empty if the entity fits.
pub fn fits_explain(
    entity: &HDict,
    spec_qname: &str,
    ns: &DefNamespace,
    ctx: Option<QueryContext<'_>>,
) -> Vec<FitIssue> {
    // Resolution goes through the namespace so this agrees with filter
    // evaluation about what a `lib::Name` term means. Doing its own bare-name
    // split here is what made `xeto::fits(e, "ph::Ahu")` disagree with
    // `matches_with_ns` on the same string.
    match ns.resolve_spec_term(spec_qname) {
        Some(SpecTerm::Spec(spec)) => {
            explain_against_spec_with_specs(entity, spec, ns.specs_map(), ns, ctx)
        }
        Some(SpecTerm::Def(name)) => match synthetic_spec_for_def(&name, spec_qname, ns) {
            Some(spec) => explain_against_spec_with_specs(entity, &spec, &HashMap::new(), ns, ctx),
            None => ns.fits_explain(entity, &name),
        },
        None => vec![FitIssue::UnknownType {
            spec: spec_qname.to_string(),
        }],
    }
}

/// Build a synthetic Spec from a def's mandatory markers, so a def-backed name
/// can be checked by the same slot machinery a real Xeto spec goes through.
///
/// `def_name` is a taxonomy symbol already resolved by
/// [`DefNamespace::resolve_spec_term`]; `qname` is the term as the caller wrote
/// it, kept only for the resulting spec's identity.
fn synthetic_spec_for_def(def_name: &str, qname: &str, ns: &DefNamespace) -> Option<Spec> {
    let def = ns.get_def(def_name)?;
    let doc = def.doc.clone();
    let lib = def.lib.clone();

    let mandatory = ns.mandatory_tags(def_name);
    let mut spec = Spec {
        qname: qname.to_string(),
        name: def_name.to_string(),
        lib,
        base: None,
        meta: std::collections::HashMap::new(),
        slots: Vec::new(),
        is_abstract: false,
        doc,
    };

    // Add mandatory markers as marker slots
    for tag in &mandatory {
        spec.slots.push(super::spec::Slot {
            name: tag.clone(),
            type_ref: None,
            meta: std::collections::HashMap::new(),
            default: None,
            is_marker: true,
            is_query: false,
            children: Vec::new(),
        });
    }

    Some(spec)
}

/// Check an entity against a resolved Spec.
#[cfg(test)]
fn explain_against_spec(entity: &HDict, spec: &Spec) -> Vec<FitIssue> {
    // No namespace and no traversal: these tests exercise markers, slot types and
    // value constraints, none of which consult either.
    let ns = DefNamespace::new();
    explain_against_spec_with_specs(entity, spec, &HashMap::new(), &ns, None)
}

/// Check an entity against an already-resolved Spec, given the namespace's own
/// spec map for walking the inheritance chain.
///
/// This is the entry point [`DefNamespace::fits_spec_term`] uses once it has
/// resolved a filter term to a Xeto spec, so filter evaluation and Xeto fitting
/// share one implementation.
///
/// [`DefNamespace::fits_spec_term`]: crate::ontology::DefNamespace::fits_spec_term
pub(crate) fn explain_against_spec_in(
    entity: &HDict,
    spec: &Spec,
    specs: &HashMap<String, Spec>,
    ns: &DefNamespace,
    ctx: Option<QueryContext<'_>>,
) -> Vec<FitIssue> {
    explain_against_spec_with_specs(entity, spec, specs, ns, ctx)
}

/// Check an entity against a resolved Spec, with access to a specs map for
/// walking the inheritance chain.
fn explain_against_spec_with_specs(
    entity: &HDict,
    spec: &Spec,
    specs: &HashMap<String, Spec>,
    ns: &DefNamespace,
    ctx: Option<QueryContext<'_>>,
) -> Vec<FitIssue> {
    let mut issues = Vec::new();

    // Level 1: Mandatory markers (walks inheritance chain)
    check_mandatory_markers(entity, spec, specs, &mut issues);

    // Level 2: Slot type checking
    check_slot_types(entity, spec, &mut issues);

    // Level 2.5: Value constraints
    check_value_constraints(entity, spec, &mut issues);

    // Level 3: Query evaluation (only when the caller can traverse)
    if let Some(ctx) = ctx {
        check_query_slots(entity, spec, specs, ns, ctx, &mut issues);
    }

    issues
}

/// Check that all mandatory marker slots are present on the entity.
/// Walks the inheritance chain to collect mandatory markers from base specs.
fn check_mandatory_markers(
    entity: &HDict,
    spec: &Spec,
    specs: &HashMap<String, Spec>,
    issues: &mut Vec<FitIssue>,
) {
    let mut all_mandatory: HashSet<String> = HashSet::new();

    // Collect mandatory markers from this spec
    for name in spec.mandatory_markers() {
        all_mandatory.insert(name.to_string());
    }

    // Walk inheritance chain
    let mut base = spec.base.clone();
    let mut visited = HashSet::new();
    while let Some(base_name) = base {
        if !visited.insert(base_name.clone()) {
            break;
        }
        if let Some(base_spec) = specs.get(&base_name) {
            for name in base_spec.mandatory_markers() {
                all_mandatory.insert(name.to_string());
            }
            base = base_spec.base.clone();
        } else {
            break;
        }
    }

    for tag in &all_mandatory {
        if entity.missing(tag) {
            issues.push(FitIssue::MissingMarker {
                tag: tag.clone(),
                spec: spec.qname.clone(),
            });
        }
    }
}

/// Check that typed (non-marker) slot values match the expected types.
fn check_slot_types(entity: &HDict, spec: &Spec, issues: &mut Vec<FitIssue>) {
    for slot in &spec.slots {
        if slot.is_marker || slot.is_query {
            continue;
        }
        // Skip optional slots
        if slot.is_maybe() {
            continue;
        }
        let type_ref = match &slot.type_ref {
            Some(t) => t.as_str(),
            None => continue,
        };

        if let Some(val) = entity.get(&slot.name) {
            let ok = match type_ref {
                "Str" => matches!(val, Kind::Str(_)),
                "Number" => matches!(val, Kind::Number(_)),
                "Ref" => matches!(val, Kind::Ref(_)),
                "Bool" => matches!(val, Kind::Bool(_)),
                "Date" => matches!(val, Kind::Date(_)),
                "Time" => matches!(val, Kind::Time(_)),
                "DateTime" => matches!(val, Kind::DateTime(_)),
                "Uri" => matches!(val, Kind::Uri(_)),
                "Coord" => matches!(val, Kind::Coord(_)),
                "List" => matches!(val, Kind::List(_)),
                "Dict" => matches!(val, Kind::Dict(_)),
                "Grid" => matches!(val, Kind::Grid(_)),
                "Marker" => matches!(val, Kind::Marker),
                _ => true, // Unknown type refs are assumed ok
            };
            if !ok {
                issues.push(FitIssue::WrongType {
                    tag: slot.name.clone(),
                    expected: type_ref.to_string(),
                    actual: kind_type_name(val).to_string(),
                });
            }
        }
        // Note: we do not report missing typed slots as errors here;
        // that would require schema-level mandatory analysis.
    }
}

/// Check value constraints on typed slots (minVal, maxVal, pattern, etc.)
fn check_value_constraints(entity: &HDict, spec: &Spec, issues: &mut Vec<FitIssue>) {
    for slot in &spec.slots {
        if slot.is_marker || slot.is_query {
            continue;
        }
        let val = match entity.get(&slot.name) {
            Some(v) => v,
            None => continue,
        };

        // minVal / maxVal for Numbers
        if let Kind::Number(num) = val {
            if let Some(Kind::Number(min)) = slot.meta.get("minVal")
                && num.val < min.val
            {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "minVal".into(),
                    detail: format!("{} < {}", num.val, min.val),
                });
            }
            if let Some(Kind::Number(max)) = slot.meta.get("maxVal")
                && num.val > max.val
            {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "maxVal".into(),
                    detail: format!("{} > {}", num.val, max.val),
                });
            }
            // unitless constraint
            if slot.meta.contains_key("unitless")
                && let Some(unit) = &num.unit
            {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "unitless".into(),
                    detail: format!("expected no unit, got '{}'", unit),
                });
            }
            // unit constraint
            if let Some(Kind::Str(expected_unit)) = slot.meta.get("unit") {
                match &num.unit {
                    Some(u) if u != expected_unit => {
                        issues.push(FitIssue::ConstraintViolation {
                            tag: slot.name.clone(),
                            constraint: "unit".into(),
                            detail: format!("expected unit '{}', got '{}'", expected_unit, u),
                        });
                    }
                    None => {
                        issues.push(FitIssue::ConstraintViolation {
                            tag: slot.name.clone(),
                            constraint: "unit".into(),
                            detail: format!("expected unit '{}', got unitless", expected_unit),
                        });
                    }
                    _ => {}
                }
            }
        }

        // minSize / maxSize / nonEmpty / pattern for Strings
        if let Kind::Str(s) = val {
            if let Some(Kind::Number(min)) = slot.meta.get("minSize")
                && (s.len() as f64) < min.val
            {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "minSize".into(),
                    detail: format!("length {} < {}", s.len(), min.val),
                });
            }
            if let Some(Kind::Number(max)) = slot.meta.get("maxSize")
                && (s.len() as f64) > max.val
            {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "maxSize".into(),
                    detail: format!("length {} > {}", s.len(), max.val),
                });
            }
            if slot.meta.contains_key("nonEmpty") && s.trim().is_empty() {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "nonEmpty".into(),
                    detail: "string is empty or whitespace only".into(),
                });
            }
            if let Some(Kind::Str(pattern)) = slot.meta.get("pattern") {
                match regex::Regex::new(pattern) {
                    Ok(re) => {
                        if !re.is_match(s) {
                            issues.push(FitIssue::ConstraintViolation {
                                tag: slot.name.clone(),
                                constraint: "pattern".into(),
                                detail: format!("'{}' does not match pattern '{}'", s, pattern),
                            });
                        }
                    }
                    Err(e) => {
                        issues.push(FitIssue::ConstraintViolation {
                            tag: slot.name.clone(),
                            constraint: "pattern".into(),
                            detail: format!("invalid regex pattern '{}': {}", pattern, e),
                        });
                    }
                }
            }
        }

        // minSize / maxSize for Lists
        if let Kind::List(items) = val {
            if let Some(Kind::Number(min)) = slot.meta.get("minSize")
                && (items.len() as f64) < min.val
            {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "minSize".into(),
                    detail: format!("list length {} < {}", items.len(), min.val),
                });
            }
            if let Some(Kind::Number(max)) = slot.meta.get("maxSize")
                && (items.len() as f64) > max.val
            {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "maxSize".into(),
                    detail: format!("list length {} > {}", items.len(), max.val),
                });
            }
        }
    }
}

/// Level 3: Evaluate query slots by traversing entity relationships.
///
/// A query slot constrains what an entity must be able to *reach*. Two forms:
///
/// - **Forward** — `Query<of:Ahu, via:"airRef+">`: follow `airRef` from this
///   entity, transitively when the path ends in `+`.
/// - **Inverse** — `Query<of:Vav, inverse:"ph.equips::AhuVav.ahu">`: find the
///   entities whose named slot reaches *this* one. The reference names a forward
///   query slot on another spec, and that slot's `via` supplies the tag to search
///   on, so an inverse query is the forward one read backwards.
///
/// `of` is enforced in both directions: reaching something is not enough, it has
/// to be the declared type. Skipping that check made `Query<of:Ahu, via:"airRef+">`
/// satisfied by reaching *anything*.
fn check_query_slots(
    entity: &HDict,
    spec: &Spec,
    specs: &HashMap<String, Spec>,
    ns: &DefNamespace,
    ctx: QueryContext<'_>,
    issues: &mut Vec<FitIssue>,
) {
    for slot in &spec.slots {
        if !slot.is_query {
            continue;
        }
        let of_type = str_meta(slot, "of");
        let via_path = str_meta(slot, "via");
        let inverse_ref = str_meta(slot, "inverse");

        let found: Vec<&HDict> = if let Some(via) = via_path {
            let (ref_tag, transitive) = split_via(via);
            traverse_refs(entity, ref_tag, transitive, ctx.forward)
        } else if let Some(inverse) = inverse_ref {
            match resolve_inverse_via(inverse, specs) {
                Some((ref_tag, transitive)) => {
                    // An inverse query the caller cannot answer is reported, not
                    // skipped. Skipping is what made these specs match anything.
                    let Some(reverse) = ctx.inverse else {
                        issues.push(FitIssue::ConstraintViolation {
                            tag: slot.name.clone(),
                            constraint: "query".into(),
                            detail: format!(
                                "inverse query '{inverse}' needs a reverse index, \
                                 and this caller supplied none"
                            ),
                        });
                        continue;
                    };
                    traverse_refs_inverse(entity, &ref_tag, transitive, reverse)
                }
                None => {
                    // A reference to a spec or slot that does not exist cannot be
                    // evaluated either way. Fail closed, consistently with how an
                    // unknown spec name is treated.
                    issues.push(FitIssue::ConstraintViolation {
                        tag: slot.name.clone(),
                        constraint: "query".into(),
                        detail: format!(
                            "inverse query names '{inverse}', which is not a \
                             forward query slot in this namespace"
                        ),
                    });
                    continue;
                }
            }
        } else {
            // Neither direction declared: nothing to evaluate.
            continue;
        };

        // `of` narrows what counts as reached. Absent, anything does.
        let matching = match of_type {
            Some(of) => found
                .iter()
                .filter(|candidate| fits_of(candidate, of, &spec.lib, specs, ns))
                .count(),
            None => found.len(),
        };

        if matching == 0 && !slot.is_maybe() {
            let detail = match (via_path, inverse_ref, of_type) {
                (Some(via), _, Some(of)) => {
                    format!("no '{of}' reachable via '{via}'")
                }
                (Some(via), _, None) => format!("no entities reachable via '{via}'"),
                (None, Some(inv), Some(of)) => {
                    format!("no '{of}' reaches this entity through '{inv}'")
                }
                (None, Some(inv), None) => {
                    format!("no entities reach this entity through '{inv}'")
                }
                (None, None, _) => unreachable!("continue above covers this"),
            };
            issues.push(FitIssue::ConstraintViolation {
                tag: slot.name.clone(),
                constraint: "query".into(),
                detail,
            });
        }
    }
}

/// Does `candidate` count as the type an `of:` names?
///
/// `of` is written unqualified — `of:Vav`, `of:Target` — and where it resolves
/// depends on what is in scope. A spec in the same library is checked first,
/// because `DefNamespace::resolve_spec_term` only finds a Xeto spec under its
/// exact qualified name; a bare `Target` would fall through to the def rungs and
/// resolve to nothing, or worse, to an unrelated global def that happens to share
/// the name.
///
/// The check is structural and shallow: no query context is passed down, so query
/// slots on the `of` type itself are not evaluated. `of` asks what a reached
/// entity *is*, and resolving that should not recursively drag in the whole graph.
fn fits_of(
    candidate: &HDict,
    of: &str,
    enclosing_lib: &str,
    specs: &HashMap<String, Spec>,
    ns: &DefNamespace,
) -> bool {
    if !of.contains("::")
        && let Some(local) = specs.get(&format!("{enclosing_lib}::{of}"))
    {
        return explain_against_spec_with_specs(candidate, local, specs, ns, None).is_empty();
    }
    ns.fits_spec_term(candidate, of)
}

/// Read a string-valued slot meta entry.
fn str_meta<'s>(slot: &'s super::spec::Slot, key: &str) -> Option<&'s str> {
    match slot.meta.get(key) {
        Some(Kind::Str(s)) => Some(s.as_str()),
        _ => None,
    }
}

/// Split a `via` path into its ref tag and whether it is transitive.
///
/// `"equipRef+"` follows `equipRef` repeatedly; `"equipRef"` follows it once.
fn split_via(via: &str) -> (&str, bool) {
    match via.strip_suffix('+') {
        Some(stripped) => (stripped, true),
        None => (via, false),
    }
}

/// Resolve `inverse:"lib::Spec.slot"` to the ref tag and transitivity of the
/// forward query slot it names.
///
/// Returns `None` when the spec or the slot does not exist, or when the named
/// slot is not itself a forward query — all of which make the inverse
/// unevaluatable rather than trivially true.
fn resolve_inverse_via(inverse: &str, specs: &HashMap<String, Spec>) -> Option<(String, bool)> {
    let (spec_qname, slot_name) = inverse.rsplit_once('.')?;
    let target = specs.get(spec_qname)?;
    let slot = target.slots.iter().find(|s| s.name == slot_name)?;
    if !slot.is_query {
        return None;
    }
    let via = str_meta(slot, "via")?;
    let (tag, transitive) = split_via(via);
    Some((tag.to_string(), transitive))
}

/// Find the entities that reach `entity` through `ref_tag`, optionally
/// transitively.
///
/// The mirror of [`traverse_refs`]. Transitive here means descendants: the
/// entities pointing at this one, plus the entities pointing at those, and so on.
fn traverse_refs_inverse<'a>(
    entity: &HDict,
    ref_tag: &str,
    transitive: bool,
    reverse: &InverseResolver<'a>,
) -> Vec<&'a HDict> {
    let Some(Kind::Ref(start)) = entity.get("id") else {
        // Without an id there is nothing for anything else to point at. This is
        // not an error — an unsaved dict genuinely has no inbound refs.
        return Vec::new();
    };

    let mut results = Vec::new();
    let mut visited = HashSet::new();
    let mut queue = vec![start.clone()];

    while let Some(target) = queue.pop() {
        if !visited.insert(target.val.clone()) {
            continue;
        }
        for source in reverse(&target, ref_tag) {
            results.push(source);
            if transitive
                && let Some(Kind::Ref(source_id)) = source.get("id")
                && !visited.contains(&source_id.val)
            {
                queue.push(source_id.clone());
            }
        }
        if !transitive {
            break;
        }
    }

    results
}

/// Follow ref tags from an entity, optionally transitively.
fn traverse_refs<'a>(
    entity: &HDict,
    ref_tag: &str,
    transitive: bool,
    resolver: &EntityResolver<'a>,
) -> Vec<&'a HDict> {
    let mut results = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue = Vec::new();

    // Seed with the ref value from the starting entity
    if let Some(Kind::Ref(r)) = entity.get(ref_tag) {
        queue.push(r.clone());
    }

    while let Some(ref_val) = queue.pop() {
        if !visited.insert(ref_val.val.clone()) {
            continue;
        }
        if let Some(target) = resolver(&ref_val) {
            if transitive && let Some(Kind::Ref(next)) = target.get(ref_tag) {
                queue.push(next.clone());
            }
            results.push(target);
        }
    }
    results
}

/// Return a human-readable type name for a Kind value.
fn kind_type_name(val: &Kind) -> &'static str {
    match val {
        Kind::Null => "Null",
        Kind::Marker => "Marker",
        Kind::NA => "NA",
        Kind::Remove => "Remove",
        Kind::Bool(_) => "Bool",
        Kind::Number(_) => "Number",
        Kind::Str(_) => "Str",
        Kind::Ref(_) => "Ref",
        Kind::Uri(_) => "Uri",
        Kind::Symbol(_) => "Symbol",
        Kind::Date(_) => "Date",
        Kind::Time(_) => "Time",
        Kind::DateTime(_) => "DateTime",
        Kind::Coord(_) => "Coord",
        Kind::XStr(_) => "XStr",
        Kind::List(_) => "List",
        Kind::Dict(_) => "Dict",
        Kind::Grid(_) => "Grid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{HRef, Number};
    use crate::ontology::trio_loader::load_trio;

    /// Build a small namespace for testing.
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
def:^site
doc:\"A site\"
is:[^entity]
lib:^lib:ph
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
    fn entity_fits_with_all_markers() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);

        assert!(fits(&entity, "ahu", &ns, None));
    }

    #[test]
    fn entity_missing_mandatory_marker_fails() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        // Missing "equip" marker

        assert!(!fits(&entity, "ahu", &ns, None));
    }

    #[test]
    fn fits_explain_returns_missing_marker_issues() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        // Missing "equip"

        let issues = fits_explain(&entity, "ahu", &ns, None);
        assert!(!issues.is_empty());

        let has_equip_issue = issues
            .iter()
            .any(|i| matches!(i, FitIssue::MissingMarker { tag, .. } if tag == "equip"));
        assert!(has_equip_issue);
    }

    #[test]
    fn fits_explain_empty_when_valid() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);

        let issues = fits_explain(&entity, "ahu", &ns, None);
        assert!(issues.is_empty());
    }

    #[test]
    fn type_checking_wrong_type() {
        // Build a spec with a typed slot and check type mismatch
        let spec = Spec {
            qname: "test::Foo".to_string(),
            name: "Foo".to_string(),
            lib: "test".to_string(),
            base: None,
            meta: std::collections::HashMap::new(),
            slots: vec![super::super::spec::Slot {
                name: "name".to_string(),
                type_ref: Some("Str".to_string()),
                meta: std::collections::HashMap::new(),
                default: None,
                is_marker: false,
                is_query: false,
                children: Vec::new(),
            }],
            is_abstract: false,
            doc: String::new(),
        };

        let mut entity = HDict::new();
        entity.set("name", Kind::Number(Number::unitless(42.0))); // wrong type

        let issues = explain_against_spec(&entity, &spec);
        assert!(!issues.is_empty());
        let has_wrong_type = issues.iter().any(|i| {
            matches!(i, FitIssue::WrongType { tag, expected, actual }
                if tag == "name" && expected == "Str" && actual == "Number")
        });
        assert!(has_wrong_type);
    }

    #[test]
    fn type_checking_correct_type() {
        let spec = Spec {
            qname: "test::Foo".to_string(),
            name: "Foo".to_string(),
            lib: "test".to_string(),
            base: None,
            meta: std::collections::HashMap::new(),
            slots: vec![
                super::super::spec::Slot {
                    name: "name".to_string(),
                    type_ref: Some("Str".to_string()),
                    meta: std::collections::HashMap::new(),
                    default: None,
                    is_marker: false,
                    is_query: false,
                    children: Vec::new(),
                },
                super::super::spec::Slot {
                    name: "area".to_string(),
                    type_ref: Some("Number".to_string()),
                    meta: std::collections::HashMap::new(),
                    default: None,
                    is_marker: false,
                    is_query: false,
                    children: Vec::new(),
                },
                super::super::spec::Slot {
                    name: "siteRef".to_string(),
                    type_ref: Some("Ref".to_string()),
                    meta: std::collections::HashMap::new(),
                    default: None,
                    is_marker: false,
                    is_query: false,
                    children: Vec::new(),
                },
            ],
            is_abstract: false,
            doc: String::new(),
        };

        let mut entity = HDict::new();
        entity.set("name", Kind::Str("Test".to_string()));
        entity.set("area", Kind::Number(Number::unitless(1000.0)));
        entity.set("siteRef", Kind::Ref(HRef::from_val("site-1")));

        let issues = explain_against_spec(&entity, &spec);
        assert!(issues.is_empty());
    }

    #[test]
    fn maybe_slots_are_skipped() {
        let mut meta = std::collections::HashMap::new();
        meta.insert("maybe".to_string(), Kind::Marker);

        let spec = Spec {
            qname: "test::Foo".to_string(),
            name: "Foo".to_string(),
            lib: "test".to_string(),
            base: None,
            meta: std::collections::HashMap::new(),
            slots: vec![
                super::super::spec::Slot {
                    name: "optional".to_string(),
                    type_ref: None,
                    meta: meta.clone(),
                    default: None,
                    is_marker: true,
                    is_query: false,
                    children: Vec::new(),
                },
                super::super::spec::Slot {
                    name: "optionalStr".to_string(),
                    type_ref: Some("Str".to_string()),
                    meta,
                    default: None,
                    is_marker: false,
                    is_query: false,
                    children: Vec::new(),
                },
            ],
            is_abstract: false,
            doc: String::new(),
        };

        let entity = HDict::new(); // empty entity

        let issues = explain_against_spec(&entity, &spec);
        assert!(issues.is_empty()); // all slots are maybe, so no issues
    }

    #[test]
    fn kind_type_name_coverage() {
        assert_eq!(kind_type_name(&Kind::Null), "Null");
        assert_eq!(kind_type_name(&Kind::Marker), "Marker");
        assert_eq!(kind_type_name(&Kind::Bool(true)), "Bool");
        assert_eq!(kind_type_name(&Kind::Str("x".into())), "Str");
        assert_eq!(
            kind_type_name(&Kind::Number(Number::unitless(1.0))),
            "Number"
        );
        assert_eq!(kind_type_name(&Kind::Ref(HRef::from_val("x"))), "Ref");
    }

    #[test]
    fn fitting_checks_inherited_markers() {
        // Parent spec with mandatory marker "equip"
        let mut parent = Spec::new("test::Equip", "test", "Equip");
        parent.slots.push(super::super::spec::Slot {
            name: "equip".to_string(),
            type_ref: None,
            meta: std::collections::HashMap::new(),
            default: None,
            is_marker: true,
            is_query: false,
            children: Vec::new(),
        });

        // Child spec with mandatory marker "ahu", inheriting from Equip
        let mut child = Spec::new("test::Ahu", "test", "Ahu");
        child.base = Some("test::Equip".to_string());
        child.slots.push(super::super::spec::Slot {
            name: "ahu".to_string(),
            type_ref: None,
            meta: std::collections::HashMap::new(),
            default: None,
            is_marker: true,
            is_query: false,
            children: Vec::new(),
        });

        let mut specs = HashMap::new();
        specs.insert("test::Equip".to_string(), parent);
        specs.insert("test::Ahu".to_string(), child.clone());

        // Entity with only "ahu" marker, missing inherited "equip"
        let mut entity = HDict::new();
        entity.set("ahu", Kind::Marker);

        let issues =
            explain_against_spec_with_specs(&entity, &child, &specs, &DefNamespace::new(), None);
        assert!(!issues.is_empty());
        let has_equip_issue = issues
            .iter()
            .any(|i| matches!(i, FitIssue::MissingMarker { tag, .. } if tag == "equip"));
        assert!(
            has_equip_issue,
            "should report missing inherited 'equip' marker"
        );

        // Entity with both markers should pass
        let mut entity2 = HDict::new();
        entity2.set("ahu", Kind::Marker);
        entity2.set("equip", Kind::Marker);

        let issues2 =
            explain_against_spec_with_specs(&entity2, &child, &specs, &DefNamespace::new(), None);
        assert!(issues2.is_empty(), "should pass with all markers present");
    }

    #[test]
    fn constraint_min_val() {
        let mut meta = HashMap::new();
        meta.insert("minVal".to_string(), Kind::Number(Number::unitless(0.0)));
        let spec = Spec {
            qname: "test::Temp".into(),
            name: "Temp".into(),
            lib: "test".into(),
            base: None,
            meta: HashMap::new(),
            is_abstract: false,
            doc: String::new(),
            slots: vec![super::super::spec::Slot {
                name: "value".into(),
                type_ref: Some("Number".into()),
                meta,
                default: None,
                is_marker: false,
                is_query: false,
                children: vec![],
            }],
        };
        let mut entity = HDict::new();
        entity.set("value", Kind::Number(Number::unitless(-5.0)));
        let issues = explain_against_spec(&entity, &spec);
        assert!(issues.iter().any(|i| matches!(
            i,
            FitIssue::ConstraintViolation { constraint, .. } if constraint == "minVal"
        )));
    }

    #[test]
    fn constraint_max_val() {
        let mut meta = HashMap::new();
        meta.insert("maxVal".to_string(), Kind::Number(Number::unitless(100.0)));
        let spec = Spec {
            qname: "test::Pct".into(),
            name: "Pct".into(),
            lib: "test".into(),
            base: None,
            meta: HashMap::new(),
            is_abstract: false,
            doc: String::new(),
            slots: vec![super::super::spec::Slot {
                name: "pct".into(),
                type_ref: Some("Number".into()),
                meta,
                default: None,
                is_marker: false,
                is_query: false,
                children: vec![],
            }],
        };
        let mut entity = HDict::new();
        entity.set("pct", Kind::Number(Number::unitless(150.0)));
        let issues = explain_against_spec(&entity, &spec);
        assert!(issues.iter().any(|i| matches!(
            i,
            FitIssue::ConstraintViolation { constraint, .. } if constraint == "maxVal"
        )));
    }

    #[test]
    fn constraint_pattern() {
        let mut meta = HashMap::new();
        meta.insert(
            "pattern".to_string(),
            Kind::Str(r"^\d{4}-\d{2}-\d{2}$".into()),
        );
        let spec = Spec {
            qname: "test::Dated".into(),
            name: "Dated".into(),
            lib: "test".into(),
            base: None,
            meta: HashMap::new(),
            is_abstract: false,
            doc: String::new(),
            slots: vec![super::super::spec::Slot {
                name: "dateStr".into(),
                type_ref: Some("Str".into()),
                meta,
                default: None,
                is_marker: false,
                is_query: false,
                children: vec![],
            }],
        };
        let mut entity = HDict::new();
        entity.set("dateStr", Kind::Str("not-a-date".into()));
        let issues = explain_against_spec(&entity, &spec);
        assert!(issues.iter().any(|i| matches!(
            i,
            FitIssue::ConstraintViolation { constraint, .. } if constraint == "pattern"
        )));

        // Valid date should pass
        let mut entity2 = HDict::new();
        entity2.set("dateStr", Kind::Str("2025-01-15".into()));
        assert!(explain_against_spec(&entity2, &spec).is_empty());
    }

    #[test]
    fn constraint_non_empty() {
        let mut meta = HashMap::new();
        meta.insert("nonEmpty".to_string(), Kind::Marker);
        let spec = Spec {
            qname: "test::Named".into(),
            name: "Named".into(),
            lib: "test".into(),
            base: None,
            meta: HashMap::new(),
            is_abstract: false,
            doc: String::new(),
            slots: vec![super::super::spec::Slot {
                name: "dis".into(),
                type_ref: Some("Str".into()),
                meta,
                default: None,
                is_marker: false,
                is_query: false,
                children: vec![],
            }],
        };
        let mut entity = HDict::new();
        entity.set("dis", Kind::Str("  ".into()));
        let issues = explain_against_spec(&entity, &spec);
        assert!(issues.iter().any(|i| matches!(
            i,
            FitIssue::ConstraintViolation { constraint, .. } if constraint == "nonEmpty"
        )));
    }

    #[test]
    fn constraint_unitless() {
        let mut meta = HashMap::new();
        meta.insert("unitless".to_string(), Kind::Marker);
        let spec = Spec {
            qname: "test::Count".into(),
            name: "Count".into(),
            lib: "test".into(),
            base: None,
            meta: HashMap::new(),
            is_abstract: false,
            doc: String::new(),
            slots: vec![super::super::spec::Slot {
                name: "count".into(),
                type_ref: Some("Number".into()),
                meta,
                default: None,
                is_marker: false,
                is_query: false,
                children: vec![],
            }],
        };
        let mut entity = HDict::new();
        entity.set("count", Kind::Number(Number::new(5.0, Some("kg".into()))));
        let issues = explain_against_spec(&entity, &spec);
        assert!(issues.iter().any(|i| matches!(
            i,
            FitIssue::ConstraintViolation { constraint, .. } if constraint == "unitless"
        )));
    }

    #[test]
    fn constraint_list_max_size() {
        let mut meta = HashMap::new();
        meta.insert("maxSize".to_string(), Kind::Number(Number::unitless(3.0)));
        let spec = Spec {
            qname: "test::Limited".into(),
            name: "Limited".into(),
            lib: "test".into(),
            base: None,
            meta: HashMap::new(),
            is_abstract: false,
            doc: String::new(),
            slots: vec![super::super::spec::Slot {
                name: "items".into(),
                type_ref: Some("List".into()),
                meta,
                default: None,
                is_marker: false,
                is_query: false,
                children: vec![],
            }],
        };
        let mut entity = HDict::new();
        entity.set("items", Kind::List(vec![Kind::Marker; 5]));
        let issues = explain_against_spec(&entity, &spec);
        assert!(issues.iter().any(|i| matches!(
            i,
            FitIssue::ConstraintViolation { constraint, .. } if constraint == "maxSize"
        )));
    }

    #[test]
    fn valid_constraints_produce_no_issues() {
        let mut meta = HashMap::new();
        meta.insert("minVal".to_string(), Kind::Number(Number::unitless(0.0)));
        meta.insert("maxVal".to_string(), Kind::Number(Number::unitless(100.0)));
        let spec = Spec {
            qname: "test::Pct".into(),
            name: "Pct".into(),
            lib: "test".into(),
            base: None,
            meta: HashMap::new(),
            is_abstract: false,
            doc: String::new(),
            slots: vec![super::super::spec::Slot {
                name: "pct".into(),
                type_ref: Some("Number".into()),
                meta,
                default: None,
                is_marker: false,
                is_query: false,
                children: vec![],
            }],
        };
        let mut entity = HDict::new();
        entity.set("pct", Kind::Number(Number::unitless(50.0)));
        assert!(explain_against_spec(&entity, &spec).is_empty());
    }

    #[test]
    fn query_traversal_follows_refs() {
        let mut parent = HDict::new();
        parent.set("id", Kind::Ref(HRef::from_val("parent")));
        parent.set("equip", Kind::Marker);

        let mut child = HDict::new();
        child.set("id", Kind::Ref(HRef::from_val("child")));
        child.set("equipRef", Kind::Ref(HRef::from_val("parent")));

        let entities: HashMap<String, HDict> =
            vec![("parent".into(), parent), ("child".into(), child.clone())]
                .into_iter()
                .collect();

        let resolver = |r: &HRef| -> Option<&HDict> { entities.get(&r.val) };

        let reachable = traverse_refs(&child, "equipRef", false, &resolver);
        assert_eq!(reachable.len(), 1);
    }

    #[test]
    fn query_traversal_transitive() {
        let mut a = HDict::new();
        a.set("id", Kind::Ref(HRef::from_val("a")));
        a.set("siteRef", Kind::Ref(HRef::from_val("b")));

        let mut b = HDict::new();
        b.set("id", Kind::Ref(HRef::from_val("b")));
        b.set("siteRef", Kind::Ref(HRef::from_val("c")));

        let mut c = HDict::new();
        c.set("id", Kind::Ref(HRef::from_val("c")));

        let entities: HashMap<String, HDict> =
            vec![("a".into(), a.clone()), ("b".into(), b), ("c".into(), c)]
                .into_iter()
                .collect();

        let resolver = |r: &HRef| -> Option<&HDict> { entities.get(&r.val) };

        let reachable = traverse_refs(&a, "siteRef", true, &resolver);
        assert_eq!(reachable.len(), 2); // b and c
    }

    #[test]
    fn traverse_refs_handles_cycles() {
        let mut a = HDict::new();
        a.set("id", Kind::Ref(HRef::from_val("a")));
        a.set("equipRef", Kind::Ref(HRef::from_val("b")));

        let mut b = HDict::new();
        b.set("id", Kind::Ref(HRef::from_val("b")));
        b.set("equipRef", Kind::Ref(HRef::from_val("a")));

        let entities: HashMap<String, HDict> =
            vec![("a".into(), a), ("b".into(), b)].into_iter().collect();

        let resolver = |r: &HRef| -> Option<&HDict> { entities.get(&r.val) };

        let mut entity = HDict::new();
        entity.set("equipRef", Kind::Ref(HRef::from_val("a")));

        let reachable = traverse_refs(&entity, "equipRef", true, &resolver);
        assert_eq!(reachable.len(), 2); // a + b, no infinite loop
    }

    #[test]
    fn fits_with_resolver_none_works() {
        let ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);
        assert!(fits(&entity, "ahu", &ns, None));
    }

    #[test]
    fn invalid_regex_pattern_produces_constraint_violation() {
        let mut meta = HashMap::new();
        // An invalid regex (unclosed group)
        meta.insert("pattern".to_string(), Kind::Str(r"(\d+".into()));
        let spec = Spec {
            qname: "test::BadPattern".into(),
            name: "BadPattern".into(),
            lib: "test".into(),
            base: None,
            meta: HashMap::new(),
            is_abstract: false,
            doc: String::new(),
            slots: vec![super::super::spec::Slot {
                name: "code".into(),
                type_ref: Some("Str".into()),
                meta,
                default: None,
                is_marker: false,
                is_query: false,
                children: vec![],
            }],
        };
        let mut entity = HDict::new();
        entity.set("code", Kind::Str("anything".into()));
        let issues = explain_against_spec(&entity, &spec);
        assert!(
            issues.iter().any(|i| matches!(
                i,
                FitIssue::ConstraintViolation { constraint, detail, .. }
                    if constraint == "pattern" && detail.contains("invalid regex")
            )),
            "should report invalid regex pattern as constraint violation"
        );
    }
}
