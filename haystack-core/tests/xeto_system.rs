//! Independent literals from Xeto 873b922451d3ef4c0c9c08ef3daa542f352d69f3:
//! sys.api/funcs.xeto, sys.api/types.xeto, sys/timezones.xeto and Enums.md.
use haystack_core::{
    data::{HDict, HGrid},
    kinds::{HDateTime, HRef, Kind, NominalScalar, Uri},
    xeto::{parse_xeto, read_by_id::ReadByIdProfile},
};
use std::collections::BTreeSet;

#[test]
fn bare_keyed_enum_members_preserve_metadata_without_becoming_typed_members() {
    let ast = parse_xeto(r#"Suit: Enum { clubs <key:"Clubs">, diamonds }"#).unwrap();
    let slots = &ast.specs[0].slots;
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0].type_ref, None);
    assert_eq!(slots[0].meta.get("key"), Some(&Kind::Str("Clubs".into())));
    assert_eq!(slots[1].type_ref, None);
    assert!(slots[1].meta.is_empty());
}

fn nominal(profile: &ReadByIdProfile, name: &str, text: &str) -> Kind {
    Kind::Nominal(
        NominalScalar::new(
            name,
            &profile.provenance().repository,
            &profile.provenance().commit,
            text,
        )
        .unwrap(),
    )
}
fn about(profile: &ReadByIdProfile) -> HDict {
    let mut row = HDict::new();
    row.set("serverName", Kind::Str("test server".into()));
    let time = Kind::DateTime(HDateTime::new(
        chrono::DateTime::parse_from_rfc3339("2026-10-08T12:00:00Z").unwrap(),
        "UTC",
    ));
    row.set("serverTime", time.clone());
    row.set("serverBootTime", time);
    row.set("tz", nominal(profile, "sys::TimeZone", "UTC"));
    row.set(
        "protocolVersions",
        Kind::List(vec![Kind::Str("4".into()), Kind::Str("5".into())]),
    );
    row.set("productName", Kind::Str("rusty-haystack".into()));
    row.set("productVersion", Kind::Str("0.9.0".into()));
    row
}

#[test]
fn selected_system_functions_and_complete_keyed_timezone_are_admitted() {
    let p = ReadByIdProfile::load_http_pinned().unwrap();
    assert_eq!(
        p.operations()
            .map(|s| s.spec.qname.as_str())
            .collect::<Vec<_>>(),
        [
            "sys.api::about",
            "sys.api::close",
            "sys.api::filetypes",
            "sys.api::libs",
            "sys.api::ops",
            "sys.api::read",
            "sys.api::readAll",
            "sys.api::readById",
            "sys.api::readByIds",
        ]
    );
    let tz = p.declaration("sys::TimeZone").unwrap();
    assert_eq!(tz.source.path, "src/xeto/sys/timezones.xeto");
    assert_eq!(
        tz.source.sha256,
        "60ace34ae6b9ceb55cd19a0582c49b2129de0a4e775341b4b5455d4de7f82ab7"
    );
    assert_eq!(tz.spec.slots.len(), 341);
    let keys: BTreeSet<_> = tz
        .spec
        .slots
        .iter()
        .map(|slot| {
            assert_eq!(slot.type_ref.as_deref(), Some("sys::TimeZone"));
            match slot.meta.get("key") {
                Some(Kind::Str(key)) => key.as_str(),
                _ => slot.name.as_str(),
            }
        })
        .collect();
    assert_eq!(keys.len(), 341);
    for key in ["UTC", "New_York", "GMT+1"] {
        assert!(keys.contains(key));
    }
    for key in ["utc", "new_York", "gmtPlus1", "America/New_York"] {
        assert!(!keys.contains(key));
    }
    assert!(!tz.spec.meta.contains_key("val"));
    assert!(!p.provenance().complete_libraries);
    assert!(p.libraries().all(|lib| !lib.complete));
    for qname in ["sys::Spec", "sys.files::Json", "sys.api::watchPoll"] {
        assert!(p.declaration(qname).is_none());
    }
    let close = p.declaration("sys.api::close").unwrap();
    assert_eq!(close.spec.meta.get("op"), Some(&Kind::Marker));
    assert!(!close.spec.meta.contains_key("noSideEffects"));
    p.fit_result("sys.api::close", &Kind::None).unwrap();
    assert!(p.fit_result("sys.api::close", &Kind::Null).is_err());
    let args = p
        .fit_arguments("sys.api::readAll", &{
            let mut args = HDict::new();
            args.set("filter", nominal(&p, "sys::Filter", "site"));
            args
        })
        .unwrap();
    assert_eq!(args.values().get("opts"), Some(&Kind::Null));
}

