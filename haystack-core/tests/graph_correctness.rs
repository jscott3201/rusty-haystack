//! Graph behavior checked against manual fixtures and an index-free query oracle.

use haystack_core::data::HDict;
use haystack_core::filter::{matches, parse_filter};
use haystack_core::graph::{DiffOp, EntityGraph, GraphError, GraphSubscriber, SharedGraph};
use haystack_core::kinds::{HRef, Kind, Number};
use haystack_core::xeto::QueryContext;
use tokio::sync::broadcast::error::TryRecvError;

fn entity(id: &str, tags: &[(&str, Kind)]) -> HDict {
    let mut row = patch(tags);
    row.set(
        "id",
        Kind::Ref(HRef::new(id, Some(format!("Display {id}")))),
    );
    row
}

fn patch(tags: &[(&str, Kind)]) -> HDict {
    let mut row = HDict::new();
    for (name, value) in tags {
        row.set(*name, value.clone());
    }
    row
}

fn number(value: f64) -> Kind {
    Kind::Number(Number::unitless(value))
}

fn ids(rows: Vec<&HDict>) -> Vec<String> {
    let mut result: Vec<_> = rows
        .into_iter()
        .map(|row| row.id().unwrap().val.clone())
        .collect();
    result.sort();
    result
}

fn assert_rejected_id_patch(id: Kind) {
    let graph = SharedGraph::default();
    let original = entity(
        "equip",
        &[
            (
                "siteRef",
                Kind::Ref(HRef::new("site", Some("Original site".into()))),
            ),
            ("curVal", number(21.0)),
        ],
    );
    graph.add(original.clone()).unwrap();
    let mut rx = graph.subscribe();
    let version = graph.version();
    for _ in 0..2 {
        assert_eq!(graph.read_all("curVal == 21", 0).unwrap().len(), 1);
    }
    let changes = patch(&[
        ("id", id),
        ("siteRef", Kind::Ref(HRef::from_val("other-site"))),
        ("curVal", number(99.0)),
        ("newTag", Kind::Marker),
    ]);
    assert!(matches!(
        graph.update("equip", changes),
        Err(GraphError::ImmutableId)
    ));
    assert_eq!(graph.version(), version);
    assert_eq!(graph.len(), 1);
    assert_eq!(graph.get("equip").unwrap(), original);
    assert_eq!(
        graph.get("equip").unwrap().id().unwrap().dis.as_deref(),
        Some("Display equip")
    );
    assert!(
        matches!(graph.get("equip").unwrap().get("siteRef"), Some(Kind::Ref(id)) if id.dis.as_deref() == Some("Original site"))
    );
    assert!(graph.get("renamed").is_none());
    assert_eq!(graph.refs_from("equip", Some("siteRef")), ["site"]);
    assert_eq!(graph.refs_to("site", Some("siteRef")), ["equip"]);
    assert!(graph.refs_to("other-site", None).is_empty());
    assert_eq!(graph.read_all("curVal == 21", 0).unwrap().len(), 1);
    assert_eq!(
        graph
            .read_all("curVal == 21 and not newTag", 0)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        graph
            .read(|g| g.value_index().eq_lookup("curVal", &number(21.0)))
            .len(),
        1
    );
    assert!(
        graph
            .read_all("curVal == 99 or newTag", 0)
            .unwrap()
            .is_empty()
    );
    assert!(graph.changes_since(version).unwrap().is_empty());
    let history = graph.changes_since(0).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].op, DiffOp::Add);
    assert_eq!(
        history[0]
            .new
            .as_ref()
            .unwrap()
            .id()
            .unwrap()
            .dis
            .as_deref(),
        Some("Display equip")
    );
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn id_rename_rejects_entire_patch() {
    assert_rejected_id_patch(Kind::Ref(HRef::from_val("renamed")));
}

#[test]
fn id_removal_rejects_entire_patch() {
    assert_rejected_id_patch(Kind::Remove);
}

#[test]
fn id_retyping_rejects_entire_patch() {
    assert_rejected_id_patch(Kind::Str("equip".into()));
}

