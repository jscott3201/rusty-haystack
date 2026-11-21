use haystack_core::xeto::repository::{LibraryPackage, LibraryPin};
#[test]
fn independent_store_and_deflate_descriptor_packages_validate() {
    for bytes in [
        include_bytes!("fixtures/repository/sample-store.xetolib").as_slice(),
        include_bytes!("fixtures/repository/sample-deflate.xetolib").as_slice(),
        include_bytes!("fixtures/repository/sample-utf8-unsigned.xetolib").as_slice(),
    ] {
        let pin = LibraryPin::new("sample", "1.0.0".parse().unwrap()).unwrap();
        LibraryPackage::from_bytes(pin, bytes.to_vec()).expect("valid independent package");
    }
    for bytes in [
        include_bytes!("fixtures/repository/dependent-store.xetolib").as_slice(),
        include_bytes!("fixtures/repository/dependent-deflate.xetolib").as_slice(),
    ] {
        LibraryPackage::from_bytes(pin("dependent", "1.0.0"), bytes.to_vec())
            .expect("valid independent dependent package");
    }
}
#[path = "common/repository.rs"]
mod fixture;
use fixture::*;
use haystack_core::xeto::{
    catalog::Catalog,
    repository::{LibraryRepository, PackageError, PackageSelection, Sha256Digest},
};
#[test]
fn exact_bytes_independent_hash_and_immutable_store() {
    let bytes = include_bytes!("fixtures/repository/sample-deflate.xetolib");
    let package = LibraryPackage::from_bytes(pin("sample", "1.0.0"), bytes.to_vec()).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/repository/manifest.json")).unwrap();
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(package.bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert_eq!(
        hash,
        manifest["sample-deflate.xetolib"]["sha256"]
            .as_str()
            .unwrap()
    );
    assert_eq!(package.digest(), Sha256Digest::compute(bytes));
    assert_eq!(package.metadata().digest, Some(package.digest()));
    let store = LibraryRepository::new("Fixture", vec![package.clone(), package]).unwrap();
    assert_eq!(store.packages().count(), 1);
    assert_eq!(store.get(&pin("sample", "1.0.0")).unwrap().bytes(), bytes);
    let repacked = LibraryPackage::from_bytes(
        pin("sample", "1.0.0"),
        include_bytes!("fixtures/repository/sample-store.xetolib").to_vec(),
    )
    .unwrap();
    assert!(matches!(
        LibraryRepository::new(
            "Fixture",
            vec![store.packages().next().unwrap().clone(), repacked]
        ),
        Err(PackageError::Conflict)
    ));
}
#[test]
fn full_dependency_closure_is_additive_and_preserves_archive_identity() {
    let sample = LibraryPackage::from_bytes(
        pin("sample", "1.0.0"),
        include_bytes!("fixtures/repository/sample-deflate.xetolib").to_vec(),
    )
    .unwrap();
    let dependent = LibraryPackage::from_bytes(
        pin("dependent", "1.0.0"),
        include_bytes!("fixtures/repository/dependent-store.xetolib").to_vec(),
    )
    .unwrap();
    let selection = PackageSelection::new(
        vec![pin("dependent", "1.0.0")],
        vec![dependent, sample],
        vec![],
    )
    .unwrap();
    let base = Catalog::load_http_pinned().unwrap();
    let before = base.selection_identity();
    let next = selection.admit(&base).unwrap();
    assert!(next.declaration("dependent::Child").is_some());
    assert!(base.declaration("dependent::Child").is_none());
    assert_eq!(before, base.selection_identity());
    assert_ne!(before, next.selection_identity());
    assert_eq!(
        selection.admit(&next).unwrap().selection_identity(),
        next.selection_identity()
    );
    let repack = LibraryPackage::from_bytes(
        pin("sample", "1.0.0"),
        include_bytes!("fixtures/repository/sample-store.xetolib").to_vec(),
    )
    .unwrap();
    assert!(matches!(
        PackageSelection::new(vec![pin("sample", "1.0.0")], vec![repack], vec![])
            .unwrap()
            .admit(&next),
        Err(PackageError::Conflict)
    ));
    let source = "pragma: Lib <version:\"1.0.0\",depends:{{lib:\"sys\",versions:\"5.0.0\"}}>\nThing: Dict { name:Str }";
    let modified = next.with_project("sample", source, &[]).unwrap();
    assert!(matches!(
        selection.admit(&modified),
        Err(PackageError::Conflict)
    ));
}
#[test]
fn dependency_failures_do_not_mutate_base() {
    let base = Catalog::load_http_pinned().unwrap();
    let before = base.selection_identity();
    let a = package("alpha", "1.0.0", &[("beta", "1.0.0")], "Thing: Dict {}");
    let b = package("beta", "1.0.0", &[("alpha", "1.0.0")], "Thing: Dict {}");
    let cycle =
        PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a.clone(), b], vec![]).unwrap();
    assert!(matches!(cycle.admit(&base), Err(PackageError::Cycle)));
    let missing = PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a], vec![]).unwrap();
    assert!(matches!(
        missing.admit(&base),
        Err(PackageError::MissingDependency)
    ));
    let a = package("alpha", "1.0.0", &[("sys", "4.0.0")], "Thing: Dict {}");
    assert!(matches!(
        PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a], vec![])
            .unwrap()
            .admit(&base),
        Err(PackageError::MissingDependency)
    ));
    let a = package("alpha", "1.0.0", &[("sys", "5.0.0")], "Thing: Str");
    assert!(matches!(
        PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a], vec![])
            .unwrap()
            .admit(&base),
        Err(PackageError::Catalog(_))
    ));
    let a = package("alpha", "1.0.0", &[("sys", "5.0.0")], "Thing: Dict {}");
    assert!(
        PackageSelection::new(
            vec![pin("alpha", "1.0.0")],
            vec![a.clone(), a.clone()],
            vec![]
        )
        .is_err()
    );
    assert!(PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a; 17], vec![]).is_err());
    assert_eq!(before, base.selection_identity());
}
#[test]
fn malformed_zip_structures_content_and_bounded_expansion_are_rejected() {
    let original = include_bytes!("fixtures/repository/sample-deflate.xetolib");
    for end in [0, 4, 30, original.len() - 1, original.len() - 22] {
        assert!(
            LibraryPackage::from_bytes(pin("sample", "1.0.0"), original[..end].to_vec()).is_err()
        );
    }
    for offset in [6, 8, 14, original.len() - 6, 40] {
        let mut bytes = original.to_vec();
        bytes[offset] ^= 0x40;
        assert!(
            LibraryPackage::from_bytes(pin("sample", "1.0.0"), bytes).is_err(),
            "offset {offset}"
        );
    }
    let central = original
        .windows(4)
        .position(|w| w == b"PK\x01\x02")
        .unwrap();
    for offset in [
        central + 8,
        central + 16,
        central + 24,
        central + 41,
        central + 42,
    ] {
        let mut bytes = original.to_vec();
        bytes[offset] ^= 0x40;
        assert!(LibraryPackage::from_bytes(pin("sample", "1.0.0"), bytes).is_err());
    }
    // Consistent lying expanded sizes still cannot exceed the bounded inflater.
    let mut lying = original.to_vec();
    let descriptor_at = original
        .windows(4)
        .position(|w| w == b"PK\x07\x08")
        .unwrap();
    lying[central + 24..central + 28].copy_from_slice(&1u32.to_le_bytes());
    lying[descriptor_at + 12..descriptor_at + 16].copy_from_slice(&1u32.to_le_bytes());
    assert!(matches!(
        LibraryPackage::from_bytes(pin("sample", "1.0.0"), lying),
        Err(PackageError::Invalid("DEFLATE size or trailing input"))
    ));
    let descriptor = original
        .windows(4)
        .position(|w| w == b"PK\x07\x08")
        .unwrap();
    let mut bytes = original.to_vec();
    bytes[descriptor + 4] ^= 1;
    assert!(LibraryPackage::from_bytes(pin("sample", "1.0.0"), bytes).is_err());
    assert!(LibraryPackage::from_bytes(pin("sample", "1.0.0"), vec![0; 1024 * 1024 + 1]).is_err());
    for name in [
        "../lib.xeto",
        "/lib.xeto",
        "lib\\xeto",
        "xeto-meta.props",
        "other.xeto",
    ] {
        assert!(
            LibraryPackage::from_bytes(
                pin("sample", "1.0.0"),
                zip(&[("xeto-meta.props", b""), (name, b"")])
            )
            .is_err()
        );
    }
    assert!(
        LibraryPackage::from_bytes(
            pin("sample", "1.0.0"),
            zip(&[("xeto-meta.props", b""), ("lib.xeto", &vec![b' '; 65537])])
        )
        .is_err()
    );
}
#[test]
fn metadata_source_agreement_and_raw_dependency_shape_are_strict() {
    let source = "pragma: Lib <version:\"1.0.0\",depends:{{lib:\"sys\",versions:\"5.0.0\"}}>\nThing: Dict {}";
    let valid = "name=sample\nversion=1.0.0\nmaturity=stable\ndepends=sys 5.0.0\n";
    for props in [
        valid.replace("name=sample", "name=other"),
        valid.replace("1.0.0", "01.0.0"),
        valid.replace("stable", "alpha"),
        valid.replace("5.0.0", "4.0.0"),
        format!("{valid}name=sample\n"),
        format!("{valid}#comment\n"),
        format!("{valid}doc=bad\\q\n"),
        format!("{valid}doc=extra\n"),
    ] {
        assert!(
            LibraryPackage::from_bytes(
                pin("sample", "1.0.0"),
                zip(&[
                    ("xeto-meta.props", props.as_bytes()),
                    ("lib.xeto", source.as_bytes())
                ])
            )
            .is_err()
        );
    }
    for depends in [
        "\"sys\"",
        "{\"sys\"}",
        "{{lib:\"sys\"}}",
        "{{lib:\"sys\",versions:\"5.x.x\"}}",
        "{{lib:\"sys\",versions:\"5.0.0\",other:1}}",
        "{{lib:\"sys\",versions:\"5.0.0\"},{lib:\"sys\",versions:\"5.0.0\"}}",
    ] {
        let source = format!("pragma: Lib <version:\"1.0.0\",depends:{depends}>\nThing: Dict {{}}");
        assert!(
            LibraryPackage::from_bytes(
                pin("sample", "1.0.0"),
                zip(&[
                    ("xeto-meta.props", valid.as_bytes()),
                    ("lib.xeto", source.as_bytes())
                ])
            )
            .is_err()
        );
    }
}
#[test]
fn ordinary_props_writer_escapes_round_trip_without_rewriting_source() {
    let source = "pragma: Lib <version:\"1.0.0\",doc:\"https://a=b\\n\\t\\\\😀\">\nThing: Dict {}";
    let props = "name=sample\nversion=1.0.0\nmaturity=stable\ndepends=\ndoc=https:\\u002f/a\\u003db\\n\\t\\\\\\ud83d\\ude00\n";
    let bytes = zip(&[
        ("xeto-meta.props", props.as_bytes()),
        ("lib.xeto", source.as_bytes()),
    ]);
    let package = LibraryPackage::from_bytes(pin("sample", "1.0.0"), bytes.clone()).unwrap();
    assert_eq!(package.bytes(), bytes);
    assert_eq!(
        package.metadata().doc.as_deref(),
        Some("https://a=b\n\t\\😀")
    );
}
#[test]
fn closure_versions_reachability_depth_and_existing_source_are_guarded() {
    let base = Catalog::load_http_pinned().unwrap();
    let a = package(
        "alpha",
        "1.0.0",
        &[("sys", "5.0.0"), ("beta", "1.0.0")],
        "Thing: Dict {}",
    );
    let b = package("beta", "2.0.0", &[("sys", "5.0.0")], "Thing: Dict {}");
    assert!(matches!(
        PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a, b], vec![])
            .unwrap()
            .admit(&base),
        Err(PackageError::Conflict)
    ));
    let a = package("alpha", "1.0.0", &[("sys", "5.0.0")], "Thing: Dict {}");
    let b = package("beta", "1.0.0", &[("sys", "5.0.0")], "Thing: Dict {}");
    assert!(matches!(
        PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a.clone(), b], vec![])
            .unwrap()
            .admit(&base),
        Err(PackageError::Invalid("unreachable supplied package"))
    ));
    let source = "pragma: Lib <version:\"1.0.0\",depends:{{lib:\"sys\",versions:\"5.0.0\"}}>\nThing: Dict {}";
    let project = base.with_project("alpha", source, &[]).unwrap();
    assert!(matches!(
        PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a], vec![])
            .unwrap()
            .admit(&project),
        Err(PackageError::Conflict)
    ));
    let mut chain = Vec::new();
    for index in 0..16 {
        let name = format!("chain{index}");
        let next = format!("chain{}", index + 1);
        let deps = if index == 15 {
            vec![("sys", "5.0.0")]
        } else {
            vec![(next.as_str(), "1.0.0"), ("sys", "5.0.0")]
        };
        chain.push(package(&name, "1.0.0", &deps, "Thing: Dict {}"));
    }
    assert!(matches!(
        PackageSelection::new(
            vec![pin("chain15", "1.0.0"), pin("chain0", "1.0.0")],
            chain.clone(),
            vec![]
        )
        .unwrap()
        .admit(&base),
        Err(PackageError::Limit("dependency depth"))
    ));
    assert!(matches!(
        PackageSelection::new(vec![pin("chain0", "1.0.0")], chain, vec![])
            .unwrap()
            .admit(&base),
        Err(PackageError::Limit("dependency depth"))
    ));
    let unsupported = package(
        "ordinary_name",
        "1.0.0",
        &[("sys", "5.0.0")],
        "Thing: Dict {}",
    );
    assert!(matches!(
        PackageSelection::new(
            vec![pin("ordinary_name", "1.0.0")],
            vec![unsupported],
            vec![]
        )
        .unwrap()
        .admit(&base),
        Err(PackageError::Catalog(_))
    ));
}
#[test]
fn marker_associations_and_default_maturity_survive_idempotent_admission() {
    let source = "pragma: Lib <version:\"1.0.0\",depends:{{lib:\"sys\",versions:\"5.0.0\"}}>\nThing: Dict { sample }";
    let props = "name=sample\nversion=1.0.0\nmaturity=stable\ndepends=sys 5.0.0\n";
    let package = LibraryPackage::from_bytes(
        pin("sample", "1.0.0"),
        zip(&[
            ("xeto-meta.props", props.as_bytes()),
            ("lib.xeto", source.as_bytes()),
        ]),
    )
    .unwrap();
    let selection = PackageSelection::new(
        vec![pin("sample", "1.0.0")],
        vec![package.clone()],
        vec![("sample".into(), "sample::Thing".into())],
    )
    .unwrap();
    let base = Catalog::load_http_pinned().unwrap();
    let next = selection.admit(&base).unwrap();
    assert_eq!(
        next.libraries()
            .find(|l| l.name == "sample")
            .unwrap()
            .maturity,
        "stable"
    );
    assert_eq!(
        selection.admit(&next).unwrap().selection_identity(),
        next.selection_identity()
    );
    let changed = PackageSelection::new(
        vec![pin("sample", "1.0.0")],
        vec![package],
        vec![("changed".into(), "sample::Thing".into())],
    )
    .unwrap();
    assert!(matches!(changed.admit(&next), Err(PackageError::Conflict)));
}

