// Xeto structural type fitting -- checks whether an entity fits a Xeto spec.

use std::collections::{HashMap, HashSet};

use crate::data::HDict;
use crate::kinds::{HRef, Kind};
use crate::ontology::DefNamespace;
use crate::ontology::validation::FitIssue;

use super::spec::Spec;

/// Entity resolver function type for query evaluation.
/// Given a ref, returns the entity dict if it exists.
pub type EntityResolver = dyn Fn(&HRef) -> Option<HDict>;

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
    ns: &mut DefNamespace,
    resolver: Option<&EntityResolver>,
) -> bool {
    fits_explain(entity, spec_qname, ns, resolver).is_empty()
}

/// Explain why an entity does or does not fit a Xeto spec.
///
/// Returns a list of `FitIssue` items; empty if the entity fits.
pub fn fits_explain(
    entity: &HDict,
    spec_qname: &str,
    ns: &mut DefNamespace,
    resolver: Option<&EntityResolver>,
) -> Vec<FitIssue> {
    // Try to look up as a Xeto spec first.
    // If the spec is not found in our local registry, fall back to
    // the DefNamespace for traditional Haystack 4 def-based fitting.
    let spec = resolve_spec(spec_qname, ns);
    match spec {
        Some(spec) => explain_against_spec_with_specs(entity, &spec, &HashMap::new(), resolver),
        None => {
            // Fall back to plain def-based fitting
            // Strip any lib:: prefix to get bare def name
            let bare_name = spec_qname.split("::").last().unwrap_or(spec_qname);
            ns.fits_explain(entity, bare_name)
        }
    }
}

/// Attempt to resolve a spec from the DefNamespace.
///
/// This builds a synthetic Spec from the def's mandatory markers
/// and slot information when available.
fn resolve_spec(spec_qname: &str, ns: &mut DefNamespace) -> Option<Spec> {
    // Extract bare name
    let bare_name = spec_qname.split("::").last().unwrap_or(spec_qname);

    // Check if the def exists in the namespace
    let def = ns.get_def(bare_name)?;
    let doc = def.doc.clone();
    let lib = def.lib.clone();

    // Build a synthetic Spec from mandatory markers
    let mandatory = ns.mandatory_tags(bare_name);
    let mut spec = Spec {
        qname: spec_qname.to_string(),
        name: bare_name.to_string(),
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
    explain_against_spec_with_specs(entity, spec, &HashMap::new(), None)
}

/// Check an entity against a resolved Spec, with access to a specs map for
/// walking the inheritance chain.
fn explain_against_spec_with_specs(
    entity: &HDict,
    spec: &Spec,
    specs: &HashMap<String, Spec>,
    resolver: Option<&EntityResolver>,
) -> Vec<FitIssue> {
    let mut issues = Vec::new();

    // Level 1: Mandatory markers (walks inheritance chain)
    check_mandatory_markers(entity, spec, specs, &mut issues);

    // Level 2: Slot type checking
    check_slot_types(entity, spec, &mut issues);

    // Level 2.5: Value constraints
    check_value_constraints(entity, spec, &mut issues);

    // Level 3: Query evaluation (only when resolver is provided)
    if let Some(resolver) = resolver {
        check_query_slots(entity, spec, resolver, &mut issues);
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
fn check_query_slots(
    entity: &HDict,
    spec: &Spec,
    resolver: &EntityResolver,
    issues: &mut Vec<FitIssue>,
) {
    for slot in &spec.slots {
        if !slot.is_query {
            continue;
        }
        // Extract "of" type and "via" path from slot meta
        let of_type = slot.meta.get("of").and_then(|v| {
            if let Kind::Str(s) = v {
                Some(s.as_str())
            } else {
                None
            }
        });

        let via_path = slot.meta.get("via").and_then(|v| {
            if let Kind::Str(s) = v {
                Some(s.as_str())
            } else {
                None
            }
        });

        if let (Some(_of_type), Some(via)) = (of_type, via_path) {
            // Parse via path: "equipRef+" means follow equipRef transitively
            let (ref_tag, transitive) = if let Some(stripped) = via.strip_suffix('+') {
                (stripped, true)
            } else {
                (via, false)
            };

            // Traverse from entity following ref_tag
            let reachable = traverse_refs(entity, ref_tag, transitive, resolver);

            // For non-maybe query slots, having no reachable entities is an issue
            if reachable.is_empty() && !slot.is_maybe() {
                issues.push(FitIssue::ConstraintViolation {
                    tag: slot.name.clone(),
                    constraint: "query".into(),
                    detail: format!("no entities reachable via '{}'", via),
                });
            }
        }
    }
}

/// Follow ref tags from an entity, optionally transitively.
fn traverse_refs(
    entity: &HDict,
    ref_tag: &str,
    transitive: bool,
    resolver: &EntityResolver,
) -> Vec<HDict> {
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
        let mut ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);

        assert!(fits(&entity, "ahu", &mut ns, None));
    }

    #[test]
    fn entity_missing_mandatory_marker_fails() {
        let mut ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        // Missing "equip" marker

        assert!(!fits(&entity, "ahu", &mut ns, None));
    }

    #[test]
    fn fits_explain_returns_missing_marker_issues() {
        let mut ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
        entity.set("ahu", Kind::Marker);
        // Missing "equip"

        let issues = fits_explain(&entity, "ahu", &mut ns, None);
        assert!(!issues.is_empty());

        let has_equip_issue = issues
            .iter()
            .any(|i| matches!(i, FitIssue::MissingMarker { tag, .. } if tag == "equip"));
        assert!(has_equip_issue);
    }

    #[test]
    fn fits_explain_empty_when_valid() {
        let mut ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);

        let issues = fits_explain(&entity, "ahu", &mut ns, None);
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

        let issues = explain_against_spec_with_specs(&entity, &child, &specs, None);
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

        let issues2 = explain_against_spec_with_specs(&entity2, &child, &specs, None);
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

        let resolver = move |r: &HRef| -> Option<HDict> { entities.get(&r.val).cloned() };

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

        let resolver = move |r: &HRef| -> Option<HDict> { entities.get(&r.val).cloned() };

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

        let resolver = move |r: &HRef| -> Option<HDict> { entities.get(&r.val).cloned() };

        let mut entity = HDict::new();
        entity.set("equipRef", Kind::Ref(HRef::from_val("a")));

        let reachable = traverse_refs(&entity, "equipRef", true, &resolver);
        assert_eq!(reachable.len(), 2); // a + b, no infinite loop
    }

    #[test]
    fn fits_with_resolver_none_works() {
        let mut ns = build_test_ns();
        let mut entity = HDict::new();
        entity.set("ahu", Kind::Marker);
        entity.set("equip", Kind::Marker);
        assert!(fits(&entity, "ahu", &mut ns, None));
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
