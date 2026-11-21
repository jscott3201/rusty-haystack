//! Independent pinned signature oracle: ph.api/funcs.xeto lines 199 and 227,
//! Xeto 873b922451d3ef4c0c9c08ef3daa542f352d69f3. Storage controls remain a
//! project extension; this generic Grid declaration supplies no receipt type.
use haystack_core::{
    data::{HDict, HGrid},
    kinds::Kind,
    xeto::catalog::{ActivatedCatalog, Catalog},
};
#[test]
fn history_signatures_whole_grid_and_method_metadata_match_the_exact_pin() {
    for catalog in [
        Catalog::load_http_pinned().unwrap(),
        Catalog::load_protocol_pinned().unwrap(),
    ] {
        for (name, read_only) in [("ph.api::hisRead", true), ("ph.api::hisWrite", false)] {
            let spec = catalog.declaration(name).unwrap();
            assert_eq!(spec.source.path, "src/xeto/ph.api/funcs.xeto");
            assert_eq!(
                spec.source.sha256,
                "15a30e0c13098b1e9ca7477b97c1e15b1b63da17bae56f83c364bc0f24f93c1b"
            );
            assert_eq!(spec.member_of.as_deref(), Some("sys::Funcs"));
            assert_eq!(spec.spec.base.as_deref(), Some("sys::Func"));
            assert_eq!(spec.spec.meta.get("op"), Some(&Kind::Marker));
            assert_eq!(spec.spec.meta.get("opGrid"), Some(&Kind::Marker));
            assert_eq!(spec.spec.meta.contains_key("noSideEffects"), read_only);
            assert_eq!(
                spec.spec
                    .slots
                    .iter()
                    .map(|s| (
                        s.name.as_str(),
                        s.type_ref.as_deref(),
                        s.is_maybe(),
                        s.default.is_some()
                    ))
                    .collect::<Vec<_>>(),
                [
                    ("req", Some("sys::Grid"), false, false),
                    ("returns", Some("sys::Grid"), false, false)
                ]
            );
            let mut args = HDict::new();
            args.set("req", Kind::Grid(Box::new(HGrid::new())));
            assert!(catalog.fit_arguments(name, &args).is_ok());
            args.set("req", Kind::Str("not a grid".into()));
            assert!(catalog.fit_arguments(name, &args).is_err());
        }
        assert!(catalog.declaration("ph.api::hisReceipt").is_none());
        let observation = ActivatedCatalog::new(catalog, None).unwrap();
        for name in ["ph.api::hisRead", "ph.api::hisWrite"] {
            assert!(observation.callable(name).unwrap().op_grid);
        }
    }
    assert!(
        Catalog::load_pinned()
            .unwrap()
            .declaration("ph.api::hisRead")
            .is_none()
    );
}
