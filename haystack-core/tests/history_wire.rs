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
