//! Independent Jeto fixtures from the retained 873b9224 contract.
use haystack_core::{
    codecs::jeto::{self, Context, Limits},
    kinds::{Float, Kind, Number},
};
fn decode(text: &str, expected: Option<&str>) -> Kind {
    jeto::decode(
        text.as_bytes(),
        &Context::standard(),
        expected,
        Limits::default(),
    )
    .unwrap()
}
#[test]
fn numeric_lexemes_and_expected_types_preserve_independent_identity() {
    for (wire, expected) in [
        ("42", Kind::Int(42)),
        ("42.0", Kind::Float(Float::new(42.0))),
        ("4.2e1", Kind::Float(Float::new(42.0))),
        ("-0", Kind::Int(0)),
        ("-0.0", Kind::Float(Float::from_bits(1 << 63))),
        ("-0e0", Kind::Float(Float::from_bits(1 << 63))),
    ] {
        assert_eq!(decode(wire, None), expected, "{wire}");
    }
    assert_eq!(
        decode("42", Some("sys::Float")),
        Kind::Float(Float::new(42.0))
    );
    assert_eq!(
        decode("42", Some("sys::Number")),
        Kind::Number(Number::unitless(42.0))
    );
    assert_eq!(
        decode("9007199254740993.0", Some("sys::Int")),
        Kind::Int(9_007_199_254_740_993)
    );
    assert_eq!(decode("1e3", Some("sys::Int")), Kind::Int(1000));
    assert_eq!(decode("73", Some("sys::Bool")), Kind::Int(73));
}
#[test]
fn contextual_integer_conversion_is_checked_before_binary64_rounding() {
    for text in [
        "9223372036854775807",
        "9223372036854775807.0",
        "9.223372036854775807e18",
    ] {
        assert_eq!(decode(text, Some("sys::Int")), Kind::Int(i64::MAX));
    }
    for text in [
        "-9223372036854775808",
        "-9223372036854775808.0",
        "-9.223372036854775808e18",
    ] {
        assert_eq!(decode(text, Some("sys::Int")), Kind::Int(i64::MIN));
    }
    for text in [
        "9223372036854775808",
        "-9223372036854775809",
        "1.5",
        "1e9999",
        "1e-9999",
    ] {
        assert!(
            jeto::decode(
                text.as_bytes(),
                &Context::standard(),
                Some("sys::Int"),
                Limits::default()
            )
            .is_err(),
            "{text}"
        );
    }
}

