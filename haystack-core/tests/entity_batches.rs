//! Atomic entity commit units and native mutation/feed compatibility.
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind},
};

fn entity(id: &str) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row.set("site", Kind::Marker);
    row
}

#[test]
fn trusted_changes_since_max_is_empty_without_arithmetic_overflow() {
    let mut graph = EntityGraph::new();
    graph.add(entity("a")).unwrap();
    assert!(graph.changes_since(u64::MAX).unwrap().is_empty());
}

#[test]
fn equal_revision_replacement_wakes_and_retired_graph_never_revives_incarnation() {
    let graph = SharedGraph::new(EntityGraph::new());
    graph.add(entity("a")).unwrap();
    let first_incarnation = graph.read(EntityGraph::incarnation);
    let mut notifications = graph.subscribe();
    let mut replacement = EntityGraph::new();
    replacement.add(entity("b")).unwrap();
    let retired = graph.write(|current| std::mem::replace(current, replacement));
    assert_eq!(notifications.try_recv().unwrap(), 1);
    let replacement_incarnation = graph.read(EntityGraph::incarnation);
    assert_ne!(replacement_incarnation, first_incarnation);
    graph.write(|current| *current = retired);
    assert_eq!(notifications.try_recv().unwrap(), 1);
    assert_ne!(graph.read(EntityGraph::incarnation), first_incarnation);
    assert_ne!(
        graph.read(EntityGraph::incarnation),
        replacement_incarnation
    );
}

use haystack_core::{
    graph::{
        BatchError, BatchLimits, ChangeCursorError, CommitSpan, DiffOp, EntityOperation, GraphWake,
    },
    kinds::Number,
    ontology::DefNamespace,
};

