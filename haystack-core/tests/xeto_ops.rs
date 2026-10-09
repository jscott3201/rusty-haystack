//! Independent oracle transcribed from Project-Haystack/xeto
//! 873b922451d3ef4c0c9c08ef3daa542f352d69f3, sys.api/funcs.xeto:88-95
//! and sys.api/types.xeto:54-73. No complete-library claim.
use haystack_core::data::{HDict, HGrid};
use haystack_core::kinds::{HRef, Kind};
use haystack_core::xeto::catalog::Catalog;

const OPS: &str = "sys.api::ops";

#[test]
fn pinned_ops_closure_retains_identity_and_zero_argument_contract() {
    let profile = Catalog::load_http_pinned().unwrap();
    let ops = profile
        .declaration(OPS)
        .expect("pinned ops must be admitted");
    assert_eq!(ops.member_of.as_deref(), Some("sys::Funcs"));
    assert_eq!(ops.spec.base.as_deref(), Some("sys::Func"));
    assert_eq!(ops.spec.meta.get("op"), Some(&Kind::Marker));
    assert_eq!(ops.spec.meta.get("noSideEffects"), Some(&Kind::Marker));
    assert_eq!(ops.spec.slots.len(), 1);
    let returns = &ops.spec.slots[0];
    assert_eq!(returns.name, "returns");
    assert_eq!(returns.type_ref.as_deref(), Some("sys::Grid"));
    assert_eq!(
        returns.meta.get("of"),
        Some(&Kind::Ref(HRef::from_val("sys.api::OpInfo")))
    );
    assert!(!returns.is_maybe());
    assert!(
        profile
            .fit_arguments(OPS, &HDict::new())
            .unwrap()
            .values()
            .is_empty()
    );
    for key in ["unexpected", "returns"] {
        let mut args = HDict::new();
        args.set(key, Kind::Null);
        assert!(profile.fit_arguments(OPS, &args).is_err());
    }
    assert_eq!(
        profile
            .operations()
            .map(|op| op.spec.qname.as_str())
            .collect::<Vec<_>>(),
        [
            // Selected from ph.api/funcs.xeto:33 (M2-PR05); see xeto_nav.rs.
            "ph.api::nav",
            "sys.api::about",
            "sys.api::close",
            "sys.api::filetypes",
            "sys.api::libs",
            OPS,
            "sys.api::read",
            "sys.api::readAll",
            "sys.api::readById",
            "sys.api::readByIds"
        ]
    );
    for name in ["sys.api::OpInfo", "sys::Grid"] {
        assert!(profile.declaration(name).is_some());
    }
    for name in [
        "sys::Spec",
        "sys::Entity",
        "sys.api::watchPoll",
        "sys.api::pointWrite",
    ] {
        assert!(profile.declaration(name).is_none());
    }
    let info = profile.declaration("sys.api::OpInfo").unwrap();
    assert_eq!(info.spec.base.as_deref(), Some("sys::Dict"));
    assert_eq!(
        info.spec
            .slots
            .iter()
            .map(|slot| (
                slot.name.as_str(),
                slot.type_ref.as_deref().unwrap(),
                slot.is_maybe()
            ))
            .collect::<Vec<_>>(),
        [
            ("qname", "sys::Str", false),
            ("doc", "sys::Str", true),
            ("noSideEffects", "sys::Marker", true),
            ("signature", "sys::Str", false)
        ]
    );
    assert!(!profile.provenance().complete_libraries);
}

fn result(row: HDict) -> Kind {
    Kind::Grid(Box::new(HGrid::from_parts(HDict::new(), vec![], vec![row])))
}
fn valid_row() -> HDict {
    let mut row = HDict::new();
    row.set("qname", Kind::Str(OPS.into()));
    row.set("signature", Kind::Str("() -> Grid<of:OpInfo>".into()));
    row
}

#[test]
fn ops_result_fits_actual_grid_and_each_op_info_row() {
    let profile = Catalog::load_http_pinned().unwrap();
    profile
        .fit_result(OPS, &Kind::Grid(Box::default()))
        .unwrap();
    profile.fit_result(OPS, &result(valid_row())).unwrap();
    let mut row = valid_row();
    row.set("doc", Kind::Str("Supported operations.".into()));
    row.set("noSideEffects", Kind::Marker);
    // Structural specification information survives PR03 decoding and is not
    // an undeclared domain field in an otherwise correctly typed row.
    row.set("spec", Kind::Ref(HRef::from_val("sys.api::OpInfo")));
    profile.fit_result(OPS, &result(row.clone())).unwrap();
    for key in ["qname", "signature"] {
        let mut wrong = row.clone();
        wrong.remove_tag(key);
        assert!(
            profile.fit_result(OPS, &result(wrong)).is_err(),
            "missing {key}"
        );
    }
    for (key, value) in [
        ("doc", Kind::Int(7)),
        ("noSideEffects", Kind::Bool(true)),
        ("qname", Kind::Marker),
        ("signature", Kind::Null),
    ] {
        let mut wrong = row.clone();
        wrong.set(key, value);
        assert!(
            profile.fit_result(OPS, &result(wrong)).is_err(),
            "wrong {key}"
        );
    }
    let mut last = valid_row();
    last.remove_tag("signature");
    assert!(
        profile
            .fit_result(
                OPS,
                &Kind::Grid(Box::new(HGrid::from_parts(
                    HDict::new(),
                    vec![],
                    vec![valid_row(), valid_row(), last]
                )))
            )
            .is_err()
    );
    for wrong in [
        Kind::Dict(Box::new(valid_row())),
        Kind::List(vec![]),
        Kind::Null,
        Kind::Bool(false),
    ] {
        assert!(profile.fit_result(OPS, &wrong).is_err());
    }
}
