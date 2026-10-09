//! Independent expectations from the complete pinned PR07 declarations at Xeto
//! 873b922451d3ef4c0c9c08ef3daa542f352d69f3 (sys/types.xeto:179,
//! ph/kinds.xeto:22, ph.protocols/base.xeto:13, ph.protocols/modbus.xeto:17-96)
//! plus clearly project-owned fixture declarations. No complete-library claim.
use haystack_core::{
    codecs::jeto::{self, Boxing, Limits},
    data::HDict,
    graph::{EntityGraph, GraphWake, SharedGraph},
    kinds::{HRef, Kind, NominalScalar, Number},
    xeto::catalog::{
        ActivatedCatalog, ActivationControl, ActivationError, Catalog, FitEnvironment, FitRecord,
        PINNED_XETO_REVISION, ProfileError,
    },
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

const REPOSITORY: &str = "https://github.com/Project-Haystack/xeto";

// Project-owned; not attributed to upstream.
const PROJECT: &str = r#"
pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"},{lib:"ph",versions:"5.0.0"},{lib:"ph.protocols",versions:"5.0.0"}}>
Owner: sys::Entity { name:Str }
AddressFeature: ph::Feature { addr:ph.protocols::ModbusAddr }
Asset: sys::Entity {
  details:AddressFeature
  owner:Ref?<of:Owner>
  label:Str?
}
Pump: Asset { pump }
"#;

fn project() -> Catalog {
    Catalog::load_protocol_pinned()
        .unwrap()
        .with_project("fixture", PROJECT, &[("pump", "fixture::Pump")])
        .unwrap()
}
fn nominal(name: &str, text: &str) -> Kind {
    Kind::Nominal(NominalScalar::new(name, REPOSITORY, PINNED_XETO_REVISION, text).unwrap())
}
fn owner() -> HDict {
    let mut owner = HDict::new();
    owner.set("id", Kind::Ref(HRef::from_val("owner-1")));
    owner.set("spec", Kind::Ref(HRef::from_val("fixture::Owner")));
    owner.set("name", Kind::Str("Owner".into()));
    owner
}
fn pump() -> HDict {
    let mut addr = HDict::new();
    addr.set(
        "spec",
        Kind::Ref(HRef::from_val("ph.protocols::ModbusAddr")),
    );
    addr.set("addr", Kind::Str("400001".into()));
    addr.set("encoding", nominal("ph.protocols::ModbusEncoding", "f4"));
    addr.set("access", nominal("ph.protocols::ModbusAccess", "r"));
    addr.set("bitIndex", Kind::Int(0));
    addr.set(
        "scale",
        nominal("ph.protocols::ModbusScaleExpr", "+32768 /10"),
    );
    let mut details = HDict::new();
    details.set("spec", Kind::Ref(HRef::from_val("fixture::AddressFeature")));
    details.set("addr", Kind::Dict(Box::new(addr)));
    let mut pump = HDict::new();
    pump.set("id", Kind::Ref(HRef::from_val("pump-1")));
    pump.set("spec", Kind::Ref(HRef::from_val("fixture::Pump")));
    pump.set("pump", Kind::Marker);
    pump.set("details", Kind::Dict(Box::new(details)));
    pump.set("owner", Kind::Ref(HRef::from_val("owner-1")));
    pump
}
fn records() -> BTreeMap<String, HDict> {
    BTreeMap::from([("owner-1".into(), owner())])
}
struct Env<'a>(&'a BTreeMap<String, HDict>);
impl<'a> FitEnvironment<'a> for Env<'a> {
    type Error = ();
    fn work(&mut self, _: usize) -> Result<(), ()> {
        Ok(())
    }
    fn retain(&mut self, _: usize) -> Result<(), ()> {
        Ok(())
    }
    fn depth(&mut self, depth: usize) -> Result<(), ()> {
        if depth > 64 { Err(()) } else { Ok(()) }
    }
    fn record(&mut self, id: &HRef) -> Result<Option<FitRecord<'a>>, ()> {
        Ok(self.0.get(&id.val).map(FitRecord::Borrowed))
    }
}
fn fit(catalog: &Catalog, row: &HDict, records: &BTreeMap<String, HDict>) -> Result<(), String> {
    catalog
        .fit_entity("fixture::Pump", row, &mut Env(records))
        .map_err(|error| match error {
            haystack_core::xeto::catalog::FitError::Invalid(ProfileError::Fit { slot, .. }) => slot,
            other => format!("{other:?}"),
        })
}
fn edit_address(row: &mut HDict, key: &str, value: Option<Kind>) {
    let Some(Kind::Dict(mut details)) = row.remove_tag("details") else {
        panic!("details")
    };
    let Some(Kind::Dict(mut addr)) = details.remove_tag("addr") else {
        panic!("addr")
    };
    match value {
        Some(value) => addr.set(key, value),
        None => {
            addr.remove_tag(key);
        }
    }
    details.set("addr", Kind::Dict(addr));
    row.set("details", Kind::Dict(details));
}

