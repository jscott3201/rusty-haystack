// End-to-end integration test exercising the full Haystack pipeline:
// load defs -> build entities -> graph CRUD -> filter queries ->
// ref traversal -> validate -> encode/decode round-trip through
// Zinc, JSON v4, JSON v3, and Trio.

use chrono::{FixedOffset, TimeZone, Timelike};
use haystack_core::codecs::codec_for;
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::filter;
use haystack_core::graph::EntityGraph;
use haystack_core::kinds::{HDateTime, HRef, Kind, Number};
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

// ── Cross-codec DateTime conformance ──
//
// The per-codec round-trip tests encode and decode with the *same* codec, so
// they check an encoder against its own decoder rather than against a second
// implementation. DateTime is the kind most prone to drift between them —
// Niagara and SkySpark have historically disagreed on offset formatting, on
// `Z` versus `+00:00` for UTC, and on the tz-name suffix — so these walk a
// value through every ordered pair of codecs instead.

/// Codecs that preserve values across an encode/decode round trip. CSV is
/// excluded: it flattens values to display strings and cannot round-trip.
const ROUND_TRIP_CODECS: &[&str] = &[
    "text/zinc",
    "application/json",
    "application/json;v=3",
    "text/trio",
];

fn grid_with_datetime(ts: &HDateTime) -> HGrid {
    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("p1")));
    entity.set("ts", Kind::DateTime(ts.clone()));
    HGrid::from_parts(
        HDict::new(),
        vec![HCol::new("id"), HCol::new("ts")],
        vec![entity],
    )
}

/// Re-encode `grid` with `mime` and decode it back.
fn hop(grid: &HGrid, mime: &str) -> HGrid {
    let codec = codec_for(mime).unwrap_or_else(|| panic!("no codec for {mime}"));
    let encoded = codec
        .encode_grid(grid)
        .unwrap_or_else(|e| panic!("encode failed for {mime}: {e}"));
    codec
        .decode_grid(&encoded)
        .unwrap_or_else(|e| panic!("decode failed for {mime}: {e}\n--- payload ---\n{encoded}"))
}

fn datetime_of(grid: &HGrid, ctx: &str) -> HDateTime {
    let row = grid
        .rows
        .first()
        .unwrap_or_else(|| panic!("{ctx}: grid lost its only row"));
    match row.get("ts") {
        Some(Kind::DateTime(dt)) => dt.clone(),
        other => panic!("{ctx}: `ts` is {other:?}, expected a DateTime"),
    }
}

#[test]
fn datetime_survives_every_cross_codec_hop() {
    let cases: Vec<(&str, HDateTime)> = vec![
        (
            "negative offset (winter, New_York)",
            HDateTime::new(
                FixedOffset::west_opt(5 * 3600)
                    .unwrap()
                    .with_ymd_and_hms(2024, 1, 1, 8, 12, 5)
                    .unwrap(),
                "New_York",
            ),
        ),
        (
            "negative offset (summer, New_York)",
            HDateTime::new(
                FixedOffset::west_opt(4 * 3600)
                    .unwrap()
                    .with_ymd_and_hms(2024, 7, 1, 8, 12, 5)
                    .unwrap(),
                "New_York",
            ),
        ),
        (
            "positive offset (Tokyo)",
            HDateTime::new(
                FixedOffset::east_opt(9 * 3600)
                    .unwrap()
                    .with_ymd_and_hms(2024, 3, 15, 23, 45, 30)
                    .unwrap(),
                "Tokyo",
            ),
        ),
        (
            "zero offset (UTC)",
            HDateTime::new(
                FixedOffset::east_opt(0)
                    .unwrap()
                    .with_ymd_and_hms(2024, 6, 30, 0, 0, 0)
                    .unwrap(),
                "UTC",
            ),
        ),
        (
            "half-hour offset (Kolkata)",
            HDateTime::new(
                FixedOffset::east_opt(5 * 3600 + 1800)
                    .unwrap()
                    .with_ymd_and_hms(2024, 2, 29, 6, 15, 0)
                    .unwrap(),
                "Kolkata",
            ),
        ),
        (
            "fractional seconds (New_York)",
            HDateTime::new(
                FixedOffset::west_opt(5 * 3600)
                    .unwrap()
                    .with_ymd_and_hms(2024, 1, 1, 8, 12, 5)
                    .unwrap()
                    .with_nanosecond(123_000_000)
                    .unwrap(),
                "New_York",
            ),
        ),
    ];

    for (label, original) in &cases {
        for from in ROUND_TRIP_CODECS {
            for to in ROUND_TRIP_CODECS {
                let ctx = format!("{label}: {from} -> {to}");

                let first = hop(&grid_with_datetime(original), from);
                let after_first = datetime_of(&first, &ctx);
                assert_eq!(&after_first, original, "{ctx}: lost on the first hop");
                assert_eq!(
                    after_first.dt.offset().local_minus_utc(),
                    original.dt.offset().local_minus_utc(),
                    "{ctx}: UTC offset lost on the first hop"
                );

                let second = hop(&first, to);
                let after_second = datetime_of(&second, &ctx);

                // Three fields matter independently. `DateTime<FixedOffset>`
                // compares as an instant, so a codec that normalised every
                // value to +00:00 would still pass the first assertion; the
                // offset has to be compared explicitly. And the instant can
                // survive while the tz name is dropped, which still breaks a
                // consumer that renders local time.
                assert_eq!(
                    after_second.dt, original.dt,
                    "{ctx}: instant changed across codecs"
                );
                assert_eq!(
                    after_second.dt.offset().local_minus_utc(),
                    original.dt.offset().local_minus_utc(),
                    "{ctx}: UTC offset changed across codecs"
                );
                assert_eq!(
                    after_second.tz_name, original.tz_name,
                    "{ctx}: timezone name changed across codecs"
                );
            }
        }
    }
}

