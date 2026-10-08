//! Independently transcribed signature from Project-Haystack/xeto at
//! 873b922451d3ef4c0c9c08ef3daa542f352d69f3, src/xeto/sys.api/funcs.xeto.
use haystack_core::xeto::parse_xeto;

#[test]
fn pinned_read_by_id_signature_parses() {
    let parsed = parse_xeto(
        "+Funcs { readById: Func <op, noSideEffects> { id: Ref?, checked: Bool \"true\", returns: Dict? } }",
    ).expect("the selected upstream function grammar must parse");
    assert_eq!(parsed.specs.len(), 1);
    let function = &parsed.specs[0].slots[0];
    assert_eq!(function.name, "readById");
    assert_eq!(function.children.len(), 3);
    assert!(function.children[0].is_maybe);
    assert!(function.children[2].is_maybe);
}

use haystack_core::data::HDict;
use haystack_core::kinds::{Float, HRef, Kind};
use haystack_core::xeto::read_by_id::{
    ArgumentOrigin, ProfileError, READ_BY_ID_UPSTREAM_COMMIT, ReadByIdProfile,
};
use std::collections::BTreeSet;

const OP: &str = "sys.api::readById";

#[test]
fn catalog_matches_independent_pinned_declaration_oracle() {
    let profile = ReadByIdProfile::load_pinned().unwrap();
    // These expectations were transcribed from upstream, not generated from the
    // extraction manifest or this implementation's resolution output.
    let expected = [
        ("sys::Obj", None),
        ("sys::Scalar", Some("sys::Obj")),
        ("sys::Marker", Some("sys::Scalar")),
        ("sys::Str", Some("sys::Scalar")),
        ("sys::Bool", Some("sys::Scalar")),
        ("sys::Ref", Some("sys::Scalar")),
        ("sys::Collection", Some("sys::Obj")),
        ("sys::Dict", Some("sys::Collection")),
        ("sys::Interface", Some("sys::Dict")),
        ("sys::Func", Some("sys::Dict")),
        ("sys::Funcs", Some("sys::Interface")),
        (OP, Some("sys::Func")),
    ];
    assert_eq!(profile.declarations().count(), expected.len());
    for (qname, base) in expected {
        assert_eq!(
            profile.declaration(qname).unwrap().spec.base.as_deref(),
            base,
            "{qname}"
        );
    }
    let function = profile.declaration(OP).unwrap();
    assert_eq!(function.spec.lib, "sys.api");
    assert_eq!(function.member_of.as_deref(), Some("sys::Funcs"));
    assert_eq!(function.source.path, "src/xeto/sys.api/funcs.xeto");
    assert_eq!(function.spec.meta.get("op"), Some(&Kind::Marker));
    assert_eq!(function.spec.meta.get("noSideEffects"), Some(&Kind::Marker));
    let slots = &function.spec.slots;
    assert_eq!(
        slots.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["id", "checked", "returns"]
    );
    assert_eq!(slots[0].type_ref.as_deref(), Some("sys::Ref"));
    assert!(slots[0].is_maybe());
    assert_eq!(slots[0].default, None);
    assert_eq!(slots[1].type_ref.as_deref(), Some("sys::Bool"));
    assert_eq!(slots[1].default, Some(Kind::Bool(true)));
    assert_eq!(slots[1].meta.get("val"), Some(&Kind::Bool(true)));
    assert!(!slots[1].is_maybe());
    assert_eq!(slots[2].type_ref.as_deref(), Some("sys::Dict"));
    assert!(slots[2].is_maybe());
    let inherited_return = &profile.declaration("sys::Func").unwrap().spec.slots[0];
    assert_eq!(inherited_return.name, "returns");
    assert_eq!(inherited_return.type_ref.as_deref(), Some("sys::Obj"));
    assert!(inherited_return.is_maybe());
    assert_eq!(
        profile
            .declaration("sys::Bool")
            .unwrap()
            .spec
            .meta
            .get("val"),
        Some(&Kind::Bool(false))
    );
    assert_eq!(
        profile
            .declaration("sys::Ref")
            .unwrap()
            .spec
            .meta
            .get("val"),
        Some(&Kind::Ref(HRef::from_val("x")))
    );
    assert_eq!(
        profile
            .declaration("sys::Marker")
            .unwrap()
            .spec
            .meta
            .get("val"),
        Some(&Kind::Marker)
    );
    let augmentation = &profile.augmentations()[0];
    assert_eq!(augmentation.library, "sys.api");
    assert_eq!(augmentation.target, "sys::Funcs");
    assert_eq!(augmentation.members, [OP]);
    assert_eq!(augmentation.meta.get("mixin"), Some(&Kind::Marker));
}

