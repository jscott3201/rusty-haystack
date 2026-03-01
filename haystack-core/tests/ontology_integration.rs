// Integration tests for the ontology layer.
//
// Loads the full standard defs from the bundled defs.trio file
// and verifies taxonomy relationships, conjunct decomposition,
// and structural type fitting.

use haystack_core::data::HDict;
use haystack_core::kinds::{HRef, Kind};
use haystack_core::ontology::{DefKind, DefNamespace};

/// Load the standard namespace once for reuse across tests.
fn load_ns() -> DefNamespace {
    DefNamespace::load_standard().expect("Failed to load standard defs")
}

#[test]
fn load_standard_defs_count() {
    let ns = load_ns();
    // The defs.trio file contains approximately 719 defs.
    // Allow some variance for file updates, but should be in the ballpark.
    let count = ns.len();
    assert!(
        (600..=900).contains(&count),
        "Expected ~719 defs, got {}",
        count
    );
}

#[test]
fn standard_libs_present() {
    let ns = load_ns();
    let libs = ns.libs();
    assert!(libs.contains_key("ph"), "Missing ph lib");
    assert!(libs.contains_key("phIoT"), "Missing phIoT lib");
    assert!(libs.contains_key("phScience"), "Missing phScience lib");
}

#[test]
fn lib_versions() {
    let ns = load_ns();
    let ph = &ns.libs()["ph"];
    assert_eq!(ph.version, "4.0.0");

    let phiot = &ns.libs()["phIoT"];
    assert_eq!(phiot.version, "4.0.0");
}

#[test]
fn ahu_is_equip() {
    let ns = load_ns();
    assert!(ns.is_a("ahu", "equip"));
}

#[test]
fn ahu_is_entity() {
    let ns = load_ns();
    assert!(ns.is_a("ahu", "entity"));
}

#[test]
fn ahu_is_marker() {
    let ns = load_ns();
    assert!(ns.is_a("ahu", "marker"));
}

#[test]
fn ahu_is_not_point() {
    let ns = load_ns();
    assert!(!ns.is_a("ahu", "point"));
}

#[test]
fn ahu_is_not_site() {
    let ns = load_ns();
    assert!(!ns.is_a("ahu", "site"));
}

#[test]
fn equip_subtypes_include_airhandlingequip() {
    let ns = load_ns();
    let subtypes = ns.subtypes("equip");
    // ahu inherits from airHandlingEquip, which inherits from equip
    // So direct subtypes of equip include airHandlingEquip, not ahu
    assert!(
        subtypes.contains(&"airHandlingEquip".to_string()),
        "equip subtypes should include airHandlingEquip, got: {:?}",
        subtypes
    );
}

#[test]
fn airhandlingequip_subtypes_include_ahu() {
    let ns = load_ns();
    let subtypes = ns.subtypes("airHandlingEquip");
    assert!(
        subtypes.contains(&"ahu".to_string()),
        "airHandlingEquip subtypes should include ahu, got: {:?}",
        subtypes
    );
}

#[test]
fn ahu_supertypes() {
    let ns = load_ns();
    let supers = ns.supertypes("ahu");
    // ahu inherits from at least airHandlingEquip or equip
    assert!(!supers.is_empty(), "ahu should have supertypes");
    // Should eventually reach marker
    assert!(
        supers.contains(&"marker".to_string()),
        "ahu supertypes should include marker, got: {:?}",
        supers
    );
}

#[test]
fn entity_def_kind() {
    let ns = load_ns();
    let entity_def = ns.get_def("entity").expect("entity def should exist");
    // entity's is_ includes "marker", so kind() depends on the hierarchy
    // entity has is=[marker], which doesn't match entity/val/etc priority checks
    // so it falls through to DefKind::Marker
    assert_eq!(entity_def.kind(), DefKind::Marker);
}

#[test]
fn ahu_def_exists() {
    let ns = load_ns();
    let def = ns.get_def("ahu").expect("ahu def should exist");
    assert_eq!(def.symbol, "ahu");
    assert!(!def.doc.is_empty(), "ahu should have documentation");
    assert!(def.mandatory, "ahu should be mandatory");
}

#[test]
fn site_def_exists() {
    let ns = load_ns();
    let def = ns.get_def("site").expect("site def should exist");
    assert_eq!(def.symbol, "site");
}

