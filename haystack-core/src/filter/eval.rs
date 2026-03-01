// Filter evaluator — matches a FilterNode against an HDict entity.

use super::ast::{CmpOp, FilterNode, Path};
use crate::data::HDict;
use crate::kinds::{HRef, Kind};
use crate::ontology::DefNamespace;

/// Callback type that resolves a `Ref` to the target entity dict.
type ResolveRef<'a> = Option<&'a dyn Fn(&HRef) -> Option<HDict>>;

/// Evaluate a filter against an entity dict.
///
/// `resolve_ref` is an optional callback that resolves a `Ref` to the target
/// entity dict. It is required for multi-segment path traversal.
///
/// `namespace` is an optional ontology namespace used for SpecMatch evaluation
/// (e.g. `ph::Ahu`). If `None`, SpecMatch always returns false.
pub fn matches(node: &FilterNode, entity: &HDict, resolve_ref: ResolveRef<'_>) -> bool {
    matches_with_ns(node, entity, resolve_ref, None)
}

/// Evaluate a filter with ontology namespace support for SpecMatch.
pub fn matches_with_ns(
    node: &FilterNode,
    entity: &HDict,
    resolve_ref: ResolveRef<'_>,
    namespace: Option<&DefNamespace>,
) -> bool {
    match node {
        FilterNode::Has(path) => resolve_path(path, entity, resolve_ref).is_some(),
        FilterNode::Missing(path) => resolve_path(path, entity, resolve_ref).is_none(),
        FilterNode::Cmp { path, op, val } => match resolve_path(path, entity, resolve_ref) {
            Some(actual) => compare(&actual, op, val),
            None => false,
        },
        FilterNode::And(left, right) => {
            matches_with_ns(left, entity, resolve_ref, namespace)
                && matches_with_ns(right, entity, resolve_ref, namespace)
        }
        FilterNode::Or(left, right) => {
            matches_with_ns(left, entity, resolve_ref, namespace)
                || matches_with_ns(right, entity, resolve_ref, namespace)
        }
        FilterNode::SpecMatch(spec) => {
            match namespace {
                Some(ns) => {
                    // Extract simple type name: "ph::Ahu" → "ahu", "ph.equips::Ahu" → "ahu"
                    let type_name = spec.rsplit("::").next().unwrap_or(spec).to_lowercase();
                    ns.fits(entity, &type_name)
                }
                None => false,
            }
        }
    }
}

/// Resolve a path against an entity dict, following Ref links via resolve_ref.
///
// Note: Unlike the Python reference which raises ValueError when resolve_ref
// is None for multi-segment paths, we silently return false. This is intentional:
// filters should evaluate to false for unresolvable paths rather than crashing.
fn resolve_path(path: &Path, entity: &HDict, resolve_ref: ResolveRef<'_>) -> Option<Kind> {
    if path.is_single() {
        return entity.get(path.first()).cloned();
    }

    // Multi-segment path: need resolve_ref
    let resolve = resolve_ref?;

    let segments = &path.0;
    let mut current_entity = entity.clone();

    // Walk all segments except the last one — each intermediate value must be a Ref
    for seg in &segments[..segments.len() - 1] {
        match current_entity.get(seg) {
            Some(Kind::Ref(r)) => {
                current_entity = resolve(r)?;
            }
            _ => return None,
        }
    }

    // Return the final segment's value
    let last = &segments[segments.len() - 1];
    current_entity.get(last).cloned()
}

/// Compare two Kind values using the given comparison operator.
fn compare(actual: &Kind, op: &CmpOp, expected: &Kind) -> bool {
    match op {
        CmpOp::Eq => actual == expected,
        CmpOp::Ne => actual != expected,
        CmpOp::Lt | CmpOp::Le | CmpOp::Gt | CmpOp::Ge => ordered_cmp(actual, op, expected),
    }
}