#[test]
fn datetime_utc_offset_spelling_is_pinned_per_codec() {
    // UTC is the case implementations most often disagree on, writing `Z` in
    // one codec and `+00:00` in another. Our four encoders must not drift
    // apart, and a round-trip assertion cannot catch drift because each codec
    // reads back whatever it wrote. So pin the wire text itself.
    let utc = HDateTime::new(
        FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2024, 6, 30, 12, 0, 0)
            .unwrap(),
        "UTC",
    );

    let expected: &[(&str, &str)] = &[
        ("text/zinc", "2024-06-30T12:00:00+00:00 UTC"),
        ("application/json;v=3", "t:2024-06-30T12:00:00+00:00 UTC"),
        ("application/json", "2024-06-30T12:00:00+00:00"),
        ("text/trio", "2024-06-30T12:00:00+00:00 UTC"),
    ];

    for (mime, spelling) in expected {
        let encoded = codec_for(mime)
            .unwrap()
            .encode_grid(&grid_with_datetime(&utc))
            .unwrap();
        assert!(
            encoded.contains(spelling),
            "{mime}: expected UTC spelled `{spelling}`\n--- payload ---\n{encoded}"
        );
    }
}

#[test]
fn utc_z_from_external_producers_decodes_everywhere() {
    // Round-trip tests can only cover what our own encoders emit, and all four
    // write UTC as `+00:00`. Niagara and SkySpark emit `Z`, so the decode side
    // needs hand-written payloads or the interop gap is invisible. JSON v3 used
    // to reject the `Z UTC` form outright: its offset scan only recognised a
    // trailing `Z`, so a following tz name pushed the whole string into the
    // datetime parser.
    let expected = FixedOffset::east_opt(0)
        .unwrap()
        .with_ymd_and_hms(2024, 6, 30, 12, 0, 0)
        .unwrap();

    // A bare `Z` carries no name; every codec must fill in UTC rather than
    // leaving it empty, so the tz name is "UTC" in all of these.
    let payloads: &[(&str, &str, &str)] = &[
        (
            "text/zinc",
            "Z with tz name",
            "ver:\"3.0\"\nts\n2024-06-30T12:00:00Z UTC\n",
        ),
        (
            "text/zinc",
            "bare Z",
            "ver:\"3.0\"\nts\n2024-06-30T12:00:00Z\n",
        ),
        (
            "application/json;v=3",
            "Z with tz name",
            "{\"meta\":{\"ver\":\"3.0\"},\"cols\":[{\"name\":\"ts\"}],\"rows\":[{\"ts\":\"t:2024-06-30T12:00:00Z UTC\"}]}",
        ),
        (
            "application/json;v=3",
            "bare Z",
            "{\"meta\":{\"ver\":\"3.0\"},\"cols\":[{\"name\":\"ts\"}],\"rows\":[{\"ts\":\"t:2024-06-30T12:00:00Z\"}]}",
        ),
        (
            "application/json",
            "Z with tz field",
            "{\"_kind\":\"grid\",\"cols\":[{\"name\":\"ts\"}],\"rows\":[{\"ts\":{\"_kind\":\"dateTime\",\"tz\":\"UTC\",\"val\":\"2024-06-30T12:00:00Z\"}}]}",
        ),
        (
            "application/json",
            "Z with no tz field",
            "{\"_kind\":\"grid\",\"cols\":[{\"name\":\"ts\"}],\"rows\":[{\"ts\":{\"_kind\":\"dateTime\",\"val\":\"2024-06-30T12:00:00Z\"}}]}",
        ),
        (
            "text/trio",
            "Z with tz name",
            "ts: 2024-06-30T12:00:00Z UTC\n",
        ),
    ];

    for (mime, shape, payload) in payloads {
        let ctx = format!("{mime} / {shape}");
        let grid = codec_for(mime)
            .unwrap_or_else(|| panic!("no codec for {mime}"))
            .decode_grid(payload)
            .unwrap_or_else(|e| panic!("{ctx}: decode failed: {e}"));
        let ts = datetime_of(&grid, &ctx);
        assert_eq!(ts.dt, expected, "{ctx}: wrong instant");
        assert_eq!(ts.tz_name, "UTC", "{ctx}: wrong timezone name");

        // Re-encoding must then agree with every other codec, so a `Z` that
        // arrived from outside survives onward conversion — name included.
        for onward in ROUND_TRIP_CODECS {
            let ctx2 = format!("{ctx} -> {onward}");
            let onward_ts = datetime_of(&hop(&grid, onward), &ctx2);
            assert_eq!(onward_ts.dt, expected, "{ctx2}: instant changed");
            assert_eq!(onward_ts.tz_name, "UTC", "{ctx2}: timezone name changed");
        }
    }
}

