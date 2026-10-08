use chrono::DateTime;
use haystack_core::{
    codecs::{codec_for, history::*, typed},
    graph::GraphState,
    kinds::{HDateTime, HRef, Kind, Number},
};
fn dt(text: &str) -> HDateTime {
    HDateTime::new(DateTime::parse_from_rfc3339(text).unwrap(), "UTC")
}
fn result() -> HistoryReadResult {
    HistoryReadResult {
        metadata: HistoryMetadata {
            id: "p".into(),
            requested_range: "2024-06-01".into(),
            evaluated_at: dt("2024-06-01T12:00:00Z"),
            start: dt("2024-06-01T00:00:00Z"),
            end: dt("2024-06-02T00:00:00Z"),
            schema: HistorySchema {
                kind: HistoryKind::Number,
                unit: Some("°C".into()),
                timezone: "UTC".into(),
            },
            capabilities: HistoryCapabilities::BOUNDED_LIVE,
            bounds: HistoryBounds {
                batch_rows: 128,
                batch_bytes: 128 * 1024,
                total_rows: 1000,
                total_bytes: 1024 * 1024,
                total_work: 4 * 1024 * 1024,
                response_bytes: 1024 * 1024,
            },
            graph: GraphState {
                incarnation: [1; 16],
                revision: u64::MAX,
                catalog_generation: 3,
            },
            policy_observation: [2; 16],
            history: HistoryState {
                authority: [3; 16],
                incarnation: [4; 16],
                generation: u64::MAX,
            },
            coverage: HistoryCoverage {
                retained_start: Some(dt("2024-06-01T00:00:00Z")),
                retained_end: Some(dt("2024-06-01T23:59:59.999999999Z")),
                retained_count: 3,
                evicted_through: Some(dt("2024-05-31T00:00:00Z")),
            },
        },
        samples: vec![
            HistorySample {
                ts: dt("2024-06-01T00:00:00Z"),
                val: Kind::Number(Number::new(12.5, Some("°C".into()))),
            },
            HistorySample {
                ts: dt("2024-06-01T12:00:00Z"),
                val: Kind::NA,
            },
            HistorySample {
                ts: dt("2024-06-01T23:59:59.999999999Z"),
                val: Kind::Number(Number::unitless(-0.0)),
            },
        ],
        terminal: HistoryTerminal::Complete,
    }
}
fn request() -> HistoryReadRequest {
    HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01".into(),
    }
}
#[test]
fn ordinary_rows_and_typed_control_round_trip_in_all_admitted_formats() {
    for mime in ["text/zinc", "application/json", "application/json;v=3"] {
        let codec = codec_for(mime).unwrap();
        assert_eq!(
            decode_request(&encode_request(&request(), codec).unwrap(), codec).unwrap(),
            request()
        );
        let value = result();
        let decoded = decode_result(&encode_result(&value, codec).unwrap(), codec).unwrap();
        assert_eq!(decoded, value);
        validate_for_request(&decoded, &request()).unwrap();
        assert_eq!(decoded.metadata.history.generation, u64::MAX);
        let Kind::Number(number) = &decoded.samples[2].val else {
            panic!()
        };
        assert_eq!(number.val.to_bits(), (-0.0f64).to_bits());
    }
}
#[test]
fn partial_terminal_rows_and_provenance_survive() {
    for terminal in [
        HistoryTerminal::Limited(HistoryReason::Rows),
        HistoryTerminal::Interrupted(HistoryReason::Cancelled),
        HistoryTerminal::Failed(HistoryReason::Provider),
    ] {
        let mut value = result();
        value.samples.truncate(1);
        value.terminal = terminal;
        for mime in ["text/zinc", "application/json", "application/json;v=3"] {
            let codec = codec_for(mime).unwrap();
            assert_eq!(
                decode_result(&encode_result(&value, codec).unwrap(), codec).unwrap(),
                value
            );
        }
    }
}
#[test]
fn missing_or_contradictory_control_and_wrong_point_are_protocol_errors() {
    let mut grid = result_grid(&result()).unwrap();
    grid.meta.remove_tag("history");
    assert!(result_from_grid(&grid).is_err());
    let mut grid = result_grid(&result()).unwrap();
    let Some(Kind::Str(control)) = grid.meta.get("history") else {
        panic!()
    };
    let Kind::Dict(mut control) = typed::decode(control.as_bytes()).unwrap() else {
        panic!()
    };
    control.set("returnedCount", Kind::Str("999".into()));
    grid.meta.set(
        "history",
        Kind::Str(String::from_utf8(typed::encode(&Kind::Dict(control)).unwrap()).unwrap()),
    );
    assert!(result_from_grid(&grid).is_err());
    let mut grid = result_grid(&result()).unwrap();
    grid.meta.set("id", Kind::Ref(HRef::from_val("different")));
    assert!(validate_for_request(&result_from_grid(&grid).unwrap(), &request()).is_err());
    let mut value = result();
    value.samples.truncate(1);
    assert!(result_grid(&value).is_err());
    let mut value = result();
    value.metadata.start = dt("2024-05-31T00:00:00Z");
    assert!(validate_for_request(&value, &request()).is_err());
}
#[test]
fn schema_order_range_and_control_version_are_strict() {
    let mut value = result();
    value.samples[1].val = Kind::Int(2);
    assert!(result_grid(&value).is_err());
    let mut value = result();
    value.samples[1].ts = value.samples[0].ts.clone();
    assert!(result_grid(&value).is_err());
    let mut value = result();
    value.samples[2].ts = value.metadata.end.clone();
    assert!(result_grid(&value).is_err());
    let mut grid = request_grid(&request()).unwrap();
    grid.meta.set(
        "history",
        Kind::Str("{\"version\":2,\"value\":{\"kind\":\"null\"}}".into()),
    );
    assert!(request_from_grid(&grid).is_err());
    assert!(encode_request(&request(), codec_for("text/trio").unwrap()).is_err());
    assert!(
        decode_request(
            &vec![b' '; MAX_REQUEST_BYTES + 1],
            codec_for("text/zinc").unwrap()
        )
        .is_err()
    );
}

