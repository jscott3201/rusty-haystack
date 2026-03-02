// Graph-wide validation against a DefNamespace.

use std::collections::{HashMap, HashSet};

use crate::graph::EntityGraph;
use crate::kinds::Kind;

use super::namespace::DefNamespace;
use super::validation::FitIssue;

/// Summary statistics for a validation run.
#[derive(Debug, Clone)]
pub struct ValidationSummary {
    pub total_entities: usize,
    pub valid: usize,
    pub with_warnings: usize,
    pub with_errors: usize,
    pub untyped: usize,
}

/// Full validation report for a graph.
#[derive(Debug)]
pub struct ValidationReport {
    /// Per-entity fit issues (entity_id → issues).
    pub entity_issues: HashMap<String, Vec<FitIssue>>,
    /// Dangling references: (entity_id, tag_name, missing_ref_val).
    pub dangling_refs: Vec<(String, String, String)>,
    /// Fraction of entities that match at least one known spec (0.0 - 1.0).
    pub spec_coverage: f64,
    /// Summary statistics.
    pub summary: ValidationSummary,
}

/// Validate all entities in the graph against loaded specs.
///
/// Checks every entity against the namespace for spec conformance,
/// collects dangling ref issues, and computes coverage statistics.
pub fn validate_graph(graph: &EntityGraph, ns: &DefNamespace) -> ValidationReport {
    let all = graph.all();
    let total = all.len();

    // Collect all entity ids for dangling-ref checking.
    let id_set: HashSet<&str> = all
        .iter()
        .filter_map(|e| e.id().map(|r| r.val.as_str()))
        .collect();

    let mut entity_issues: HashMap<String, Vec<FitIssue>> = HashMap::new();
    let mut dangling_refs: Vec<(String, String, String)> = Vec::new();
    let mut typed_count: usize = 0;
    let mut error_count: usize = 0;
    let mut mandatory_cache: HashMap<&str, HashSet<String>> = HashMap::new();

    for entity in &all {
        let entity_id = match entity.id() {
            Some(r) => r.val.clone(),
            None => continue,
        };

        // Determine types this entity claims via marker tags that are known defs.
        let mut is_typed = false;
        let mut issues: Vec<FitIssue> = Vec::new();

        for (tag_name, val) in entity.iter() {
            if !matches!(val, Kind::Marker) {
                continue;
            }
            if !ns.contains(tag_name) {
                continue;
            }
            is_typed = true;
            let mandatory = mandatory_cache
                .entry(tag_name)
                .or_insert_with(|| ns.mandatory_tags(tag_name));
            for tag in mandatory.iter() {
                if entity.missing(tag) {
                    issues.push(FitIssue::MissingMarker {
                        tag: tag.clone(),
                        spec: tag_name.to_string(),
                    });
                }
            }
        }

        // Check dangling refs.
        for (tag_name, val) in entity.iter() {
            if tag_name == "id" {
                continue;
            }
            if let Kind::Ref(href) = val {
                if !id_set.contains(href.val.as_str()) {
                    dangling_refs.push((entity_id.clone(), tag_name.to_string(), href.val.clone()));
                }
            }
        }

        if !issues.is_empty() {
            error_count += 1;
            entity_issues.insert(entity_id, issues);
        }
        if is_typed {
            typed_count += 1;
        }
    }

    let untyped = total - typed_count;
    let spec_coverage = if total == 0 {
        0.0
    } else {
        typed_count as f64 / total as f64
    };

    ValidationReport {
        entity_issues,
        dangling_refs,
        spec_coverage,
        summary: ValidationSummary {
            total_entities: total,
            valid: total - error_count - untyped,
            with_warnings: 0,
            with_errors: error_count,
            untyped,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::HDict;
    use crate::graph::EntityGraph;
    use crate::kinds::{HRef, Kind};
    use crate::ontology::DefNamespace;

    fn empty_ns() -> DefNamespace {
        DefNamespace::new()
    }

    fn make_entity(id: &str, tags: &[(&str, Kind)]) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        for (k, v) in tags {
            d.set(*k, v.clone());
        }
        d
    }

    #[test]
    fn validate_graph_empty_graph() {
        let g = EntityGraph::new();
        let ns = empty_ns();
        let report = validate_graph(&g, &ns);
        assert_eq!(report.summary.total_entities, 0);
        assert_eq!(report.summary.valid, 0);
        assert_eq!(report.summary.with_warnings, 0);
        assert_eq!(report.summary.with_errors, 0);
        assert_eq!(report.summary.untyped, 0);
        assert!(report.entity_issues.is_empty());
        assert!(report.dangling_refs.is_empty());
        assert_eq!(report.spec_coverage, 0.0);
    }

    #[test]
    fn validate_graph_dangling_refs() {
        let mut g = EntityGraph::new();
        let e = make_entity("e1", &[("siteRef", Kind::Ref(HRef::from_val("missing")))]);
        g.add(e).unwrap();
        let ns = empty_ns();
        let report = validate_graph(&g, &ns);
        assert_eq!(report.dangling_refs.len(), 1);
        assert_eq!(report.dangling_refs[0].0, "e1");
        assert_eq!(report.dangling_refs[0].1, "siteRef");
        assert_eq!(report.dangling_refs[0].2, "missing");
    }

    #[test]
    fn validate_graph_no_dangling_refs() {
        let mut g = EntityGraph::new();
        let e1 = make_entity("site1", &[("dis", Kind::Str("Site 1".into()))]);
        let e2 = make_entity("equip1", &[("siteRef", Kind::Ref(HRef::from_val("site1")))]);
        g.add(e1).unwrap();
        g.add(e2).unwrap();
        let ns = empty_ns();
        let report = validate_graph(&g, &ns);
        assert!(report.dangling_refs.is_empty());
    }

    #[test]
    fn validate_graph_summary_counts() {
        let mut g = EntityGraph::new();
        // Two entities, neither has marker tags that are defs → both untyped
        let e1 = make_entity("a", &[("dis", Kind::Str("A".into()))]);
        let e2 = make_entity("b", &[("dis", Kind::Str("B".into()))]);
        g.add(e1).unwrap();
        g.add(e2).unwrap();
        let ns = empty_ns();
        let report = validate_graph(&g, &ns);
        assert_eq!(report.summary.total_entities, 2);
        assert_eq!(report.summary.untyped, 2);
        assert_eq!(report.summary.with_errors, 0);
        assert_eq!(report.spec_coverage, 0.0);
    }
}
