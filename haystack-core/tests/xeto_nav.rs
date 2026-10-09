//! Independent oracle transcribed from Project-Haystack/xeto
//! 873b922451d3ef4c0c9c08ef3daa542f352d69f3, not generated from the loader:
//!
//! - ph.api/funcs.xeto:33 `nav: Func <op, opGrid, noSideEffects> { req: Grid, returns: Grid }`
//! - ph.api/lib.xeto:13-17 depends on `sys`, `sys.api` and `ph`
//! - sys/spec.xeto:149-153 `opGrid: Marker?`
//!
//! The selection is a partial `ph.api` library; no complete-library claim.
use haystack_core::{
    data::{HCol, HDict, HGrid},
    kinds::{HRef, Kind},
    xeto::catalog::{ActivatedCatalog, ArgumentOrigin, Catalog},
};

const NAV: &str = "ph.api::nav";
const FUNCS_SHA256: &str = "15a30e0c13098b1e9ca7477b97c1e15b1b63da17bae56f83c364bc0f24f93c1b";
const LIB_SHA256: &str = "061cb812a5bbd035ea9e44f341edb44fa789d7e8cd26c3eca44439f37dbb9b21";

fn grid(cols: &[&str], rows: Vec<HDict>) -> Kind {
    Kind::Grid(Box::new(HGrid::from_parts(
        HDict::new(),
        cols.iter().map(|name| HCol::new(*name)).collect(),
        rows,
    )))
}
fn args(value: Kind) -> HDict {
    let mut args = HDict::new();
    args.set("req", value);
    args
}

#[test]
fn nav_signature_metadata_and_partial_library_identity_match_the_pin() {
    for catalog in [
        Catalog::load_http_pinned().unwrap(),
        Catalog::load_protocol_pinned().unwrap(),
    ] {
        let nav = catalog.declaration(NAV).expect("selected nav");
        assert_eq!(nav.member_of.as_deref(), Some("sys::Funcs"));
        assert_eq!(nav.spec.base.as_deref(), Some("sys::Func"));
        assert_eq!(nav.spec.meta.len(), 3);
        for marker in ["op", "opGrid", "noSideEffects"] {
            assert_eq!(nav.spec.meta.get(marker), Some(&Kind::Marker), "{marker}");
        }
        assert_eq!(
            nav.spec
                .slots
                .iter()
                .map(|slot| (
                    slot.name.as_str(),
                    slot.type_ref.as_deref().unwrap(),
                    slot.is_maybe(),
                    slot.default.is_none(),
                    slot.meta.contains_key("of"),
                ))
                .collect::<Vec<_>>(),
            [
                ("req", "sys::Grid", false, true, false),
                ("returns", "sys::Grid", false, true, false),
            ]
        );
        assert!(
            nav.spec
                .doc
                .contains("Navigate a project for learning and discovery."),
            "{:?}",
            nav.spec.doc
        );
        assert_eq!(nav.source.path, "src/xeto/ph.api/funcs.xeto");
        assert_eq!(nav.source.sha256, FUNCS_SHA256);
        assert_eq!(nav.source.declarations, [NAV]);

        let library = catalog
            .libraries()
            .find(|library| library.name == "ph.api")
            .expect("partial ph.api identity");
        assert!(!library.complete);
        assert_eq!(library.version, "5.0.0");
        assert_eq!(library.maturity, "alpha");
        assert_eq!(library.depends, ["sys", "sys.api", "ph"]);
        assert_eq!(library.declarations, [NAV]);
        let pragma = catalog
            .provenance()
            .files
            .iter()
            .find(|file| file.path == "src/xeto/ph.api/lib.xeto")
            .unwrap();
        assert_eq!(
            (pragma.role.as_str(), pragma.sha256.as_str()),
            ("library", LIB_SHA256)
        );

        // The rest of the same upstream file stays outside every selection.
        for name in [
            "watchSub",
            "watchUnsub",
            "watchPoll",
            "pointWrite",
            "hisRead",
            "hisWrite",
        ] {
            assert!(catalog.declaration(&format!("ph.api::{name}")).is_none());
            assert!(catalog.declaration(&format!("sys.api::{name}")).is_none());
        }
        let op_grid = catalog
            .metadata()
            .find(|field| field.slot.name == "opGrid")
            .expect("opGrid metadata field");
        assert_eq!(op_grid.slot.type_ref.as_deref(), Some("sys::Marker"));
        assert!(op_grid.slot.is_maybe());
        assert_eq!(op_grid.source.path, "src/xeto/sys/spec.xeto");
        assert!(!catalog.provenance().complete_libraries);
        assert!(catalog.libraries().all(|library| !library.complete));
    }

    // In the HTTP closure `ph` and `sys.refs` are retained dependency-only
    // identities: verified bytes, no admitted declarations, not advertised.
    let http = Catalog::load_http_pinned().unwrap();
    assert_eq!(
        http.libraries()
            .map(|library| library.name.as_str())
            .collect::<Vec<_>>(),
        ["ph.api", "sys", "sys.api"]
    );
    for name in ["ph", "sys.refs"] {
        let file = http
            .provenance()
            .files
            .iter()
            .find(|file| file.library == name)
            .unwrap();
        assert_eq!(file.role, "dependency");
        assert!(http.declarations().all(|d| d.spec.lib != name));
    }
    // The native readById profile is unchanged by the navigation selection.
    let native = Catalog::load_pinned().unwrap();
    assert!(native.declaration(NAV).is_none());
    assert!(native.metadata().all(|field| field.slot.name != "opGrid"));
}