#[test]
fn atomic_mixed_batch_updates_records_indexes_adjacency_cache_and_one_span() {
    let mut graph = EntityGraph::new();
    graph.index_field("n");
    for id in ["left", "right", "gone"] {
        graph.add(entity(id)).unwrap();
    }
    let mut point = entity("point");
    point.set("siteRef", Kind::Ref(HRef::from_val("left")));
    point.set("n", Kind::Number(Number::unitless(1.0)));
    point.set("oldTag", Kind::Marker);
    graph.add(point).unwrap();
    assert_eq!(
        graph
            .read_all("siteRef == @left and n == 1", 0)
            .unwrap()
            .len(),
        1
    );
    let before = graph.version();
    let mut patch = HDict::new();
    patch.set(
        "id",
        Kind::Ref(HRef::new("point", Some("display changed".into()))),
    );
    patch.set("siteRef", Kind::Ref(HRef::from_val("right")));
    patch.set("n", Kind::Number(Number::unitless(2.0)));
    patch.set("oldTag", Kind::Remove);
    patch.set("newTag", Kind::Marker);
    let mut added = entity("added");
    added.set("equipRef", Kind::Ref(HRef::from_val("point")));
    let operations = [
        EntityOperation::Patch {
            id: "point".into(),
            changes: patch,
        },
        EntityOperation::Remove { id: "gone".into() },
        EntityOperation::Add(added),
    ];
    let prepared = graph
        .prepare_batch(before, &operations, BatchLimits::default())
        .unwrap();
    assert_eq!(graph.version(), before);
    assert!(graph.get("gone").is_some());
    assert!(graph.get("added").is_none());
    let span = CommitSpan {
        first: before + 1,
        last: before + 3,
    };
    assert_eq!(graph.apply_prepared(prepared).unwrap(), Some(span));
    assert_eq!(graph.version(), before + 3);
    assert!(graph.get("gone").is_none());
    assert!(graph.get("point").unwrap().missing("oldTag"));
    assert!(graph.get("point").unwrap().has("newTag"));
    assert_eq!(
        graph.get("point").unwrap().id().unwrap().dis.as_deref(),
        Some("display changed")
    );
    assert!(
        graph
            .read_all("siteRef == @left and n == 1", 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        graph
            .read_all("siteRef == @right and n == 2", 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(graph.refs_to("point", Some("equipRef")), vec!["added"]);
    assert_eq!(graph.refs_from("point", Some("siteRef")), vec!["right"]);
    assert!(graph.refs_to("left", Some("siteRef")).is_empty());
    let units: Vec<_> = graph.change_units_since(before).unwrap().collect();
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].span, span);
    assert_eq!(units[0].len(), 3);
    let kinds: Vec<_> = units[0].diffs().map(|diff| diff.op.clone()).collect();
    assert_eq!(kinds, vec![DiffOp::Update, DiffOp::Remove, DiffOp::Add]);
}

#[test]
fn invalid_duplicate_stale_catalog_index_and_limit_failures_leave_state_and_feed_unchanged() {
    let mut graph = EntityGraph::new();
    graph.add(entity("a")).unwrap();
    let head = graph.version();
    let before = graph.get("a").unwrap().clone();
    let mut patch = HDict::new();
    patch.set("changed", Kind::Marker);
    let invalid = [
        EntityOperation::Patch {
            id: "a".into(),
            changes: patch.clone(),
        },
        EntityOperation::Add(entity("a")),
    ];
    assert!(
        graph
            .prepare_batch(head, &invalid, BatchLimits::default())
            .is_err()
    );
    let duplicate = [
        EntityOperation::Patch {
            id: "a".into(),
            changes: patch,
        },
        EntityOperation::Remove { id: "a".into() },
    ];
    assert!(matches!(
        graph.prepare_batch(head, &duplicate, BatchLimits::default()),
        Err(BatchError::DuplicateTarget)
    ));
    assert!(matches!(
        graph.prepare_batch(head, &[], BatchLimits::default()),
        Err(BatchError::Empty)
    ));
    assert!(matches!(
        graph.prepare_batch(
            head,
            &[EntityOperation::Add(entity("b"))],
            BatchLimits {
                max_retained_bytes: 1,
                ..BatchLimits::default()
            }
        ),
        Err(BatchError::Limit)
    ));
    assert_eq!(graph.version(), head);
    assert_eq!(graph.get("a"), Some(&before));
    assert!(graph.change_units_since(head).unwrap().next().is_none());
    let prepared = graph
        .prepare_batch(
            head,
            &[EntityOperation::Add(entity("b"))],
            BatchLimits::default(),
        )
        .unwrap();
    graph.set_namespace(DefNamespace::new());
    assert!(matches!(
        graph.apply_prepared(prepared),
        Err(BatchError::Conflict)
    ));
    let prepared = graph
        .prepare_batch(
            head,
            &[EntityOperation::Add(entity("b"))],
            BatchLimits::default(),
        )
        .unwrap();
    graph.index_field("newIndex");
    assert!(matches!(
        graph.apply_prepared(prepared),
        Err(BatchError::Conflict)
    ));
    let prepared = graph
        .prepare_batch(
            head,
            &[EntityOperation::Add(entity("b"))],
            BatchLimits::default(),
        )
        .unwrap();
    graph.add(entity("native")).unwrap();
    assert!(matches!(
        graph.apply_prepared(prepared),
        Err(BatchError::Conflict)
    ));
    assert!(graph.get("b").is_none());
    assert_eq!(graph.version(), head + 1);
}

#[test]
fn retention_evicts_whole_units_and_positions_require_complete_boundaries() {
    let mut graph = EntityGraph::with_changelog_capacity(4);
    let operations: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|id| EntityOperation::Add(entity(id)))
        .collect();
    let prepared = graph
        .prepare_batch(0, &operations, BatchLimits::default())
        .unwrap();
    graph.apply_prepared(prepared).unwrap();
    assert!(matches!(
        graph.change_units_since(1),
        Err(ChangeCursorError::InsideUnit {
            span: CommitSpan { first: 1, last: 3 }
        })
    ));
    assert!(matches!(
        graph.change_units_since(u64::MAX),
        Err(ChangeCursorError::Future { head: 3 })
    ));
    graph.add(entity("d")).unwrap();
    assert_eq!(
        graph
            .change_units_since(0)
            .unwrap()
            .map(|unit| unit.len())
            .collect::<Vec<_>>(),
        vec![3, 1]
    );
    graph.add(entity("e")).unwrap();
    assert_eq!(graph.floor_version(), 3);
    assert!(matches!(
        graph.change_units_since(2),
        Err(ChangeCursorError::Gap(_))
    ));
    assert_eq!(
        graph
            .change_units_since(3)
            .unwrap()
            .map(|unit| unit.span)
            .collect::<Vec<_>>(),
        vec![
            CommitSpan { first: 4, last: 4 },
            CommitSpan { first: 5, last: 5 }
        ]
    );
    let oversized: Vec<_> = (0..5)
        .map(|i| EntityOperation::Add(entity(&format!("extra-{i}"))))
        .collect();
    assert!(matches!(
        graph.prepare_batch(5, &oversized, BatchLimits::default()),
        Err(BatchError::Limit)
    ));
    assert_eq!(graph.version(), 5);
}