#[test]
fn empty_update_checks_existence_without_mutating() {
    let graph = SharedGraph::default();
    graph.add(entity("equip", &[])).unwrap();
    let mut rx = graph.subscribe();
    let version = graph.version();
    graph.update("equip", HDict::new()).unwrap();
    assert!(
        matches!(graph.update("missing", HDict::new()), Err(GraphError::NotFound(id)) if id == "missing")
    );
    assert_eq!(graph.version(), version);
    assert!(graph.changes_since(version).unwrap().is_empty());
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn same_id_full_row_and_display_updates_remain_observable() {
    let graph = SharedGraph::default();
    let row = entity("equip", &[("curVal", number(21.0))]);
    graph.add(row.clone()).unwrap();
    assert_eq!(graph.read_all("curVal == 21", 0).unwrap().len(), 1);
    let mut rx = graph.subscribe();
    graph.update("equip", row).unwrap();
    assert_eq!(rx.try_recv().unwrap(), 2); // Nonempty equal patches keep their revision.
    graph
        .update(
            "equip",
            patch(&[(
                "id",
                Kind::Ref(HRef::new("equip", Some("New display".into()))),
            )]),
        )
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), 3);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    let row = graph.read_all("curVal == 21", 0).unwrap().pop().unwrap();
    assert_eq!(row.id().unwrap().dis.as_deref(), Some("New display"));
    let changes = graph.changes_since(2).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0]
            .changed_tags
            .as_ref()
            .unwrap()
            .id()
            .unwrap()
            .dis
            .as_deref(),
        Some("New display")
    );
    assert_eq!(
        changes[0]
            .previous_tags
            .as_ref()
            .unwrap()
            .id()
            .unwrap()
            .dis
            .as_deref(),
        Some("Display equip")
    );
}

/// Full evaluation over every stored row, without candidate indexes or caches.
/// Manual expected IDs below independently constrain the shared evaluator too.
fn scan(graph: &EntityGraph, expression: &str) -> Vec<String> {
    let ast = parse_filter(expression).unwrap();
    let resolve = |id: &HRef| graph.get(&id.val);
    ids(graph
        .all()
        .into_iter()
        .filter(|row| matches(&ast, row, Some(QueryContext::forward_only(&resolve))))
        .collect())
}

fn assert_query(graph: &EntityGraph, expression: &str, expected: &[&str]) {
    let mut expected: Vec<_> = expected.iter().map(|id| (*id).to_owned()).collect();
    expected.sort();
    assert_eq!(scan(graph, expression), expected, "scan: {expression}");
    // Bounded queries populate only the AST cache. Unlimited queries also fill
    // the result cache; repeat both paths, including after mutations below.
    for limit in [usize::MAX, usize::MAX, 0, 0] {
        assert_eq!(
            ids(graph.read_all(expression, limit).unwrap()),
            expected,
            "query: {expression}, limit={limit}"
        );
    }
}

#[test]
fn ref_comparisons_with_display_and_nested_paths_match_scan() {
    let mut graph = EntityGraph::new();
    graph
        .add(entity("s-a", &[("geoCity", Kind::Str("A".into()))]))
        .unwrap();
    graph
        .add(entity("s-b", &[("geoCity", Kind::Str("B".into()))]))
        .unwrap();
    for (id, value) in [
        (
            "e-a",
            Kind::Ref(HRef::new("s-a", Some("First label".into()))),
        ),
        (
            "e-b",
            Kind::Ref(HRef::new("s-a", Some("Other label".into()))),
        ),
        ("e-c", Kind::Ref(HRef::from_val("s-b"))),
        ("e-text", Kind::Str("s-a".into())),
    ] {
        graph
            .add(entity(id, &[("equip", Kind::Marker), ("siteRef", value)]))
            .unwrap();
    }
    assert_query(&graph, "siteRef == @s-a", &["e-a", "e-b"]);
    assert_query(&graph, "siteRef == @s-a \"Query label\"", &["e-a", "e-b"]);
    assert_query(&graph, "siteRef != @s-a", &["e-c", "e-text"]);
    assert_query(
        &graph,
        "(siteRef == @s-a and equip) or geoCity == \"B\"",
        &["e-a", "e-b", "s-b"],
    );
    assert_query(
        &graph,
        "equip and (siteRef == @s-b or siteRef->geoCity == \"A\")",
        &["e-a", "e-b", "e-c"],
    );
    assert_query(&graph, "siteRef->geoCity == \"A\"", &["e-a", "e-b"]);
}

