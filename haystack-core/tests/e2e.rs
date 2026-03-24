// End-to-end integration test exercising the full Haystack pipeline:
// load defs -> build entities -> graph CRUD -> filter queries ->
// ref traversal -> validate -> encode/decode round-trip through
// Zinc, JSON v4, JSON v3, and Trio.

use haystack_core::codecs::codec_for;
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::filter;
use haystack_core::graph::EntityGraph;
use haystack_core::kinds::{HRef, Kind, Number};
use haystack_core::ontology::DefNamespace;

#[test]
fn full_pipeline_load_build_query_validate_encode_decode() {
    // 1. Load standard defs
    let ns = DefNamespace::load_standard().unwrap();
    assert!(ns.len() > 600, "Expected many defs, got {}", ns.len());

    // 2. Build entities
    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Main Campus".into()));
    site.set("geoCity", Kind::Str("Richmond".into()));
    site.set(
        "area",
        Kind::Number(Number::new(50000.0, Some("ft\u{00b2}".into()))),
    );

    let mut ahu = HDict::new();
    ahu.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    ahu.set("ahu", Kind::Marker);
    ahu.set("equip", Kind::Marker);
    ahu.set("airHandlingEquip", Kind::Marker);
    ahu.set("dis", Kind::Str("AHU-1".into()));
    ahu.set("siteRef", Kind::Ref(HRef::from_val("site-1")));

    let mut temp_point = HDict::new();
    temp_point.set("id", Kind::Ref(HRef::from_val("temp-1")));
    temp_point.set("point", Kind::Marker);
    temp_point.set("temp", Kind::Marker);
    temp_point.set("sensor", Kind::Marker);
    temp_point.set("dis", Kind::Str("Discharge Temp".into()));
    temp_point.set("equipRef", Kind::Ref(HRef::from_val("ahu-1")));
    temp_point.set("siteRef", Kind::Ref(HRef::from_val("site-1")));
    temp_point.set(
        "curVal",
        Kind::Number(Number::new(72.5, Some("\u{00b0}F".into()))),
    );

    // 3. Build graph with namespace
    let mut graph = EntityGraph::with_namespace(ns);
    graph.add(site).unwrap();
    graph.add(ahu).unwrap();
    graph.add(temp_point).unwrap();

    // 4. Query with filters
    let result = graph.read("site", 0).unwrap();
    assert_eq!(result.rows.len(), 1);

    let result = graph.read("equip", 0).unwrap();
    assert_eq!(result.rows.len(), 1);

    let result = graph.read("point", 0).unwrap();
    assert_eq!(result.rows.len(), 1);

    let result = graph.read("point or equip", 0).unwrap();
    assert_eq!(result.rows.len(), 2);

    // Comparison filter
    let result = graph.read("curVal > 70\u{00b0}F", 0).unwrap();
    assert_eq!(result.rows.len(), 1);

    // 5. Verify ref traversal
    let ahu_refs = graph.refs_from("ahu-1", Some("siteRef"));
    assert_eq!(ahu_refs, vec!["site-1".to_string()]);

    let mut site_back = graph.refs_to("site-1", None);
    site_back.sort();
    assert!(
        site_back.len() >= 2,
        "Expected at least 2 refs to site-1, got: {:?}",
        site_back
    ); // ahu and temp_point both ref site-1

    // 6. Validate
    let issues = graph.validate();
    // Should have no dangling refs since all refs are connected.
    let dangling: Vec<_> = issues
        .iter()
        .filter(|i| i.issue_type == "dangling_ref")
        .collect();
    assert!(
        dangling.is_empty(),
        "Unexpected dangling refs: {:?}",
        dangling
    );

    // 7. Export to grid
    let export = graph.to_grid("").unwrap();
    assert_eq!(export.rows.len(), 3);

    // 9. Encode to Zinc
    let zinc = codec_for("text/zinc").unwrap();
    let zinc_str = zinc.encode_grid(&export).unwrap();
    assert!(!zinc_str.is_empty());

    // 10. Decode back from Zinc
    let decoded = zinc.decode_grid(&zinc_str).unwrap();
    assert_eq!(decoded.rows.len(), 3);

    // 11. Encode to JSON v4
    let json = codec_for("application/json").unwrap();
    let json_str = json.encode_grid(&export).unwrap();
    assert!(!json_str.is_empty());

    // 12. Decode back from JSON
    let decoded_json = json.decode_grid(&json_str).unwrap();
    assert_eq!(decoded_json.rows.len(), 3);

    // 13. Verify round-trip fidelity: re-import decoded grid into a new graph.
    let reimported = EntityGraph::from_grid(&decoded, None).unwrap();
    assert_eq!(reimported.len(), 3);
    assert!(reimported.contains("site-1"));
    assert!(reimported.contains("ahu-1"));
    assert!(reimported.contains("temp-1"));

    // Re-imported graph should still answer filter queries.
    let reimported_sites = reimported.read_all("site", 0).unwrap();
    assert_eq!(reimported_sites.len(), 1);
}

