use haystack_core::{
    codecs::{codec_for, history::HistorySample, history_mutation::*},
    data::HGrid,
    kinds::{HDateTime, Kind, Number},
};
fn request() -> HistoryWriteRequest {
    HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: [1; 16],
            point: "p".into(),
            incarnation: [2; 16],
            operation_id: "history-1".into(),
        },
        expected_generation: 9_007_199_254_740_993,
        samples: vec![HistorySample {
            ts: HDateTime::new(
                chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00.123456789Z").unwrap(),
                "UTC",
            ),
            val: Kind::Number(Number::new(
                f64::from_bits(0x3fd5555555555555),
                Some("°F".into()),
            )),
        }],
    }
}
#[test]
fn original_order_zone_number_bits_and_unit_spelling_survive_all_h4_codecs() {
    let mut request = request();
    request.samples.push(HistorySample {
        val: Kind::NA,
        ..request.samples[0].clone()
    });
    let canonical = canonical_request(&request).unwrap();
    for mime in ["text/zinc", "application/json;v=3", "application/json"] {
        let codec = codec_for(mime).unwrap();
        let bytes = encode_request(&request, codec).unwrap();
        let decoded = decode_request(&bytes, codec).unwrap();
        assert_eq!(canonical_request(&decoded).unwrap(), canonical, "{mime}");
        assert_eq!(decoded.expected_generation, 9_007_199_254_740_993);
    }
    for mutation in [0, 1, 2, 3] {
        let mut changed = request.clone();
        match mutation {
            0 => changed.samples.reverse(),
            1 => changed.samples[0].ts.tz_name = "GMT".into(),
            2 => {
                if let Kind::Number(n) = &mut changed.samples[0].val {
                    n.unit = Some("fahrenheit".into());
                }
            }
            _ => changed.expected_generation += 1,
        }
        assert_ne!(canonical_request(&changed).unwrap(), canonical);
    }
}
#[test]
fn native_types_and_noncanonical_nan_do_not_project_into_a_wire_write() {
    for value in [
        Kind::Int(4),
        Kind::Null,
        Kind::Number(Number::unitless(f64::from_bits(0xfff8000000000001))),
        Kind::Number(Number::new(f64::NAN, Some("°F".into()))),
    ] {
        let mut request = request();
        request.samples[0].val = value;
        assert!(canonical_request(&request).is_ok());
        assert!(request_grid(&request).is_err());
    }
    let mut request = request();
    request.samples[0].val = Kind::Number(Number::unitless(f64::NAN));
    assert!(request_grid(&request).is_ok());
}
#[test]
fn receipts_preserve_exact_state_and_err_does_not_erase_typed_outcome() {
    let request = request();
    let outcome = HistoryWriteOutcome::Committed(HistoryWriteReceipt {
        identity: request.identity.clone(),
        before_generation: request.expected_generation,
        after_generation: request.expected_generation + 1,
        change_sequence: u64::MAX,
        submitted_samples: 1,
        unique_samples: 1,
        retained_samples: 1,
        evicted_samples: 0,
        qualification: HistoryReceiptQualification::EphemeralMemory,
    });
    for mime in ["text/zinc", "application/json;v=3", "application/json"] {
        let codec = codec_for(mime).unwrap();
        assert_eq!(
            decode_lookup(&encode_lookup(&request.identity, codec).unwrap(), codec).unwrap(),
            request.identity
        );
        let mut grid = outcome_grid(&outcome).unwrap();
        grid.meta.set("err", Kind::Marker);
        let bytes = codec.encode_grid(&grid).unwrap();
        let decoded = decode_outcome(bytes.as_bytes(), codec).unwrap();
        assert_eq!(decoded, outcome);
        validate_for_request(&decoded, &request).unwrap();
    }
    assert!(outcome_from_grid(&HGrid::new()).is_err());
    let mut changed = request.clone();
    changed.expected_generation += 1;
    assert!(validate_for_request(&outcome, &changed).is_err());
}
#[test]
fn controls_missing_values_malformed_units_and_unbounded_source_are_rejected() {
    let mut grid = request_grid(&request()).unwrap();
    grid.rows[0].remove_tag("val");
    assert!(request_from_grid(&grid).is_err());
    let mut grid = request_grid(&request()).unwrap();
    grid.meta.remove_tag("historyWrite");
    assert!(request_from_grid(&grid).is_err());
    let mut request = request();
    request.samples[0].val = Kind::Str("x".repeat(MAX_SOURCE_BYTES));
    assert!(request.source_bytes().is_err());
    let codec = codec_for("application/json").unwrap();
    let body = encode_request(&crate::request(), codec).unwrap();
    let body = String::from_utf8(body)
        .unwrap()
        .replace("\"unit\":\"°F\"", "\"unit\":null");
    assert!(decode_request(body.as_bytes(), codec).is_err());
}