#[test]
fn selected_protocol_closure_preserves_complete_declarations_and_provenance() {
    let catalog = Catalog::load_protocol_pinned().unwrap();
    let bootstrap = Catalog::load_http_pinned().unwrap();
    let added = catalog
        .declarations()
        .filter(|d| bootstrap.declaration(&d.spec.qname).is_none())
        .map(|d| d.spec.qname.as_str())
        .collect::<Vec<_>>();
    // Eight selected declarations plus the sys::This metadata dependency.
    assert_eq!(
        added,
        [
            "ph.protocols::ModbusAccess",
            "ph.protocols::ModbusAddr",
            "ph.protocols::ModbusByteOrder",
            "ph.protocols::ModbusEncoding",
            "ph.protocols::ModbusScaleExpr",
            "ph.protocols::ProtocolAddr",
            "ph::Feature",
            "sys::Entity",
            "sys::This"
        ]
    );
    let entity = &catalog.declaration("sys::Entity").unwrap().spec;
    assert_eq!(entity.base.as_deref(), Some("sys::Dict"));
    assert!(entity.is_abstract);
    assert_eq!(entity.slots.len(), 2);
    assert_eq!(entity.slots[0].type_ref.as_deref(), Some("sys::Ref"));
    assert!(!entity.slots[0].is_maybe());
    assert!(entity.slots[1].is_maybe());
    assert!(matches!(entity.slots[1].meta.get("of"), Some(Kind::Ref(r)) if r.val == "sys::Spec"));
    let address = &catalog
        .declaration("ph.protocols::ModbusAddr")
        .unwrap()
        .spec;
    assert_eq!(address.base.as_deref(), Some("ph.protocols::ProtocolAddr"));
    assert_eq!(
        address
            .slots
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        [
            "addr",
            "encoding",
            "bitIndex",
            "access",
            "scale",
            "byteOrder",
            "dis"
        ]
    );
    assert!(
        matches!(address.slots[0].meta.get("pattern"), Some(Kind::Str(p)) if p == "[0134]\\d{5}")
    );
    assert_eq!(
        catalog.enum_keys("ph.protocols::ModbusAccess").unwrap(),
        ["r", "rw", "w"]
    );
    assert_eq!(
        catalog.enum_keys("ph.protocols::ModbusEncoding").unwrap(),
        ["bit", "f4", "f8", "s1", "s2", "s4", "s8", "u1", "u2", "u4"]
    );
    assert_eq!(
        catalog.enum_keys("ph.protocols::ModbusByteOrder").unwrap(),
        ["be", "le", "leb", "lew"]
    );
    assert_eq!(
        catalog
            .declaration("ph.protocols::ModbusScaleExpr")
            .unwrap()
            .spec
            .base
            .as_deref(),
        Some("sys::Scalar")
    );
    assert!(catalog.libraries().all(|library| !library.complete));
    assert!(!catalog.provenance().complete_libraries);
    assert_eq!(catalog.provenance().commit, PINNED_XETO_REVISION);
    assert!(
        catalog
            .libraries()
            .find(|library| library.name == "ph")
            .unwrap()
            .depends
            .contains(&"sys.refs".into())
    );
    // Dependency-only identity: retained source, no admitted declarations.
    let refs = catalog
        .provenance()
        .files
        .iter()
        .find(|file| file.library == "sys.refs")
        .unwrap();
    assert_eq!(refs.role, "dependency");
    assert_eq!(
        refs.sha256,
        "33ef7470efd40fe528ab10c6e54bc64b1f3ceb1a6543061fd38617aa3f490742"
    );
    assert!(catalog.declarations().all(|d| d.spec.lib != "sys.refs"));
    // Dependency identity is not advertised as an admitted library.
    assert!(
        catalog
            .libraries()
            .all(|library| library.name != "sys.refs")
    );
    assert_eq!(
        catalog
            .libraries()
            .map(|library| library.name.as_str())
            .collect::<Vec<_>>(),
        ["ph", "ph.protocols", "sys", "sys.api"]
    );
    let selected: Vec<_> = catalog
        .provenance()
        .files
        .iter()
        .filter(|file| file.role == "selected-types")
        .flat_map(|file| file.declarations.iter().map(String::as_str))
        .collect();
    assert_eq!(selected.len(), 7);
    for qname in &selected {
        let source = &catalog.declaration(qname).unwrap().source;
        assert_eq!(source.role, "selected-types");
        assert_eq!(source.sha256.len(), 64);
    }
    for excluded in [
        "sys::Spec",
        "ph::PhEntity",
        "ph::Site",
        "ph::Device",
        "sys::Query",
    ] {
        assert!(catalog.declaration(excluded).is_none(), "{excluded}");
    }
}

#[test]
fn declaration_defaults_are_constructed_separately_from_fitting() {
    let catalog = project();
    let address = &catalog
        .declaration("ph.protocols::ModbusAddr")
        .unwrap()
        .spec;
    let Some(Kind::Nominal(default)) = address.slots[3].default.as_ref() else {
        panic!("typed enum default")
    };
    assert_eq!(default.spec(), "ph.protocols::ModbusAccess");
    assert_eq!(default.text(), "r");
    assert_eq!(default.catalog(), REPOSITORY);
    assert_eq!(default.revision(), PINNED_XETO_REVISION);
    assert_eq!(
        catalog.slot_default("ph.protocols::ModbusAddr", "access"),
        Some(&nominal("ph.protocols::ModbusAccess", "r"))
    );
    assert_eq!(
        catalog.slot_default("ph.protocols::ModbusAddr", "encoding"),
        None
    );
    // Fitting never applies the construction default to stored data.
    let mut row = pump();
    edit_address(&mut row, "access", None);
    let before = row.clone();
    assert_eq!(
        fit(&catalog, &row, &records()).unwrap_err(),
        "fixture::Pump.details.addr.access"
    );
    assert_eq!(row, before);
}

#[test]
fn valid_pump_fits_inherited_nested_and_reference_constraints_unchanged() {
    let catalog = project();
    let (row, records) = (pump(), records());
    let before = (row.clone(), records.clone());
    fit(&catalog, &row, &records).unwrap();
    assert_eq!((row, records), before);
    // spec:@fixture::Pump resolves a catalog declaration with no graph entity
    // of that name; the Entity.spec<of:Spec> constraint is catalog-only.
    assert!(!self::records().contains_key("fixture::Pump"));
}

