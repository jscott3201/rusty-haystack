//! Literal keyed-enum examples from pinned Enums.md plus complete TimeZone.
use haystack_core::{
    codecs::jeto::{self, Boxing, Context, Definition, Encoding, Limits},
    kinds::{Kind, NominalScalar},
    xeto::catalog::Catalog,
};
fn context() -> Context {
    let p = Catalog::load_http_pinned().unwrap();
    Context::new(
        &p.provenance().repository,
        &p.provenance().commit,
        vec![
            Definition::Enum {
                name: "sys::TimeZone".into(),
                keys: p.enum_keys("sys::TimeZone").unwrap().to_vec(),
            },
            Definition::Enum {
                name: "test::Suit".into(),
                keys: vec!["Clubs".into(), "diamonds".into()],
            },
        ],
    )
    .unwrap()
}
#[test]
fn finite_enum_context_preserves_exact_key_and_nominal_identity() {
    let context = context();
    for (name, key) in [
        ("sys::TimeZone", "UTC"),
        ("sys::TimeZone", "New_York"),
        ("sys::TimeZone", "GMT+1"),
        ("test::Suit", "Clubs"),
        ("test::Suit", "diamonds"),
    ] {
        let native = Kind::Nominal(
            NominalScalar::new(name, context.catalog(), context.revision(), key).unwrap(),
        );
        let plain = serde_json::to_vec(key).unwrap();
        assert_eq!(
            jeto::decode(&plain, &context, Some(name), Limits::default()).unwrap(),
            native
        );
        let boxed = serde_json::to_vec(&serde_json::json!({"spec":name,"val":key})).unwrap();
        assert_eq!(
            jeto::decode(&boxed, &context, None, Limits::default()).unwrap(),
            native
        );
        assert_eq!(
            jeto::encode(
                &native,
                &context,
                Some(name),
                Boxing::Auto,
                Limits::default()
            )
            .unwrap()
            .into_exact()
            .unwrap(),
            plain
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &jeto::encode(&native, &context, None, Boxing::Auto, Limits::default())
                    .unwrap()
                    .into_exact()
                    .unwrap()
            )
            .unwrap(),
            serde_json::from_slice::<serde_json::Value>(&boxed).unwrap()
        );
        assert!(matches!(
            jeto::encode(
                &Kind::Str(key.into()),
                &context,
                Some(name),
                Boxing::Auto,
                Limits::default()
            )
            .unwrap(),
            Encoding::Unsupported { .. }
        ));
    }
    for (name, key) in [
        ("sys::TimeZone", "utc"),
        ("sys::TimeZone", "new_York"),
        ("sys::TimeZone", "America/New_York"),
        ("sys::TimeZone", "unknown"),
        ("test::Suit", "clubs"),
    ] {
        assert!(
            jeto::decode(
                &serde_json::to_vec(key).unwrap(),
                &context,
                Some(name),
                Limits::default()
            )
            .is_err()
        );
    }
    for value in [
        NominalScalar::new("sys::TimeZone", "wrong", context.revision(), "UTC").unwrap(),
        NominalScalar::new("sys::TimeZone", context.catalog(), "wrong", "UTC").unwrap(),
        NominalScalar::new(
            "sys::TimeZone",
            context.catalog(),
            context.revision(),
            "utc",
        )
        .unwrap(),
    ] {
        assert!(matches!(
            jeto::encode(
                &Kind::Nominal(value),
                &context,
                Some("sys::TimeZone"),
                Boxing::Auto,
                Limits::default()
            )
            .unwrap(),
            Encoding::Unsupported { .. }
        ));
    }
}
#[test]
fn finite_enum_tables_are_bounded_and_reject_duplicate_effective_keys() {
    for keys in [
        vec![],
        vec![String::new()],
        (0..257)
            .map(|i| format!("{i:03}{}", "x".repeat(253)))
            .collect(),
        vec!["UTC".into(), "UTC".into()],
        vec!["x".repeat(257)],
        (0..4097).map(|i| i.to_string()).collect(),
    ] {
        assert!(
            Context::new(
                "test",
                "1",
                vec![Definition::Enum {
                    name: "test::Range".into(),
                    keys
                }]
            )
            .is_err()
        );
    }
    let p = Catalog::load_http_pinned().unwrap();
    let context = context();
    for key in p.enum_keys("sys::TimeZone").unwrap() {
        let actual = jeto::decode(
            &serde_json::to_vec(key).unwrap(),
            &context,
            Some("sys::TimeZone"),
            Limits::default(),
        )
        .unwrap();
        assert!(
            matches!(actual, Kind::Nominal(value) if value.spec() == "sys::TimeZone" && value.text() == key)
        );
    }
}