fn numeric_graph() -> EntityGraph {
    let mut graph = EntityGraph::new();
    for (id, value) in [
        ("neg-inf", number(f64::NEG_INFINITY)),
        ("negative", number(-1.0)),
        ("minus-zero", number(-0.0)),
        ("plus-zero", number(0.0)),
        ("one", number(1.0)),
        ("above-one", number(f64::from_bits(1.0_f64.to_bits() + 1))),
        ("inf", number(f64::INFINITY)),
        ("nan", number(f64::NAN)),
        ("other-nan", number(f64::from_bits(f64::NAN.to_bits() + 1))),
        (
            "minus-unit",
            Kind::Number(Number::new(-0.0, Some("kW".into()))),
        ),
        (
            "plus-unit",
            Kind::Number(Number::new(0.0, Some("kW".into()))),
        ),
        (
            "one-unit",
            Kind::Number(Number::new(1.0, Some("kW".into()))),
        ),
        ("text", Kind::Str("1".into())),
        ("reference", Kind::Ref(HRef::from_val("target"))),
        ("boolean", Kind::Bool(true)),
        ("marker", Kind::Marker),
        ("null", Kind::Null),
        (
            "date",
            Kind::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
        ),
    ] {
        graph
            .add(entity(
                id,
                &[("curVal", value.clone()), ("unindexed", value)],
            ))
            .unwrap();
    }
    graph.add(entity("missing", &[])).unwrap();
    graph
}

#[test]
fn signed_zero_ordered_bounds_do_not_omit_equal_values() {
    let graph = numeric_graph();
    for (expression, expected) in [
        (
            "curVal >= 0",
            vec!["minus-zero", "plus-zero", "one", "above-one", "inf"],
        ),
        (
            "curVal <= -0",
            vec!["neg-inf", "negative", "minus-zero", "plus-zero"],
        ),
        ("curVal >= 0kW", vec!["minus-unit", "plus-unit", "one-unit"]),
        ("curVal <= -0kW", vec!["minus-unit", "plus-unit"]),
    ] {
        assert_query(&graph, expression, &expected);
        assert_query(
            &graph,
            &expression.replace("curVal", "unindexed"),
            &expected,
        );
    }
}

#[test]
fn numeric_equality_units_nan_and_neighboring_bounds_match_scan() {
    let graph = numeric_graph();
    for (expression, expected) in [
        ("curVal == 0", vec!["plus-zero"]),
        ("curVal == -0", vec!["minus-zero"]),
        ("curVal == 1kW", vec!["one-unit"]),
        ("curVal == NaN", vec!["nan"]),
        ("curVal > 1", vec!["above-one", "inf"]),
        ("curVal >= 1", vec!["one", "above-one", "inf"]),
        (
            "curVal < 1",
            vec!["neg-inf", "negative", "minus-zero", "plus-zero"],
        ),
        (
            "curVal <= 1",
            vec!["neg-inf", "negative", "minus-zero", "plus-zero", "one"],
        ),
        ("curVal > 1kW", vec![]),
        ("curVal > INF", vec![]),
        ("curVal >= INF", vec!["inf"]),
        ("curVal <= -INF", vec!["neg-inf"]),
        ("curVal > NaN", vec![]),
    ] {
        assert_query(&graph, expression, &expected);
        assert_query(
            &graph,
            &expression.replace("curVal", "unindexed"),
            &expected,
        );
    }
}