#[test]
fn codec_round_trip_all_formats() {
    // Build a single-row grid with varied tag types.
    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("test-1")));
    entity.set("site", Kind::Marker);
    entity.set("dis", Kind::Str("Test Site".into()));
    entity.set(
        "area",
        Kind::Number(Number::new(4500.0, Some("ft\u{00b2}".into()))),
    );
    entity.set("geoCity", Kind::Str("Richmond".into()));
    entity.set("enabled", Kind::Bool(true));

    let cols = vec![
        HCol::new("id"),
        HCol::new("site"),
        HCol::new("dis"),
        HCol::new("area"),
        HCol::new("geoCity"),
        HCol::new("enabled"),
    ];
    let grid = HGrid::from_parts(HDict::new(), cols, vec![entity]);

    // Round-trip through each supported codec.
    for mime in &[
        "text/zinc",
        "application/json",
        "application/json;v=3",
        "text/trio",
    ] {
        let codec = codec_for(mime).unwrap_or_else(|| panic!("Codec not found for {mime}"));
        let encoded = codec
            .encode_grid(&grid)
            .unwrap_or_else(|e| panic!("Encode failed for {mime}: {e}"));
        assert!(!encoded.is_empty(), "Encoded output empty for {mime}");
        let decoded = codec
            .decode_grid(&encoded)
            .unwrap_or_else(|e| panic!("Decode failed for {mime}: {e}"));
        assert_eq!(
            decoded.rows.len(),
            1,
            "Round-trip row count mismatch for {mime}: expected 1, got {}",
            decoded.rows.len()
        );

        // Verify the id survived the round-trip.
        let row = &decoded.rows[0];
        let id = row
            .id()
            .unwrap_or_else(|| panic!("Missing id after {mime} round-trip"));
        assert_eq!(id.val, "test-1", "Id mismatch after {mime} round-trip");

        // Verify key string tag survived.
        assert_eq!(
            row.get("dis"),
            Some(&Kind::Str("Test Site".into())),
            "dis mismatch after {mime} round-trip"
        );
    }
}

#[test]
fn filter_round_trip_parse_and_match() {
    // Build entities with different characteristics.
    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("s1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Alpha".into()));

    let mut equip = HDict::new();
    equip.set("id", Kind::Ref(HRef::from_val("e1")));
    equip.set("equip", Kind::Marker);
    equip.set("dis", Kind::Str("AHU".into()));
    equip.set("siteRef", Kind::Ref(HRef::from_val("s1")));

    let mut point = HDict::new();
    point.set("id", Kind::Ref(HRef::from_val("p1")));
    point.set("point", Kind::Marker);
    point.set("sensor", Kind::Marker);
    point.set(
        "curVal",
        Kind::Number(Number::new(55.0, Some("\u{00b0}F".into()))),
    );
    point.set("equipRef", Kind::Ref(HRef::from_val("e1")));

    // Test various filter expressions.
    let test_cases: Vec<(&str, Vec<bool>)> = vec![
        ("site", vec![true, false, false]),
        ("equip", vec![false, true, false]),
        ("point", vec![false, false, true]),
        ("site or equip", vec![true, true, false]),
        ("point and sensor", vec![false, false, true]),
        ("not equip", vec![true, false, true]),
        ("curVal > 50\u{00b0}F", vec![false, false, true]),
        ("curVal < 50\u{00b0}F", vec![false, false, false]),
        ("dis == \"Alpha\"", vec![true, false, false]),
    ];

    let entities = [&site, &equip, &point];

    for (expr, expected) in test_cases {
        let ast = filter::parse_filter(expr)
            .unwrap_or_else(|e| panic!("Failed to parse filter: {expr}: {e}"));
        for (i, entity) in entities.iter().enumerate() {
            let result = filter::matches(&ast, entity, None);
            assert_eq!(
                result, expected[i],
                "Filter '{expr}' on entity {i}: expected {}, got {}",
                expected[i], result
            );
        }
    }
}