use haystack_core::{
    codecs::{
        jeto::{Boxing, Definition, Encoding, Error, Limit},
        typed,
    },
    kinds::{HRef, NominalScalar, Uri},
};
fn exact(value: &Kind, context: &Context, expected: Option<&str>, boxing: Boxing) -> Vec<u8> {
    jeto::encode(value, context, expected, boxing, Limits::default())
        .unwrap()
        .into_exact()
        .unwrap()
}
fn identity(a: &Kind, b: &Kind) {
    assert_eq!(typed::encode(a).unwrap(), typed::encode(b).unwrap());
}
#[test]
fn boxed_scalars_have_string_values_and_explicit_type_precedence() {
    let context = Context::standard();
    for (wire, native) in [
        (r#"{"spec":"sys::Bool","val":"true"}"#, Kind::Bool(true)),
        (r#"{"spec":"sys::Int","val":"42"}"#, Kind::Int(42)),
        (
            r#"{"spec":"sys::Float","val":"42.0"}"#,
            Kind::Float(Float::new(42.0)),
        ),
        (
            r#"{"spec":"sys::Number","val":"72.5°F"}"#,
            Kind::Number(Number::new(72.5, Some("°F".into()))),
        ),
        (r#"{"spec":"sys::Marker","val":"✓"}"#, Kind::Marker),
        (r#"{"spec":"sys::None","val":"∅"}"#, Kind::None),
        (r#"{"spec":"sys::NA","val":"NA"}"#, Kind::NA),
        (
            r#"{"spec":"sys::Buf","val":"-_8"}"#,
            Kind::Buf(vec![0xfb, 0xff]),
        ),
        (
            r#"{"spec":"sys::Uri","val":"https://example.com/a"}"#,
            Kind::Uri(Uri::new("https://example.com/a")),
        ),
    ] {
        identity(
            &jeto::decode(
                wire.as_bytes(),
                &context,
                Some("sys::Str"),
                Limits::default(),
            )
            .unwrap(),
            &native,
        );
        let encoded = exact(&native, &context, None, Boxing::All);
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            value,
            serde_json::from_str::<serde_json::Value>(wire).unwrap()
        );
        identity(
            &jeto::decode(&encoded, &context, None, Limits::default()).unwrap(),
            &native,
        );
    }
    assert_eq!(decode("true", Some("sys::Number")), Kind::Bool(true));
    assert_eq!(
        decode(r#""2024-11-26""#, None),
        Kind::Str("2024-11-26".into())
    );
}
#[test]
fn every_boxing_mode_reports_ref_display_and_numeric_identity() {
    let context = Context::standard();
    let native = Kind::Ref(HRef::new("xyz-123", Some("Carytown".into())));
    for mode in [Boxing::Auto, Boxing::All] {
        let wire = exact(&native, &context, Some("sys::Ref"), mode);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
            serde_json::json!({"spec":"sys::Ref","val":"xyz-123","dis":"Carytown"})
        );
        identity(
            &jeto::decode(&wire, &context, Some("sys::Ref"), Limits::default()).unwrap(),
            &native,
        );
    }
    assert!(matches!(
        jeto::encode(
            &native,
            &context,
            Some("sys::Ref"),
            Boxing::None,
            Limits::default()
        )
        .unwrap(),
        Encoding::Lossy { .. }
    ));
    assert!(matches!(
        jeto::encode(
            &Kind::Number(Number::unitless(42.0)),
            &context,
            None,
            Boxing::None,
            Limits::default()
        )
        .unwrap(),
        Encoding::Lossy { .. }
    ));
    assert_eq!(
        exact(&Kind::Bool(true), &context, None, Boxing::None),
        b"true"
    );
    let wire = exact(&Kind::Int(42), &context, Some("sys::Float"), Boxing::Auto);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&wire).unwrap()["spec"],
        "sys::Int"
    );
}
#[test]
fn nominal_context_validates_provenance_and_keeps_exact_text() {
    let context = Context::new(
        "catalog-A",
        "revision-7",
        vec![Definition::Nominal {
            name: "fixture::Code".into(),
            pattern: "[0-9]+".into(),
        }],
    )
    .unwrap();
    let native = Kind::Nominal(
        NominalScalar::new("fixture::Code", "catalog-A", "revision-7", "000042").unwrap(),
    );
    identity(
        &jeto::decode(
            br#""000042""#,
            &context,
            Some("fixture::Code"),
            Limits::default(),
        )
        .unwrap(),
        &native,
    );
    let wire = exact(&native, &context, None, Boxing::Auto);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&wire).unwrap(),
        serde_json::json!({"spec":"fixture::Code","val":"000042"})
    );
    identity(
        &jeto::decode(&wire, &context, None, Limits::default()).unwrap(),
        &native,
    );
    assert!(jeto::decode(&wire, &Context::standard(), None, Limits::default()).is_err());
    for (catalog, revision) in [("catalog-B", "revision-7"), ("catalog-A", "revision-8")] {
        let other = Kind::Nominal(
            NominalScalar::new("fixture::Code", catalog, revision, "000042").unwrap(),
        );
        assert!(matches!(
            jeto::encode(&other, &context, None, Boxing::All, Limits::default()).unwrap(),
            Encoding::Unsupported { .. }
        ));
    }
    assert!(
        jeto::decode(
            br#""no""#,
            &context,
            Some("fixture::Code"),
            Limits::default()
        )
        .is_err()
    );
    assert!(matches!(
        jeto::encode(
            &Kind::Str("000042".into()),
            &context,
            Some("fixture::Code"),
            Boxing::Auto,
            Limits::default()
        )
        .unwrap(),
        Encoding::Unsupported { .. }
    ));
}
#[test]
fn malformed_boxes_unknown_types_and_noncanonical_buf_reject() {
    for wire in [
        r#"{"spec":"sys::Bool","val":true}"#,
        r#"{"spec":"sys::Int","val":"1.5"}"#,
        r#"{"spec":"sys::Float","val":"1e9999"}"#,
        r#"{"spec":"sys::Ref","val":"a b"}"#,
        r#"{"spec":"unknown::Type","val":"x"}"#,
        r#"{"spec":5,"val":"x"}"#,
        r#"{"spec":"sys::Bool","val":"true","extra":0}"#,
    ] {
        assert!(
            jeto::decode(
                wire.as_bytes(),
                &Context::standard(),
                None,
                Limits::default()
            )
            .is_err(),
            "{wire}"
        );
    }
    for wire in ["+/8", "-_8=", "-_9", "A"] {
        assert!(
            jeto::decode(
                serde_json::to_string(wire).unwrap().as_bytes(),
                &Context::standard(),
                Some("sys::Buf"),
                Limits::default()
            )
            .is_err()
        );
    }
}
#[test]
fn syntax_and_document_budgets_fail_before_success() {
    for wire in [r#"{"a":1,"a":2}"#, "[1,]", "01", "1 true", r#""\uD800""#] {
        assert!(
            jeto::decode(
                wire.as_bytes(),
                &Context::standard(),
                None,
                Limits::default()
            )
            .is_err()
        );
    }
    let limits = Limits {
        max_input_bytes: 3,
        ..Limits::default()
    };
    assert!(matches!(
        jeto::decode(b"true", &Context::standard(), None, limits),
        Err(Error::Budget(Limit::Input))
    ));
    let limits = Limits {
        max_nodes: 2,
        ..Limits::default()
    };
    assert!(matches!(
        jeto::decode(b"[1,2]", &Context::standard(), None, limits),
        Err(Error::Budget(Limit::Nodes))
    ));
    let limits = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    assert!(matches!(
        jeto::decode(b"[[1]]", &Context::standard(), None, limits),
        Err(Error::Budget(Limit::Depth))
    ));
}