#[test]
fn retained_dependency_depth_matches_single_batch_and_preserves_base() {
    let base = Catalog::load_http_pinned().unwrap();
    let mut chain = Vec::new();
    for index in (0..16).rev() {
        let name = format!("chain{index}");
        let next = format!("chain{}", index + 1);
        let deps = if index == 15 {
            vec![("sys", "5.0.0")]
        } else {
            vec![(next.as_str(), "1.0.0"), ("sys", "5.0.0")]
        };
        chain.push(package(&name, "1.0.0", &deps, "Thing: Dict {}"));
    }
    let all = PackageSelection::new(vec![pin("chain0", "1.0.0")], chain.clone(), vec![]).unwrap();
    assert!(matches!(
        all.admit(&base),
        Err(PackageError::Limit("dependency depth"))
    ));
    let mut retained = base;
    for next in &chain[..15] {
        retained = PackageSelection::new(vec![next.pin().clone()], vec![next.clone()], vec![])
            .unwrap()
            .admit(&retained)
            .unwrap();
    }
    let before = retained.selection_identity();
    let last = PackageSelection::new(
        vec![pin("chain0", "1.0.0")],
        vec![chain[15].clone()],
        vec![],
    )
    .unwrap();
    assert!(matches!(
        last.admit(&retained),
        Err(PackageError::Limit("dependency depth"))
    ));
    assert_eq!(retained.selection_identity(), before);
    assert!(retained.declaration("chain0::Thing").is_none());
}

