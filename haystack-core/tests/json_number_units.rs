use haystack_core::{
    codecs::{Codec, CodecError, json::Json4Codec},
    kinds::{Kind, Number},
};
#[test]
fn json_number_unit_is_absent_or_string() {
    assert_eq!(
        Json4Codec
            .decode_scalar(r#"{"_kind":"number","val":3}"#)
            .unwrap(),
        Kind::Number(Number::unitless(3.0))
    );
    assert_eq!(
        Json4Codec
            .decode_scalar(r#"{"_kind":"number","val":3,"unit":"°C"}"#)
            .unwrap(),
        Kind::Number(Number::new(3.0, Some("°C".into())))
    );
    for invalid in ["null", "false", "3", "[]", "{}"] {
        let source = format!(r#"{{"_kind":"number","val":3,"unit":{invalid}}}"#);
        assert!(
            matches!(
                Json4Codec.decode_scalar(&source),
                Err(CodecError::Parse { .. })
            ),
            "accepted unit {invalid}"
        );
    }
}
#[test]
fn malformed_number_unit_in_history_grid_is_rejected() {
    let source = r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[{"name":"ts"},{"name":"val"}],"rows":[{"ts":{"_kind":"dateTime","val":"2024-06-01T00:00:00Z","tz":"UTC"},"val":{"_kind":"number","val":3,"unit":false}}]}"#;
    assert!(matches!(
        Json4Codec.decode_grid(source),
        Err(CodecError::Parse { .. })
    ));
}