#[test]
fn review_number_point_unit_is_required_but_samples_can_be_unitless() {
    let mut value = result();
    value.metadata.schema.unit = None;
    value.samples[0].val = Kind::Number(Number::unitless(12.5));
    assert!(result_grid(&value).is_err());
    value.metadata.schema.unit = Some("°C".into());
    let mut grid = result_grid(&value).unwrap();
    rewrite_control(&mut grid, "unit", Kind::Null);
    assert!(result_from_grid(&grid).is_err());
}
fn rewrite_control(grid: &mut haystack_core::data::HGrid, key: &str, value: Kind) {
    let Some(Kind::Str(control)) = grid.meta.get("history") else {
        panic!()
    };
    let Kind::Dict(mut control) = typed::decode(control.as_bytes()).unwrap() else {
        panic!()
    };
    control.set(key, value);
    grid.meta.set(
        "history",
        Kind::Str(String::from_utf8(typed::encode(&Kind::Dict(control)).unwrap()).unwrap()),
    );
}
#[test]
fn review_complete_requires_prefix_and_suffix_retained_endpoints() {
    for prefix in [true, false] {
        let mut value = result();
        if prefix {
            value.metadata.end = dt("2024-06-01T13:00:00Z");
            value.samples.pop();
            value.samples.remove(0);
        } else {
            value.metadata.start = dt("2024-06-01T01:00:00Z");
            value.samples.remove(0);
            value.samples.pop();
        }
        assert!(result_grid(&value).is_err(), "missing known endpoint");
        for terminal in [
            HistoryTerminal::Limited(HistoryReason::Rows),
            HistoryTerminal::Interrupted(HistoryReason::Cancelled),
            HistoryTerminal::Failed(HistoryReason::Provider),
        ] {
            value.terminal = terminal;
            let mut grid = result_grid(&value).unwrap();
            rewrite_control(&mut grid, "terminal", Kind::Str("complete".into()));
            rewrite_control(&mut grid, "reason", Kind::Str("none".into()));
            assert!(result_from_grid(&grid).is_err());
        }
    }
}
#[test]
fn review_h4_nan_identity_in_all_three_codecs() {
    for mime in ["text/zinc", "application/json", "application/json;v=3"] {
        let codec = codec_for(mime).unwrap();
        for bits in [
            0x7ff8_0000_0000_0001,
            0xfff8_0000_0000_0000,
            0x7ff0_0000_0000_0001,
        ] {
            let mut value = result();
            value.samples[0].val = Kind::Number(Number::unitless(f64::from_bits(bits)));
            assert!(
                encode_result(&value, codec).is_err(),
                "noncanonical NaN admitted by {mime}"
            );
        }
        for n in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut value = result();
            value.samples[0].val = Kind::Number(Number::unitless(n));
            let decoded = decode_result(&encode_result(&value, codec).unwrap(), codec).unwrap();
            let Kind::Number(actual) = &decoded.samples[0].val else {
                panic!()
            };
            assert_eq!(actual.val.to_bits(), n.to_bits());
        }
    }
}

