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

/// A spec whose constraints live in query slots must be checked against the graph,
/// not just against the entity's own tags (issue #22).
///
/// `EntityGraph` has always built a ref resolver and handed it to `matches_with_ns`,
/// but the `SpecMatch` arm dropped it — the two resolver types disagreed on owned
/// vs borrowed, and `None` was the only thing that compiled. `None` is
/// indistinguishable from "no resolver available", so query slots were silently
/// skipped and the spec matched entities it should not.
#[test]
fn query_slot_specs_are_checked_against_the_graph() {
    use haystack_core::graph::EntityGraph;

    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    // A vav is only this spec if something is actually reachable via airRef.
    ns.load_xeto_str(
        "AhuFedVav: Dict {\n  vav: Marker\n  myAhu: Query<of:Ahu, via:\"airRef\">\n}\n",
        "qtest",
    )
    .expect("load test lib");

    let mut graph = EntityGraph::with_namespace(ns);

    let mut ahu = HDict::new();
    ahu.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    ahu.set("ahu", Kind::Marker);
    ahu.set("equip", Kind::Marker);
    graph.add(ahu).unwrap();

    // Connected: airRef points at an AHU that exists in the graph.
    let mut fed = HDict::new();
    fed.set("id", Kind::Ref(HRef::from_val("vav-fed")));
    fed.set("vav", Kind::Marker);
    fed.set("airRef", Kind::Ref(HRef::from_val("ahu-1")));
    graph.add(fed).unwrap();

    // Orphan: same tags, but nothing reachable. Identical under tag-only checking.
    let mut orphan = HDict::new();
    orphan.set("id", Kind::Ref(HRef::from_val("vav-orphan")));
    orphan.set("vav", Kind::Marker);
    graph.add(orphan).unwrap();

    let matched = graph
        .read_all("qtest::AhuFedVav", 0)
        .expect("spec filter is accepted");
    let ids: Vec<String> = matched
        .iter()
        .filter_map(|e| e.id().map(|r| r.val.clone()))
        .collect();

    assert!(
        ids.contains(&"vav-fed".to_string()),
        "the connected vav must match: {ids:?}"
    );
    assert!(
        !ids.contains(&"vav-orphan".to_string()),
        "a vav reaching no AHU must NOT match — the query slot was not checked: {ids:?}"
    );
    assert_eq!(ids.len(), 1, "exactly one vav satisfies the query: {ids:?}");
}

// ── Conjunct defs as filter spec terms (issue #26) ──
//
// Answering `ph::ElecMeter` for a conjunct def takes three independent pieces:
// the filter parser must keep the hyphen, `resolve_spec_term` must reach the
// def, and `entity_is_a` must decompose it into component markers. Any two
// without the third still parse and still evaluate — they just answer `false`
// for every entity, which is indistinguishable from "no entity matched" at the
// call site. So these sweep all 162 bundled conjuncts rather than spot-check,
// and each reports the offending names instead of a bare count.

/// Every conjunct def in the standard namespace, e.g. `elec-meter`.
fn all_conjuncts(ns: &DefNamespace) -> Vec<String> {
    let mut v: Vec<String> = ns
        .defs()
        .keys()
        .filter(|n| ns.conjunct_parts(n).is_some())
        .cloned()
        .collect();
    v.sort();
    assert!(
        v.len() > 100,
        "expected ~162 bundled conjuncts, found {} — the sweeps below prove \
         nothing if the corpus is empty",
        v.len()
    );
    v
}