/// A tz name containing `Z` must not be mistaken for the UTC designator.
#[test]
fn tz_name_containing_z_is_not_read_as_utc() {
    let grid = codec_for("application/json;v=3")
        .unwrap()
        .decode_grid(
            "{\"meta\":{\"ver\":\"3.0\"},\"cols\":[{\"name\":\"ts\"}],\
             \"rows\":[{\"ts\":\"t:2024-06-30T12:00:00+01:00 Zurich\"}]}",
        )
        .expect("Zurich payload decodes");
    let ts = datetime_of(&grid, "Zurich");
    assert_eq!(ts.tz_name, "Zurich");
    assert_eq!(ts.dt.offset().local_minus_utc(), 3600);
}

/// A lowercase `t` separator must be rejected by every codec, not accepted by
/// some and silently downgraded by others.
///
/// RFC 3339 §5.6 permits `2024-06-30t12:00:00Z`, so `parse_from_rfc3339` accepts
/// it and both JSON codecs used to decode it as a DateTime. Zinc checked for an
/// uppercase `T` and, finding none, fell through to a bare `Date` — dropping the
/// time, the offset and the timezone with no error anywhere. A consumer reading
/// `ts` got a Date where a DateTime was sent, and since a Date never compares
/// equal to a DateTime under Haystack filter semantics, the damage surfaced later
/// as a `ts >=` range returning the wrong rows.
///
/// The ratified rule is strict everywhere: all four codecs reject it.
#[test]
fn lowercase_t_separator_is_rejected_by_every_codec() {
    let payloads: &[(&str, &str)] = &[
        ("text/zinc", "ver:\"3.0\"\nts\n2024-06-30t12:00:00Z\n"),
        (
            "application/json",
            "{\"_kind\":\"grid\",\"meta\":{\"ver\":\"4.0\"},\"cols\":[{\"name\":\"ts\"}],\
             \"rows\":[{\"ts\":{\"_kind\":\"dateTime\",\"val\":\"2024-06-30t12:00:00Z\"}}]}",
        ),
        (
            "application/json;v=3",
            "{\"meta\":{\"ver\":\"3.0\"},\"cols\":[{\"name\":\"ts\"}],\
             \"rows\":[{\"ts\":\"t:2024-06-30t12:00:00Z UTC\"}]}",
        ),
    ];

    for (mime, payload) in payloads {
        let result = codec_for(mime).unwrap().decode_grid(payload);
        assert!(
            result.is_err(),
            "{mime}: a lowercase 't' must be a parse error, got {:?}",
            result.map(|g| g.rows.first().and_then(|r| r.get("ts")).cloned()),
        );
    }
}

/// The uppercase form must keep working — the guard above rejects a separator,
/// not the datetime grammar.
#[test]
fn uppercase_t_separator_still_decodes_everywhere() {
    let expected = FixedOffset::east_opt(0)
        .unwrap()
        .with_ymd_and_hms(2024, 6, 30, 12, 0, 0)
        .unwrap();

    let payloads: &[(&str, &str)] = &[
        ("text/zinc", "ver:\"3.0\"\nts\n2024-06-30T12:00:00Z\n"),
        (
            "application/json",
            "{\"_kind\":\"grid\",\"meta\":{\"ver\":\"4.0\"},\"cols\":[{\"name\":\"ts\"}],\
             \"rows\":[{\"ts\":{\"_kind\":\"dateTime\",\"val\":\"2024-06-30T12:00:00Z\"}}]}",
        ),
        (
            "application/json;v=3",
            "{\"meta\":{\"ver\":\"3.0\"},\"cols\":[{\"name\":\"ts\"}],\
             \"rows\":[{\"ts\":\"t:2024-06-30T12:00:00Z UTC\"}]}",
        ),
    ];

    for (mime, payload) in payloads {
        let grid = codec_for(mime)
            .unwrap()
            .decode_grid(payload)
            .unwrap_or_else(|e| panic!("{mime}: uppercase T must still decode: {e}"));
        assert_eq!(datetime_of(&grid, mime).dt, expected, "{mime}");
    }
}

/// A bare Date is still a Date. The guard must not turn every date into an error.
#[test]
fn bare_date_still_decodes_as_date() {
    let grid = codec_for("text/zinc")
        .unwrap()
        .decode_grid("ver:\"3.0\"\nd\n2024-06-30\n")
        .expect("a bare date is valid Zinc");
    assert!(
        matches!(grid.rows[0].get("d"), Some(Kind::Date(_))),
        "expected Date, got {:?}",
        grid.rows[0].get("d")
    );
}