#[test]
fn missing_inherited_and_nested_required_slots_fail_at_exact_paths() {
    let catalog = project();
    for (tag, path) in [("id", "fixture::Pump.id"), ("pump", "fixture::Pump.pump")] {
        let mut row = pump();
        row.remove_tag(tag);
        assert_eq!(fit(&catalog, &row, &records()).unwrap_err(), path);
    }
    for (tag, path) in [
        ("addr", "fixture::Pump.details.addr.addr"),
        ("encoding", "fixture::Pump.details.addr.encoding"),
    ] {
        let mut row = pump();
        edit_address(&mut row, tag, None);
        assert_eq!(fit(&catalog, &row, &records()).unwrap_err(), path);
    }
    let mut row = pump();
    let Some(Kind::Dict(mut details)) = row.remove_tag("details") else {
        panic!()
    };
    details.remove_tag("addr");
    row.set("details", Kind::Dict(details));
    assert_eq!(
        fit(&catalog, &row, &records()).unwrap_err(),
        "fixture::Pump.details.addr"
    );
}

#[test]
fn pattern_integer_bounds_enum_and_scalar_identity_are_exact() {
    let catalog = project();
    for (key, value) in [
        ("bitIndex", Kind::Int(0)),
        ("bitIndex", Kind::Int(15)),
        ("bitIndex", Kind::Null),
        ("addr", Kind::Str("000001".into())),
        ("addr", Kind::Str("465536".into())),
        ("encoding", nominal("ph.protocols::ModbusEncoding", "u2")),
        ("access", nominal("ph.protocols::ModbusAccess", "rw")),
        ("byteOrder", nominal("ph.protocols::ModbusByteOrder", "lew")),
        ("scale", Kind::Null),
    ] {
        let mut row = pump();
        edit_address(&mut row, key, Some(value.clone()));
        fit(&catalog, &row, &records()).unwrap_or_else(|e| panic!("{key}={value:?}: {e}"));
    }
    let foreign = |name: &str, text: &str, catalog: &str, revision: &str| {
        Kind::Nominal(NominalScalar::new(name, catalog, revision, text).unwrap())
    };
    for (key, value) in [
        ("addr", Kind::Str("foo".into())),
        ("addr", Kind::Str("200001".into())),
        ("addr", Kind::Str("4000011".into())),
        ("bitIndex", Kind::Int(-1)),
        ("bitIndex", Kind::Int(16)),
        ("bitIndex", Kind::Number(Number::unitless(7.0))),
        ("scale", Kind::Bool(true)),
        ("scale", Kind::Int(7)),
        ("scale", Kind::Str("*10".into())),
        ("scale", Kind::Dict(Box::default())),
        ("encoding", nominal("ph.protocols::ModbusEncoding", "RW")),
        ("encoding", nominal("ph.protocols::ModbusEncoding", "F4")),
        ("encoding", Kind::Str("f4".into())),
        ("encoding", nominal("ph.protocols::ModbusAccess", "r")),
        (
            "encoding",
            foreign(
                "ph.protocols::ModbusEncoding",
                "f4",
                REPOSITORY,
                &"0".repeat(40),
            ),
        ),
        (
            "encoding",
            foreign(
                "ph.protocols::ModbusEncoding",
                "f4",
                "https://example.com/xeto",
                PINNED_XETO_REVISION,
            ),
        ),
        ("access", nominal("ph.protocols::ModbusAccess", "RW")),
        ("access", Kind::Null),
        ("byteOrder", Kind::Str("be".into())),
    ] {
        let mut row = pump();
        edit_address(&mut row, key, Some(value.clone()));
        assert!(
            fit(&catalog, &row, &records()).is_err(),
            "accepted invalid {key}={value:?}"
        );
    }
}

#[test]
fn nullable_slots_follow_native_nullable_semantics() {
    let catalog = project();
    for (key, value) in [
        ("label", None),
        ("label", Some(Kind::Null)),
        ("label", Some(Kind::Str("P-1".into()))),
        ("owner", None),
        ("owner", Some(Kind::Null)),
        ("spec", Some(Kind::Null)),
    ] {
        let mut row = pump();
        match value {
            Some(value) => row.set(key, value),
            None => {
                row.remove_tag(key);
            }
        }
        fit(&catalog, &row, &records()).unwrap_or_else(|e| panic!("{key}: {e}"));
    }
    for (key, value) in [
        ("label", Kind::Int(1)),
        ("owner", Kind::Str("owner-1".into())),
        ("details", Kind::Null),
        ("id", Kind::Null),
    ] {
        let mut row = pump();
        row.set(key, value);
        assert!(fit(&catalog, &row, &records()).is_err(), "{key}");
    }
}

#[test]
fn catalog_and_entity_references_resolve_through_distinct_sources() {
    let catalog = project();
    // spec:@unknown::Pump rejects even when a record has that identity.
    let mut records = records();
    let mut lookalike = owner();
    lookalike.set("id", Kind::Ref(HRef::from_val("unknown::Pump")));
    records.insert("unknown::Pump".into(), lookalike);
    let mut row = pump();
    row.set("spec", Kind::Ref(HRef::from_val("unknown::Pump")));
    assert!(fit(&catalog, &row, &records).is_err());
    // owner:@owner-1 is resolved through the record view and must fit Owner.
    for spec in [
        None,
        Some("fixture::Missing"),
        Some("fixture::AddressFeature"),
    ] {
        let mut records = self::records();
        let owner = records.get_mut("owner-1").unwrap();
        owner.remove_tag("spec");
        if let Some(spec) = spec {
            owner.set("spec", Kind::Ref(HRef::from_val(spec)));
        }
        assert!(fit(&catalog, &pump(), &records).is_err(), "{spec:?}");
    }
    let mut wrong = self::records();
    wrong.get_mut("owner-1").unwrap().remove_tag("name");
    assert!(fit(&catalog, &pump(), &wrong).is_err());
    // Dangling target.
    assert!(fit(&catalog, &pump(), &BTreeMap::new()).is_err());
    // Nested structural spec cannot be an ignored extension.
    let mut row = pump();
    edit_address(
        &mut row,
        "spec",
        Some(Kind::Ref(HRef::from_val("missing::Address"))),
    );
    assert!(fit(&catalog, &row, &self::records()).is_err());
}