#[path = "fixtures/finite_numbers.rs"]
mod finite_numbers;
#[test]
fn second_review_finite_number_oracle_preserves_original_bits_in_all_h4_formats() {
    for (number, bits) in finite_numbers::FINITE_NUMBERS {
        assert_eq!(number.to_bits(), bits, "independent fixture bits");
        for unit in [None, Some("°C".to_owned())] {
            let mut original = result();
            original.samples[0].val = Kind::Number(Number::new(number, unit.clone()));
            for mime in ["text/zinc", "application/json", "application/json;v=3"] {
                let codec = codec_for(mime).unwrap();
                let decoded =
                    decode_result(&encode_result(&original, codec).unwrap(), codec).unwrap();
                validate_for_request(&decoded, &request()).unwrap();
                assert_eq!(decoded.terminal, HistoryTerminal::Complete);
                let Kind::Number(actual) = &decoded.samples[0].val else {
                    panic!()
                };
                assert_eq!(actual.val.to_bits(), bits, "{mime}: {number:?}");
                assert_eq!(actual.unit, unit);
            }
        }
    }
}

#[test]
fn scoped_zinc_history_response_requires_complete_rows_and_scalars() {
    let codec = codec_for("text/zinc").unwrap();
    let encoded = String::from_utf8(encode_result(&result(), codec).unwrap()).unwrap();
    let mut rows: Vec<_> = encoded.lines().map(str::to_owned).collect();
    rows[2].push_str(",unused");
    assert!(decode_result(rows.join("\n").as_bytes(), codec).is_err());
    let malformed = encoded.replacen("12.5°C", "NaN1", 1);
    assert_ne!(malformed, encoded);
    assert!(decode_result(malformed.as_bytes(), codec).is_err());
}