#[test]
fn about_requires_real_typed_fields_and_exact_timezone_membership() {
    let p = ReadByIdProfile::load_http_pinned().unwrap();
    let row = about(&p);
    p.fit_result("sys.api::about", &Kind::Dict(Box::new(row.clone())))
        .unwrap();
    for field in [
        "serverName",
        "serverTime",
        "serverBootTime",
        "tz",
        "protocolVersions",
        "productName",
        "productVersion",
    ] {
        let mut invalid = row.clone();
        invalid.remove_tag(field);
        assert!(
            p.fit_result("sys.api::about", &Kind::Dict(Box::new(invalid)))
                .is_err(),
            "missing {field}"
        );
    }
    for key in ["UTC", "New_York", "GMT+1"] {
        let mut valid = row.clone();
        valid.set("tz", nominal(&p, "sys::TimeZone", key));
        p.fit_result("sys.api::about", &Kind::Dict(Box::new(valid)))
            .unwrap();
    }
    let wrong_identity = Kind::Nominal(
        NominalScalar::new(
            "sys::TimeZone",
            "other catalog",
            &p.provenance().commit,
            "UTC",
        )
        .unwrap(),
    );
    for value in [
        Kind::Str("UTC".into()),
        Kind::Bool(true),
        nominal(&p, "sys::TimeZone", "utc"),
        nominal(&p, "sys::TimeZone", "America/New_York"),
        nominal(&p, "sys::Version", "UTC"),
        wrong_identity,
    ] {
        let mut invalid = row.clone();
        invalid.set("tz", value);
        assert!(
            p.fit_result("sys.api::about", &Kind::Dict(Box::new(invalid)))
                .is_err()
        );
    }
    for (field, value) in [
        ("serverTime", Kind::Str("2026-10-08T12:00:00Z".into())),
        ("protocolVersions", Kind::List(vec![Kind::Int(5)])),
        (
            "protocolVersions",
            Kind::List(vec![Kind::Str("five".into())]),
        ),
        ("productUri", Kind::Str("https://example.test".into())),
        ("vendorName", Kind::Int(1)),
        ("whoami", Kind::Marker),
    ] {
        let mut invalid = row.clone();
        invalid.set(field, value);
        assert!(
            p.fit_result("sys.api::about", &Kind::Dict(Box::new(invalid)))
                .is_err(),
            "wrong {field}"
        );
    }
    let mut optional = row;
    optional.set("productUri", Kind::Uri(Uri::new("https://example.test")));
    optional.set("vendorName", Kind::Str("Example".into()));
    optional.set("whoami", Kind::Str("authenticated caller".into()));
    p.fit_result("sys.api::about", &Kind::Dict(Box::new(optional)))
        .unwrap();
}

#[test]
fn selected_read_arguments_and_library_filetype_rows_fit_concrete_types() {
    let p = ReadByIdProfile::load_http_pinned().unwrap();
    let mut args = HDict::new();
    args.set(
        "ids",
        Kind::List(vec![
            Kind::Ref(HRef::from_val("b")),
            Kind::Ref(HRef::from_val("b")),
        ]),
    );
    let bound = p.fit_arguments("sys.api::readByIds", &args).unwrap();
    assert_eq!(bound.values().get("checked"), Some(&Kind::Bool(true)));
    args.set("ids", Kind::List(vec![Kind::Str("b".into())]));
    assert!(p.fit_arguments("sys.api::readByIds", &args).is_err());
    let mut args = HDict::new();
    args.set("filter", Kind::Str("site".into()));
    assert!(p.fit_arguments("sys.api::read", &args).is_err());
    args.set("filter", nominal(&p, "sys::Filter", "site"));
    p.fit_arguments("sys.api::read", &args).unwrap();
    let mut lib = HDict::new();
    lib.set("name", Kind::Str("sys".into()));
    lib.set("version", nominal(&p, "sys::Version", "5.0.0"));
    let grid = |row| Kind::Grid(Box::new(HGrid::from_parts(HDict::new(), vec![], vec![row])));
    p.fit_result("sys.api::libs", &grid(lib.clone())).unwrap();
    lib.set("version", Kind::Str("5.0.0".into()));
    assert!(p.fit_result("sys.api::libs", &grid(lib)).is_err());
    let mut file = HDict::new();
    for (key, value) in [
        ("name", "zinc"),
        ("dis", "Zinc"),
        ("mime", "text/zinc"),
        ("fileExt", "zinc"),
    ] {
        file.set(key, Kind::Str(value.into()));
    }
    file.set("fileSpec", Kind::Ref(HRef::from_val("sys.files::ZincFile")));
    file.set("canRead", Kind::Bool(true));
    file.set("canWrite", Kind::Bool(true));
    p.fit_result("sys.api::filetypes", &grid(file.clone()))
        .unwrap();
    file.set("canRead", Kind::Marker);
    assert!(p.fit_result("sys.api::filetypes", &grid(file)).is_err());
}
