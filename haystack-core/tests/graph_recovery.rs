use haystack_core::{
    data::HDict,
    graph::{BatchLimits, EntityGraph, EntityOperation, RecoveryLimits},
    kinds::{HRef, Kind},
    xeto::catalog::{ActivatedCatalog, Catalog},
};
use std::sync::Arc;

fn row(id: &str) -> HDict {
    let mut value = HDict::new();
    value.set("id", Kind::Ref(HRef::from_val(id)));
    value.set("explicitNull", Kind::Null);
    value
}

#[test]
fn malformed_units_duplicate_rows_and_limits_do_not_restore_partial_state() {
    let catalog =
        Arc::new(ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap());
    let mut graph = EntityGraph::new();
    graph
        .compare_initialize_catalog(graph.state(), None, catalog.clone())
        .unwrap();
    graph.add(row("a")).unwrap();
    graph.add(row("b")).unwrap();
    let valid = graph.image(RecoveryLimits::default()).unwrap();
    let mut duplicate = valid.clone();
    duplicate.rows.push(row("a"));
    assert!(EntityGraph::restore(duplicate, catalog.clone(), RecoveryLimits::default()).is_err());
    let mut gap = valid.clone();
    gap.changes.remove(0);
    assert!(EntityGraph::restore(gap, catalog.clone(), RecoveryLimits::default()).is_err());
    let mut partial = valid.clone();
    partial.changes[0].span.last = 2;
    assert!(EntityGraph::restore(partial, catalog.clone(), RecoveryLimits::default()).is_err());
    assert!(
        graph
            .image(RecoveryLimits {
                max_rows: 1,
                ..RecoveryLimits::default()
            })
            .is_err()
    );
    assert!(
        EntityGraph::restore(
            valid,
            catalog,
            RecoveryLimits {
                max_bytes: 1,
                ..RecoveryLimits::default()
            }
        )
        .is_err()
    );
}
#[test]
fn restore_preserves_identity_revision_complete_units_and_absence() {
    let catalog =
        Arc::new(ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap());
    let mut graph = EntityGraph::new();
    graph
        .compare_initialize_catalog(graph.state(), None, catalog.clone())
        .unwrap();
    let batch = graph
        .prepare_batch(
            0,
            &[
                EntityOperation::Add(row("a")),
                EntityOperation::Add(row("b")),
            ],
            BatchLimits::default(),
        )
        .unwrap();
    graph.apply_prepared(batch).unwrap();
    let state = graph.state();
    let image = graph.image(RecoveryLimits::default()).unwrap();
    let restored = EntityGraph::restore(image, catalog, RecoveryLimits::default())
        .expect("valid checked recovery");
    assert_eq!(restored.state(), state);
    assert_eq!(restored.len(), 2);
    assert!(restored.get("a").unwrap().get("missing").is_none());
    assert!(matches!(
        restored.get("a").unwrap().get("explicitNull"),
        Some(Kind::Null)
    ));
    let units: Vec<_> = restored.change_units_since(0).unwrap().collect();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].len(), 2);
}