#[test]
fn review_scoped_read_output_preserves_leaps_and_rejects_second_offsets() {
    let leap = dt("2016-12-31T23:59:60.500Z");
    let mut value = result();
    value.metadata.requested_range = "2016-12-31".into();
    value.metadata.evaluated_at = dt("2016-12-31T12:00:00Z");
    value.metadata.start = dt("2016-12-31T00:00:00Z");
    value.metadata.end = dt("2017-01-01T00:00:00Z");
    value.metadata.coverage = HistoryCoverage {
        retained_start: Some(leap.clone()),
        retained_end: Some(leap.clone()),
        retained_count: 1,
        evicted_through: None,
    };
    value.samples = vec![HistorySample {
        ts: leap.clone(),
        val: Kind::Number(Number::unitless(1.0)),
    }];
    for mime in ["text/zinc", "application/json;v=3", "application/json"] {
        let codec = codec_for(mime).unwrap();
        let decoded = decode_result(&encode_result(&value, codec).unwrap(), codec).unwrap();
        assert_eq!(decoded.samples[0].ts, leap, "{mime}");
    }
    let offset = chrono::FixedOffset::west_opt(17_762).unwrap();
    let historic = HDateTime::new(
        chrono::DateTime::parse_from_rfc3339("1880-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&offset),
        "New_York",
    );
    value.metadata.schema.timezone = "New_York".into();
    value.metadata.requested_range = "1880-05-31".into();
    value.metadata.evaluated_at = historic.clone();
    value.metadata.start = HDateTime::new(historic.dt - chrono::Duration::hours(1), "New_York");
    value.metadata.end = HDateTime::new(historic.dt + chrono::Duration::hours(1), "New_York");
    value.metadata.coverage.retained_start = Some(historic.clone());
    value.metadata.coverage.retained_end = Some(historic.clone());
    value.samples[0].ts = historic.clone();
    validate_result(&value).unwrap();
    for mime in ["text/zinc", "application/json;v=3", "application/json"] {
        assert!(
            encode_result(&value, codec_for(mime).unwrap()).is_err(),
            "{mime}"
        );
    }
    assert_eq!(value.samples[0].ts, historic);
}

fn native_timestamp(
    seconds: i64,
    nanos: u32,
    offset: i32,
    zone: &str,
) -> haystack_core::kinds::HDateTime {
    let source = format!(
        r#"{{"version":1,"value":{{"kind":"dateTime","seconds":"{seconds}","nanos":{nanos},"offset":{offset},"timezone":"{zone}"}}}}"#
    );
    let Kind::DateTime(value) = haystack_core::codecs::typed::decode(source.as_bytes()).unwrap()
    else {
        panic!()
    };
    value
}

#[test]
fn second_review_empty_read_headers_require_representable_timestamps() {
    for (seconds, nanos, offset, zone) in [
        (-2_827_008_000, 0, -17_762, "New_York"),
        (58, 1_500_000_000, 0, "UTC"),
        (253_402_300_800, 500_000_000, 0, "UTC"),
        (8_210_266_876_799, 0, 3600, "GMT-1"),
    ] {
        let timestamp = native_timestamp(seconds, nanos, offset, zone);
        let mut value = result();
        value.metadata.schema.timezone = zone.into();
        value.metadata.start = timestamp.clone();
        value.metadata.end = timestamp.clone();
        value.metadata.evaluated_at = timestamp.clone();
        value.metadata.coverage = HistoryCoverage {
            retained_start: None,
            retained_end: None,
            retained_count: 0,
            evicted_through: None,
        };
        value.samples.clear();
        validate_result(&value).unwrap();
        for mime in ["text/zinc", "application/json;v=3", "application/json"] {
            assert!(
                encode_result(&value, codec_for(mime).unwrap()).is_err(),
                "{mime}: seconds={seconds}, nanos={nanos}, offset={offset}"
            );
        }
        assert_eq!(value.metadata.start.dt.timestamp(), seconds);
        assert_eq!(value.metadata.start.dt.timestamp_subsec_nanos(), nanos);
        assert_eq!(value.metadata.end.dt.offset().local_minus_utc(), offset);
    }
}
#[test]
fn second_review_every_read_timestamp_is_checked_before_projection() {
    let invalid = native_timestamp(58, 1_500_000_000, 0, "UTC");
    for field in [
        "start",
        "end",
        "evaluated",
        "retainedStart",
        "retainedEnd",
        "evicted",
        "sample",
    ] {
        let mut value = result();
        value.metadata.start = native_timestamp(0, 0, 0, "UTC");
        value.metadata.end = native_timestamp(120, 0, 0, "UTC");
        value.metadata.coverage = HistoryCoverage {
            retained_start: Some(native_timestamp(30, 0, 0, "UTC")),
            retained_end: Some(native_timestamp(90, 0, 0, "UTC")),
            retained_count: 3,
            evicted_through: None,
        };
        value.samples = vec![HistorySample {
            ts: native_timestamp(60, 0, 0, "UTC"),
            val: Kind::NA,
        }];
        value.terminal = HistoryTerminal::Limited(HistoryReason::Rows);
        match field {
            "start" => value.metadata.start = invalid.clone(),
            "end" => {
                value.metadata.end = invalid.clone();
                value.samples.clear();
            }
            "evaluated" => value.metadata.evaluated_at = invalid.clone(),
            "retainedStart" => value.metadata.coverage.retained_start = Some(invalid.clone()),
            "retainedEnd" => {
                value.metadata.coverage.retained_end = Some(invalid.clone());
                value.samples.clear();
            }
            "evicted" => {
                value.metadata.coverage.evicted_through = Some(invalid.clone());
                value.metadata.coverage.retained_start = Some(native_timestamp(60, 0, 0, "UTC"));
            }
            "sample" => value.samples[0].ts = invalid.clone(),
            _ => unreachable!(),
        }
        validate_result(&value).unwrap();
        assert!(result_grid(&value).is_err(), "{field}");
    }
}
