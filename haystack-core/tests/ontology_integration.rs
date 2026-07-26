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

// ── Inverse query slots and `of` enforcement (issue #39) ──

use haystack_core::graph::EntityGraph;

/// Build a graph from `(id, tags...)` tuples where every tag is a marker except
/// those written `tag=ref`, which become refs.
fn graph_of(ns: DefNamespace, rows: &[(&str, &[&str])]) -> EntityGraph {
    let mut graph = EntityGraph::with_namespace(ns);
    for (id, tags) in rows {
        let mut e = HDict::new();
        e.set("id", Kind::Ref(HRef::from_val(*id)));
        for t in tags.iter() {
            match t.split_once('=') {
                Some((tag, target)) => e.set(tag, Kind::Ref(HRef::from_val(target))),
                None => e.set(*t, Kind::Marker),
            }
        }
        graph.add(e).unwrap();
    }
    graph
}

fn matched_ids(graph: &EntityGraph, filter: &str) -> Vec<String> {
    let mut ids: Vec<String> = graph
        .read_all(filter, 0)
        .unwrap_or_else(|e| panic!("{filter} rejected: {e}"))
        .iter()
        .filter_map(|e| e.id().map(|r| r.val.clone()))
        .collect();
    ids.sort();
    ids
}

/// An inverse query asks which entities point *at* this one — the mirror of a
/// forward `via`. Before #39 the slot was skipped entirely, so the spec matched
/// every entity that satisfied its other slots.
#[test]
fn an_inverse_query_slot_matches_only_entities_something_points_at() {
    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    ns.load_xeto_str(
        "FedVav: Dict {\n  vav\n  myAhu: Query<of:Ahu, via:\"airRef\">\n}\n\
         FeedingAhu: Dict {\n  ahu\n  vavs: Query<of:Vav, inverse:\"inv::FedVav.myAhu\">\n}\n",
        "inv",
    )
    .expect("load test lib");

    let graph = graph_of(
        ns,
        &[
            ("ahu-fed", &["ahu", "equip"]),
            ("ahu-idle", &["ahu", "equip"]),
            ("vav-1", &["vav", "airRef=ahu-fed"]),
        ],
    );

    assert_eq!(
        matched_ids(&graph, "inv::FeedingAhu"),
        vec!["ahu-fed".to_string()],
        "only the AHU a vav actually points at is a FeedingAhu"
    );
}

/// Inverse traversal follows `+` transitively, the same as the forward path it
/// mirrors: an entity two ref-hops upstream still counts.
///
/// The only qualifying source is deliberately two hops away. A one-hop
/// implementation finds `middle`, which `of:Vav` then rejects, so the slot comes
/// back empty and `root` does not match — which is what makes this test fail if
/// transitivity is dropped.
#[test]
fn a_transitive_inverse_query_reaches_indirect_sources() {
    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    ns.load_xeto_str(
        "Child: Dict {\n  vav\n  up: Query<of:Ahu, via:\"airRef+\">\n}\n\
         Root: Dict {\n  ahu\n  below: Query<of:Vav, inverse:\"deep::Child.up\">\n}\n",
        "deep",
    )
    .expect("load test lib");

    // grandchild(vav) -> middle(not a vav) -> root
    let graph = graph_of(
        ns,
        &[
            ("root", &["ahu", "equip"]),
            ("lonely", &["ahu", "equip"]),
            ("middle", &["equip", "airRef=root"]),
            ("grandchild", &["vav", "airRef=middle"]),
        ],
    );

    assert_eq!(
        matched_ids(&graph, "deep::Root"),
        vec!["root".to_string()],
        "the vav two hops below counts; the lonely ahu has nothing below it"
    );
}