#[test]
fn all_admitted_special_and_finite_number_bits_and_aliases_survive_wire_identity() {
    for bits in [
        0,
        1 << 63,
        1,
        0x0010000000000000,
        0x3fefffffffffffff,
        0x3fd5555555555555,
        0x7fefffffffffffff,
        f64::INFINITY.to_bits(),
        f64::NEG_INFINITY.to_bits(),
        f64::NAN.to_bits(),
    ] {
        let mut request = request();
        request.samples[0].val = Kind::Number(Number::unitless(f64::from_bits(bits)));
        let canonical = canonical_request(&request).unwrap();
        for mime in ["text/zinc", "application/json;v=3", "application/json"] {
            let codec = codec_for(mime).unwrap();
            assert_eq!(
                canonical_request(
                    &decode_request(&encode_request(&request, codec).unwrap(), codec).unwrap()
                )
                .unwrap(),
                canonical,
                "{mime}: {bits:x}"
            );
        }
    }
    for unit in ["USD", "$", "fahrenheit", "°F"] {
        let mut request = request();
        request.samples[0].val = Kind::Number(Number::new(42.0, Some(unit.into())));
        for mime in ["text/zinc", "application/json;v=3", "application/json"] {
            let codec = codec_for(mime).unwrap();
            assert_eq!(
                canonical_request(
                    &decode_request(&encode_request(&request, codec).unwrap(), codec).unwrap()
                )
                .unwrap(),
                canonical_request(&request).unwrap()
            );
        }
    }
    let mut invalid = request();
    invalid.samples[0].val = Kind::Number(Number::new(1.0, Some("".into())));
    assert!(request_grid(&invalid).is_err());
    invalid = request();
    invalid.samples[0].ts.tz_name.clear();
    assert!(request_grid(&invalid).is_err());
}

#[test]
fn special_number_units_reach_history_admission_without_erasure() {
    for value in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        for unit in ["°F", "fahrenheit", "$"] {
            let mut grid = request_grid(&request()).unwrap();
            grid.rows[0].set("val", Kind::Number(Number::new(value, Some(unit.into()))));
            for mime in ["text/zinc", "application/json;v=3", "application/json"] {
                let codec = codec_for(mime).unwrap();
                let decoded =
                    decode_request(codec.encode_grid(&grid).unwrap().as_bytes(), codec).unwrap();
                let Kind::Number(number) = &decoded.samples[0].val else {
                    panic!()
                };
                assert_eq!(number.val.to_bits(), value.to_bits(), "{mime}");
                assert_eq!(number.unit.as_deref(), Some(unit), "{mime}: {value}{unit}");
            }
        }
    }
}

#[test]
fn scoped_zinc_rejects_unconsumed_sample_suffixes_and_surplus_cells() {
    let codec = codec_for("text/zinc").unwrap();
    let mut grid = request_grid(&request()).unwrap();
    grid.rows[0].set("val", Kind::Number(Number::unitless(17.0)));
    let encoded = codec.encode_grid(&grid).unwrap();
    let lines = encoded.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    for row in [
        format!("{},unused", lines[2]),
        lines[2].replace(",17", ",NaN1"),
        lines[2].replace(",17", ",-INF1"),
    ] {
        let source = format!("{}\n{}\n{row}\n", lines[0], lines[1]);
        assert!(decode_request(source.as_bytes(), codec).is_err(), "{row}");
    }
}

#[test]
fn zinc_special_number_scalar_units_and_inf_prefixed_xstr_remain_distinct() {
    let codec = codec_for("text/zinc").unwrap();
    for (source, bits, unit) in [
        ("INF°F", f64::INFINITY.to_bits(), "°F"),
        ("-INFfahrenheit", f64::NEG_INFINITY.to_bits(), "fahrenheit"),
        ("NaN$", f64::NAN.to_bits(), "$"),
    ] {
        let Kind::Number(number) = codec.decode_scalar(source).unwrap() else {
            panic!()
        };
        assert_eq!(number.val.to_bits(), bits);
        assert_eq!(number.unit.as_deref(), Some(unit));
    }
    for source in ["INFType(\"value\")", "NaNType(\"value\")"] {
        assert!(matches!(
            codec.decode_scalar(source).unwrap(),
            Kind::XStr(_)
        ));
    }
}