#[test]
fn discovery_reports_only_the_same_admitted_subset() {
    let profile = ReadByIdProfile::load_pinned().unwrap();
    assert_eq!(
        profile
            .operations()
            .map(|s| s.spec.qname.as_str())
            .collect::<Vec<_>>(),
        [OP]
    );
    let mut discovered = BTreeSet::new();
    for library in profile.libraries() {
        assert!(!library.complete);
        assert_eq!(library.version, "5.0.0");
        assert_eq!(library.maturity, "alpha");
        for qname in &library.declarations {
            let declaration = profile.declaration(qname).unwrap();
            assert_eq!(declaration.spec.lib, library.name);
            discovered.insert(qname.clone());
        }
        if library.name == "sys.api" {
            assert_eq!(library.depends, ["sys"]);
            assert_eq!(library.declarations, [OP]);
        } else {
            assert!(library.depends.is_empty());
            assert_eq!(library.metadata_fields.len(), 10);
        }
    }
    assert_eq!(
        discovered,
        profile
            .declarations()
            .map(|s| s.spec.qname.clone())
            .collect()
    );
    for unadmitted in [
        "sys::Spec",
        "sys::Lib",
        "sys::Entity",
        "sys::Int",
        "sys.api::read",
        "sys.api::Funcs",
    ] {
        assert!(profile.declaration(unadmitted).is_none(), "{unadmitted}");
    }
    assert_eq!(
        profile
            .metadata()
            .map(|m| m.slot.name.as_str())
            .collect::<Vec<_>>(),
        [
            "abstract",
            "doc",
            "maybe",
            "mixin",
            "noInherit",
            "noSideEffects",
            "op",
            "pattern",
            "sealed",
            "val"
        ]
    );
    assert_eq!(profile.provenance().commit, READ_BY_ID_UPSTREAM_COMMIT);
    assert_eq!(profile.provenance().files.len(), 7);
    assert!(!profile.provenance().complete_libraries);
    let funcs = profile
        .provenance()
        .files
        .iter()
        .find(|f| f.path.ends_with("sys.api/funcs.xeto"))
        .unwrap();
    assert_eq!(
        funcs.sha256,
        "e1930a35a9a9c5e15d2ad04456a396319ac88b16d3902ac432a500d2505bba0e"
    );
}

#[test]
fn no_inherit_metadata_is_applied_from_the_selected_schema() {
    let profile = ReadByIdProfile::load_pinned().unwrap();
    let scalar = profile.effective_metadata("sys::Scalar").unwrap();
    assert!(scalar.contains_key("abstract"));
    assert!(!scalar.contains_key("sealed"));
    let reference = profile.effective_metadata("sys::Ref").unwrap();
    assert!(!reference.contains_key("abstract"));
    assert!(!reference.contains_key("sealed"));
    assert!(reference.contains_key("pattern"));
}