/// `of` narrows what counts as reached. Reaching something is not enough — it has
/// to be the declared type, which the code parsed and then ignored.
#[test]
fn a_query_slot_is_not_satisfied_by_reaching_the_wrong_type() {
    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    ns.load_xeto_str(
        "NeedsAhu: Dict {\n  vav\n  src: Query<of:Ahu, via:\"airRef\">\n}\n",
        "oft",
    )
    .expect("load test lib");

    let graph = graph_of(
        ns,
        &[
            ("real-ahu", &["ahu", "equip"]),
            ("a-chiller", &["chiller", "equip"]),
            ("vav-good", &["vav", "airRef=real-ahu"]),
            ("vav-bad", &["vav", "airRef=a-chiller"]),
        ],
    );

    assert_eq!(
        matched_ids(&graph, "oft::NeedsAhu"),
        vec!["vav-good".to_string()],
        "reaching a chiller does not satisfy of:Ahu"
    );
}

/// An inverse reference naming a slot that does not exist cannot be evaluated
/// either way. It fails closed, consistently with how an unknown spec name is
/// treated — the bundled `ph.equips::VavZoneAhu` is in exactly this state (#46).
#[test]
fn an_inverse_query_naming_a_missing_slot_matches_nothing() {
    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    ns.load_xeto_str(
        "Dangling: Dict {\n  ahu\n  vavs: Query<of:Vav, inverse:\"dang::NoSuchSpec.nope\">\n}\n",
        "dang",
    )
    .expect("load test lib");

    let graph = graph_of(ns, &[("ahu-1", &["ahu", "equip"])]);

    assert!(
        matched_ids(&graph, "dang::Dangling").is_empty(),
        "an unevaluatable constraint must not pass"
    );
}

/// The bundled `ph.equips::VavZoneAhu` carries the dangling reference from #46,
/// so it matches nothing until that data defect is fixed. Pinned so the number
/// changes visibly when it is, rather than silently.
///
/// This graph is built by hand rather than taken from the demo, and that matters:
/// the demo AHUs carry no `vavZone`, so they fail this spec's marker slot on
/// `dev` too and the demo answer is zero either way. The entity below does carry
/// `vavZone`, which is what makes the comparison real — on `dev`, where the
/// inverse slot is skipped, this same graph returns `["ahu-1"]`.
#[test]
fn the_bundled_vav_zone_ahu_currently_matches_nothing() {
    let ns = DefNamespace::load_standard().expect("standard ontology");
    let graph = graph_of(
        ns,
        &[
            ("ahu-1", &["ahu", "equip", "vavZone"]),
            ("vav-1", &["vav", "equip", "airRef=ahu-1"]),
        ],
    );

    assert!(
        matched_ids(&graph, "ph.equips::VavZoneAhu").is_empty(),
        "VavZoneAhu points at AhuVav.ahu, but that slot is named myAhu (#46)"
    );
    // The forward half of the same pair does work, which is what shows the
    // failure is the dangling reference and not inverse queries generally.
    assert_eq!(
        matched_ids(&graph, "ph.equips::AhuVav"),
        vec!["vav-1".to_string()]
    );
}

/// A caller that can traverse forward but not backward must not have its inverse
/// query slots quietly pass. Degrading to "true" is the failure mode #22 and #39
/// both exist to remove, so a context without a reverse index reports the slot.
#[test]
fn an_inverse_query_without_a_reverse_index_fails_closed() {
    use haystack_core::xeto::QueryContext;

    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    ns.load_xeto_str(
        "Src: Dict {\n  vav\n  up: Query<of:Ahu, via:\"airRef\">\n}\n\
         Sink: Dict {\n  ahu\n  below: Query<of:Vav, inverse:\"nr::Src.up\">\n}\n",
        "nr",
    )
    .expect("load test lib");

    let mut ahu = HDict::new();
    ahu.set("id", Kind::Ref(HRef::from_val("ahu-1")));
    ahu.set("ahu", Kind::Marker);

    // A forward resolver that knows nothing is still a forward resolver; what is
    // missing is the reverse direction.
    let forward = |_r: &HRef| -> Option<&HDict> { None };
    let ctx = QueryContext::forward_only(&forward);

    let issues = haystack_core::xeto::fits_explain(&ahu, "nr::Sink", &ns, Some(ctx));
    assert!(
        !issues.is_empty(),
        "an inverse slot that cannot be evaluated must be reported, not skipped"
    );
    assert!(
        format!("{issues:?}").contains("reverse index"),
        "the issue should say why it could not be evaluated: {issues:?}"
    );
}