#[test]
fn graph_crud_lifecycle() {
    let mut g = EntityGraph::new();

    // Add
    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    site.set("dis", Kind::Str("Original".into()));
    g.add(site).unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(g.version(), 1);

    // Read
    let entity = g.get("site-1").unwrap();
    assert_eq!(entity.get("dis"), Some(&Kind::Str("Original".into())));

    // Update
    let mut changes = HDict::new();
    changes.set("dis", Kind::Str("Updated".into()));
    changes.set("geoCity", Kind::Str("Richmond".into()));
    g.update("site-1", changes).unwrap();
    assert_eq!(g.version(), 2);
    let entity = g.get("site-1").unwrap();
    assert_eq!(entity.get("dis"), Some(&Kind::Str("Updated".into())));
    assert_eq!(entity.get("geoCity"), Some(&Kind::Str("Richmond".into())));

    // Remove
    let removed = g.remove("site-1").unwrap();
    assert!(removed.has("site"));
    assert_eq!(g.len(), 0);
    assert_eq!(g.version(), 3);

    // Verify changelog
    let all_changes = g.changes_since(0).unwrap();
    assert_eq!(all_changes.len(), 3);
}

#[test]
fn multi_codec_grid_fidelity() {
    // Build a more complex grid with multiple rows and varied types.
    let mut rows = Vec::new();
    for i in 0..5 {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(format!("r-{i}"))));
        d.set("dis", Kind::Str(format!("Row {i}")));
        d.set(
            "val",
            Kind::Number(Number::new(i as f64 * 10.0, Some("kW".into()))),
        );
        d.set("active", Kind::Bool(i % 2 == 0));
        if i > 0 {
            d.set("parentRef", Kind::Ref(HRef::from_val("r-0")));
        }
        rows.push(d);
    }

    let mut col_names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in &rows {
        for name in row.tag_names() {
            if seen.insert(name.to_string()) {
                col_names.push(name.to_string());
            }
        }
    }
    col_names.sort();
    let cols: Vec<HCol> = col_names.iter().map(|n| HCol::new(n.as_str())).collect();
    let grid = HGrid::from_parts(HDict::new(), cols, rows);

    // Encode to Zinc, decode, re-encode to JSON, decode -- verify integrity.
    let zinc = codec_for("text/zinc").unwrap();
    let zinc_str = zinc.encode_grid(&grid).unwrap();
    let from_zinc = zinc.decode_grid(&zinc_str).unwrap();
    assert_eq!(from_zinc.rows.len(), 5);

    let json = codec_for("application/json").unwrap();
    let json_str = json.encode_grid(&from_zinc).unwrap();
    let from_json = json.decode_grid(&json_str).unwrap();
    assert_eq!(from_json.rows.len(), 5);

    // All ids should survive the multi-codec chain.
    let graph = EntityGraph::from_grid(&from_json, None).unwrap();
    assert_eq!(graph.len(), 5);
    for i in 0..5 {
        assert!(
            graph.contains(&format!("r-{i}")),
            "Missing entity r-{i} after multi-codec round-trip"
        );
    }

    // Ref traversal should work on the re-imported graph.
    let sources = graph.refs_to("r-0", None);
    assert_eq!(
        sources.len(),
        4,
        "Expected 4 refs to r-0, got {}",
        sources.len()
    );
}

#[test]
fn ontology_fits_and_validation_in_graph() {
    let ns = DefNamespace::load_standard().unwrap();

    // Verify taxonomy basics.
    assert!(ns.is_a("ahu", "equip"));
    assert!(ns.is_a("ahu", "entity"));
    assert!(!ns.is_a("ahu", "point"));

    // Valid AHU entity (has all mandatory markers).
    let mut valid_ahu = HDict::new();
    valid_ahu.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    valid_ahu.set("ahu", Kind::Marker);
    valid_ahu.set("equip", Kind::Marker);
    valid_ahu.set("airHandlingEquip", Kind::Marker);
    assert!(ns.fits(&valid_ahu, "ahu"));

    // Invalid AHU (missing equip marker).
    let mut invalid_ahu = HDict::new();
    invalid_ahu.set("id", Kind::Ref(HRef::from_val("ahu-2")));
    invalid_ahu.set("ahu", Kind::Marker);
    assert!(!ns.fits(&invalid_ahu, "ahu"));

    // Explain why it does not fit.
    let issues = ns.fits_explain(&invalid_ahu, "ahu");
    assert!(!issues.is_empty());

    // Build a graph with namespace and validate.
    let mut graph = EntityGraph::with_namespace(ns);

    let mut site = HDict::new();
    site.set("id", Kind::Ref(HRef::from_val("site-1")));
    site.set("site", Kind::Marker);
    graph.add(site).unwrap();

    graph.add(valid_ahu).unwrap();
    graph.add(invalid_ahu).unwrap();

    let validation_issues = graph.validate();
    let missing_marker_issues: Vec<_> = validation_issues
        .iter()
        .filter(|i| i.issue_type == "missing_marker")
        .collect();
    assert!(
        !missing_marker_issues.is_empty(),
        "Should find missing marker issues for invalid ahu"
    );
}