#[test]
fn native_binding_retains_presence_and_parameter_default_origins() {
    let profile = ReadByIdProfile::load_pinned().unwrap();
    // Independent behavior oracle: Haxall Api5Test.fan at
    // aded27993c2d4834eca4b44ca55671417a1a7ea2, lines 70-71 and 228-231.
    // Omitted id binds null, never the Ref construction default @x.
    let input = HDict::new();
    let args = profile.fit_arguments(OP, &input).unwrap();
    assert!(input.is_empty());
    assert_eq!(args.values().get("id"), Some(&Kind::Null));
    assert_eq!(args.origin("id"), Some(ArgumentOrigin::MissingNullable));
    assert_eq!(args.values().get("checked"), Some(&Kind::Bool(true)));
    assert_eq!(
        args.origin("checked"),
        Some(ArgumentOrigin::ParameterDefault)
    );
    assert!(!args.values().has("returns"));
    let mut input = HDict::new();
    input.set("id", Kind::Null);
    input.set("checked", Kind::Bool(false));
    let args = profile.fit_arguments(OP, &input).unwrap();
    assert_eq!(args.values(), &input);
    assert_eq!(args.origin("id"), Some(ArgumentOrigin::Explicit));
    assert_eq!(args.origin("checked"), Some(ArgumentOrigin::Explicit));
    input.set("id", Kind::Ref(HRef::from_val("site-1")));
    assert_eq!(profile.fit_arguments(OP, &input).unwrap().values(), &input);
}

#[test]
fn present_wrong_kinds_do_not_fit_nullable_or_defaulted_parameters() {
    let profile = ReadByIdProfile::load_pinned().unwrap();
    for (name, value) in [
        ("id", Kind::Str("site-1".into())),
        ("id", Kind::Bool(false)),
        ("id", Kind::None),
        ("id", Kind::Ref(HRef::from_val("bad ref"))),
        ("checked", Kind::Null),
        ("checked", Kind::None),
        ("checked", Kind::Str("true".into())),
        ("checked", Kind::Marker),
    ] {
        let mut input = HDict::new();
        input.set(name, value);
        match profile.fit_arguments(OP, &input).unwrap_err() {
            ProfileError::Fit { path, slot, .. } => {
                assert_eq!(path, "src/xeto/sys.api/funcs.xeto");
                assert_eq!(slot, format!("{OP}.{name}"));
            }
            other => panic!("expected fitting diagnostic, got {other}"),
        }
    }
    for name in ["returns", "other"] {
        let mut input = HDict::new();
        input.set(name, Kind::Null);
        assert!(
            matches!(profile.fit_arguments(OP, &input), Err(ProfileError::Fit { message, .. }) if message == "unknown argument")
        );
    }
}

#[test]
fn nullable_result_is_dict_or_null_and_keeps_rich_dict_contents() {
    let profile = ReadByIdProfile::load_pinned().unwrap();
    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("site-1")));
    entity.set("count", Kind::Int(i64::MAX));
    entity.set("precise", Kind::Float(Float::new(-0.0)));
    entity.set("typedAbsence", Kind::None);
    entity.set("explicitNull", Kind::Null);
    entity.set("payload", Kind::Buf(vec![0, 255]));
    let value = Kind::Dict(Box::new(entity));
    profile.fit_result(OP, &value).unwrap();
    profile.fit_result(OP, &Kind::Null).unwrap();
    for wrong in [
        Kind::None,
        Kind::List(vec![]),
        Kind::Str("dict".into()),
        Kind::Bool(false),
    ] {
        assert!(
            matches!(profile.fit_result(OP, &wrong), Err(ProfileError::Fit { slot, .. }) if slot.ends_with(".returns"))
        );
    }
    assert!(
        profile
            .fit_arguments("sys.api::read", &HDict::new())
            .is_err()
    );
}

#[test]
fn compatibility_loader_cannot_misreport_an_augmentation_as_a_type() {
    let ns = haystack_core::ontology::DefNamespace::new();
    let result = haystack_core::xeto::loader::load_xeto_source(
        "+Funcs { readById: Func { returns: Dict? } }",
        "sys.api",
        &ns,
    );
    assert!(
        matches!(result, Err(haystack_core::xeto::XetoError::Load(message)) if message.contains("admission profile"))
    );
}