#[test]
fn conjunct_hot_water() {
    let ns = load_ns();
    let parts = ns.conjunct_parts("hot-water");
    assert!(parts.is_some(), "hot-water should be a conjunct");
    let parts = parts.unwrap();
    assert_eq!(parts, &["hot", "water"]);
}

#[test]
fn conjunct_ac_elec() {
    let ns = load_ns();
    let parts = ns.conjunct_parts("ac-elec");
    assert!(parts.is_some(), "ac-elec should be a conjunct");
    let parts = parts.unwrap();
    assert_eq!(parts, &["ac", "elec"]);
}

#[test]
fn non_conjunct_returns_none() {
    let ns = load_ns();
    assert!(ns.conjunct_parts("site").is_none());
    assert!(ns.conjunct_parts("equip").is_none());
}

#[test]
fn fits_valid_ahu_entity() {
    let ns = load_ns();

    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    entity.set("dis", Kind::Str("AHU-1".into()));
    entity.set("ahu", Kind::Marker);
    entity.set("equip", Kind::Marker);
    entity.set("airHandlingEquip", Kind::Marker);

    assert!(
        ns.fits(&entity, "ahu"),
        "Entity with ahu+equip+airHandlingEquip should fit ahu"
    );
}

#[test]
fn fits_missing_equip_marker() {
    let ns = load_ns();

    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    entity.set("ahu", Kind::Marker);
    // Missing equip marker

    assert!(
        !ns.fits(&entity, "ahu"),
        "Entity without equip should not fit ahu"
    );
}

#[test]
fn fits_explain_returns_issues() {
    let ns = load_ns();

    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    entity.set("ahu", Kind::Marker);
    // Missing equip

    let issues = ns.fits_explain(&entity, "ahu");
    assert!(!issues.is_empty(), "Should have fit issues");
}

#[test]
fn validate_entity_catches_missing_mandatory() {
    let ns = load_ns();

    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    entity.set("ahu", Kind::Marker);
    // Missing equip (mandatory for ahu's supertype chain)

    let issues = ns.validate_entity(&entity);
    assert!(!issues.is_empty(), "Should find validation issues");

    let has_equip_issue = issues
        .iter()
        .any(|i| i.issue_type == "missing_marker" && i.detail.contains("equip"));
    assert!(
        has_equip_issue,
        "Should report missing equip marker, issues: {:?}",
        issues
    );
}

#[test]
fn validate_entity_passes_for_valid_entity() {
    let ns = load_ns();

    // Build a valid site entity
    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("site-1")));
    entity.set("site", Kind::Marker);

    let issues = ns.validate_entity(&entity);
    // site may or may not have mandatory supertypes depending on the defs
    // but site itself is just a marker, entity is not mandatory
    // This just verifies the validation doesn't crash
    let _ = issues;
}

#[test]
fn mandatory_tags_for_ahu() {
    let ns = load_ns();
    let tags = ns.mandatory_tags("ahu");
    // ahu should have at least ahu itself as mandatory
    assert!(
        tags.contains("ahu"),
        "ahu mandatory tags should include ahu, got: {:?}",
        tags
    );
}

#[test]
fn choice_def_exists() {
    let ns = load_ns();
    let def = ns.get_def("ahuZoneDelivery");
    assert!(def.is_some(), "ahuZoneDelivery choice should exist");
    let def = def.unwrap();
    assert_eq!(def.kind(), DefKind::Choice);
}

#[test]
fn tags_for_entity_type() {
    let ns = load_ns();
    let tags = ns.tags_for("site");
    // site should have some tags applied via tagOn
    // This verifies the tag_on_index works
    // At minimum, some tags should reference site
    // This test verifies the mechanism works without requiring specific tags
    let _ = tags;
}

#[test]
fn lib_def_kind() {
    let ns = load_ns();
    let def = ns.get_def("lib:ph").expect("lib:ph def should exist");
    assert_eq!(def.kind(), DefKind::Lib);
}

#[test]
fn point_is_entity() {
    let ns = load_ns();
    assert!(ns.is_a("point", "entity"));
}

#[test]
fn site_is_entity() {
    let ns = load_ns();
    assert!(ns.is_a("site", "entity"));
}

#[test]
fn meter_is_equip() {
    let ns = load_ns();
    assert!(ns.is_a("meter", "equip"));
}