/// Perform an ordered comparison between two Kind values.
/// Returns false if the types are not orderable or not comparable.
fn ordered_cmp(actual: &Kind, op: &CmpOp, expected: &Kind) -> bool {
    use std::cmp::Ordering;

    let ordering = match (actual, expected) {
        (Kind::Number(a), Kind::Number(b)) => a.partial_cmp(b),
        (Kind::Str(a), Kind::Str(b)) => a.partial_cmp(b),
        (Kind::Date(a), Kind::Date(b)) => a.partial_cmp(b),
        (Kind::Time(a), Kind::Time(b)) => a.partial_cmp(b),
        (Kind::DateTime(a), Kind::DateTime(b)) => a.partial_cmp(b),
        _ => None,
    };

    match ordering {
        Some(ord) => match op {
            CmpOp::Lt => ord == Ordering::Less,
            CmpOp::Le => ord == Ordering::Less || ord == Ordering::Equal,
            CmpOp::Gt => ord == Ordering::Greater,
            CmpOp::Ge => ord == Ordering::Greater || ord == Ordering::Equal,
            _ => unreachable!(), // Eq/Ne handled above
        },
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::parse_filter;
    use crate::kinds::Number;
    use chrono::NaiveDate;

    fn make_site_entity() -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val("site-1")));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str("Main Site".into()));
        d.set(
            "area",
            Kind::Number(Number::new(4500.0, Some("ft²".into()))),
        );
        d.set("geoCity", Kind::Str("Richmond".into()));
        d
    }

    fn make_equip_entity() -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val("equip-1")));
        d.set("equip", Kind::Marker);
        d.set("dis", Kind::Str("AHU-1".into()));
        d.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
        d.set("temp", Kind::Number(Number::new(72.5, Some("°F".into()))));
        d
    }

    // ── Has tests ──

    #[test]
    fn has_present_tag() {
        let entity = make_site_entity();
        let filter = parse_filter("site").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn has_absent_tag() {
        let entity = make_site_entity();
        let filter = parse_filter("equip").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    // ── Missing tests ──

    #[test]
    fn missing_absent_tag() {
        let entity = make_site_entity();
        let filter = parse_filter("not equip").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn missing_present_tag() {
        let entity = make_site_entity();
        let filter = parse_filter("not site").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    // ── Cmp tests ──

    #[test]
    fn cmp_eq_string() {
        let entity = make_site_entity();
        let filter = parse_filter("dis == \"Main Site\"").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_eq_string_no_match() {
        let entity = make_site_entity();
        let filter = parse_filter("dis == \"Other Site\"").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_ne_string() {
        let entity = make_site_entity();
        let filter = parse_filter("dis != \"Other\"").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_gt_number_same_unit() {
        let entity = make_equip_entity();
        let filter = parse_filter("temp > 70°F").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_gt_number_same_unit_no_match() {
        let entity = make_equip_entity();
        let filter = parse_filter("temp > 80°F").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_number_different_units() {
        let entity = make_equip_entity();
        // temp is 72.5°F, comparing with °C should return false
        let filter = parse_filter("temp > 70°C").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_le_number() {
        let entity = make_equip_entity();
        let filter = parse_filter("temp <= 72.5°F").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_lt_number() {
        let entity = make_equip_entity();
        let filter = parse_filter("temp < 72.5°F").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_ge_number() {
        let entity = make_equip_entity();
        let filter = parse_filter("temp >= 72.5°F").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_eq_ref() {
        let entity = make_equip_entity();
        let filter = parse_filter("siteRef == @site-1").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_eq_ref_no_match() {
        let entity = make_equip_entity();
        let filter = parse_filter("siteRef == @site-2").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_missing_tag() {
        let entity = make_site_entity();
        // Tag doesn't exist, comparison should return false
        let filter = parse_filter("temp > 72").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_date_gt() {
        let mut entity = HDict::new();
        entity.set(
            "installed",
            Kind::Date(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap()),
        );
        let filter = parse_filter("installed > 2024-01-01").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_date_lt() {
        let mut entity = HDict::new();
        entity.set(
            "installed",
            Kind::Date(NaiveDate::from_ymd_opt(2023, 6, 15).unwrap()),
        );
        let filter = parse_filter("installed < 2024-01-01").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_string_ordering() {
        let mut entity = HDict::new();
        entity.set("name", Kind::Str("Charlie".into()));
        let filter = parse_filter("name > \"Alpha\"").unwrap();
        assert!(matches(&filter, &entity, None));
        let filter = parse_filter("name < \"Zulu\"").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    // ── And / Or tests ──

    #[test]
    fn and_both_true() {
        let entity = make_site_entity();
        let filter = parse_filter("site and dis == \"Main Site\"").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn and_one_false() {
        let entity = make_site_entity();
        let filter = parse_filter("site and equip").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn or_one_true() {
        let entity = make_site_entity();
        let filter = parse_filter("site or equip").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn or_both_false() {
        let entity = make_site_entity();
        let filter = parse_filter("equip or point").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn complex_and_or() {
        let entity = make_site_entity();
        let filter = parse_filter("(site or equip) and dis == \"Main Site\"").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    // ── Path traversal tests ──

    #[test]
    fn single_segment_path() {
        let entity = make_site_entity();
        let filter = parse_filter("dis == \"Main Site\"").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn multi_segment_path_no_resolver() {
        let entity = make_equip_entity();
        // Without resolve_ref, multi-segment paths cannot be resolved
        let filter = parse_filter("siteRef->dis").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn multi_segment_path_with_resolver() {
        let entity = make_equip_entity();
        let site = make_site_entity();
        let resolver = move |r: &HRef| -> Option<HDict> {
            if r.val == "site-1" {
                Some(site.clone())
            } else {
                None
            }
        };
        let filter = parse_filter("siteRef->dis == \"Main Site\"").unwrap();
        assert!(matches(&filter, &entity, Some(&resolver)));
    }

    #[test]
    fn multi_segment_path_has() {
        let entity = make_equip_entity();
        let site = make_site_entity();
        let resolver = move |r: &HRef| -> Option<HDict> {
            if r.val == "site-1" {
                Some(site.clone())
            } else {
                None
            }
        };
        let filter = parse_filter("siteRef->area").unwrap();
        assert!(matches(&filter, &entity, Some(&resolver)));
    }

    #[test]
    fn multi_segment_path_missing_intermediate() {
        let entity = make_equip_entity();
        let resolver = |_r: &HRef| -> Option<HDict> { None };
        // siteRef resolves to a ref but resolver returns None
        let filter = parse_filter("siteRef->dis").unwrap();
        assert!(!matches(&filter, &entity, Some(&resolver)));
    }

    #[test]
    fn multi_segment_path_not_a_ref() {
        let mut entity = HDict::new();
        // "dis" is a string, not a ref, so dis->foo should fail
        entity.set("dis", Kind::Str("hello".into()));
        let resolver = |_r: &HRef| -> Option<HDict> { None };
        let filter = parse_filter("dis->foo").unwrap();
        assert!(!matches(&filter, &entity, Some(&resolver)));
    }

    // ── SpecMatch tests ──

    #[test]
    fn spec_match_without_namespace_returns_false() {
        let entity = make_site_entity();
        let filter = parse_filter("ph::Site").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn spec_match_with_namespace() {
        use crate::ontology::DefNamespace;
        let ns = DefNamespace::load_standard().unwrap();
        let entity = make_site_entity();
        // Entity has 'site' marker, should fit "site" type
        let filter = parse_filter("ph::Site").unwrap();
        assert!(matches_with_ns(&filter, &entity, None, Some(&ns)));
    }

    #[test]
    fn spec_match_no_fit() {
        use crate::ontology::DefNamespace;
        let ns = DefNamespace::load_standard().unwrap();
        let entity = make_site_entity();
        // Entity has 'site' but not 'equip', should not fit "equip"
        let filter = parse_filter("ph::Equip").unwrap();
        assert!(!matches_with_ns(&filter, &entity, None, Some(&ns)));
    }

    #[test]
    fn spec_match_qualified_name() {
        use crate::ontology::DefNamespace;
        let ns = DefNamespace::load_standard().unwrap();
        let entity = make_equip_entity();
        let filter = parse_filter("ph.equips::Equip").unwrap();
        assert!(matches_with_ns(&filter, &entity, None, Some(&ns)));
    }

    // ── Edge cases ──

    #[test]
    fn cmp_incompatible_types() {
        // Comparing a string with a number using > should return false
        let mut entity = HDict::new();
        entity.set("val", Kind::Str("hello".into()));
        let filter = parse_filter("val > 42").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_marker_not_orderable() {
        let mut entity = HDict::new();
        entity.set("site", Kind::Marker);
        let filter = parse_filter("site > 42").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn eq_marker() {
        let mut entity = HDict::new();
        entity.set("site", Kind::Marker);
        let filter = parse_filter("site == M").unwrap();
        assert!(matches(&filter, &entity, None));
    }

    #[test]
    fn cmp_unitless_numbers() {
        let mut entity = HDict::new();
        entity.set("count", Kind::Number(Number::unitless(10.0)));
        let filter = parse_filter("count > 5").unwrap();
        assert!(matches(&filter, &entity, None));
        let filter = parse_filter("count < 5").unwrap();
        assert!(!matches(&filter, &entity, None));
    }

    #[test]
    fn three_segment_path() {
        // equip -> siteRef -> area
        let equip = make_equip_entity();
        let site = make_site_entity();
        let resolver = move |r: &HRef| -> Option<HDict> {
            if r.val == "site-1" {
                Some(site.clone())
            } else {
                None
            }
        };
        let filter = parse_filter("siteRef->area > 4000ft²").unwrap();
        assert!(matches(&filter, &equip, Some(&resolver)));
    }

    // ── DateTime comparison tests ──

    #[test]
    fn cmp_datetime_gt() {
        use crate::kinds::HDateTime;
        use chrono::TimeZone;
        // Test that DateTime > comparison works
        let offset = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let dt1 = offset.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap();
        let dt2 = offset.with_ymd_and_hms(2024, 1, 1, 8, 0, 0).unwrap();

        let mut entity = HDict::new();
        entity.set("updated", Kind::DateTime(HDateTime::new(dt1, "New_York")));

        let node = FilterNode::Cmp {
            path: Path::single("updated"),
            op: CmpOp::Gt,
            val: Kind::DateTime(HDateTime::new(dt2, "New_York")),
        };
        assert!(matches(&node, &entity, None));
    }

    #[test]
    fn cmp_datetime_lt() {
        use crate::kinds::HDateTime;
        use chrono::TimeZone;
        let offset = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let dt1 = offset.with_ymd_and_hms(2024, 1, 1, 8, 0, 0).unwrap();
        let dt2 = offset.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap();

        let mut entity = HDict::new();
        entity.set("updated", Kind::DateTime(HDateTime::new(dt1, "New_York")));

        let node = FilterNode::Cmp {
            path: Path::single("updated"),
            op: CmpOp::Lt,
            val: Kind::DateTime(HDateTime::new(dt2, "New_York")),
        };
        assert!(matches(&node, &entity, None));
    }
}