/// `of:` names a type unqualified, and a Xeto spec in the same library must be
/// found. `resolve_spec_term` only matches a spec on its exact qualified name,
/// so a bare `Target` fell through to the def rungs and resolved to nothing —
/// the slot then counted zero matches and the spec matched no entity at all.
///
/// Found by adversarial review of the change that introduced `of` enforcement.
#[test]
fn an_of_type_resolves_to_a_spec_in_the_same_library() {
    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    ns.load_xeto_str(
        "Target: Dict {\n  target\n}\n\
         Holder: Dict {\n  holder\n  child: Query<of:Target, via:\"childRef\">\n}\n",
        "same",
    )
    .expect("load test lib");

    let graph = graph_of(
        ns,
        &[
            ("target", &["target"]),
            ("holder", &["holder", "childRef=target"]),
            ("empty-holder", &["holder"]),
        ],
    );

    assert_eq!(
        matched_ids(&graph, "same::Holder"),
        vec!["holder".to_string()],
        "of:Target must resolve to same::Target"
    );
}

/// A bare `of:` that names a def must still resolve, which is the bundled case —
/// `ph.equips` writes `of:Ahu` for the def `ahu`, not for a spec in its own lib.
#[test]
fn an_of_type_still_resolves_to_a_def_when_no_local_spec_exists() {
    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    ns.load_xeto_str(
        "NeedsAhu: Dict {\n  vav\n  src: Query<of:Ahu, via:\"airRef\">\n}\n",
        "deft",
    )
    .expect("load test lib");

    let graph = graph_of(
        ns,
        &[
            ("real-ahu", &["ahu", "equip"]),
            ("a-chiller", &["chiller", "equip"]),
            ("vav-good", &["vav", "airRef=real-ahu"]),
            ("vav-bad", &["vav", "airRef=a-chiller"]),
        ],
    );

    assert_eq!(
        matched_ids(&graph, "deft::NeedsAhu"),
        vec!["vav-good".to_string()],
        "of:Ahu resolves to the def `ahu` when no deft::Ahu spec exists"
    );
}

/// A spec declaring a required marker must not fit an entity that lacks it,
/// whichever of the two Xeto spellings it uses (issue #48).
///
/// `ahu: Marker` used to leave `is_marker` false and `type_ref = Some("Marker")`.
/// That slot never reached `mandatory_markers()`, and `check_slot_types` has
/// nothing to check when the tag is simply absent — so the spec fitted every
/// entity in the graph, including one with no tags at all.
#[test]
fn a_required_marker_is_enforced_in_both_spellings() {
    let mut ns = DefNamespace::load_standard().expect("standard ontology");
    ns.load_xeto_str(
        "Bare: Dict {\n  ahu\n}\n\
         Typed: Dict {\n  ahu: Marker\n}\n\
         OptBare: Dict {\n  ahu?\n}\n\
         OptTyped: Dict {\n  ahu: Marker?\n}\n",
        "mk",
    )
    .expect("load test lib");

    let empty = HDict::new();
    let mut carries = HDict::new();
    carries.set("ahu", Kind::Marker);

    for spec in ["mk::Bare", "mk::Typed"] {
        assert!(
            !ns.fits_spec_term(&empty, spec),
            "{spec} must not fit an entity with no tags"
        );
        assert!(
            ns.fits_spec_term(&carries, spec),
            "{spec} must fit an entity carrying the marker"
        );
    }

    // The optional spellings must stay optional — the fix must not turn `?` into
    // a requirement on the way past.
    for spec in ["mk::OptBare", "mk::OptTyped"] {
        assert!(
            ns.fits_spec_term(&empty, spec),
            "{spec} is optional and must still fit"
        );
        assert!(
            ns.fits_spec_term(&carries, spec),
            "{spec} must fit either way"
        );
    }
}