#[test]
fn independent_binary64_vectors_units_and_specials_preserve_bits() {
    let context = Context::standard();
    for (text, bits) in [
        ("0.0", 0),
        ("-0.0", 1 << 63),
        ("1.0", 0x3ff0000000000000),
        ("-1.0", 0xbff0000000000000),
        ("0.1", 0x3fb999999999999a),
        ("5e-324", 1),
        ("2.2250738585072014e-308", 0x0010000000000000),
        ("1.7976931348623157e308", 0x7fefffffffffffff),
        ("1.0000000000000002", 0x3ff0000000000001),
        ("0.9999999999999999", 0x3fefffffffffffff),
    ] {
        identity(
            &decode(text, Some("sys::Float")),
            &Kind::Float(Float::from_bits(bits)),
        );
        for native in [
            Kind::Float(Float::from_bits(bits)),
            Kind::Number(Number::unitless(f64::from_bits(bits))),
            Kind::Number(Number::new(f64::from_bits(bits), Some("m²".into()))),
        ] {
            for mode in [Boxing::Auto, Boxing::All] {
                let bytes = exact(&native, &context, None, mode);
                identity(
                    &jeto::decode(&bytes, &context, None, Limits::default()).unwrap(),
                    &native,
                );
            }
        }
    }
    for (text, bits) in [
        ("NaN", 0x7ff8000000000000),
        ("INF", 0x7ff0000000000000),
        ("-INF", 0xfff0000000000000),
    ] {
        for (spec, native) in [
            ("sys::Float", Kind::Float(Float::from_bits(bits))),
            (
                "sys::Number",
                Kind::Number(Number::unitless(f64::from_bits(bits))),
            ),
        ] {
            let wire = format!(r#"{{"spec":"{spec}","val":"{text}"}}"#);
            identity(
                &jeto::decode(wire.as_bytes(), &context, None, Limits::default()).unwrap(),
                &native,
            );
            for mode in [Boxing::Auto, Boxing::All] {
                identity(
                    &jeto::decode(
                        &exact(&native, &context, None, mode),
                        &context,
                        None,
                        Limits::default(),
                    )
                    .unwrap(),
                    &native,
                );
            }
        }
    }
    for native in [
        Kind::Float(Float::from_bits(0x7ff8000000000001)),
        Kind::Float(Float::from_bits(0xfff8000000000000)),
        Kind::Number(Number::new(f64::INFINITY, Some("kW".into()))),
        Kind::Number(Number::new(1.0, Some(String::new()))),
    ] {
        assert!(matches!(
            jeto::encode(&native, &context, None, Boxing::All, Limits::default()).unwrap(),
            Encoding::Unsupported { .. }
        ));
    }
}
#[test]
fn temporal_text_preserves_fraction_offset_and_timezone_without_lookup() {
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, Timelike};
    use haystack_core::kinds::HDateTime;
    let context = Context::standard();
    let date = Kind::Date(NaiveDate::from_ymd_opt(2024, 11, 26).unwrap());
    let time = Kind::Time(NaiveTime::from_hms_nano_opt(23, 59, 59, 1_250_000_000).unwrap());
    let dt = DateTime::parse_from_rfc3339("2024-11-26T09:17:23.123456789-04:00").unwrap();
    let datetime = Kind::DateTime(HDateTime::new(dt, "Source_Zone"));
    for (text, spec, native) in [
        ("2024-11-26", "sys::Date", date),
        ("23:59:60.25", "sys::Time", time),
        (
            "2024-11-26T09:17:23.123456789-04:00 Source_Zone",
            "sys::DateTime",
            datetime,
        ),
    ] {
        let wire = serde_json::to_vec(text).unwrap();
        identity(
            &jeto::decode(&wire, &context, Some(spec), Limits::default()).unwrap(),
            &native,
        );
        for mode in [Boxing::Auto, Boxing::All] {
            identity(
                &jeto::decode(
                    &exact(&native, &context, None, mode),
                    &context,
                    None,
                    Limits::default(),
                )
                .unwrap(),
                &native,
            );
        }
    }
    let second_offset = DateTime::from_timestamp(0, 123)
        .unwrap()
        .with_timezone(&FixedOffset::east_opt(1234).unwrap());
    let non_minute_leap = NaiveTime::from_hms_opt(1, 2, 3)
        .unwrap()
        .with_nanosecond(1_000_000_000)
        .unwrap();
    for native in [
        Kind::DateTime(HDateTime::new(second_offset, "Source_Zone")),
        Kind::Time(non_minute_leap),
    ] {
        assert!(matches!(
            jeto::encode(&native, &context, None, Boxing::All, Limits::default()).unwrap(),
            Encoding::Unsupported { .. }
        ));
    }
    for text in [
        "2024-11-26t09:17:23Z",
        "2024-11-26T09:17:23ΩΩΩΩ",
        "2024-11-26T09:17:23+00:20:34 Source_Zone",
        "2024-11-26T09:17:23.1234567890Z",
        "2024-11-26T09:17:23-04:00",
    ] {
        assert!(
            jeto::decode(
                serde_json::to_string(text).unwrap().as_bytes(),
                &context,
                Some("sys::DateTime"),
                Limits::default()
            )
            .is_err(),
            "{text}"
        );
    }
}