#[test]
fn unsupported_and_unresolved_selections_fail_closed() {
    let base = Catalog::load_protocol_pinned().unwrap();
    let header = r#"pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"},{lib:"ph",versions:"5.0.0"}}>"#;
    // Issue #21: a missing base is a resolution error, never a match-all type.
    for body in [
        "Bad: missing::Thing { }",
        "Bad: ph::PhEntity { }",
        "Bad: ph::Site { }",
        "Bad: sys::Spec { }",
    ] {
        let source = format!("{header}\n{body}\n");
        assert!(
            matches!(
                base.with_project("bad", &source, &[]),
                Err(ProfileError::Resolve { .. })
            ),
            "{body}"
        );
    }
    // Issue #46: the bundled VavZoneAhu inverse/Query snippet remains an
    // explicit unsupported selection. No rename or typo correction is implied.
    let issue_46 = format!(
        "{header}\nVavZoneAhu : sys::Entity {{\n  vavZone\n  vavs: Query <of:Vav, inverse:\"ph.equips::AhuVav.ahu\">\n}}\n"
    );
    assert!(matches!(
        base.with_project("bad", &issue_46, &[]),
        Err(ProfileError::Resolve { message, .. }) if message.contains("query")
    ));
    // Parse failures keep their project source path and original line.
    let malformed = format!("{header}\nGood: sys::Dict {{ }}\nBad: sys::Dict {{ name: }}\n");
    match base.with_project("bad", &malformed, &[]) {
        Err(ProfileError::Parse { path, line, .. }) => {
            assert_eq!(path, "project/bad/lib.xeto");
            assert_eq!(line, 3);
        }
        other => panic!("expected a located parse error: {other:?}"),
    }
    let global = format!("{header}\n*site: Marker\n");
    assert!(base.with_project("bad", &global, &[]).is_err());
    // Upstream libraries cannot be replaced by project-owned bytes.
    assert!(
        base.with_project("ph", &format!("{header}\nX: sys::Dict {{ }}\n"), &[])
            .is_err()
    );
    // Issue #21 witness: arbitrary records no longer fit protocol addresses.
    let mut weather = HDict::new();
    weather.set("id", Kind::Ref(HRef::from_val("w")));
    weather.set("dis", Kind::Str("Weather".into()));
    weather.set("weather", Kind::Marker);
    for qname in ["ph.protocols::ModbusAddr", "ph.protocols::ProtocolAddr"] {
        assert!(
            base.fit_entity(qname, &weather, &mut Env(&BTreeMap::new()))
                .is_err(),
            "{qname} must not match every entity"
        );
    }
}

#[test]
fn generic_dict_results_encode_selected_nominals_with_additive_contexts() {
    let first = ActivatedCatalog::new(project(), None).unwrap();
    let unrelated = r#"pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"}}>
Tag: sys::Dict { note:Str? }
"#;
    let second = ActivatedCatalog::new(
        project().with_project("extra", unrelated, &[]).unwrap(),
        None,
    )
    .unwrap();
    second.check_provenance(&first).unwrap();
    let bootstrap = ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap();
    let value = Kind::Dict(Box::new(pump()));
    let encode = |observation: &ActivatedCatalog| {
        let context = &observation.callable("sys.api::readById").unwrap().context;
        jeto::encode(
            &value,
            context,
            Some("sys::Dict"),
            Boxing::Auto,
            Limits::default(),
        )
        .map_err(|error| format!("{error:?}"))
        .and_then(|encoding| {
            encoding
                .into_exact()
                .map_err(|issues| format!("{issues:?}"))
        })
    };
    // The bootstrap context cannot represent the selected nominal values.
    assert!(encode(&bootstrap).is_err());
    let bytes = encode(&first).unwrap();
    // Additive same-origin context: identical bytes and unchanged values.
    assert_eq!(encode(&second).unwrap(), bytes);
    for observation in [&first, &second] {
        let context = &observation.callable("sys.api::readById").unwrap().context;
        assert_eq!(
            jeto::decode(&bytes, context, Some("sys::Dict"), Limits::default()).unwrap(),
            value
        );
    }
    assert_ne!(first.selection_identity(), second.selection_identity());
    assert_ne!(first.selection_identity(), PINNED_XETO_REVISION);
}

#[test]
fn derived_namespace_restores_displaced_specs_and_uses_strict_matching() {
    use haystack_core::ontology::DefNamespace;
    let mut base = DefNamespace::new();
    base.register_spec(haystack_core::xeto::Spec::new(
        "sys::Entity",
        "sys",
        "Entity",
    ));
    base.register_spec(haystack_core::xeto::Spec::new(
        "legacy::Thing",
        "legacy",
        "Thing",
    ));
    let first = ActivatedCatalog::new(project(), Some(&base)).unwrap();
    let ns = first.namespace();
    assert!(ns.get_spec("fixture::Pump").is_some());
    assert!(ns.get_spec("legacy::Thing").is_some());
    assert_eq!(
        ns.get_spec("sys::Entity").unwrap().slots.len(),
        2,
        "selection supplies sys::Entity"
    );
    let rebased = ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), Some(ns)).unwrap();
    let ns = rebased.namespace();
    assert!(
        ns.get_spec("fixture::Pump").is_none(),
        "old selection removed"
    );
    assert!(
        ns.get_spec("sys::Entity").unwrap().slots.is_empty(),
        "displaced spec restored"
    );
    assert!(ns.get_spec("legacy::Thing").is_some());
    assert_eq!(ns.specs(Some("fixture")).len(), 0);
    // Uncontrolled filter evaluation uses the strict selected fitter.
    let mut graph = EntityGraph::with_namespace(first.namespace().clone());
    graph.add(owner()).unwrap();
    graph.add(pump()).unwrap();
    let mut bad = pump();
    bad.set("id", Kind::Ref(HRef::from_val("pump-2")));
    edit_address(&mut bad, "bitIndex", Some(Kind::Int(16)));
    graph.add(bad).unwrap();
    let mut loose = HDict::new();
    loose.set("id", Kind::Ref(HRef::from_val("loose")));
    loose.set("pump", Kind::Marker);
    graph.add(loose).unwrap();
    let ids: Vec<_> = graph
        .read_all("fixture::Pump", 0)
        .unwrap()
        .into_iter()
        .map(|row| row.id().unwrap().val.clone())
        .collect();
    assert_eq!(ids, ["pump-1"]);
}