#[test]
fn inequality_preserves_other_units_and_unindexed_kinds() {
    let graph = numeric_graph();
    // Every present kind except the exact Number(1, no unit) is unequal.
    let expected = [
        "neg-inf",
        "negative",
        "minus-zero",
        "plus-zero",
        "above-one",
        "inf",
        "nan",
        "other-nan",
        "minus-unit",
        "plus-unit",
        "one-unit",
        "text",
        "reference",
        "boolean",
        "marker",
        "null",
        "date",
    ];
    assert_query(&graph, "curVal != 1", &expected);
    assert_query(&graph, "unindexed != 1", &expected);
    assert_query(
        &graph,
        "curVal != @target and (curVal == T or curVal == 1kW)",
        &["boolean", "one-unit"],
    );
    assert_query(
        &graph,
        "curVal != T and (curVal == @target or curVal == \"1\")",
        &["reference", "text"],
    );
    assert_query(
        &graph,
        "curVal != \"1\" and (curVal == T or curVal == 1)",
        &["boolean", "one"],
    );
    assert_query(
        &graph,
        "curVal != 0 and curVal <= 0",
        &["neg-inf", "negative", "minus-zero"],
    );
}

#[test]
fn indexed_fields_with_unsupported_literal_kinds_fall_back_safely() {
    let graph = numeric_graph();
    assert_query(&graph, "curVal == T", &["boolean"]);
    assert_query(&graph, "curVal == @target", &["reference"]);
    assert_query(&graph, "curVal >= 2024-01-02", &["date"]);
    assert_query(&graph, "curVal == N", &["null"]);
    assert_query(&graph, "curVal > \"0\"", &["text"]);
    assert_query(&graph, "curVal < \"2\"", &["text"]);
}

#[test]
fn registering_an_index_after_population_backfills_before_use() {
    let mut graph = EntityGraph::new();
    graph
        .add(entity("a", &[("temperature", number(10.0))]))
        .unwrap();
    graph
        .add(entity("b", &[("temperature", number(20.0))]))
        .unwrap();
    assert_query(&graph, "temperature >= 10", &["a", "b"]);
    let version = graph.version();
    graph.index_field("temperature");
    assert_eq!(graph.version(), version);
    assert_query(&graph, "temperature == 10", &["a"]); // Uncached on registration.
    graph.index_field("temperature"); // Idempotent: no duplicate index entries.
    assert_eq!(
        graph
            .value_index()
            .eq_lookup("temperature", &number(10.0))
            .len(),
        1
    );
    graph
        .update("a", patch(&[("temperature", number(30.0))]))
        .unwrap();
    assert_query(&graph, "temperature >= 10", &["a", "b"]);
    assert_query(&graph, "temperature == 10", &[]);
    graph.rebuild_value_index();
    assert_query(&graph, "temperature > 20", &["a"]);
    assert_query(&graph, "temperature == 20", &["b"]);
}

#[test]
fn retyping_a_ref_removes_both_adjacency_directions() {
    let mut graph = EntityGraph::new();
    graph
        .add(entity(
            "child",
            &[("equipRef", Kind::Ref(HRef::from_val("parent")))],
        ))
        .unwrap();
    graph
        .update("child", patch(&[("equipRef", Kind::Str("parent".into()))]))
        .unwrap();
    assert!(graph.refs_from("child", None).is_empty());
    assert!(graph.refs_to("parent", None).is_empty());
    assert_query(&graph, "equipRef == @parent", &[]);
    assert_query(&graph, "equipRef == \"parent\"", &["child"]);
}

fn assert_ref_state(graph: &EntityGraph, site_a: &[&str], site_b: &[&str], text: &[&str]) {
    assert_query(graph, "siteRef == @s-a", site_a);
    assert_query(graph, "siteRef == @s-b", site_b);
    assert_query(graph, "siteRef == \"s-b\"", text);
    assert_query(graph, "siteRef->geoCity == \"A\"", site_a);
    let resolved_b = if graph.contains("s-b") { site_b } else { &[] };
    assert_query(graph, "siteRef->geoCity == \"B\"", resolved_b);
    for (target, expected) in [("s-a", site_a), ("s-b", site_b)] {
        let mut actual = graph.refs_to(target, Some("siteRef"));
        actual.sort();
        assert_eq!(actual, expected);
        for id in expected {
            assert_eq!(graph.refs_from(id, Some("siteRef")), [target]);
        }
    }
}

