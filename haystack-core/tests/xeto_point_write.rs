//! Independent oracle: Xeto 873b922451d3ef4c0c9c08ef3daa542f352d69f3,
//! src/xeto/ph.api/funcs.xeto:145. Dynamic val remains Obj, not Number.
use haystack_core::{
    data::{HDict, HGrid},
    kinds::{HRef, Kind, Number},
    xeto::catalog::Catalog,
};

#[test]
fn pinned_point_write_signature_preserves_action_presence_and_dynamic_value_identity() {
    for catalog in [
        Catalog::load_http_pinned().unwrap(),
        Catalog::load_protocol_pinned().unwrap(),
    ] {
        let name = "ph.api::pointWrite";
        let declaration = catalog
            .declaration(name)
            .expect("selected pinned pointWrite declaration");
        assert_eq!(
            declaration.source.sha256,
            "15a30e0c13098b1e9ca7477b97c1e15b1b63da17bae56f83c364bc0f24f93c1b"
        );
        assert_eq!(declaration.spec.meta.get("op"), Some(&Kind::Marker));
        assert!(!declaration.spec.meta.contains_key("opGrid"));
        assert!(!declaration.spec.meta.contains_key("noSideEffects"));
        let slots: Vec<_> = declaration
            .spec
            .slots
            .iter()
            .map(|s| {
                (
                    s.name.as_str(),
                    s.type_ref.as_deref(),
                    s.is_maybe(),
                    s.default.is_some(),
                )
            })
            .collect();
        assert_eq!(
            slots,
            [
                ("id", Some("sys::Ref"), false, false),
                ("level", Some("sys::Number"), true, false),
                ("val", Some("sys::Obj"), true, false),
                ("who", Some("sys::Str"), true, false),
                ("duration", Some("sys::Number"), true, false),
                ("returns", Some("sys::Grid"), false, false),
            ]
        );
        let mut args = HDict::new();
        args.set("id", Kind::Ref(HRef::from_val("p")));
        let bound = catalog.fit_arguments(name, &args).unwrap();
        for slot in ["level", "val", "who", "duration"] {
            assert_eq!(bound.values().get(slot), Some(&Kind::Null));
        }
        args.set("level", Kind::Number(Number::unitless(8.0)));
        args.set("val", Kind::Int(42));
        assert_eq!(
            catalog
                .fit_arguments(name, &args)
                .unwrap()
                .values()
                .get("val"),
            Some(&Kind::Int(42))
        );
        args.set("level", Kind::Int(8));
        assert!(catalog.fit_arguments(name, &args).is_err());
        assert!(
            catalog
                .fit_result(name, &Kind::Grid(Box::new(HGrid::new())))
                .is_ok()
        );
    }
}