fn retained_source(version: &str, extra: &str) -> String {
    format!(
        "pragma: Lib <version:\"{version}\",depends:{{{{lib:\"sys\",versions:\"5.0.0\"}}{extra}}}>\nThing: sys::Dict {{}}"
    )
}
fn retained_pair() -> (Catalog, PackageSelection) {
    let base = Catalog::load_http_pinned()
        .unwrap()
        .with_project("alpha", &retained_source("1.0.0", ""), &[])
        .unwrap()
        .with_project(
            "beta",
            &retained_source("1.0.0", ",{lib:\"alpha\",versions:\"1.0.0\"}"),
            &[],
        )
        .unwrap();
    let selection = PackageSelection::new(
        vec![pin("gamma", "1.0.0")],
        vec![package(
            "gamma",
            "1.0.0",
            &[("beta", "1.0.0"), ("sys", "5.0.0")],
            "Thing: Dict {}",
        )],
        vec![],
    )
    .unwrap();
    (base, selection)
}
#[test]
fn retained_dependency_cycle_is_rejected_only_when_reachable() {
    let (base, selection) = retained_pair();
    // Source-only base dependencies are allowed when their complete pins agree.
    assert!(selection.admit(&base).is_ok());
    let cyclic = base
        .with_project(
            "alpha",
            &retained_source("1.0.0", ",{lib:\"beta\",versions:\"1.0.0\"}"),
            &[],
        )
        .unwrap();
    let before = cyclic.selection_identity();
    assert!(matches!(selection.admit(&cyclic), Err(PackageError::Cycle)));
    assert_eq!(cyclic.selection_identity(), before);
    let unrelated = PackageSelection::new(
        vec![pin("delta", "1.0.0")],
        vec![package(
            "delta",
            "1.0.0",
            &[("sys", "5.0.0")],
            "Thing: Dict {}",
        )],
        vec![],
    )
    .unwrap()
    .admit(&cyclic)
    .unwrap();
    assert!(unrelated.declaration("alpha::Thing").is_some());
    assert!(unrelated.declaration("beta::Thing").is_some());
    assert!(unrelated.declaration("delta::Thing").is_some());
}
#[test]
fn retained_dependency_transitive_pin_change_is_rejected() {
    let (base, selection) = retained_pair();
    let changed = base
        .with_project("alpha", &retained_source("2.0.0", ""), &[])
        .unwrap();
    let before = changed.selection_identity();
    assert!(matches!(
        selection.admit(&changed),
        Err(PackageError::MissingDependency)
    ));
    assert_eq!(changed.selection_identity(), before);
}