#[test]
fn parser_rejects_excessive_nesting_and_duplicate_metadata() {
    let deeply_nested = format!("{}{}", "T: Dict { ".repeat(65), "}".repeat(65));
    assert!(
        matches!(parse_xeto(&deeply_nested), Err(haystack_core::xeto::XetoError::Parse { message, .. })
        if message.contains("nesting exceeds 64"))
    );
    assert!(
        matches!(parse_xeto("T: Dict <op, op>"), Err(haystack_core::xeto::XetoError::Parse { message, .. })
        if message.contains("duplicate metadata"))
    );
}

#[test]
fn parser_preserves_augmentation_qualified_member_refs_and_type_defaults() {
    let parsed = parse_xeto("+Funcs { f: Func { x: sys::Func.returns } }").unwrap();
    assert!(parsed.specs[0].is_augmentation);
    assert_eq!(
        parsed.specs[0].slots[0].children[0].type_ref.as_deref(),
        Some("sys::Func.returns")
    );
    let scalar = parse_xeto("Bool: Scalar <sealed> \"false\"").unwrap();
    let spec = haystack_core::xeto::spec::spec_from_def(&scalar.specs[0], "sys");
    assert_eq!(spec.meta.get("val"), Some(&Kind::Str("false".into())));
}

#[test]
fn pinned_http_closure_is_reachable_and_error_fields_fit_their_declared_types() {
    let profile = ReadByIdProfile::load_http_pinned().unwrap();
    assert_eq!(profile.provenance().profile, "pinned-xeto-readById-http");
    assert_eq!(profile.provenance().commit, READ_BY_ID_UPSTREAM_COMMIT);
    for name in [
        "sys::Number",
        "sys::Int",
        "sys::List",
        "sys.api::ApiVersion",
        "sys.api::ApiErr",
        "sys.api::UnknownFuncErr",
        "sys.api::UnsupportedVersionErr",
        "sys.api::AmbiguousFuncErr",
        "sys.api::MethodNotAllowedErr",
    ] {
        assert!(profile.declaration(name).is_some(), "{name}");
    }
    for name in [
        "sys.api::RateLimitErr",
        "sys.api::UnknownProjErr",
        "sys::Spec",
    ] {
        assert!(profile.declaration(name).is_none(), "{name}");
    }
    let version = profile
        .declaration("sys.api::UnsupportedVersionErr")
        .unwrap();
    assert_eq!(version.spec.slots[0].type_ref.as_deref(), Some("sys::List"));
    assert_eq!(
        version.spec.slots[0].meta.get("of"),
        Some(&Kind::Ref(HRef::from_val("sys.api::ApiVersion")))
    );
    let mut error = HDict::new();
    error.set("status", Kind::Int(400));
    error.set("dis", Kind::Str("Unsupported version".into()));
    error.set(
        "allow",
        Kind::List(vec![Kind::Str("4".into()), Kind::Str("5".into())]),
    );
    profile
        .fit_api_error("sys.api::UnsupportedVersionErr", &error)
        .unwrap();
    error.set("allow", Kind::List(vec![Kind::Str("five".into())]));
    assert!(
        profile
            .fit_api_error("sys.api::UnsupportedVersionErr", &error)
            .is_err()
    );
    error.remove_tag("allow");
    assert!(
        profile
            .fit_api_error("sys.api::UnsupportedVersionErr", &error)
            .is_err()
    );
    error.set(
        "status",
        Kind::Number(haystack_core::kinds::Number::unitless(400.0)),
    );
    assert!(profile.fit_api_error("sys.api::ApiErr", &error).is_err());
    assert!(
        profile
            .fit_api_error("sys.api::RateLimitErr", &error)
            .is_err()
    );
}