// ── Activation ──

#[derive(Default)]
struct Control {
    work: usize,
    limit: Option<usize>,
    waits: usize,
    published: usize,
    sealed: bool,
    reject_admission: bool,
    chunk: Option<usize>,
    revalidations: Option<usize>,
    retained: usize,
    peak_retained: usize,
    #[allow(clippy::type_complexity)]
    on_wait: Option<Box<dyn FnMut(usize)>>,
}
impl ActivationControl for Control {
    type Error = &'static str;
    fn work(&mut self, amount: usize) -> Result<(), &'static str> {
        self.work = self.work.saturating_add(amount);
        if self.limit.is_some_and(|limit| self.work > limit) {
            return Err("work");
        }
        Ok(())
    }
    fn retain(&mut self, bytes: usize) -> Result<(), &'static str> {
        self.retained += bytes;
        self.peak_retained = self.peak_retained.max(self.retained);
        Ok(())
    }
    fn release(&mut self, bytes: usize) {
        self.retained -= bytes;
    }
    fn depth(&mut self, depth: usize) -> Result<(), &'static str> {
        if depth > 64 { Err("depth") } else { Ok(()) }
    }
    fn wait(&mut self) -> Result<Duration, &'static str> {
        self.waits += 1;
        if let Some(hook) = self.on_wait.as_mut() {
            hook(self.waits);
        }
        Ok(Duration::from_millis(100))
    }
    fn chunk_records(&self) -> usize {
        self.chunk.unwrap_or(256)
    }
    fn max_revalidations(&self) -> usize {
        self.revalidations.unwrap_or(3)
    }
    fn admit(
        &mut self,
        candidate: &Arc<ActivatedCatalog>,
    ) -> Result<(), ActivationError<&'static str>> {
        if self.reject_admission {
            return Err(ActivationError::Unsupported {
                declaration: candidate
                    .catalog()
                    .declaration("sys.api::readById")
                    .unwrap()
                    .spec
                    .qname
                    .clone(),
                reason: "fixture handler refused".into(),
            });
        }
        Ok(())
    }
    fn publish(&mut self) -> Result<(), &'static str> {
        self.published += 1;
        if self.sealed { Err("sealed") } else { Ok(()) }
    }
}

struct Managed {
    graph: SharedGraph,
    bootstrap: Arc<ActivatedCatalog>,
}
fn managed(rows: &[HDict]) -> Managed {
    let graph = SharedGraph::new(EntityGraph::new());
    for row in rows {
        graph.add(row.clone()).unwrap();
    }
    let bootstrap =
        Arc::new(ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap());
    let state = graph.state();
    graph
        .write(|g| g.compare_initialize_catalog(state, None, bootstrap.clone()))
        .unwrap();
    Managed { graph, bootstrap }
}
fn snapshot(graph: &SharedGraph) -> (haystack_core::graph::GraphState, usize, Vec<HDict>) {
    graph.read(|g| {
        (
            g.state(),
            Arc::as_ptr(g.activated_catalog().unwrap()) as usize,
            g.all().into_iter().cloned().collect(),
        )
    })
}
fn assert_no_wake(wakes: &mut tokio::sync::broadcast::Receiver<GraphWake>) {
    assert!(wakes.try_recv().is_err());
}

#[test]
fn compare_activation_requires_full_state_and_observation_identity() {
    let mut graph = EntityGraph::new();
    let bootstrap =
        Arc::new(ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap());
    let next = Arc::new(ActivatedCatalog::new(project(), None).unwrap());
    let initial = graph.state();
    graph
        .compare_initialize_catalog(initial, None, bootstrap.clone())
        .unwrap();
    let base = graph.namespace_arc().cloned();
    assert!(
        graph
            .compare_initialize_catalog(graph.state(), base.as_ref(), next.clone())
            .is_err()
    );
    let before = graph.state();
    assert!(
        graph
            .namespace()
            .unwrap()
            .resolve_spec_term("fixture::Pump")
            .is_none()
    );
    assert_eq!(
        graph
            .compare_activate_catalog(before, &bootstrap, next.clone())
            .unwrap(),
        before.catalog_generation + 1
    );
    assert!(Arc::ptr_eq(graph.activated_catalog().unwrap(), &next));
    assert!(
        graph
            .namespace()
            .unwrap()
            .resolve_spec_term("fixture::Pump")
            .is_some()
    );
    assert_eq!(graph.state().revision, before.revision);
    assert_eq!(graph.state().incarnation, before.incarnation);
    assert!(
        graph
            .compare_activate_catalog(before, &bootstrap, bootstrap.clone())
            .is_err()
    );
    let newer = graph.state();
    assert!(
        graph
            .compare_activate_catalog(newer, &next, next.clone())
            .is_err()
    );
    assert!(
        graph
            .compare_activate_catalog(newer, &bootstrap, bootstrap.clone())
            .is_err()
    );
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("new")));
    graph.add(row).unwrap();
    assert!(
        graph
            .compare_activate_catalog(newer, &next, bootstrap.clone())
            .is_err()
    );
    assert!(Arc::ptr_eq(graph.activated_catalog().unwrap(), &next));
    assert!(
        next.callable("sys.api::readById")
            .unwrap()
            .context
            .contains("ph.protocols::ModbusScaleExpr")
    );
}