#[test]
fn cached_queries_and_adjacency_track_a_mutation_sequence() {
    let mut graph = EntityGraph::new();
    graph
        .add(entity("s-a", &[("geoCity", Kind::Str("A".into()))]))
        .unwrap();
    graph
        .add(entity("s-b", &[("geoCity", Kind::Str("B".into()))]))
        .unwrap();
    assert_ref_state(&graph, &[], &[], &[]);
    for id in ["child", "other"] {
        graph
            .add(entity(
                id,
                &[(
                    "siteRef",
                    Kind::Ref(HRef::new("s-a", Some("Site A".into()))),
                )],
            ))
            .unwrap();
    }
    assert_ref_state(&graph, &["child", "other"], &[], &[]);
    graph
        .update(
            "child",
            patch(&[("siteRef", Kind::Ref(HRef::from_val("s-b")))]),
        )
        .unwrap();
    assert_ref_state(&graph, &["other"], &["child"], &[]);
    graph
        .update(
            "child",
            patch(&[(
                "siteRef",
                Kind::Ref(HRef::new("s-b", Some("New label".into()))),
            )]),
        )
        .unwrap();
    assert_ref_state(&graph, &["other"], &["child"], &[]);
    let row = graph.read_all("siteRef == @s-b", 0).unwrap()[0];
    assert!(
        matches!(row.get("siteRef"), Some(Kind::Ref(id)) if id.dis.as_deref() == Some("New label"))
    );
    graph
        .update("child", patch(&[("siteRef", Kind::Str("s-b".into()))]))
        .unwrap();
    assert_ref_state(&graph, &["other"], &[], &["child"]);
    graph
        .update("child", patch(&[("siteRef", Kind::Remove)]))
        .unwrap();
    assert_ref_state(&graph, &["other"], &[], &[]);
    graph.remove("other").unwrap();
    assert_ref_state(&graph, &[], &[], &[]);
    graph
        .add(entity(
            "other",
            &[("siteRef", Kind::Ref(HRef::from_val("s-b")))],
        ))
        .unwrap();
    assert_ref_state(&graph, &[], &["other"], &[]);
    graph.remove("s-b").unwrap(); // Incoming refs remain valid dangling refs.
    assert_ref_state(&graph, &[], &["other"], &[]);
    graph
        .add(entity("s-b", &[("geoCity", Kind::Str("B".into()))]))
        .unwrap();
    assert_ref_state(&graph, &[], &["other"], &[]);
    assert_eq!(graph.version(), 12);
    let diffs = graph.changes_since(0).unwrap();
    assert_eq!(
        diffs.iter().map(|diff| diff.version).collect::<Vec<_>>(),
        (1..=12).collect::<Vec<_>>()
    );
    assert_eq!(diffs[6].op, DiffOp::Update);
    assert!(
        matches!(diffs[6].previous_tags.as_ref().unwrap().get("siteRef"), Some(Kind::Ref(id)) if id.dis.as_deref() == Some("New label"))
    );
    assert_eq!(
        diffs[6].changed_tags.as_ref().unwrap().get("siteRef"),
        Some(&Kind::Str("s-b".into()))
    );
}