use haystack_core::data::{HCol, HDict, HGrid};
use std::collections::BTreeMap;
fn container_context() -> Context {
    Context::new(
        "fixtures",
        "r3",
        vec![
            Definition::Dict {
                name: "test::Row".into(),
                members: BTreeMap::from([
                    ("n".into(), "sys::Number".into()),
                    ("date".into(), "sys::Date".into()),
                    ("dates".into(), "test::Dates".into()),
                ]),
            },
            Definition::Dict {
                name: "test::OtherRow".into(),
                members: BTreeMap::from([("n".into(), "sys::Float".into())]),
            },
            Definition::List {
                name: "test::Dates".into(),
                of: "sys::Date".into(),
            },
            Definition::Grid {
                name: "test::Grid".into(),
                of: Some("test::Row".into()),
            },
        ],
    )
    .unwrap()
}
fn contextual(text: &str, context: &Context, expected: Option<&str>) -> Kind {
    jeto::decode(text.as_bytes(), context, expected, Limits::default()).unwrap()
}
fn dict_kind(tags: impl IntoIterator<Item = (&'static str, Kind)>) -> Kind {
    let mut dict = HDict::new();
    for (name, value) in tags {
        dict.set(name, value);
    }
    Kind::Dict(Box::new(dict))
}
#[test]
fn recursive_dict_and_list_contexts_override_without_guessing_and_remove_only_dict_null() {
    let context = container_context();
    let date = contextual(r#""2024-11-26""#, &context, Some("sys::Date"));
    let wire = r#"{"spec":"test::Row","n":42,"dates":["2024-11-26",null],"absent":null,"none":{"spec":"sys::None","val":"∅"},"na":{"spec":"sys::NA","val":"NA"},"plain":{"n":42}}"#;
    let native = dict_kind([
        ("spec", Kind::Ref(HRef::new("test::Row", None))),
        ("n", Kind::Number(Number::unitless(42.0))),
        ("dates", Kind::List(vec![date.clone(), Kind::Null])),
        ("none", Kind::None),
        ("na", Kind::NA),
        ("plain", dict_kind([("n", Kind::Int(42))])),
    ]);
    identity(&contextual(wire, &context, Some("test::OtherRow")), &native);
    for boxing in [Boxing::Auto, Boxing::All] {
        let bytes = exact(&native, &context, Some("test::OtherRow"), boxing);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["spec"], "test::Row");
        identity(
            &jeto::decode(&bytes, &context, Some("test::OtherRow"), Limits::default()).unwrap(),
            &native,
        );
    }
    let list = Kind::List(vec![date, Kind::Null]);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&exact(
            &list,
            &context,
            Some("test::Dates"),
            Boxing::Auto
        ))
        .unwrap(),
        serde_json::json!(["2024-11-26", null])
    );
    identity(
        &contextual(
            r#"{"n":42,"date":"2024-11-26"}"#,
            &context,
            Some("test::Row"),
        ),
        &dict_kind([
            ("n", Kind::Number(Number::unitless(42.0))),
            (
                "date",
                contextual(r#""2024-11-26""#, &context, Some("sys::Date")),
            ),
        ]),
    );
}
#[test]
fn grid_row_column_and_box_contexts_preserve_structure_order_sparse_cells_and_metadata() {
    let context = container_context();
    let wire = r#"{"spec":"sys::Grid","of":"test::Row","meta":{"title":"Original"},"cols":[{"name":"n","of":"sys::Number","meta":{"unit":"kW"}},{"name":"date"}],"rows":[{"spec":"test::OtherRow","n":42},{"n":{"spec":"sys::Int","val":"43"}},{"date":"2024-11-26"}]}"#;
    let Kind::Grid(grid) = contextual(wire, &context, None) else {
        panic!("must decode a Grid")
    };
    assert_eq!(
        grid.cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["n", "date"]
    );
    assert_eq!(
        grid.meta.get("of"),
        Some(&Kind::Ref(HRef::new("test::Row", None)))
    );
    assert!(!grid.meta.has("spec"));
    assert_eq!(grid.meta.get("title"), Some(&Kind::Str("Original".into())));
    assert_eq!(
        grid.cols[0].meta.get("of"),
        Some(&Kind::Ref(HRef::new("sys::Number", None)))
    );
    assert_eq!(grid.cols[0].meta.get("unit"), Some(&Kind::Str("kW".into())));
    assert_eq!(
        grid.rows[0].get("n"),
        Some(&Kind::Number(Number::unitless(42.0)))
    );
    assert_eq!(
        grid.rows[0].get("spec"),
        Some(&Kind::Ref(HRef::new("test::OtherRow", None)))
    );
    assert_eq!(grid.rows[1].get("n"), Some(&Kind::Int(43)));
    assert!(!grid.rows[2].has("n"));
    assert!(matches!(grid.rows[2].get("date"), Some(Kind::Date(_))));
    for boxing in [Boxing::Auto, Boxing::All] {
        let native = Kind::Grid(grid.clone());
        let bytes = exact(&native, &context, None, boxing);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["spec"], "sys::Grid");
        assert_eq!(json["of"], "test::Row");
        assert_eq!(json["cols"][0]["of"], "sys::Number");
        assert_eq!(json["cols"][0]["name"], "n");
        assert!(json["meta"].get("of").is_none());
        assert!(json["cols"][0]["meta"].get("of").is_none());
        identity(
            &jeto::decode(&bytes, &context, None, Limits::default()).unwrap(),
            &native,
        );
    }
    let Kind::Grid(subclass) = contextual(
        r#"{"spec":"test::Grid","cols":[{"name":"n"}],"rows":[{"n":42}]}"#,
        &context,
        None,
    ) else {
        panic!()
    };
    assert_eq!(
        subclass.meta.get("spec"),
        Some(&Kind::Ref(HRef::new("test::Grid", None)))
    );
    assert_eq!(
        subclass.rows[0].get("n"),
        Some(&Kind::Number(Number::unitless(42.0)))
    );
    identity(
        &jeto::decode(
            &exact(&Kind::Grid(subclass.clone()), &context, None, Boxing::Auto),
            &context,
            None,
            Limits::default(),
        )
        .unwrap(),
        &Kind::Grid(subclass),
    );
}
#[test]
fn nested_grid_contexts_do_not_escape_their_owner() {
    let context = container_context();
    let wire = r#"{"spec":"sys::Grid","cols":[{"name":"n","of":"sys::Number"},{"name":"nested"}],"rows":[{"n":7,"nested":{"spec":"sys::Grid","cols":[{"name":"n"}],"rows":[{"n":8}]}}]}"#;
    let Kind::Grid(grid) = contextual(wire, &context, None) else {
        panic!()
    };
    assert_eq!(
        grid.rows[0].get("n"),
        Some(&Kind::Number(Number::unitless(7.0)))
    );
    let Some(Kind::Grid(nested)) = grid.rows[0].get("nested") else {
        panic!()
    };
    assert_eq!(nested.rows[0].get("n"), Some(&Kind::Int(8)));
    let native = Kind::Grid(grid);
    identity(
        &jeto::decode(
            &exact(&native, &context, None, Boxing::All),
            &context,
            None,
            Limits::default(),
        )
        .unwrap(),
        &native,
    );
}
#[test]
fn malformed_grid_and_structural_collisions_are_rejected_without_dropping_data() {
    let context = container_context();
    for wire in [
        r#"{"spec":"sys::Grid","cols":[{"name":"n"},{"name":"n"}],"rows":[]}"#,
        r#"{"spec":"sys::Grid","cols":[{"name":"n"}],"rows":[{"extra":1}]}"#,
        r#"{"spec":"sys::Grid","cols":[{"name":"spec"}],"rows":[]}"#,
        r#"{"spec":"sys::Grid","cols":[{"name":"n","extra":1}],"rows":[]}"#,
        r#"{"spec":"sys::Grid","cols":[{"name":"n","of":"unknown::Type"}],"rows":[]}"#,
        r#"{"spec":"sys::Grid","cols":[{"name":"n","of":{"spec":"sys::Ref","val":"sys::Int"}}],"rows":[]}"#,
        r#"{"spec":"sys::Grid","of":"sys::Number","cols":[],"rows":[]}"#,
        r#"{"spec":"test::Grid","of":"test::OtherRow","cols":[],"rows":[]}"#,
        r#"{"spec":"sys::Grid","meta":{"spec":"sys::Dict"},"cols":[],"rows":[]}"#,
        r#"{"spec":"sys::Grid","meta":{"of":"test::Row"},"cols":[],"rows":[]}"#,
        r#"{"spec":"sys::Grid","cols":[{"name":"n","meta":{"of":"sys::Int"}}],"rows":[]}"#,
        r#"{"spec":"sys::Grid","cols":[],"rows":[[]]}"#,
        r#"{"spec":"sys::Grid","cols":[],"rows":[],"ignored":1}"#,
        r#"{"spec":"test::Dates","val":[]}"#,
    ] {
        assert!(
            jeto::decode(wire.as_bytes(), &context, None, Limits::default()).is_err(),
            "{wire}"
        );
    }
    let mut bad = HDict::new();
    bad.set("x", Kind::Null);
    let outcome = jeto::encode(
        &Kind::Dict(Box::new(bad)),
        &context,
        None,
        Boxing::Auto,
        Limits::default(),
    )
    .unwrap();
    let Encoding::Unsupported { issues } = outcome else {
        panic!("dict null cannot be exact")
    };
    assert_eq!(issues[0].reason, jeto::Reason::NullDictMember);
    assert_eq!(
        issues[0].path,
        vec![haystack_core::kinds::ValuePathSegment::Tag("x".into())]
    );
    for grid in [
        HGrid::from_parts(HDict::new(), vec![HCol::new("n"), HCol::new("n")], vec![]),
        HGrid::from_parts(
            HDict::new(),
            vec![],
            vec![{
                let mut d = HDict::new();
                d.set("extra", Kind::Int(1));
                d
            }],
        ),
        HGrid::from_parts(
            {
                let mut d = HDict::new();
                d.set("spec", Kind::Ref(HRef::new("sys::Grid", None)));
                d
            },
            vec![],
            vec![],
        ),
        HGrid::from_parts(
            {
                let mut d = HDict::new();
                d.set(
                    "of",
                    Kind::Ref(HRef::new("test::Row", Some("display".into()))),
                );
                d
            },
            vec![],
            vec![],
        ),
    ] {
        assert!(matches!(
            jeto::encode(
                &Kind::Grid(Box::new(grid)),
                &context,
                None,
                Boxing::All,
                Limits::default()
            )
            .unwrap(),
            Encoding::Unsupported { .. }
        ));
    }
}
#[derive(Default)]
struct Receipt {
    work: usize,
    retained: usize,
    nodes: usize,
    depth: usize,
}
impl jeto::Meter for Receipt {
    type Error = std::convert::Infallible;
    fn charge(&mut self, cost: jeto::Charge) -> Result<(), Self::Error> {
        match cost {
            jeto::Charge::Work(n) => self.work += n,
            jeto::Charge::Retained(n) => self.retained += n,
            jeto::Charge::Nodes(n) => self.nodes += n,
            jeto::Charge::Depth(n) => self.depth = self.depth.max(n),
            _ => {}
        }
        Ok(())
    }
}
#[test]
fn wide_sparse_grids_and_tail_rows_consume_one_cumulative_budget() {
    let context = container_context();
    let cols = (0..24)
        .map(|i| serde_json::json!({"name":format!("c{i}"),"of":"sys::Number"}))
        .collect::<Vec<_>>();
    let rows = (0..48)
        .map(|i| serde_json::json!({"c23":i}))
        .collect::<Vec<_>>();
    let bytes =
        serde_json::to_vec(&serde_json::json!({"spec":"sys::Grid","cols":cols,"rows":rows}))
            .unwrap();
    let mut receipt = Receipt::default();
    let Kind::Grid(grid) = jeto::decode_metered(&bytes, &context, None, &mut receipt).unwrap()
    else {
        panic!()
    };
    assert_eq!(grid.cols.len(), 24);
    assert_eq!(grid.rows.len(), 48);
    assert_eq!(grid.rows[47].len(), 1);
    assert_eq!(
        grid.rows[47].get("c23"),
        Some(&Kind::Number(Number::unitless(47.0)))
    );
    for (limit, limits) in [
        (
            Limit::Work,
            Limits {
                max_work: receipt.work - 1,
                ..Limits::default()
            },
        ),
        (
            Limit::Retained,
            Limits {
                max_retained_bytes: receipt.retained - 1,
                ..Limits::default()
            },
        ),
        (
            Limit::Nodes,
            Limits {
                max_nodes: receipt.nodes - 1,
                ..Limits::default()
            },
        ),
    ] {
        assert!(
            matches!(jeto::decode(&bytes,&context,None,limits),Err(Error::Budget(found)) if found==limit)
        );
    }
    let mut tails = vec![serde_json::json!({"n":1})];
    tails.extend((0..128).map(|_| serde_json::json!({"n":{"spec":"sys::Number","val":"42.5kWh"}})));
    let tail_wire = serde_json::to_vec(
        &serde_json::json!({"spec":"sys::Grid","cols":[{"name":"n"}],"rows":tails}),
    )
    .unwrap();
    let mut tail_receipt = Receipt::default();
    let Kind::Grid(grid) =
        jeto::decode_metered(&tail_wire, &context, None, &mut tail_receipt).unwrap()
    else {
        panic!()
    };
    assert_eq!(grid.rows.len(), 129);
    assert!(matches!(
        jeto::decode(
            &tail_wire,
            &context,
            None,
            Limits {
                max_work: tail_receipt.work / 2,
                ..Limits::default()
            }
        ),
        Err(Error::Budget(Limit::Work))
    ));
    let mut malformed: serde_json::Value = serde_json::from_slice(&tail_wire).unwrap();
    malformed["rows"][128]["n"]["val"] = serde_json::json!(42);
    assert!(
        jeto::decode(
            &serde_json::to_vec(&malformed).unwrap(),
            &context,
            None,
            Limits::default()
        )
        .is_err()
    );
}

