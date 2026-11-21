//! Independently authored v1 payload vectors and real graph/H4 consumers.
use std::collections::{HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use haystack_core::codecs::{CodecError, codec_for, typed};
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::filter::{CmpOp, FilterNode, Path, matches};
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::kinds::{
    Float, H4Projection, HRef, Kind, NominalScalar, Number, ProjectionPolicy, ProjectionReason,
    ValuePathSegment,
};

fn scalar(json: &str) -> Result<Kind, typed::TypedPayloadError> {
    typed::decode(format!(r#"{{"version":1,"value":{json}}}"#).as_bytes())
}
fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
fn fixture() -> HDict {
    let Kind::Dict(dict) = typed::decode(include_bytes!("fixtures/typed-v1-entity.json")).unwrap()
    else {
        panic!("dict fixture");
    };
    *dict
}
fn nominal(catalog: &str) -> Kind {
    Kind::Nominal(NominalScalar::new("example::Serial", catalog, "sha256:abc", "000042").unwrap())
}

#[test]
fn independent_fixture_reaches_shared_graph_read_without_type_guessing() {
    let dict = fixture();
    assert_eq!(dict.get("int"), Some(&Kind::Int(42)));
    assert_eq!(
        dict.get("float"),
        Some(&Kind::Float(Float::from_bits(0x4045000000000000)))
    );
    assert_eq!(
        dict.get("number"),
        Some(&Kind::Number(Number::new(42.0, Some("kW".into()))))
    );
    assert_eq!(
        dict.get("exactInt"),
        Some(&Kind::Int(9_007_199_254_740_993))
    );
    assert_eq!(dict.get("buf"), Some(&Kind::Buf(vec![0, 255, 16, 128])));
    assert_eq!(dict.get("nominal"), Some(&nominal("catalog-A")));
    assert_eq!(dict.get("none"), Some(&Kind::None));
    assert_eq!(dict.get("null"), Some(&Kind::Null));
    assert!(dict.missing("missing"));
    assert_eq!(dict.id().unwrap().dis.as_deref(), Some("Typed entity"));
    let graph = SharedGraph::new(EntityGraph::new());
    graph.add(dict.clone()).unwrap();
    let read = graph.get("typed-1").unwrap();
    assert_eq!(read, dict);
    let grid = graph
        .read_filter("int and none and null and not missing", 0)
        .unwrap();
    assert_eq!(grid.rows, vec![dict.clone()]);
    let source = Kind::Dict(Box::new(read));
    assert!(matches!(
        source.project_h4(),
        H4Projection::Unsupported { .. }
    ));
    assert!(
        source
            .project_h4()
            .into_value(ProjectionPolicy::AllowLoss)
            .is_err()
    );
    assert_eq!(graph.get("typed-1").unwrap(), dict);
    assert_eq!(
        typed::encode(&source).unwrap(),
        include_bytes!("fixtures/typed-v1-entity.json").trim_ascii_end()
    );
}

#[test]
fn independent_integer_vectors_cover_range_and_binary64_neighbors() {
    for value in [
        i64::MIN,
        -9_007_199_254_740_993,
        -9_007_199_254_740_992,
        -9_007_199_254_740_991,
        0,
        9_007_199_254_740_991,
        9_007_199_254_740_992,
        9_007_199_254_740_993,
        i64::MAX,
    ] {
        let decoded = scalar(&format!(r#"{{"kind":"int","value":"{value}"}}"#)).unwrap();
        assert_eq!(decoded, Kind::Int(value));
        assert_eq!(
            typed::decode(&typed::encode(&decoded).unwrap()).unwrap(),
            decoded
        );
    }
    for value in [
        "9223372036854775808",
        "-9223372036854775809",
        "1.0",
        "+1",
        "01",
        "-0",
        "",
        " 1",
        "1e2",
    ] {
        assert!(
            scalar(&format!(r#"{{"kind":"int","value":"{value}"}}"#)).is_err(),
            "{value}"
        );
    }
    assert!(scalar(r#"{"kind":"int","value":1}"#).is_err());
}

#[test]
fn independent_float_vectors_preserve_every_representative_bit_pattern() {
    // Hex literals are independent IEEE-754 encodings, not encoder output.
    for (text, expected) in [
        ("0000000000000000", 0_u64),
        ("8000000000000000", 1 << 63),
        ("3ff0000000000000", 0x3ff0000000000000),
        ("7ff0000000000000", 0x7ff0000000000000),
        ("fff0000000000000", 0xfff0000000000000),
        ("7ff8000000000001", 0x7ff8000000000001),
        ("7ff8000000000002", 0x7ff8000000000002),
        ("7ff0000000000001", 0x7ff0000000000001),
        ("0000000000000001", 1),
    ] {
        let decoded = scalar(&format!(r#"{{"kind":"float","bits":"{text}"}}"#)).unwrap();
        assert_eq!(decoded, Kind::Float(Float::from_bits(expected)));
        assert_eq!(
            typed::decode(&typed::encode(&decoded).unwrap()).unwrap(),
            decoded
        );
    }
    for bits in [
        "0",
        "00000000000000000",
        "7FF8000000000001",
        "gg00000000000000",
    ] {
        assert!(scalar(&format!(r#"{{"kind":"float","bits":"{bits}"}}"#)).is_err());
    }
}

#[test]
fn number_retains_exact_unit_and_float_bits_without_reclassification() {
    for unit in [None, Some(""), Some("kW"), Some("kilowatt"), Some(" kW ")] {
        let value = Kind::Number(Number::new(
            f64::from_bits(0xfff800000000002a),
            unit.map(str::to_owned),
        ));
        assert_eq!(
            typed::decode(&typed::encode(&value).unwrap()).unwrap(),
            value
        );
        assert!(matches!(value.project_h4(), H4Projection::Exact(_)));
    }
    assert_ne!(Kind::Number(Number::unitless(1.0)), Kind::Int(1));
    assert_ne!(
        Kind::Number(Number::unitless(1.0)),
        Kind::Float(Float::new(1.0))
    );
    assert!(matches!(
        codec_for("text/zinc").unwrap().decode_scalar("42").unwrap(),
        Kind::Number(_)
    ));
}

#[test]
fn representation_identity_obeys_eq_and_hash_laws_including_containers() {
    let values = vec![
        Kind::Int(1),
        Kind::Float(Float::new(1.0)),
        Kind::Number(Number::unitless(1.0)),
        Kind::Null,
        Kind::None,
        Kind::Marker,
        Kind::NA,
        Kind::Remove,
        Kind::Float(Float::new(0.0)),
        Kind::Float(Float::new(-0.0)),
        Kind::Float(Float::from_bits(0x7ff8000000000001)),
        Kind::Float(Float::from_bits(0x7ff8000000000002)),
        nominal("catalog-A"),
        nominal("catalog-B"),
    ];
    assert_eq!(
        values.iter().cloned().collect::<HashSet<_>>().len(),
        values.len()
    );
    for value in &values {
        assert_eq!(value, value);
        assert_eq!(hash(value), hash(&value.clone()));
    }
    let mut a = HDict::new();
    let mut b = HDict::new();
    for (i, value) in values.iter().enumerate() {
        a.set(format!("v{i}"), value.clone());
    }
    for (i, value) in values.iter().enumerate().rev() {
        b.set(format!("v{i}"), value.clone());
    }
    assert_eq!(a, b);
    assert_eq!(hash(&a), hash(&b));
    let grids =
        [a, b].map(|row| Kind::Grid(Box::new(HGrid::from_parts(HDict::new(), vec![], vec![row]))));
    assert_eq!(grids[0], grids[1]);
    assert_eq!(hash(&grids[0]), hash(&grids[1]));
}

#[test]
fn nominal_identity_keeps_spec_catalog_revision_and_exact_text() {
    let make =
        |spec, catalog, revision, text| NominalScalar::new(spec, catalog, revision, text).unwrap();
    let value = make("example::Serial", "A", "r1", "00042");
    for other in [
        make("example::Other", "A", "r1", "00042"),
        make("example::Serial", "B", "r1", "00042"),
        make("example::Serial", "A", "r2", "00042"),
        make("example::Serial", "A", "r1", "42"),
    ] {
        assert_ne!(value, other);
    }
    for spec in [
        "Serial",
        "::Serial",
        "example::",
        "example::Foo::Bar",
        " example::Serial",
    ] {
        assert!(NominalScalar::new(spec, "A", "r1", "x").is_err());
    }
    assert!(NominalScalar::new("example::Serial", "", "r1", "x").is_err());
    assert!(NominalScalar::new("example::Serial", "A", "", "x").is_err());
}

#[test]
fn projection_reports_type_and_precision_loss_independently() {
    for (value, precision_loss) in [
        (0, false),
        (9_007_199_254_740_992, false),
        (9_007_199_254_740_993, true),
        (-9_007_199_254_740_993, true),
        (i64::MIN, false),
        (i64::MAX, true),
    ] {
        let source = Kind::Int(value);
        let H4Projection::Lossy {
            value: projected,
            issues,
        } = source.project_h4()
        else {
            panic!("lossy int");
        };
        assert_eq!(issues[0].reason, ProjectionReason::IntTypeErased);
        assert_eq!(
            issues
                .iter()
                .any(|i| i.reason == ProjectionReason::IntPrecisionLost),
            precision_loss
        );
        assert!(
            source
                .project_h4()
                .into_value(ProjectionPolicy::Strict)
                .is_err()
        );
        assert_eq!(
            source
                .project_h4()
                .into_value(ProjectionPolicy::AllowLoss)
                .unwrap(),
            projected
        );
        for mime in [
            "text/zinc",
            "application/json",
            "application/json;v=3",
            "text/trio",
            "text/csv",
        ] {
            assert!(codec_for(mime).unwrap().encode_scalar(&projected).is_ok());
        }
    }
    assert!(matches!(
        Kind::Float(Float::new(1.0)).project_h4(),
        H4Projection::Lossy { .. }
    ));
    assert_eq!(
        Kind::None
            .project_h4()
            .into_value(ProjectionPolicy::AllowLoss)
            .unwrap(),
        Kind::Null
    );
    for value in [Kind::Null, Kind::Marker, Kind::NA, Kind::Remove] {
        assert!(matches!(value.project_h4(), H4Projection::Exact(_)));
    }
}

#[test]
fn unsupported_nested_projection_reports_original_path_and_is_atomic() {
    let mut dict = HDict::new();
    dict.set(
        "payload",
        Kind::List(vec![Kind::Int(1), nominal("A"), Kind::Buf(vec![1])]),
    );
    let source = Kind::Dict(Box::new(dict));
    let original = source.clone();
    let H4Projection::Unsupported { issues } = source.project_h4() else {
        panic!("unsupported");
    };
    assert_eq!(issues.len(), 3);
    assert_eq!(
        issues[1].path,
        vec![
            ValuePathSegment::Tag("payload".into()),
            ValuePathSegment::Item(1)
        ]
    );
    assert_eq!(issues[1].reason, ProjectionReason::NominalUnsupported);
    assert!(
        source
            .project_h4()
            .into_value(ProjectionPolicy::AllowLoss)
            .is_err()
    );
    assert_eq!(source, original);
}

#[test]
fn h4_encoders_reject_every_rich_variant_even_in_discarded_grid_locations() {
    for value in [
        Kind::Int(1),
        Kind::Float(Float::new(1.0)),
        Kind::None,
        Kind::Buf(vec![]),
        nominal("A"),
    ] {
        let mut nested = HDict::new();
        nested.set("nested", Kind::List(vec![value.clone()]));
        for mime in [
            "text/zinc",
            "application/json",
            "application/json;v=3",
            "text/trio",
            "text/csv",
        ] {
            let codec = codec_for(mime).unwrap();
            assert!(matches!(
                codec.encode_scalar(&value),
                Err(CodecError::Unprojected { .. })
            ));
            assert!(matches!(
                codec.encode_scalar(&Kind::Dict(Box::new(nested.clone()))),
                Err(CodecError::Unprojected { .. })
            ));
            for grid in [
                HGrid::from_parts(nested.clone(), vec![], vec![]),
                HGrid::from_parts(
                    HDict::new(),
                    vec![HCol::with_meta("v", nested.clone())],
                    vec![],
                ),
                HGrid::from_parts(
                    HDict::new(),
                    vec![HCol::new("unrelated")],
                    vec![nested.clone()],
                ),
            ] {
                assert!(
                    matches!(
                        codec.encode_grid(&grid),
                        Err(CodecError::Unprojected { .. })
                    ),
                    "{mime}"
                );
                assert!(
                    matches!(
                        codec.encode_grid_header(&grid),
                        Err(CodecError::Unprojected { .. })
                    ),
                    "{mime}"
                );
            }
            assert!(codec.encode_grid_row(&[], &nested).is_err(), "{mime}");
            assert!(
                codec
                    .encode_grid_row(&[HCol::with_meta("v", nested.clone())], &HDict::new())
                    .is_err(),
                "{mime}"
            );
        }
    }
}

#[test]
fn semantic_exactness_does_not_claim_zinc_wire_fidelity() {
    let mut null_row = HDict::new();
    null_row.set("v", Kind::Null);
    let grid = HGrid::from_parts(
        HDict::new(),
        vec![HCol::new("v")],
        vec![HDict::new(), null_row],
    );
    assert!(matches!(
        Kind::Grid(Box::new(grid.clone())).project_h4(),
        H4Projection::Exact(_)
    ));
    let zinc = codec_for("text/zinc").unwrap();
    let decoded = zinc.decode_grid(&zinc.encode_grid(&grid).unwrap()).unwrap();
    assert!(decoded.rows[0].missing("v"));
    assert!(decoded.rows[1].missing("v"));
    assert_ne!(decoded, grid);
    let nan = Kind::Number(Number::unitless(f64::from_bits(0x7ff8000000000042)));
    assert!(matches!(nan.project_h4(), H4Projection::Exact(_)));
    assert_ne!(
        zinc.decode_scalar(&zinc.encode_scalar(&nan).unwrap())
            .unwrap(),
        nan
    );
}

#[test]
fn nested_payload_preserves_grid_order_metadata_null_and_missing() {
    let input = br#"{"version":1,"value":{"kind":"grid","meta":{"n":{"kind":"none"}},"cols":[{"name":"b","meta":{"unit":{"kind":"str","value":"kW"}}},{"name":"a","meta":{}}],"rows":[{"a":{"kind":"null"}},{"b":{"kind":"dict","tags":{"x":{"kind":"list","items":[{"kind":"int","value":"-9223372036854775808"},{"kind":"buf","base64":"AA=="}]}}}}]}}"#;
    let value = typed::decode(input).unwrap();
    let Kind::Grid(grid) = &value else {
        panic!("grid");
    };
    assert_eq!(
        grid.cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        vec!["b", "a"]
    );
    assert_eq!(grid.rows[0].get("a"), Some(&Kind::Null));
    assert!(grid.rows[0].missing("b"));
    assert!(grid.rows[1].missing("a"));
    assert_eq!(grid.meta.get("n"), Some(&Kind::None));
    assert_eq!(
        typed::decode(&typed::encode(&value).unwrap()).unwrap(),
        value
    );
}

#[test]
fn typed_payload_rejects_duplicate_fields_tags_and_unknown_shapes() {
    for input in [
        r#"{"version":1,"version":1,"value":{"kind":"null"}}"#,
        r#"{"version":1,"value":{"kind":"int","kind":"int","value":"1"}}"#,
        r#"{"version":1,"value":{"kind":"dict","tags":{"a":{"kind":"null"},"\u0061":{"kind":"none"}}}}"#,
        r#"{"version":1,"value":{"kind":"null","extra":0}}"#,
        r#"{"version":1,"value":{"kind":"unknown"}}"#,
        r#"{"version":1,"extra":0,"value":{"kind":"null"}}"#,
        r#"{"version":1,"value":{"kind":"float","bits":"0000000000000000","value":"0"}}"#,
        r#"{"version":1,"value":{"kind":"grid","meta":{},"cols":[{"name":"a","meta":{}},{"name":"a","meta":{}}],"rows":[]}}"#,
    ] {
        assert!(typed::decode(input.as_bytes()).is_err(), "{input}");
    }
    assert!(matches!(
        typed::decode(br#"{"version":2,"value":{"kind":"future"}}"#),
        Err(typed::TypedPayloadError::UnsupportedVersion(2))
    ));
    assert!(typed::decode(br#"{"version":1,"value":{"kind":"null"}} false"#).is_err());
}

#[test]
fn typed_payload_buf_is_canonical_and_never_a_string_fallback() {
    assert_eq!(
        scalar(r#"{"kind":"buf","base64":"AP8QgA=="}"#).unwrap(),
        Kind::Buf(vec![0, 255, 16, 128])
    );
    for text in ["AA", "AB==", "AA==\n", "____"] {
        let json = serde_json::json!({"kind":"buf", "base64":text}).to_string();
        assert!(scalar(&json).is_err(), "{text}");
    }
}

#[test]
fn typed_decode_enforces_byte_depth_and_node_limits_before_return() {
    let input = br#"{"version":1,"value":{"kind":"null"}}"#;
    let limits = typed::PayloadLimits {
        max_bytes: input.len(),
        max_depth: 2,
        max_nodes: 4,
    };
    assert_eq!(
        typed::decode_with_limits(input, limits).unwrap(),
        Kind::Null
    );
    assert!(matches!(
        typed::decode_with_limits(
            input,
            typed::PayloadLimits {
                max_bytes: input.len() - 1,
                ..limits
            }
        ),
        Err(typed::TypedPayloadError::ByteLimit { .. })
    ));
    assert!(
        typed::decode_with_limits(
            input,
            typed::PayloadLimits {
                max_depth: 1,
                ..limits
            }
        )
        .unwrap_err()
        .to_string()
        .contains("depth")
    );
    assert!(
        typed::decode_with_limits(
            input,
            typed::PayloadLimits {
                max_nodes: 3,
                ..limits
            }
        )
        .unwrap_err()
        .to_string()
        .contains("node")
    );
    assert!(matches!(
        typed::decode_with_limits(
            input,
            typed::PayloadLimits {
                max_depth: 65,
                ..limits
            }
        ),
        Err(typed::TypedPayloadError::InvalidLimits)
    ));
    let deep = format!("{}0{}", "[".repeat(1000), "]".repeat(1000));
    assert!(
        typed::decode(deep.as_bytes())
            .unwrap_err()
            .to_string()
            .contains("depth")
    );
}

fn compare(actual: Kind, op: CmpOp, expected: Kind) -> bool {
    let mut dict = HDict::new();
    dict.set("v", actual);
    matches(
        &FilterNode::Cmp {
            path: Path::single("v"),
            op,
            val: expected,
        },
        &dict,
        None,
    )
}
#[test]
fn query_numeric_comparison_is_separate_from_bit_identity() {
    assert!(compare(
        Kind::Int(9_007_199_254_740_993),
        CmpOp::Gt,
        Kind::Int(9_007_199_254_740_992)
    ));
    assert!(compare(Kind::Int(i64::MAX), CmpOp::Gt, Kind::Int(i64::MIN)));
    assert!(!compare(
        Kind::Int(1),
        CmpOp::Eq,
        Kind::Float(Float::new(1.0))
    ));
    assert!(!compare(
        Kind::Int(i64::MAX),
        CmpOp::Lt,
        Kind::Float(Float::new(f64::INFINITY))
    ));
    let pos = Kind::Float(Float::new(0.0));
    let neg = Kind::Float(Float::new(-0.0));
    assert_ne!(pos, neg);
    assert!(compare(pos, CmpOp::Eq, neg));
    let nan = Kind::Float(Float::from_bits(0x7ff8000000000001));
    assert_eq!(nan, nan);
    assert!(!compare(nan.clone(), CmpOp::Eq, nan.clone()));
    assert!(!compare(nan.clone(), CmpOp::Le, nan));
    // Existing Number filter identity remains bit-sensitive.
    assert!(!compare(
        Kind::Number(Number::unitless(0.0)),
        CmpOp::Eq,
        Kind::Number(Number::unitless(-0.0))
    ));
}

#[test]
fn indexed_h4_queries_remain_supersets_with_rich_values_present() {
    let mut graph = EntityGraph::new();
    for (i, value) in [
        Kind::Int(1),
        Kind::Float(Float::new(1.0)),
        Kind::Number(Number::unitless(1.0)),
        Kind::Number(Number::new(1.0, Some("kW".into()))),
        Kind::None,
        Kind::Null,
        Kind::Number(Number::unitless(-0.0)),
        Kind::Number(Number::unitless(2.0)),
        nominal("A"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut dict = HDict::new();
        dict.set("id", Kind::Ref(HRef::from_val(format!("e{i}"))));
        dict.set("v", value);
        graph.add(dict).unwrap();
    }
    let filters = [
        "v == 1",
        "v != 1",
        "v < 1",
        "v <= 1",
        "v > 1",
        "v >= 1",
        "v == 0",
        "v >= 0",
        "v and not missing",
        "v != 1 and v",
    ];
    let ids = |graph: &EntityGraph, filter: &str| {
        let mut ids: Vec<_> = graph
            .read_all(filter, 0)
            .unwrap()
            .iter()
            .map(|d| d.id().unwrap().val.clone())
            .collect();
        ids.sort();
        ids
    };
    let expected: Vec<_> = filters.iter().map(|f| ids(&graph, f)).collect();
    assert_eq!(expected[0], vec!["e2"]);
    assert!(expected[1].contains(&"e0".into()));
    assert!(expected[1].contains(&"e1".into()));
    graph.index_field("v");
    for (filter, expected) in filters.into_iter().zip(expected) {
        assert_eq!(ids(&graph, filter), expected, "{filter}");
    }
}

#[test]
fn existing_h4_scalars_have_independent_typed_payload_shapes() {
    use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};
    use haystack_core::kinds::{Coord, HDateTime, Symbol, Uri, XStr};
    let examples = [
        (r#"{"kind":"bool","value":true}"#, Kind::Bool(true)),
        (r#"{"kind":"str","value":"42"}"#, Kind::Str("42".into())),
        (
            r#"{"kind":"uri","value":"https://example.test/a"}"#,
            Kind::Uri(Uri::new("https://example.test/a")),
        ),
        (
            r#"{"kind":"symbol","value":"ph::site"}"#,
            Kind::Symbol(Symbol::new("ph::site")),
        ),
        (
            r#"{"kind":"date","value":"2024-02-29"}"#,
            Kind::Date(NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()),
        ),
        (
            r#"{"kind":"time","seconds":3723,"nanos":123456789}"#,
            Kind::Time(NaiveTime::from_hms_nano_opt(1, 2, 3, 123456789).unwrap()),
        ),
        (
            r#"{"kind":"dateTime","seconds":"0","nanos":123456789,"offset":1234,"timezone":"source-zone"}"#,
            Kind::DateTime(HDateTime::new(
                FixedOffset::east_opt(1234)
                    .unwrap()
                    .timestamp_opt(0, 123456789)
                    .unwrap(),
                "source-zone",
            )),
        ),
        (
            r#"{"kind":"coord","lat":"8000000000000000","lng":"3ff0000000000000"}"#,
            Kind::Coord(Coord::new(-0.0, 1.0)),
        ),
        (
            r#"{"kind":"xstr","name":"Color","value":"red"}"#,
            Kind::XStr(XStr::new("Color", "red")),
        ),
        (
            r#"{"kind":"ref","value":"r","display":""}"#,
            Kind::Ref(HRef::new("r", Some("".into()))),
        ),
    ];
    for (wire, value) in examples {
        assert_eq!(scalar(wire).unwrap(), value);
        assert_eq!(
            typed::decode(&typed::encode(&value).unwrap()).unwrap(),
            value
        );
        if let Kind::Ref(r) = value {
            let Kind::Ref(decoded) = scalar(wire).unwrap() else {
                panic!("ref");
            };
            assert_eq!(decoded.dis, r.dis);
        }
    }
}

#[test]
fn xeto_export_rejects_nested_rich_defaults_without_mutating_spec() {
    use haystack_core::xeto::{Slot, Spec, export};
    let mut spec = Spec::new("example::Thing", "example", "Thing");
    let inner = Slot {
        name: "leaf".into(),
        type_ref: Some("Obj".into()),
        meta: Default::default(),
        default: Some(Kind::List(vec![Kind::Int(42)])),
        is_marker: false,
        is_query: false,
        children: vec![],
    };
    spec.slots.push(Slot {
        name: "outer".into(),
        type_ref: Some("Obj".into()),
        meta: Default::default(),
        default: None,
        is_marker: false,
        is_query: false,
        children: vec![inner],
    });
    assert!(matches!(
        export::export_spec(&spec),
        Err(CodecError::Unprojected { .. })
    ));
    assert!(export::export_lib("example", "1", "", &[], &[&spec]).is_err());
    assert_eq!(
        spec.slots[0].children[0].default,
        Some(Kind::List(vec![Kind::Int(42)]))
    );
}

#[test]
fn deep_projection_fails_boundedly_without_rewriting_source() {
    let mut value = Kind::Buf(vec![42]);
    for _ in 0..70 {
        value = Kind::List(vec![value]);
    }
    let original = value.clone();
    let H4Projection::Unsupported { issues } = value.project_h4() else {
        panic!("depth rejection");
    };
    assert_eq!(
        issues.last().unwrap().reason,
        ProjectionReason::NestingLimit
    );
    assert_eq!(value, original);
    assert!(
        codec_for("text/zinc")
            .unwrap()
            .encode_scalar(&value)
            .is_err()
    );
    assert!(typed::encode(&value).is_err());
}

#[test]
fn typed_time_preserves_non_minute_leap_representation_and_wire_fields() {
    use chrono::{NaiveTime, Timelike};
    let exotic = NaiveTime::from_hms_opt(23, 56, 4)
        .unwrap()
        .with_nanosecond(1_333_333_333)
        .unwrap();
    let following = NaiveTime::from_hms_nano_opt(23, 56, 5, 333_333_333).unwrap();
    assert_eq!(exotic.to_string(), following.to_string());
    assert_ne!(exotic, following);
    let value = Kind::Time(exotic);
    let decoded = typed::decode(&typed::encode(&value).unwrap()).unwrap();
    assert_eq!(decoded, value);
    assert_ne!(decoded, Kind::Time(following));

    let fixture = br#"{"version":1,"value":{"kind":"time","seconds":86164,"nanos":1333333333}}"#;
    assert_eq!(typed::decode(fixture).unwrap(), value);
    assert_eq!(typed::encode(&value).unwrap(), fixture);
    let Kind::Time(decoded) = decoded else {
        panic!("time fixture");
    };
    assert_eq!(decoded.num_seconds_from_midnight(), 86164);
    assert_eq!(decoded.nanosecond(), 1_333_333_333);
}

#[test]
fn typed_datetime_preserves_setter_admitted_non_minute_leap() {
    use chrono::{DateTime, FixedOffset, Timelike};
    use haystack_core::kinds::HDateTime;
    let dt = DateTime::from_timestamp(30, 0)
        .unwrap()
        .with_nanosecond(1_000_000_000)
        .unwrap()
        .with_timezone(&FixedOffset::east_opt(0).unwrap());
    let value = Kind::DateTime(HDateTime::new(dt, "UTC-source"));
    let fixture = br#"{"version":1,"value":{"kind":"dateTime","seconds":"30","nanos":1000000000,"offset":0,"timezone":"UTC-source"}}"#;
    assert_eq!(typed::encode(&value).unwrap(), fixture);
    let decoded = typed::decode(fixture).unwrap();
    assert_eq!(decoded, value);
    let Kind::DateTime(decoded) = decoded else {
        panic!("datetime fixture");
    };
    assert_eq!(decoded.dt.timestamp(), 30);
    assert_eq!(decoded.dt.nanosecond(), 1_000_000_000);
    assert_eq!(decoded.dt.offset().local_minus_utc(), 0);
    assert_eq!(decoded.tz_name, "UTC-source");
}

#[test]
fn typed_time_and_datetime_preserve_minute_leaps_and_second_offsets() {
    use chrono::{DateTime, FixedOffset, NaiveTime, Timelike};
    use haystack_core::kinds::HDateTime;
    let ordinary_leap = NaiveTime::from_hms_nano_opt(23, 59, 59, 1_999_999_999).unwrap();
    let fixture = br#"{"version":1,"value":{"kind":"time","seconds":86399,"nanos":1999999999}}"#;
    assert_eq!(typed::decode(fixture).unwrap(), Kind::Time(ordinary_leap));
    assert_eq!(typed::encode(&Kind::Time(ordinary_leap)).unwrap(), fixture);

    let utc_leap = DateTime::from_timestamp(59, 1_250_000_000).unwrap();
    // Historical fixed offsets can include seconds. No timezone lookup is used.
    let offset = FixedOffset::east_opt(1234).unwrap();
    let local = utc_leap.with_timezone(&offset);
    let local_time = local.time();
    assert_eq!(local_time.num_seconds_from_midnight(), 1293);
    assert_eq!(local_time.nanosecond(), 1_250_000_000);
    let time_fixture =
        br#"{"version":1,"value":{"kind":"time","seconds":1293,"nanos":1250000000}}"#;
    assert_eq!(typed::decode(time_fixture).unwrap(), Kind::Time(local_time));
    assert_eq!(
        typed::encode(&Kind::Time(local_time)).unwrap(),
        time_fixture
    );

    let value = Kind::DateTime(HDateTime::new(local, "historic-offset-source"));
    let dt_fixture = br#"{"version":1,"value":{"kind":"dateTime","seconds":"59","nanos":1250000000,"offset":1234,"timezone":"historic-offset-source"}}"#;
    assert_eq!(typed::encode(&value).unwrap(), dt_fixture);
    let decoded = typed::decode(dt_fixture).unwrap();
    assert_eq!(decoded, value);
    let Kind::DateTime(decoded) = decoded else {
        panic!("datetime fixture");
    };
    assert_eq!(decoded.dt.timestamp_subsec_nanos(), 1_250_000_000);
    assert_eq!(decoded.dt.offset().local_minus_utc(), 1234);
    assert_eq!(decoded.tz_name, "historic-offset-source");
    assert_eq!(decoded.dt.time(), local_time);
}

#[test]
fn typed_temporal_fields_reject_out_of_range_values_and_old_time_text() {
    for wire in [
        r#"{"kind":"time","seconds":86400,"nanos":0}"#,
        r#"{"kind":"time","seconds":-1,"nanos":0}"#,
        r#"{"kind":"time","seconds":30,"nanos":2000000000}"#,
        r#"{"kind":"time","seconds":30,"nanos":4294967295}"#,
        r#"{"kind":"time","seconds":30,"nanos":-1}"#,
        r#"{"kind":"time","value":"23:56:05.333333333"}"#,
        r#"{"kind":"dateTime","seconds":"30","nanos":2000000000,"offset":0,"timezone":"UTC"}"#,
        r#"{"kind":"dateTime","seconds":"59","nanos":4294967295,"offset":0,"timezone":"UTC"}"#,
        r#"{"kind":"dateTime","seconds":"9223372036854775807","nanos":0,"offset":0,"timezone":"UTC"}"#,
        r#"{"kind":"dateTime","seconds":"0","nanos":0,"offset":86400,"timezone":"UTC"}"#,
    ] {
        assert!(scalar(wire).is_err(), "{wire}");
    }
}

#[test]
fn typed_datetime_preserves_maximum_date_constructor_admitted_leap() {
    use chrono::{DateTime, FixedOffset, Timelike, Utc};
    use haystack_core::kinds::HDateTime;
    const MAX_SECONDS: i64 = 8_210_266_876_799;
    assert_eq!(DateTime::<Utc>::MAX_UTC.timestamp(), MAX_SECONDS);
    let utc = DateTime::from_timestamp(MAX_SECONDS, 1_500_000_000).unwrap();
    let dt = utc.with_timezone(&FixedOffset::east_opt(1234).unwrap());
    let value = Kind::DateTime(HDateTime::new(dt, "maximum-date-source"));
    let fixture = br#"{"version":1,"value":{"kind":"dateTime","seconds":"8210266876799","nanos":1500000000,"offset":1234,"timezone":"maximum-date-source"}}"#;
    assert_eq!(typed::encode(&value).unwrap(), fixture);
    let decoded = typed::decode(fixture).unwrap();
    assert_eq!(decoded, value);
    let Kind::DateTime(decoded) = decoded else {
        panic!("datetime fixture");
    };
    assert_eq!(decoded.dt.timestamp(), MAX_SECONDS);
    assert_eq!(decoded.dt.nanosecond(), 1_500_000_000);
    assert_eq!(decoded.dt.offset().local_minus_utc(), 1234);
    assert_eq!(decoded.tz_name, "maximum-date-source");
}

#[test]
fn typed_datetime_preserves_minimum_and_maximum_date_fields() {
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, Timelike, Utc};
    use haystack_core::kinds::HDateTime;
    const MIN_SECONDS: i64 = -8_334_601_228_800;
    const MAX_SECONDS: i64 = 8_210_266_876_799;
    assert_eq!(DateTime::<Utc>::MIN_UTC.timestamp(), MIN_SECONDS);
    assert_eq!(DateTime::<Utc>::MAX_UTC.timestamp(), MAX_SECONDS);
    // Independent calendar construction covers both ends of the date range.
    // Include ordinary and non-minute leap representations, and fixed offsets
    // whose local date would cross a boundary without changing the UTC fields.
    for (date, hour, minute, second, seconds) in [
        (NaiveDate::MIN, 0, 0, 0, MIN_SECONDS),
        (NaiveDate::MIN, 0, 0, 30, MIN_SECONDS + 30),
        (NaiveDate::MIN, 0, 0, 59, MIN_SECONDS + 59),
        (NaiveDate::MAX, 23, 59, 0, MAX_SECONDS - 59),
        (NaiveDate::MAX, 23, 59, 30, MAX_SECONDS - 29),
        (NaiveDate::MAX, 23, 59, 59, MAX_SECONDS),
    ] {
        for nanos in [
            0,
            1,
            999_999_999,
            1_000_000_000,
            1_500_000_000,
            1_999_999_999,
        ] {
            let time = NaiveTime::from_hms_opt(hour, minute, second)
                .unwrap()
                .with_nanosecond(nanos)
                .unwrap();
            let utc = date.and_time(time).and_utc();
            assert_eq!(utc.timestamp(), seconds);
            for offset in [-86_399, -1234, 0, 1234, 86_399] {
                let dt = utc.with_timezone(&FixedOffset::east_opt(offset).unwrap());
                let value = Kind::DateTime(HDateTime::new(dt, "date-boundary-source"));
                let fixture = format!(
                    r#"{{"version":1,"value":{{"kind":"dateTime","seconds":"{seconds}","nanos":{nanos},"offset":{offset},"timezone":"date-boundary-source"}}}}"#
                );
                assert_eq!(typed::encode(&value).unwrap(), fixture.as_bytes());
                let decoded = typed::decode(fixture.as_bytes()).unwrap();
                assert_eq!(decoded, value);
                let Kind::DateTime(decoded) = decoded else {
                    panic!("datetime fixture");
                };
                assert_eq!(decoded.dt.timestamp(), seconds);
                assert_eq!(decoded.dt.nanosecond(), nanos);
                assert_eq!(decoded.dt.offset().local_minus_utc(), offset);
                assert_eq!(decoded.tz_name, "date-boundary-source");
            }
        }
    }
    for (seconds, nanos) in [
        (MIN_SECONDS - 1, 0),
        (MAX_SECONDS + 1, 0),
        (MIN_SECONDS, 2_000_000_000),
        (MAX_SECONDS, 2_000_000_000),
    ] {
        let fixture = format!(
            r#"{{"kind":"dateTime","seconds":"{seconds}","nanos":{nanos},"offset":0,"timezone":"UTC"}}"#
        );
        assert!(scalar(&fixture).is_err(), "{fixture}");
    }
}