#[test]
fn nav_fits_a_whole_request_grid_and_a_grid_result() {
    let profile = Catalog::load_http_pinned().unwrap();
    // `req` is required: no parameter default, not nullable.
    assert!(profile.fit_arguments(NAV, &HDict::new()).is_err());
    assert!(profile.fit_arguments(NAV, &args(Kind::Null)).is_err());
    for wrong in [
        Kind::Dict(Box::default()),
        Kind::Str("site-1".into()),
        Kind::Ref(HRef::from_val("site-1")),
        Kind::List(vec![]),
    ] {
        assert!(profile.fit_arguments(NAV, &args(wrong)).is_err());
    }
    // navId is a request-grid column, never a named argument.
    let mut named = HDict::new();
    named.set("navId", Kind::Str("site-1".into()));
    assert!(profile.fit_arguments(NAV, &named).is_err());

    let mut row = HDict::new();
    row.set("navId", Kind::Str("site-1".into()));
    for request in [grid(&[], vec![]), grid(&["navId"], vec![row])] {
        let bound = profile.fit_arguments(NAV, &args(request.clone())).unwrap();
        assert_eq!(bound.values().get("req"), Some(&request));
        assert_eq!(bound.origin("req"), Some(ArgumentOrigin::Explicit));
    }

    let mut child = HDict::new();
    child.set("navId", Kind::Str("equip-1".into()));
    child.set("id", Kind::Ref(HRef::from_val("equip-1")));
    child.set("dis", Kind::Str("AHU".into()));
    let mut leaf = HDict::new();
    leaf.set("navId", Kind::Null);
    leaf.set("dis", Kind::Str("Leaf".into()));
    profile
        .fit_result(NAV, &grid(&["dis", "id", "navId"], vec![child, leaf]))
        .unwrap();
    profile.fit_result(NAV, &grid(&["navId"], vec![])).unwrap();
    for wrong in [Kind::Null, Kind::Dict(Box::default()), Kind::None] {
        assert!(profile.fit_result(NAV, &wrong).is_err());
    }
}

#[test]
fn only_the_op_grid_declaration_passes_a_request_grid_whole() {
    let observation = ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap();
    let nav = observation.callable(NAV).unwrap();
    assert!(nav.op_grid);
    assert!(nav.strict_arguments);
    assert_eq!(
        nav.parameters
            .iter()
            .map(|(name, ty)| (name.as_str(), ty.as_str()))
            .collect::<Vec<_>>(),
        [("req", "sys::Grid")]
    );
    assert_eq!(nav.result, "sys::Grid");
    for operation in observation.catalog().operations() {
        let callable = observation.callable(&operation.spec.qname).unwrap();
        assert_eq!(callable.op_grid, operation.spec.qname == NAV);
    }
}