#[test]
fn oversized_native_diff_creates_explicit_gap_before_copy_and_later_units_resume() {
    let mut graph = EntityGraph::with_changelog_limits(10, 4096);
    graph.add(entity("a")).unwrap();
    let mut huge = entity("huge");
    huge.set("blob", Kind::Str("x".repeat(8192)));
    graph.add(huge).unwrap();
    assert!(graph.get("huge").is_some());
    assert_eq!(graph.floor_version(), 2);
    assert_eq!(graph.changelog_bytes(), 0);
    assert!(matches!(
        graph.change_units_since(0),
        Err(ChangeCursorError::Gap(_))
    ));
    graph.add(entity("b")).unwrap();
    assert_eq!(
        graph.change_units_since(2).unwrap().next().unwrap().span,
        CommitSpan { first: 3, last: 3 }
    );
}

#[test]
fn empty_patch_has_no_diff_but_equal_and_reference_display_patches_each_revise() {
    let mut graph = EntityGraph::new();
    graph.add(entity("a")).unwrap();
    let empty = graph
        .prepare_batch(
            1,
            &[EntityOperation::Patch {
                id: "a".into(),
                changes: HDict::new(),
            }],
            BatchLimits::default(),
        )
        .unwrap();
    assert_eq!(empty.after_revision(), 1);
    assert_eq!(graph.apply_prepared(empty).unwrap(), None);
    assert_eq!(graph.version(), 1);
    let mut equal = HDict::new();
    equal.set("site", Kind::Marker);
    let equal = graph
        .prepare_batch(
            1,
            &[EntityOperation::Patch {
                id: "a".into(),
                changes: equal,
            }],
            BatchLimits::default(),
        )
        .unwrap();
    graph.apply_prepared(equal).unwrap();
    let mut display = HDict::new();
    display.set("id", Kind::Ref(HRef::new("a", Some("new".into()))));
    let display = graph
        .prepare_batch(
            2,
            &[EntityOperation::Patch {
                id: "a".into(),
                changes: display,
            }],
            BatchLimits::default(),
        )
        .unwrap();
    graph.apply_prepared(display).unwrap();
    assert_eq!(graph.version(), 3);
    assert_eq!(
        graph.get("a").unwrap().id().unwrap().dis.as_deref(),
        Some("new")
    );
    assert_eq!(graph.change_units_since(1).unwrap().count(), 2);
}

