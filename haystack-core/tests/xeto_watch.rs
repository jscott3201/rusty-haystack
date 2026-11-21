//! Independent signature oracle from ph.api/funcs.xeto:73,88,114 at
//! Xeto 873b922451d3ef4c0c9c08ef3daa542f352d69f3.
use haystack_core::{
    data::HDict,
    kinds::Kind,
    xeto::catalog::{ActivatedCatalog, Catalog},
};

#[test]
fn pinned_watch_signatures_are_post_only_and_optional_markers_remain_nullable() {
    for catalog in [
        Catalog::load_http_pinned().unwrap(),
        Catalog::load_protocol_pinned().unwrap(),
    ] {
        for name in [
            "ph.api::watchSub",
            "ph.api::watchUnsub",
            "ph.api::watchPoll",
        ] {
            let spec = catalog
                .declaration(name)
                .expect("selected pinned watch declaration");
            assert_eq!(
                spec.source.sha256,
                "15a30e0c13098b1e9ca7477b97c1e15b1b63da17bae56f83c364bc0f24f93c1b"
            );
            assert_eq!(spec.spec.meta.get("op"), Some(&Kind::Marker));
            assert!(!spec.spec.meta.contains_key("noSideEffects"));
            let slots: Vec<_> = spec
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
            if name.ends_with("watchPoll") {
                assert!(!spec.spec.meta.contains_key("opGrid"));
                assert_eq!(
                    slots,
                    [
                        ("watchId", Some("sys::Str"), false, false),
                        ("refresh", Some("sys::Marker"), true, false),
                        ("curValSub", Some("sys::Marker"), true, false),
                        ("returns", Some("sys::Grid"), false, false)
                    ]
                );
                let mut args = HDict::new();
                args.set("watchId", Kind::Str("watch".into()));
                let bound = catalog.fit_arguments(name, &args).unwrap();
                assert_eq!(bound.values().get("refresh"), Some(&Kind::Null));
                assert_eq!(bound.values().get("curValSub"), Some(&Kind::Null));
                args.set("refresh", Kind::Bool(true));
                assert!(catalog.fit_arguments(name, &args).is_err());
            } else {
                assert_eq!(spec.spec.meta.get("opGrid"), Some(&Kind::Marker));
                assert_eq!(
                    slots,
                    [
                        ("req", Some("sys::Grid"), false, false),
                        ("returns", Some("sys::Grid"), false, false)
                    ]
                );
            }
        }
        let observation = ActivatedCatalog::new(catalog, None).unwrap();
        assert!(observation.callable("ph.api::watchSub").unwrap().op_grid);
        assert!(!observation.callable("ph.api::watchPoll").unwrap().op_grid);
    }
}
