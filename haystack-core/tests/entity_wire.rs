use haystack_core::{
    codecs::{
        codec_for,
        entity::{self, *},
    },
    data::HDict,
    graph::{CommitSpan, EntityOperation},
    kinds::{Float, HRef, Kind, Number},
};
fn identity() -> OperationIdentity {
    OperationIdentity {
        operation_id: "one".into(),
        dataset: [1; 16],
        incarnation: [2; 16],
    }
}
fn request(value: Kind) -> EntityBatchRequest {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("one")));
    row.set("value", value);
    EntityBatchRequest {
        identity: identity(),
        expected_revision: (1u64 << 53) + 1,
        operations: vec![EntityOperation::Add(row)],
    }
}
#[test]
fn typed_str_envelope_preserves_rich_values_and_exact_unsigned_positions_in_all_admitted_codecs() {
    let r = request(Kind::Int(i64::MAX));
    let expected = entity::encode(&r).unwrap();
    for mime in [
        "text/zinc",
        "application/json",
        "application/json;v=3",
        "text/trio",
    ] {
        let codec = codec_for(mime).unwrap();
        let grid = entity::to_grid(&r).unwrap();
        let wire = codec.encode_grid(&grid).unwrap();
        let got: EntityBatchRequest =
            entity::from_grid(&codec.decode_grid(&wire).unwrap()).unwrap();
        assert_eq!(entity::encode(&got).unwrap(), expected);
    }
    let receipt = MutationOutcome::Committed(EntityReceipt {
        identity: identity(),
        before_revision: u64::MAX - 1,
        after_revision: u64::MAX,
        span: Some(CommitSpan {
            first: u64::MAX,
            last: u64::MAX,
        }),
        qualification: ReceiptQualification::EphemeralMemory,
    });
    assert_eq!(
        entity::decode::<MutationOutcome>(&entity::encode(&receipt).unwrap()).unwrap(),
        receipt
    );
}
#[test]
fn canonical_binding_retains_variants_float_bits_units_ref_display_and_operation_order() {
    let variants = [
        Kind::Int(0),
        Kind::Float(Float::new(0.0)),
        Kind::Float(Float::new(-0.0)),
        Kind::Number(Number::unitless(0.0)),
        Kind::Number(Number::new(0.0, Some("°C".into()))),
        Kind::Float(Float::new(f64::from_bits(0x7ff8000000000001))),
        Kind::Float(Float::new(f64::from_bits(0x7ff8000000000002))),
        Kind::Ref(HRef::new("target", Some("old".into()))),
        Kind::Ref(HRef::new("target", Some("new".into()))),
    ];
    let encoded: Vec<_> = variants
        .into_iter()
        .map(|v| entity::encode(&request(v)).unwrap())
        .collect();
    for (i, a) in encoded.iter().enumerate() {
        for b in &encoded[i + 1..] {
            assert_ne!(a, b)
        }
    }
    let mut a = request(Kind::Marker);
    a.operations
        .push(EntityOperation::Remove { id: "other".into() });
    let mut b = a.clone();
    b.operations.reverse();
    assert_ne!(entity::encode(&a).unwrap(), entity::encode(&b).unwrap());
    let mut first = HDict::new();
    first.set("a", Kind::Marker);
    first.set("z", Kind::NA);
    let mut second = HDict::new();
    second.set("z", Kind::NA);
    second.set("a", Kind::Marker);
    assert_eq!(
        entity::encode(&request(Kind::Dict(Box::new(first)))).unwrap(),
        entity::encode(&request(Kind::Dict(Box::new(second)))).unwrap()
    );
}
#[test]
fn strict_schema_rejects_legacy_unknown_fields_noncanonical_revision_and_invalid_receipt() {
    use entity::EntityWire;
    let mut v = request(Kind::Marker).to_kind().unwrap();
    let Kind::Dict(root) = &mut v else { panic!() };
    root.set("unknown", Kind::Marker);
    assert!(EntityBatchRequest::from_kind(&v).is_err());
    let mut v = request(Kind::Marker).to_kind().unwrap();
    let Kind::Dict(root) = &mut v else { panic!() };
    let mut body = root.get("body").unwrap().clone();
    let Kind::Dict(d) = &mut body else { panic!() };
    d.set("expectedRevision", Kind::Str("01".into()));
    root.set("body", body);
    assert!(EntityBatchRequest::from_kind(&v).is_err());
    assert!(entity::from_grid::<ChangesRequest>(&haystack_core::data::HGrid::new()).is_err());
    let invalid = MutationOutcome::Committed(EntityReceipt {
        identity: identity(),
        before_revision: 10,
        after_revision: 12,
        span: Some(CommitSpan {
            first: 12,
            last: 12,
        }),
        qualification: ReceiptQualification::EphemeralMemory,
    });
    assert!(entity::encode(&invalid).is_err());
}

#[test]
fn identity_binding_preserves_datetime_representation_even_for_the_same_instant() {
    use haystack_core::kinds::HDateTime;
    let first = Kind::DateTime(HDateTime::new(
        chrono::DateTime::parse_from_rfc3339("2026-10-08T00:00:00+00:00").unwrap(),
        "UTC",
    ));
    let second = Kind::DateTime(HDateTime::new(
        chrono::DateTime::parse_from_rfc3339("2026-10-07T20:00:00-04:00").unwrap(),
        "New_York",
    ));
    assert_ne!(
        entity::encode(&request(first)).unwrap(),
        entity::encode(&request(second)).unwrap()
    );
}