#[test]
fn raw_write_notifies_once_for_a_batch_and_wakes_subscribers() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let graph = SharedGraph::default();
    let mut rx = graph.subscribe();
    let mut subscriber = GraphSubscriber::new(graph.clone());
    graph.write(|g| {
        g.add(entity("a", &[])).unwrap();
        g.add(entity("b", &[])).unwrap();
        g.update("a", patch(&[("curVal", number(1.0))])).unwrap();
    });
    assert_eq!(rx.try_recv().unwrap(), 3);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    let mut batch = std::pin::pin!(subscriber.next_batch());
    let Poll::Ready(Ok(diffs)) = batch.as_mut().poll(&mut Context::from_waker(Waker::noop()))
    else {
        panic!("subscriber must be ready after accepted mutations");
    };
    assert_eq!(
        diffs.iter().map(|d| d.version).collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[test]
fn raw_write_error_does_not_hide_prior_accepted_mutations() {
    let graph = SharedGraph::default();
    let mut rx = graph.subscribe();
    let result = graph.write(|g| {
        g.add(entity("a", &[]))?;
        g.add(entity("a", &[])) // Batch closures do not roll back on error.
    });
    assert!(matches!(result, Err(GraphError::DuplicateRef(id)) if id == "a"));
    assert!(graph.contains("a"));
    assert_eq!(rx.try_recv().unwrap(), 1);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(graph.changes_since(0).unwrap().len(), 1);
}

#[test]
fn raw_write_unwind_keeps_committed_changes_and_original_panic() {
    let graph = SharedGraph::default();
    let mut rx = graph.subscribe();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        graph.write(|g| {
            g.add(entity("a", &[])).unwrap();
            panic!("original graph closure panic");
        });
    }))
    .unwrap_err();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"original graph closure panic")
    );
    assert!(graph.contains("a"));
    assert_eq!(rx.try_recv().unwrap(), 1);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn unchanged_and_rejected_raw_writes_are_quiet() {
    let graph = SharedGraph::default();
    graph.add(entity("a", &[("curVal", number(1.0))])).unwrap();
    let mut rx = graph.subscribe();
    assert!(matches!(
        graph.add(entity("a", &[])),
        Err(GraphError::DuplicateRef(_))
    ));
    assert!(matches!(
        graph.remove("missing"),
        Err(GraphError::NotFound(_))
    ));
    graph.update("a", HDict::new()).unwrap();
    graph.write(|g| {
        assert!(matches!(
            g.add(entity("a", &[])),
            Err(GraphError::DuplicateRef(_))
        ));
        assert!(matches!(g.remove("missing"), Err(GraphError::NotFound(_))));
        assert!(matches!(
            g.update("missing", HDict::new()),
            Err(GraphError::NotFound(_))
        ));
        assert!(matches!(
            g.update("a", patch(&[("id", Kind::Remove)])),
            Err(GraphError::ImmutableId)
        ));
        g.update("a", HDict::new()).unwrap();
        g.index_field("custom");
        g.rebuild_value_index();
    });
    assert_eq!(graph.version(), 1);
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    graph
        .write(|g| g.update("a", patch(&[("curVal", number(1.0))])))
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), 2); // Nonempty equal patch is observable.
    graph.remove("a").unwrap();
    assert_eq!(rx.try_recv().unwrap(), 3);
    assert!(matches!(graph.remove("a"), Err(GraphError::NotFound(_))));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
}

#[test]
fn value_indexes_track_retyping_units_removal_and_recycled_ids() {
    let mut graph = EntityGraph::new();
    graph.add(entity("a", &[("curVal", number(-0.0))])).unwrap();
    assert_query(&graph, "curVal == -0", &["a"]);
    assert_query(&graph, "curVal == 0", &[]);
    graph
        .update("a", patch(&[("curVal", number(0.0))]))
        .unwrap();
    assert_query(&graph, "curVal == -0", &[]);
    assert_query(&graph, "curVal == 0", &["a"]);
    graph
        .update(
            "a",
            patch(&[("curVal", Kind::Number(Number::new(0.0, Some("kW".into()))))]),
        )
        .unwrap();
    assert_query(&graph, "curVal == 0", &[]);
    assert_query(&graph, "curVal != 0", &["a"]);
    graph
        .update("a", patch(&[("curVal", Kind::Bool(true))]))
        .unwrap();
    assert_query(&graph, "curVal == 0kW", &[]);
    assert_query(&graph, "curVal == T", &["a"]);
    graph
        .update(
            "a",
            patch(&[("curVal", Kind::Ref(HRef::from_val("target")))]),
        )
        .unwrap();
    assert_query(&graph, "curVal == T", &[]);
    assert_query(&graph, "curVal == @target", &["a"]);
    graph.remove("a").unwrap();
    graph
        .add(entity("b", &[("curVal", Kind::Str("replacement".into()))]))
        .unwrap();
    assert_query(&graph, "curVal == @target", &[]);
    assert_query(&graph, "curVal == \"replacement\"", &["b"]);
    assert!(graph.refs_to("target", None).is_empty());
    graph
        .update("b", patch(&[("curVal", Kind::Remove)]))
        .unwrap();
    assert_query(&graph, "curVal", &[]);
    assert_query(&graph, "curVal == \"replacement\"", &[]);
    graph.add(entity("a", &[("curVal", number(4.0))])).unwrap();
    graph.rebuild_value_index();
    assert_query(&graph, "curVal == 4", &["a"]);
    assert_query(&graph, "not curVal", &["b"]);
}