#[test]
fn trusted_namespace_replacement_rebases_without_stale_pairing() {
    let Managed { graph, bootstrap } = managed(&[]);
    let before = graph.state();
    graph.set_namespace(haystack_core::ontology::DefNamespace::new());
    let after = graph.read(|g| g.activated_catalog().cloned()).unwrap();
    assert!(!Arc::ptr_eq(&after, &bootstrap));
    assert_eq!(after.selection_identity(), bootstrap.selection_identity());
    assert!(graph.read(|g| Arc::ptr_eq(g.namespace_arc().unwrap(), after.namespace())));
    assert_eq!(
        graph.state().catalog_generation,
        before.catalog_generation + 1
    );
    // The retained old observation can no longer publish.
    assert!(matches!(
        graph.activate_catalog(&bootstrap, project(), &mut Control::default()),
        Err(ActivationError::Conflict)
    ));
    // A raw replacement is unmanaged and is never silently re-bootstrapped.
    graph.write(|g| *g = EntityGraph::new());
    assert!(graph.read(|g| g.activated_catalog().is_none()));
    assert!(matches!(
        graph.activate_catalog(&after, project(), &mut Control::default()),
        Err(ActivationError::Unmanaged)
    ));
}

#[test]
fn valid_activation_publishes_once_and_wakes_after_unlock() {
    let Managed { graph, bootstrap } = managed(&[owner(), pump()]);
    let rows_before = snapshot(&graph).2;
    let before = graph.state();
    let mut wakes = graph.subscribe_wakes();
    let mut revisions = graph.subscribe();
    let mut control = Control::default();
    let (next, published) = graph
        .activate_catalog(&bootstrap, project(), &mut control)
        .unwrap();
    assert_eq!(control.published, 1);
    let (state, observation, rows) = snapshot(&graph);
    assert_eq!(published, state);
    assert_eq!(observation, Arc::as_ptr(&next) as usize);
    assert_eq!(
        rows, rows_before,
        "activation never repairs or inserts data"
    );
    assert_eq!(state.revision, before.revision);
    assert_eq!(state.incarnation, before.incarnation);
    assert_eq!(state.catalog_generation, before.catalog_generation + 1);
    assert_eq!(wakes.try_recv().unwrap(), GraphWake::Catalog(state));
    assert_no_wake(&mut wakes);
    assert!(revisions.try_recv().is_err(), "no entity notification");
    assert!(graph.read(|g| g.namespace().unwrap().get_spec("fixture::Pump").is_some()));
}

#[test]
fn invalid_affected_data_preserves_complete_state_and_discloses_nothing_publicly() {
    let mut bad = pump();
    edit_address(&mut bad, "addr", Some(Kind::Str("foo".into())));
    // A record with only the bound marker is associated and must fit too.
    let mut marker_only = HDict::new();
    marker_only.set("id", Kind::Ref(HRef::from_val("marker-only")));
    marker_only.set("pump", Kind::Marker);
    for rows in [vec![owner(), bad], vec![owner(), pump(), marker_only]] {
        let Managed { graph, bootstrap } = managed(&rows);
        let before = snapshot(&graph);
        let mut wakes = graph.subscribe_wakes();
        let mut control = Control::default();
        let Err(ActivationError::Rejected(rejection)) =
            graph.activate_catalog(&bootstrap, project(), &mut control)
        else {
            panic!("expected graph-validation rejection")
        };
        assert_eq!(control.published, 0);
        let (entity, association, cause) = rejection.privileged();
        assert!(["pump-1", "marker-only"].contains(&entity));
        assert_eq!(association, "fixture::Pump");
        assert!(matches!(cause, ProfileError::Fit { .. }));
        for public in [rejection.to_string(), format!("{rejection:?}")] {
            assert!(
                !public.contains(entity) && !public.contains("fixture"),
                "{public}"
            );
        }
        assert_eq!(snapshot(&graph), before);
        assert_no_wake(&mut wakes);
    }
}

#[test]
fn unload_cannot_discard_existing_obligations() {
    let Managed { graph, bootstrap } = managed(&[owner(), pump()]);
    let (next, _) = graph
        .activate_catalog(&bootstrap, project(), &mut Control::default())
        .unwrap();
    let before = snapshot(&graph);
    let mut wakes = graph.subscribe_wakes();
    // Dropping the project library would orphan both explicit spec and marker
    // associations; untyped graphs may still unload.
    let result = graph.activate_catalog(
        &next,
        Catalog::load_protocol_pinned().unwrap(),
        &mut Control::default(),
    );
    let Err(ActivationError::Rejected(rejection)) = result else {
        panic!("unload must be rejected")
    };
    assert!(matches!(
        rejection.privileged().2,
        ProfileError::Resolve { .. }
    ));
    assert_eq!(snapshot(&graph), before);
    assert_no_wake(&mut wakes);
    let Managed { graph, bootstrap } = managed(&[]);
    let (next, _) = graph
        .activate_catalog(&bootstrap, project(), &mut Control::default())
        .unwrap();
    graph
        .activate_catalog(
            &next,
            Catalog::load_protocol_pinned().unwrap(),
            &mut Control::default(),
        )
        .unwrap();
}