#[test]
fn nested_contexts_and_generated_boxes_charge_depth_nodes_escaping_and_copies() {
    let context = container_context();
    let wire = r#"{"spec":"test::Row","dates":["2024-11-26",null,{"spec":"sys::Ref","val":"a","dis":"A"}],"plain":{"nested":[[true]]}}"#;
    let mut receipt = Receipt::default();
    let value = jeto::decode_metered(wire.as_bytes(), &context, None, &mut receipt).unwrap();
    for (limit, limits) in [
        (
            Limit::Depth,
            Limits {
                max_depth: receipt.depth - 1,
                ..Limits::default()
            },
        ),
        (
            Limit::Nodes,
            Limits {
                max_nodes: receipt.nodes - 1,
                ..Limits::default()
            },
        ),
        (
            Limit::Retained,
            Limits {
                max_retained_bytes: receipt.retained - 1,
                ..Limits::default()
            },
        ),
    ] {
        assert!(
            matches!(jeto::decode(wire.as_bytes(), &context, None, limits), Err(Error::Budget(found)) if found == limit)
        );
    }
    identity(
        &jeto::decode(
            &exact(&value, &context, None, Boxing::All),
            &context,
            None,
            Limits::default(),
        )
        .unwrap(),
        &value,
    );
    let mut values = (0..64)
        .map(|i| {
            if i % 2 == 0 {
                Kind::Bool(true)
            } else {
                Kind::Number(Number::unitless(i as f64))
            }
        })
        .collect::<Vec<_>>();
    values.push(Kind::Str("\n\t\"\\".repeat(128)));
    values.push(Kind::Ref(HRef::new("id", Some("\n\t\"\\".repeat(128)))));
    let native = Kind::List(values);
    let mut receipt = Receipt::default();
    let encoded = jeto::encode_metered(&native, &context, None, Boxing::All, &mut receipt)
        .unwrap()
        .into_exact()
        .unwrap();
    assert!(
        receipt.nodes >= 64 * 4,
        "native values and generated spec/val nodes must be charged"
    );
    assert!(receipt.depth >= 2);
    let json: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        json[0],
        serde_json::json!({"spec":"sys::Bool","val":"true"})
    );
    assert_eq!(json[1]["spec"], "sys::Number");
    assert_eq!(
        json[1]["val"].as_str().unwrap().parse::<f64>().unwrap(),
        1.0
    );
    for (limit, limits) in [
        (
            Limit::Depth,
            Limits {
                max_depth: 1,
                ..Limits::default()
            },
        ),
        (
            Limit::Nodes,
            Limits {
                max_nodes: receipt.nodes - 1,
                ..Limits::default()
            },
        ),
        (
            Limit::Retained,
            Limits {
                max_retained_bytes: receipt.retained - 1,
                ..Limits::default()
            },
        ),
        (
            Limit::Output,
            Limits {
                max_output_bytes: encoded.len() - 1,
                ..Limits::default()
            },
        ),
    ] {
        assert!(
            matches!(jeto::encode(&native, &context, None, Boxing::All, limits), Err(Error::Budget(found)) if found == limit)
        );
    }
    identity(
        &jeto::decode(&encoded, &context, None, Limits::default()).unwrap(),
        &native,
    );
    #[derive(Debug, PartialEq)]
    enum Interrupt {
        Cancelled,
    }
    let mut checkpoints = 0;
    let mut interrupted = |_: jeto::Charge| {
        checkpoints += 1;
        if checkpoints == 30 {
            Err(Interrupt::Cancelled)
        } else {
            Ok(())
        }
    };
    assert!(matches!(
        jeto::encode_metered(&native, &context, None, Boxing::All, &mut interrupted),
        Err(Error::Budget(Interrupt::Cancelled))
    ));
    assert_eq!(checkpoints, 30);
}
#[test]
fn context_is_closed_and_recursive_references_follow_only_bounded_values() {
    for definitions in [
        vec![Definition::List {
            name: "test::Missing".into(),
            of: "test::Unknown".into(),
        }],
        vec![Definition::Grid {
            name: "test::BadGrid".into(),
            of: Some("sys::Int".into()),
        }],
        vec![Definition::Dict {
            name: "test::BadDict".into(),
            members: BTreeMap::from([("x".into(), "test::Unknown".into())]),
        }],
        vec![Definition::Dict {
            name: "test::BadDict".into(),
            members: BTreeMap::from([("spec".into(), "sys::Str".into())]),
        }],
        vec![Definition::Dict {
            name: "test::BadDict".into(),
            members: BTreeMap::from([("x".into(), format!("test::{}", "X".repeat(257)))]),
        }],
        vec![Definition::Nominal {
            name: "sys::Int".into(),
            pattern: ".*".into(),
        }],
    ] {
        assert!(Context::new("fixture", "r1", definitions).is_err());
    }
    let context = Context::new(
        "fixture",
        "r1",
        vec![
            Definition::List {
                name: "test::Recursive".into(),
                of: "test::Recursive".into(),
            },
            Definition::Dict {
                name: "test::Node".into(),
                members: BTreeMap::from([("child".into(), "test::Node".into())]),
            },
        ],
    )
    .unwrap();
    assert_eq!(
        jeto::decode(
            b"[[[null]]]",
            &context,
            Some("test::Recursive"),
            Limits::default()
        )
        .unwrap(),
        Kind::List(vec![Kind::List(vec![Kind::List(vec![Kind::Null])])])
    );
    assert!(matches!(
        jeto::decode(
            b"[[[null]]]",
            &context,
            Some("test::Recursive"),
            Limits {
                max_depth: 2,
                ..Limits::default()
            }
        ),
        Err(Error::Budget(Limit::Depth))
    ));
    assert!(matches!(
        jeto::decode(
            br#"{"child":{"child":{"child":{}}}}"#,
            &context,
            Some("test::Node"),
            Limits {
                max_depth: 2,
                ..Limits::default()
            }
        ),
        Err(Error::Budget(Limit::Depth))
    ));
}

