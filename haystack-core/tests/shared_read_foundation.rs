use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind},
    ontology::DefNamespace,
};
use std::sync::Arc;

fn entity(id: &str) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row
}

#[test]
fn ordered_scan_survives_recycled_numeric_ids_and_compaction() {
    let mut graph = EntityGraph::new();
    for id in ["z", "a", "m"] {
        graph.add(entity(id)).unwrap();
    }
    graph.remove("m").unwrap();
    graph.add(entity("b")).unwrap();
    graph.compact();
    assert_eq!(
        graph
            .entities_after(None)
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        ["a", "b", "z"]
    );
    assert_eq!(
        graph
            .entities_after(Some("b"))
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        ["z"]
    );
}

#[test]
fn catalog_publication_is_atomic_and_does_not_emit_entity_changes() {
    let graph = SharedGraph::new(EntityGraph::new());
    let mut notices = graph.subscribe();
    let (version, generation, incarnation) =
        graph.read(|g| (g.version(), g.catalog_generation(), g.incarnation()));
    let first = Arc::new(DefNamespace::new());
    let next = graph
        .write(|g| g.compare_set_namespace(generation, first.clone()))
        .unwrap();
    assert_ne!(next, generation);
    graph.read(|g| {
        assert_eq!(g.version(), version);
        assert_eq!(g.incarnation(), incarnation);
        assert!(Arc::ptr_eq(g.namespace_arc().unwrap(), &first));
    });
    assert!(
        graph
            .write(|g| g.compare_set_namespace(generation, DefNamespace::new()))
            .is_err()
    );
    graph.set_namespace(DefNamespace::new());
    assert!(
        graph
            .write(|g| g.compare_set_namespace(next, DefNamespace::new()))
            .is_err()
    );
    assert!(notices.try_recv().is_err());
    graph.write(|g| *g = EntityGraph::new());
    assert_ne!(graph.read(|g| g.incarnation()), incarnation);
}

#[test]
fn filter_limits_cover_flat_ast_depth_nodes_and_interruption() {
    use haystack_core::filter::{FilterError, FilterParseLimits, parse_filter_controlled};
    let limits = FilterParseLimits {
        max_bytes: 100,
        max_nodes: 9,
        max_depth: 3,
    };
    assert!(parse_filter_controlled("a and b and c", limits, &mut || Ok(())).is_ok());
    assert!(matches!(
        parse_filter_controlled("a and b and c and d", limits, &mut || Ok(())),
        Err(FilterError::Limit)
    ));
    assert!(matches!(
        parse_filter_controlled(
            "a and (b or c)",
            FilterParseLimits {
                max_nodes: 4,
                ..limits
            },
            &mut || Ok(())
        ),
        Err(FilterError::Limit)
    ));
    assert!(matches!(
        parse_filter_controlled("a", limits, &mut || Err(FilterError::Interrupted)),
        Err(FilterError::Interrupted)
    ));
}

#[test]
fn zinc_short_null_and_surplus_cells_preserve_compatibility() {
    let codec = haystack_core::codecs::codec_for("text/zinc").unwrap();
    let grid = codec
        .decode_grid("ver:\"3.0\"\na,b,c\n1\nN,2\n3,,N,this surplus is deliberately not parsed\n")
        .unwrap();
    assert_eq!(grid.rows.len(), 3);
    assert_eq!(grid.rows[0].len(), 1);
    assert!(grid.rows[0].has("a"));
    assert!(grid.rows[0].missing("b"));
    assert_eq!(grid.rows[1].len(), 1);
    assert!(grid.rows[1].has("b"));
    assert_eq!(grid.rows[2].len(), 1);
    assert!(grid.rows[2].has("a"));
    let columns = (0..10_000)
        .map(|i| format!("x{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let wide = codec
        .decode_grid(&format!("ver:\"3.0\"\n{columns}\nN\n1\n"))
        .unwrap();
    assert_eq!(wide.cols.len(), 10_000);
    assert!(wide.rows[0].is_empty());
    assert_eq!(wide.rows[1].len(), 1);
}