#[test]
fn stops_conflicts_and_seals_never_publish() {
    type Setup = Box<dyn Fn(&SharedGraph) -> Control>;
    let cases: Vec<(&str, Setup)> = vec![
        (
            "sealed",
            Box::new(|_| Control {
                sealed: true,
                ..Control::default()
            }),
        ),
        (
            "admission",
            Box::new(|_| Control {
                reject_admission: true,
                ..Control::default()
            }),
        ),
        (
            "work",
            Box::new(|_| Control {
                limit: Some(3),
                ..Control::default()
            }),
        ),
        (
            "competing catalog",
            Box::new(|graph| {
                let graph = graph.clone();
                Control {
                    on_wait: Some(Box::new(move |n| {
                        if n == 3 {
                            graph.set_namespace(haystack_core::ontology::DefNamespace::new());
                        }
                    })),
                    ..Control::default()
                }
            }),
        ),
        (
            "replacement",
            Box::new(|graph| {
                let graph = graph.clone();
                Control {
                    on_wait: Some(Box::new(move |n| {
                        if n == 3 {
                            graph.write(|g| *g = EntityGraph::new());
                        }
                    })),
                    ..Control::default()
                }
            }),
        ),
    ];
    for (name, setup) in cases {
        let Managed { graph, bootstrap } = managed(&[owner(), pump()]);
        let mut control = setup(&graph);
        let mut wakes = graph.subscribe_wakes();
        let result = graph.activate_catalog(&bootstrap, project(), &mut control);
        let current = graph.read(|g| g.activated_catalog().cloned());
        assert!(
            current.is_none_or(|c| c.namespace().get_spec("fixture::Pump").is_none()),
            "{name} published"
        );
        match name {
            "admission" => {
                assert!(
                    matches!(&result, Err(ActivationError::Unsupported { declaration, .. })
                        if declaration == "sys.api::readById"),
                    "{name}: {result:?}"
                );
                assert_no_wake(&mut wakes);
            }
            "competing catalog" | "replacement" => {
                assert!(
                    matches!(
                        result,
                        Err(ActivationError::Conflict | ActivationError::Unmanaged)
                    ),
                    "{name}: {result:?}"
                );
                // Only the competing writer's own wake, never an activation wake.
                while let Ok(wake) = wakes.try_recv() {
                    // Bootstrap is generation 1 and the competing writer 2.
                    assert!(!matches!(wake, GraphWake::Catalog(s) if s.catalog_generation > 2));
                }
            }
            _ => {
                assert!(
                    matches!(result, Err(ActivationError::Control(_))),
                    "{name}: {result:?}"
                );
                assert_no_wake(&mut wakes);
                assert!(Arc::ptr_eq(
                    &graph.read(|g| g.activated_catalog().cloned()).unwrap(),
                    &bootstrap
                ));
            }
        }
    }
}