#[test]
fn pinned_temporal_vectors_preserve_nanos_offsets_and_zone_identity() {
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, Timelike};
    use haystack_core::kinds::HDateTime;
    let context = Context::standard();
    for (text, seconds, nanos) in [
        ("14:30:00.123456789", 52200, 123456789),
        ("14:30:00.507000000", 52200, 507000000),
        ("23:59:60.5", 86399, 1500000000),
    ] {
        let Kind::Time(time) = contextual(
            &serde_json::to_string(text).unwrap(),
            &context,
            Some("sys::Time"),
        ) else {
            panic!()
        };
        assert_eq!(time.num_seconds_from_midnight(), seconds);
        assert_eq!(time.nanosecond(), nanos);
        identity(
            &jeto::decode(
                &exact(&Kind::Time(time), &context, None, Boxing::All),
                &context,
                None,
                Limits::default(),
            )
            .unwrap(),
            &Kind::Time(time),
        );
    }
    for (text, offset, zone) in [
        ("2026-08-19T14:50:23.507-06:00 Denver", -21600, "Denver"),
        ("2026-08-19T14:50:23.507-04:00 New_York", -14400, "New_York"),
        ("2026-08-19T18:50:23.507Z UTC", 0, "UTC"),
        ("2026-08-19T18:50:23.507Z", 0, "UTC"),
    ] {
        let value = contextual(
            &serde_json::to_string(text).unwrap(),
            &context,
            Some("sys::DateTime"),
        );
        let Kind::DateTime(dt) = &value else { panic!() };
        assert_eq!(dt.dt.nanosecond(), 507000000);
        assert_eq!(dt.dt.offset().local_minus_utc(), offset);
        assert_eq!(dt.tz_name, zone);
        identity(
            &jeto::decode(
                &exact(&value, &context, None, Boxing::Auto),
                &context,
                None,
                Limits::default(),
            )
            .unwrap(),
            &value,
        );
    }
    let offset = DateTime::from_timestamp(0, 0)
        .unwrap()
        .with_timezone(&FixedOffset::east_opt(-17762).unwrap());
    let instant = DateTime::parse_from_rfc3339("2026-08-19T18:50:23Z").unwrap();
    for native in [
        Kind::DateTime(HDateTime::new(offset, "Source_Zone")),
        Kind::DateTime(HDateTime::new(instant, "")),
        Kind::Date(NaiveDate::from_ymd_opt(10000, 1, 1).unwrap()),
        Kind::Time(
            NaiveTime::from_hms_opt(23, 59, 58)
                .unwrap()
                .with_nanosecond(1500000000)
                .unwrap(),
        ),
    ] {
        assert!(matches!(
            jeto::encode(&native, &context, None, Boxing::All, Limits::default()).unwrap(),
            Encoding::Unsupported { .. }
        ));
    }
}