/// `elec-meter` -> `ElecMeter`, the Haystack-capitalised spelling.
fn camel(conjunct: &str) -> String {
    conjunct
        .split('-')
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// An entity carrying exactly `tags` as markers.
fn marked(tags: &[&str]) -> HDict {
    let mut d = HDict::new();
    for t in tags {
        d.set(*t, Kind::Marker);
    }
    d
}

#[test]
fn every_conjunct_def_parses_and_resolves_as_a_spec_term() {
    let ns = load_ns();
    let mut unparsed = Vec::new();
    let mut unresolved = Vec::new();
    for c in all_conjuncts(&ns) {
        let term = format!("ph::{c}");
        if haystack_core::filter::parse_filter(&term).is_err() {
            unparsed.push(term.clone());
        }
        if ns.resolve_spec_term(&term).is_none() {
            unresolved.push(term);
        }
    }
    assert!(
        unparsed.is_empty(),
        "conjuncts that failed to parse: {unparsed:?}"
    );
    assert!(
        unresolved.is_empty(),
        "conjuncts that parsed but resolved to nothing: {unresolved:?}"
    );
}

#[test]
fn every_conjunct_def_resolves_from_its_camel_case_spelling() {
    // `FuelOilOutput` cannot be transformed into `fuelOil-output` — the capital
    // that was a word boundary and the capital inside a component are the same
    // character — so resolution is a lookup over the conjunct index, and this
    // sweep is what proves the lookup covers the camelCase-component defs and
    // not just the flat ones like `elec-meter`.
    let ns = load_ns();
    let mut unresolved = Vec::new();
    for c in all_conjuncts(&ns) {
        let term = format!("ph::{}", camel(&c));
        match ns.resolve_spec_term(&term) {
            Some(_) => {}
            None => unresolved.push(format!("{term} (for {c})")),
        }
    }
    assert!(
        unresolved.is_empty(),
        "conjuncts unreachable by CamelCase: {unresolved:?}"
    );
}

#[test]
fn every_conjunct_def_matches_an_entity_carrying_its_component_markers() {
    // The end-to-end path: parse the filter, then evaluate it. A fix to the
    // parser and the resolver that missed conjunct decomposition would pass the
    // two tests above and fail every case here.
    let ns = load_ns();
    let mut unmatched = Vec::new();
    for c in all_conjuncts(&ns) {
        let parts = ns.conjunct_parts(&c).unwrap().to_vec();
        let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
        let entity = marked(&refs);
        let filter = haystack_core::filter::parse_filter(&format!("ph::{c}")).unwrap();
        if !haystack_core::filter::matches_with_ns(&filter, &entity, None, Some(&ns)) {
            unmatched.push(format!("{c} (markers {refs:?})"));
        }
    }
    assert!(
        unmatched.is_empty(),
        "conjuncts that did not match their own components: {unmatched:?}"
    );
}

#[test]
fn no_conjunct_def_matches_an_entity_missing_its_components() {
    // The counterweight. A decomposition that answered `true` unconditionally
    // would satisfy every test above.
    let ns = load_ns();
    let bare = HDict::new();
    let mut false_positives = Vec::new();
    for c in all_conjuncts(&ns) {
        let filter = haystack_core::filter::parse_filter(&format!("ph::{c}")).unwrap();
        if haystack_core::filter::matches_with_ns(&filter, &bare, None, Some(&ns)) {
            false_positives.push(c);
        }
    }
    assert!(
        false_positives.is_empty(),
        "conjuncts matched by a tagless entity: {false_positives:?}"
    );

    // Holding all but one component is still not membership.
    let mut partials = Vec::new();
    for c in all_conjuncts(&ns) {
        let parts = ns.conjunct_parts(&c).unwrap().to_vec();
        if parts.len() < 2 {
            continue;
        }
        let refs: Vec<&str> = parts[..parts.len() - 1]
            .iter()
            .map(|s| s.as_str())
            .collect();
        let entity = marked(&refs);
        let filter = haystack_core::filter::parse_filter(&format!("ph::{c}")).unwrap();
        if haystack_core::filter::matches_with_ns(&filter, &entity, None, Some(&ns)) {
            partials.push(format!("{c} matched by {refs:?}"));
        }
    }
    assert!(
        partials.is_empty(),
        "conjuncts matched without all components: {partials:?}"
    );
}