#[test]
fn raw_failed_and_panicked_prefixes_remain_native_singletons_and_catalog_wakes_are_distinct() {
    let graph = SharedGraph::default();
    let mut raw = graph.subscribe();
    let mut wakes = graph.subscribe_wakes();
    let result: Result<(), &'static str> = graph.write(|graph| {
        graph.add(entity("a")).unwrap();
        Err("after accepted prefix")
    });
    assert!(result.is_err());
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| graph.write(|graph| {
            graph.add(entity("b")).unwrap();
            panic!("after accepted prefix");
        })))
        .is_err()
    );
    assert_eq!(raw.try_recv().unwrap(), 1);
    assert_eq!(raw.try_recv().unwrap(), 2);
    assert!(matches!(wakes.try_recv().unwrap(), GraphWake::Entities(_)));
    assert!(matches!(wakes.try_recv().unwrap(), GraphWake::Entities(_)));
    graph.set_namespace(DefNamespace::new());
    assert!(raw.try_recv().is_err());
    assert!(
        matches!(wakes.try_recv().unwrap(), GraphWake::Catalog(state) if state.revision == 2 && state.catalog_generation == 1)
    );
    // Every wake was consumed/lost; retained traversal is still immediately available.
    graph.read(|graph| {
        assert_eq!(
            graph
                .change_units_since(0)
                .unwrap()
                .map(|unit| unit.span)
                .collect::<Vec<_>>(),
            vec![
                CommitSpan { first: 1, last: 1 },
                CommitSpan { first: 2, last: 2 }
            ]
        );
    });
}

#[test]
fn tiny_batch_is_bounded_by_existing_index_work_before_any_effect() {
    let mut graph = EntityGraph::new();
    graph.index_field("n");
    for n in 0..2000 {
        let mut row = entity(&format!("p{n}"));
        row.set("n", Kind::Number(Number::unitless(1.0)));
        row.set("siteRef", Kind::Ref(HRef::from_val("site")));
        graph.add(row).unwrap();
    }
    let head = graph.version();
    let operations = [EntityOperation::Remove { id: "p0".into() }];
    assert!(matches!(
        graph.prepare_batch(
            head,
            &operations,
            BatchLimits {
                max_work: 1000,
                ..BatchLimits::default()
            }
        ),
        Err(BatchError::Limit)
    ));
    assert_eq!(graph.version(), head);
    assert!(graph.get("p0").is_some());
    assert_eq!(graph.refs_to("site", Some("siteRef")).len(), 2000);
    assert!(graph.change_units_since(head).unwrap().next().is_none());
}

#[test]
fn byte_retention_discards_complete_multi_diff_spans() {
    let mut graph = EntityGraph::with_changelog_limits(100, 4000);
    let prepared = graph
        .prepare_batch(
            0,
            &[
                EntityOperation::Add(entity("a")),
                EntityOperation::Add(entity("b")),
            ],
            BatchLimits::default(),
        )
        .unwrap();
    graph.apply_prepared(prepared).unwrap();
    assert_eq!(graph.change_units_since(0).unwrap().count(), 1);
    graph.add(entity("c")).unwrap();
    assert!(matches!(
        graph.change_units_since(0),
        Err(ChangeCursorError::Gap(_))
    ));
    let units = graph.change_units_since(2).unwrap();
    assert_eq!(units.floor, 2);
    assert_eq!(units.count(), 1);
}
#[test]
fn lower_equal_and_higher_revision_replacements_all_emit_reset_wakes() {
    for size in [0, 1, 3] {
        let graph = SharedGraph::new(EntityGraph::new());
        graph.add(entity("a")).unwrap();
        let before = graph.state();
        let mut wakes = graph.subscribe_wakes();
        let mut next = EntityGraph::new();
        for n in 0..size {
            next.add(entity(&format!("next{n}"))).unwrap();
        }
        graph.write(|g| *g = next);
        assert!(
            matches!(wakes.try_recv().unwrap(),GraphWake::Reset(state) if state.revision==size&&state.incarnation!=before.incarnation)
        );
    }
}