#[test]
fn nominal_list_and_heterogeneous_grid_contexts_share_the_same_native_values() {
    let context = Context::new(
        "catalog-A",
        "revision-7",
        vec![
            Definition::Nominal {
                name: "fixture::Code".into(),
                pattern: "[0-9]+".into(),
            },
            Definition::List {
                name: "fixture::Codes".into(),
                of: "fixture::Code".into(),
            },
            Definition::Dict {
                name: "fixture::Row".into(),
                members: BTreeMap::from([
                    ("code".into(), "fixture::Code".into()),
                    ("codes".into(), "fixture::Codes".into()),
                ]),
            },
            Definition::Dict {
                name: "fixture::TextRow".into(),
                members: BTreeMap::from([("code".into(), "sys::Str".into())]),
            },
        ],
    )
    .unwrap();
    let code = Kind::Nominal(
        NominalScalar::new("fixture::Code", "catalog-A", "revision-7", "000042").unwrap(),
    );
    identity(
        &contextual(r#""000042""#, &context, Some("fixture::Code")),
        &code,
    );
    let dict = contextual(
        r#"{"code":"000042","codes":["000042",null]}"#,
        &context,
        Some("fixture::Row"),
    );
    identity(
        &dict,
        &dict_kind([
            ("code", code.clone()),
            ("codes", Kind::List(vec![code.clone(), Kind::Null])),
        ]),
    );
    for boxing in [Boxing::Auto, Boxing::All] {
        identity(
            &jeto::decode(
                &exact(&dict, &context, Some("fixture::Row"), boxing),
                &context,
                Some("fixture::Row"),
                Limits::default(),
            )
            .unwrap(),
            &dict,
        );
    }
    // Row-own spec overrides the grid default when no column override exists.
    let Kind::Grid(rows) = contextual(
        r#"{"spec":"sys::Grid","of":"fixture::Row","cols":[{"name":"code"}],"rows":[{"code":"000042"},{"spec":"fixture::TextRow","code":"000042"}]}"#,
        &context,
        None,
    ) else {
        panic!()
    };
    identity(rows.rows[0].get("code").unwrap(), &code);
    assert_eq!(rows.rows[1].get("code"), Some(&Kind::Str("000042".into())));
    // A column type overrides row-own spec, and a cell box overrides the column.
    let Kind::Grid(columns) = contextual(
        r#"{"spec":"sys::Grid","of":"fixture::Row","cols":[{"name":"code","of":"sys::Str"}],"rows":[{"code":"000042"},{"code":{"spec":"fixture::Code","val":"000042"}}]}"#,
        &context,
        None,
    ) else {
        panic!()
    };
    assert_eq!(
        columns.rows[0].get("code"),
        Some(&Kind::Str("000042".into()))
    );
    identity(columns.rows[1].get("code").unwrap(), &code);
    for grid in [rows, columns] {
        let native = Kind::Grid(grid);
        identity(
            &jeto::decode(
                &exact(&native, &context, None, Boxing::Auto),
                &context,
                None,
                Limits::default(),
            )
            .unwrap(),
            &native,
        );
    }
}

#[test]
fn retained_capacity_dict_scan_is_charged_before_traversal() {
    let context = Context::standard();
    let mut sparse = haystack_core::data::HDict::new();
    for index in 0..16_384 {
        sparse.set(format!("key{index:05}"), Kind::Int(42));
    }
    let keep = sparse.tag_names().last().unwrap().to_owned();
    let remove: Vec<_> = sparse
        .tag_names()
        .filter(|key| *key != keep)
        .map(str::to_owned)
        .collect();
    for key in remove {
        sparse.remove_tag(&key);
    }
    assert!(sparse.tags().capacity() > 8192);
    let mut fresh = haystack_core::data::HDict::new();
    fresh.set(&keep, Kind::Int(42));
    let sparse = Kind::Dict(Box::new(sparse));
    let fresh = Kind::Dict(Box::new(fresh));
    let tight = Limits {
        max_work: 8192,
        ..Limits::default()
    };
    assert!(jeto::encode(&fresh, &context, None, Boxing::Auto, tight).is_ok());
    assert!(matches!(
        jeto::encode(&sparse, &context, None, Boxing::Auto, tight),
        Err(Error::Budget(Limit::Work))
    ));
    assert_eq!(
        exact(&sparse, &context, None, Boxing::Auto),
        exact(&fresh, &context, None, Boxing::Auto)
    );
}

#[test]
fn full_then_deleted_dict_preserves_its_pre_deletion_scan_charge() {
    let context = Context::standard();
    let mut dict = haystack_core::data::HDict::new();
    for index in 0..16_384 {
        dict.set(format!("key{index:05}"), Kind::Int(42));
    }
    let capacity = dict.tags().capacity();
    for index in 16_384..capacity {
        dict.set(format!("key{index:05}"), Kind::Int(42));
    }
    let keep = dict.tag_names().last().unwrap().to_owned();
    let remove: Vec<_> = dict
        .tag_names()
        .filter(|key| *key != keep)
        .map(str::to_owned)
        .collect();
    for key in remove {
        dict.remove_tag(&key);
    }
    let mut fresh = haystack_core::data::HDict::new();
    fresh.set(&keep, Kind::Int(42));
    let mut retained_receipt = Receipt::default();
    let mut fresh_receipt = Receipt::default();
    let sparse = Kind::Dict(Box::new(dict));
    let fresh = Kind::Dict(Box::new(fresh));
    assert_eq!(
        jeto::encode_metered(&sparse, &context, None, Boxing::Auto, &mut retained_receipt).unwrap(),
        jeto::encode_metered(&fresh, &context, None, Boxing::Auto, &mut fresh_receipt).unwrap()
    );
    assert_eq!(
        retained_receipt.work - fresh_receipt.work,
        capacity.next_power_of_two() - 4
    );
    let tight = Limits {
        max_work: 8192,
        ..Limits::default()
    };
    assert!(matches!(
        jeto::encode(&sparse, &context, None, Boxing::Auto, tight),
        Err(Error::Budget(Limit::Work))
    ));
    assert!(
        jeto::encode(&fresh, &context, None, Boxing::Auto, tight)
            .unwrap()
            .into_exact()
            .is_ok()
    );
}

#[test]
fn immutable_nominal_patterns_are_anchored_and_reject_unsupported_boundaries() {
    for pattern in [r"\bword\b", r"\Bword\B", r"(?:a|b)*a(?:a|b){20}"] {
        assert!(
            Context::new(
                "catalog",
                "revision",
                vec![Definition::Nominal {
                    name: "test::Code".into(),
                    pattern: pattern.into()
                }]
            )
            .is_err()
        );
    }
    for (pattern, accepted, rejected) in [
        (r"\p{Greek}+", "αβ", "αβ\n"),
        (r"(?i:ab|cd)+", "AbCD", "xAbCD"),
        (r"(?:a?){128}", "a", "b"),
        (r"(?-u:\b)word(?-u:\b)", "word", "word!"),
    ] {
        let context = Context::new(
            "catalog",
            "revision",
            vec![Definition::Nominal {
                name: "test::Code".into(),
                pattern: pattern.into(),
            }],
        )
        .unwrap();
        for _ in 0..2 {
            let wire = serde_json::to_vec(accepted).unwrap();
            let actual =
                jeto::decode(&wire, &context, Some("test::Code"), Limits::default()).unwrap();
            identity(
                &actual,
                &Kind::Nominal(
                    NominalScalar::new("test::Code", "catalog", "revision", accepted).unwrap(),
                ),
            );
            assert!(
                jeto::decode(
                    &serde_json::to_vec(rejected).unwrap(),
                    &context,
                    Some("test::Code"),
                    Limits::default()
                )
                .is_err()
            );
        }
    }
}

#[test]
fn json_strings_preserve_controls_unicode_and_escape_boundaries() {
    let context = Context::standard();
    let controls: String = (0..32).map(|n| char::from_u32(n).unwrap()).collect();
    for text in [
        "".to_owned(),
        "a".repeat(4096) + "\n" + &"z".repeat(4096),
        controls,
        "é🦀\\/\"\u{2028}\u{2029}".to_owned(),
    ] {
        let native = Kind::Str(text.clone());
        let wire = serde_json::to_vec(&text).unwrap();
        identity(
            &jeto::decode(&wire, &context, None, Limits::default()).unwrap(),
            &native,
        );
        assert_eq!(exact(&native, &context, None, Boxing::Auto), wire);
    }
    for (wire, text) in [
        (r#""\uD83E\uDD80""#, "🦀"),
        (r#""\u00E9\/\b\f\n\r\t""#, "é/\u{8}\u{c}\n\r\t"),
    ] {
        assert_eq!(contextual(wire, &context, None), Kind::Str(text.into()));
    }
    for wire in [
        r#""\uD800""#,
        r#""\uDC00""#,
        r#""\uD800\u0041""#,
        r#""\uD800x""#,
        r#""\u12x4""#,
        r#""\q""#,
        "\"\u{1}\"",
    ] {
        assert!(
            jeto::decode(wire.as_bytes(), &context, None, Limits::default()).is_err(),
            "{wire}"
        );
    }
}