#[test]
fn entity_mutation_between_validation_and_publication_revalidates() {
    // An invalid write after validation must be caught by revalidation.
    let Managed { graph, bootstrap } = managed(&[owner(), pump()]);
    let writer = graph.clone();
    let mut control = Control {
        on_wait: Some(Box::new(move |n| {
            if n == 3 {
                let mut bad = pump();
                bad.set("id", Kind::Ref(HRef::from_val("pump-late")));
                edit_address(&mut bad, "bitIndex", Some(Kind::Int(16)));
                writer.add(bad).unwrap();
            }
        })),
        ..Control::default()
    };
    let observation = graph.read(|g| Arc::as_ptr(g.activated_catalog().unwrap()) as usize);
    let result = graph.activate_catalog(&bootstrap, project(), &mut control);
    assert!(
        matches!(result, Err(ActivationError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(
        graph.read(|g| Arc::as_ptr(g.activated_catalog().unwrap()) as usize),
        observation
    );
    assert_eq!(graph.state().catalog_generation, 1);

    // A valid concurrent write is revalidated and then published exactly once.
    let Managed { graph, bootstrap } = managed(&[owner(), pump()]);
    let writer = graph.clone();
    let mut control = Control {
        on_wait: Some(Box::new(move |n| {
            if n == 3 {
                let mut good = pump();
                good.set("id", Kind::Ref(HRef::from_val("pump-late")));
                writer.add(good).unwrap();
            }
        })),
        ..Control::default()
    };
    let mut wakes = graph.subscribe_wakes();
    graph
        .activate_catalog(&bootstrap, project(), &mut control)
        .unwrap();
    assert!(control.waits >= 5, "revalidated after the entity change");
    assert_eq!(
        control.published, 1,
        "the commit point is reached only after the state matched"
    );
    let wakes: Vec<_> = std::iter::from_fn(|| wakes.try_recv().ok()).collect();
    assert_eq!(
        wakes
            .iter()
            .filter(|w| matches!(w, GraphWake::Catalog(_)))
            .count(),
        1
    );
}

#[test]
fn bootstrap_initialization_ignores_entity_writes_but_not_catalog_inputs() {
    let bootstrap =
        || Arc::new(ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap());
    // Entity writes between capture and publication do not invalidate it.
    let mut graph = EntityGraph::new();
    let captured = graph.state();
    graph.add(owner()).unwrap();
    assert_eq!(
        graph
            .compare_initialize_catalog(captured, None, bootstrap())
            .unwrap(),
        captured.catalog_generation + 1
    );
    // A namespace replacement does: the observation was derived from it.
    let mut graph = EntityGraph::new();
    let captured = graph.state();
    graph.set_namespace(haystack_core::ontology::DefNamespace::new());
    assert!(
        graph
            .compare_initialize_catalog(captured, None, bootstrap())
            .is_err()
    );
    let base = graph.namespace_arc().cloned();
    let other = Arc::new(haystack_core::ontology::DefNamespace::new());
    assert!(
        graph
            .compare_initialize_catalog(graph.state(), Some(&other), bootstrap())
            .is_err()
    );
    graph
        .compare_initialize_catalog(graph.state(), base.as_ref(), bootstrap())
        .unwrap();
    assert!(graph.activated_catalog().is_some());
}

fn numbered_pump(n: usize, bit: i64) -> HDict {
    let mut row = pump();
    row.set("id", Kind::Ref(HRef::from_val(format!("pump-{n:03}"))));
    edit_address(&mut row, "bitIndex", Some(Kind::Int(bit)));
    row
}

#[test]
fn chunked_validation_restarts_after_interleaved_writes_and_bounds_sustained_writes() {
    let rows: Vec<HDict> = std::iter::once(owner())
        .chain((0..4).map(|n| numbered_pump(n, 0)))
        .collect();
    // Waits: 1 capture, 2..=6 one chunk per record, 7 publish. A write between
    // chunks restarts the scan; it is never stitched across two states.
    for (bit, valid) in [(1, true), (16, false)] {
        let Managed { graph, bootstrap } = managed(&rows);
        let writer = graph.clone();
        let mut control = Control {
            chunk: Some(1),
            on_wait: Some(Box::new(move |n| {
                if n == 3 {
                    writer.add(numbered_pump(900, bit)).unwrap();
                }
            })),
            ..Control::default()
        };
        let result = graph.activate_catalog(&bootstrap, project(), &mut control);
        if valid {
            result.unwrap();
            assert!(control.waits > 7, "restarted after the interleaved write");
            assert_eq!(control.published, 1);
        } else {
            let Err(ActivationError::Rejected(rejection)) = result else {
                panic!("the write after the first chunk must be revalidated")
            };
            assert_eq!(rejection.privileged().0, "pump-900");
            assert_eq!(control.published, 0);
        }
    }
    // Sustained writes end in a bounded Conflict, not budget or deadline
    // exhaustion, and publish nothing.
    let Managed { graph, bootstrap } = managed(&rows);
    let before = graph.read(|g| (g.catalog_generation(), g.activated_catalog().cloned()));
    let writer = graph.clone();
    let mut control = Control {
        chunk: Some(2),
        revalidations: Some(2),
        on_wait: Some(Box::new(move |n| {
            if n >= 3 {
                writer.add(numbered_pump(100 + n, 0)).unwrap();
            }
        })),
        ..Control::default()
    };
    let mut wakes = graph.subscribe_wakes();
    let result = graph.activate_catalog(&bootstrap, project(), &mut control);
    assert!(
        matches!(result, Err(ActivationError::Conflict)),
        "{result:?}"
    );
    assert_eq!(control.published, 0);
    let after = graph.read(|g| (g.catalog_generation(), g.activated_catalog().cloned()));
    assert_eq!(after.0, before.0);
    assert!(Arc::ptr_eq(
        after.1.as_ref().unwrap(),
        before.1.as_ref().unwrap()
    ));
    while let Ok(wake) = wakes.try_recv() {
        assert!(!matches!(wake, GraphWake::Catalog(_)));
    }
}

#[test]
fn per_record_fitting_retention_is_released() {
    let rows: Vec<HDict> = std::iter::once(owner())
        .chain((0..50).map(|n| numbered_pump(n, 0)))
        .collect();
    let Managed { graph, bootstrap } = managed(&rows);
    #[derive(Default)]
    struct Total(Control, usize);
    impl ActivationControl for Total {
        type Error = &'static str;
        fn work(&mut self, amount: usize) -> Result<(), &'static str> {
            self.0.work(amount)
        }
        fn retain(&mut self, bytes: usize) -> Result<(), &'static str> {
            self.1 += bytes;
            self.0.retain(bytes)
        }
        fn release(&mut self, bytes: usize) {
            self.0.release(bytes)
        }
        fn depth(&mut self, depth: usize) -> Result<(), &'static str> {
            self.0.depth(depth)
        }
        fn wait(&mut self) -> Result<Duration, &'static str> {
            self.0.wait()
        }
        fn admit(
            &mut self,
            candidate: &Arc<ActivatedCatalog>,
        ) -> Result<(), ActivationError<&'static str>> {
            self.0.admit(candidate)
        }
        fn publish(&mut self) -> Result<(), &'static str> {
            self.0.publish()
        }
    }
    let mut control = Total::default();
    graph
        .activate_catalog(&bootstrap, project(), &mut control)
        .unwrap();
    assert_eq!(control.0.retained, 0, "every record released its bytes");
    assert!(control.1 > 0);
    assert!(
        control.0.peak_retained * 20 < control.1,
        "peak {} is per record, not cumulative {}",
        control.0.peak_retained,
        control.1
    );
}

#[test]
fn codec_context_bounds_are_mirrored_at_selection_admission() {
    let header = r#"pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"}}>"#;
    let library = |prefix: &str| {
        let mut source = format!("{header}\n");
        for n in 0..110 {
            source.push_str(&format!("{prefix}{n}: sys::Dict {{ note:Str? }}\n"));
        }
        source
    };
    let one = Catalog::load_protocol_pinned()
        .unwrap()
        .with_project("bulka", &library("A"), &[])
        .unwrap();
    // Each library is within its own bound; together they exceed the codec
    // definition bound, which is reported at admission with the function.
    match one.with_project("bulkb", &library("B"), &[]) {
        Err(ProfileError::Resolve {
            declaration,
            message,
            ..
        }) => {
            assert!(declaration.starts_with("sys.api::"), "{declaration}");
            assert!(message.contains("codec definitions"), "{message}");
        }
        other => panic!("expected a located codec bound: {:?}", other.err()),
    }
    // The already admitted selection still activates.
    ActivatedCatalog::new(one, None).unwrap();
    // Patterns share the codec's 1024-byte bound and are rejected at the
    // declaring slot instead of failing later in codec compilation.
    let base = Catalog::load_protocol_pinned().unwrap();
    for (size, admitted) in [(1024, true), (1025, false)] {
        let source = format!(
            "{header}\nCode: sys::Dict {{ code: Str <pattern:\"{}\"> }}\n",
            "a".repeat(size)
        );
        match base.with_project("patterns", &source, &[]) {
            Ok(_) => assert!(admitted, "{size}-byte pattern admitted"),
            Err(ProfileError::Resolve {
                declaration,
                message,
                ..
            }) => {
                assert!(!admitted, "{size}: {message}");
                assert!(declaration.starts_with("patterns::Code"), "{declaration}");
                assert!(message.contains("1024"), "{message}");
            }
            Err(other) => panic!("{size}: {other}"),
        }
    }
}