#[test]
fn retained_dependency_metadata_cannot_silently_drop_unsupported_entries() {
    let base = Catalog::load_http_pinned().unwrap();
    let selection = PackageSelection::new(
        vec![pin("gamma", "1.0.0")],
        vec![package(
            "gamma",
            "1.0.0",
            &[("alpha", "1.0.0"), ("sys", "5.0.0")],
            "Thing: Dict {}",
        )],
        vec![],
    )
    .unwrap();
    for (extra, expected) in [
        (",1", "dependency dictionary"),
        (",{other:1}", "dependency fields"),
        (
            ",{lib:\"sys\",versions:\"5.0.0\"}",
            "duplicate or unsupported dependency",
        ),
    ] {
        let retained = base
            .with_project("alpha", &retained_source("1.0.0", extra), &[])
            .unwrap();
        assert!(
            matches!(selection.admit(&retained), Err(PackageError::Invalid(reason)) if reason == expected)
        );
    }
    let retained = base
        .with_project("alpha", &retained_source("01.0.0", ""), &[])
        .unwrap();
    assert!(matches!(
        selection.admit(&retained),
        Err(PackageError::Unsupported("noncanonical version"))
    ));
}
#[test]
fn aggregate_declaration_bound_applies_before_compile_and_across_batches() {
    let source = |count| {
        (0..count)
            .map(|n| format!("Type{n}: Dict {{}}\n"))
            .collect::<String>()
    };
    let a = package("alpha", "1.0.0", &[("sys", "5.0.0")], &source(65));
    let b = package("beta", "1.0.0", &[("sys", "5.0.0")], &source(64));
    assert!(matches!(
        PackageSelection::new(
            vec![pin("alpha", "1.0.0"), pin("beta", "1.0.0")],
            vec![a.clone(), b.clone()],
            vec![]
        ),
        Err(PackageError::Limit(
            "aggregate project declarations or slots"
        ))
    ));
    let base = Catalog::load_http_pinned().unwrap();
    let first = PackageSelection::new(vec![pin("alpha", "1.0.0")], vec![a], vec![])
        .unwrap()
        .admit(&base)
        .unwrap();
    assert!(matches!(
        PackageSelection::new(vec![pin("beta", "1.0.0")], vec![b], vec![])
            .unwrap()
            .admit(&first),
        Err(PackageError::Limit(
            "aggregate project declarations or slots"
        ))
    ));
}
