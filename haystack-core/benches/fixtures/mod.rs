//! Deterministic benchmark data and an independent identity oracle.
use haystack_core::codecs::codec_for;
use haystack_core::data::{HDict, HGrid};
use haystack_core::filter;
use haystack_core::graph::EntityGraph;
use haystack_core::kinds::{HRef, Kind, Number};
use haystack_core::xeto::QueryContext;
use sha2::{Digest, Sha256};

pub const SCALES: [usize; 2] = [12, 125];
pub const ENTITIES_PER_CAMPUS: usize = 81;

#[derive(Clone, Copy, Debug)]
pub enum Query {
    Marker,
    Reference,
    NestedReference,
}

impl Query {
    pub const ALL: [Self; 3] = [Self::Marker, Self::Reference, Self::NestedReference];

    pub fn id(self) -> &'static str {
        match self {
            Self::Marker => "marker",
            Self::Reference => "reference",
            Self::NestedReference => "nested_reference",
        }
    }

    pub fn expression(self) -> &'static str {
        match self {
            Self::Marker => "point and sensor and temp",
            Self::Reference => "point and sensor and (temp or pressure) and siteRef == @site-0",
            Self::NestedReference => {
                "point and sensor and equipRef->ahu and equipRef->siteRef->geoCity == \"Portland\""
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CacheState {
    Cold,
    AstWarm,
    ResultWarm,
}

impl CacheState {
    pub const ALL: [Self; 3] = [Self::Cold, Self::AstWarm, Self::ResultWarm];

    pub fn id(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::AstWarm => "ast_warm_result_cold",
            Self::ResultWarm => "result_warm",
        }
    }
}

pub struct CampusFixture {
    pub campuses: usize,
    rows: Vec<HDict>,
}

impl CampusFixture {
    pub fn new(campuses: usize) -> Self {
        assert!((1..=125).contains(&campuses));
        let mut rows = Vec::with_capacity(campuses * ENTITIES_PER_CAMPUS);
        for campus in 0..campuses {
            let mut site = entity(format!("site-{campus}"), &["site"]);
            site.set("dis", Kind::Str(format!("Campus {campus}")));
            site.set(
                "geoCity",
                Kind::Str(
                    if campus % 2 == 0 {
                        "Portland"
                    } else {
                        "Seattle"
                    }
                    .into(),
                ),
            );
            rows.push(site);

            for (equip, (name, markers)) in [
                ("ahu", &["equip", "ahu", "hvac"][..]),
                ("ahu", &["equip", "ahu", "hvac"]),
                ("ahu", &["equip", "ahu", "hvac"]),
                ("vav", &["equip", "vav", "hvac"]),
                ("vav", &["equip", "vav", "hvac"]),
                ("boiler", &["equip", "boiler", "hotWaterHeating"]),
                ("meter", &["equip", "meter", "elecMeter"]),
                ("weather", &["equip", "weatherStation"]),
            ]
            .iter()
            .enumerate()
            {
                let equip_id = format!("{name}-{campus}-{equip}");
                let mut equipment = entity(equip_id.clone(), markers);
                equipment.set("siteRef", reference(format!("site-{campus}")));
                rows.push(equipment);

                for (point, (kind, tags, unit, value)) in [
                    ("temp", &["sensor", "temp", "air"][..], "°F", 72.0),
                    ("pressure", &["sensor", "pressure", "air"], "inH₂O", 1.2),
                    ("flow", &["sensor", "flow", "air"], "cfm", 1500.0),
                    ("occ", &["sensor", "occ"], "%", 85.0),
                    ("damper", &["cmd", "damper"], "%", 50.0),
                    ("speed", &["cmd", "speed", "fan"], "%", 75.0),
                    ("sp", &["sp", "temp", "air"], "°F", 72.0),
                    ("enable", &["cmd", "enable"], "", 1.0),
                    ("alarm", &["sensor", "alarm"], "", 0.0),
                ]
                .iter()
                .enumerate()
                {
                    let mut row = entity(format!("pt-{campus}-{equip}-{kind}-{point}"), tags);
                    row.set("point", Kind::Marker);
                    row.set("kind", Kind::Str("Number".into()));
                    row.set("equipRef", reference(equip_id.clone()));
                    row.set("siteRef", reference(format!("site-{campus}")));
                    row.set(
                        "curVal",
                        Kind::Number(Number::new(
                            value + point as f64 * 0.5 + campus as f64 * 0.1,
                            (!unit.is_empty()).then(|| (*unit).into()),
                        )),
                    );
                    rows.push(row);
                }
            }
        }
        assert_eq!(rows.len(), campuses * ENTITIES_PER_CAMPUS);
        Self { campuses, rows }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn graph(&self, query: Query, state: CacheState) -> EntityGraph {
        let mut graph = EntityGraph::new();
        for row in &self.rows {
            graph.add(row.clone()).expect("unique benchmark entity ID");
        }
        assert_eq!(graph.len(), self.len());
        match state {
            CacheState::Cold => {}
            CacheState::AstWarm => {
                graph
                    .read(query.expression(), usize::MAX)
                    .expect("AST warmup");
            }
            CacheState::ResultWarm => {
                graph.read(query.expression(), 0).expect("result warmup");
            }
        }
        graph
    }

    /// Enumerate identities from the written fixture specification, without
    /// inspecting row tags or calling the production filter/candidate code.
    pub fn expected_ids(&self, query: Query, site_zero_moved: bool) -> Vec<String> {
        let mut ids = Vec::new();
        match query {
            Query::Marker => {
                for campus in 0..self.campuses {
                    for equip in 0..8 {
                        ids.push(format!("pt-{campus}-{equip}-temp-0"));
                    }
                }
            }
            Query::Reference => {
                for equip in 0..8 {
                    ids.push(format!("pt-0-{equip}-temp-0"));
                    ids.push(format!("pt-0-{equip}-pressure-1"));
                }
            }
            Query::NestedReference => {
                for campus in (0..self.campuses).step_by(2) {
                    if site_zero_moved && campus == 0 {
                        continue;
                    }
                    for equip in 0..3 {
                        for (kind, point) in [
                            ("temp", 0),
                            ("pressure", 1),
                            ("flow", 2),
                            ("occ", 3),
                            ("alarm", 8),
                        ] {
                            ids.push(format!("pt-{campus}-{equip}-{kind}-{point}"));
                        }
                    }
                }
            }
        }
        ids.sort();
        ids
    }

    /// Hash sorted IDs/tags with Zinc scalar encodings, independent of HashMap order.
    pub fn digest(&self) -> String {
        let zinc = codec_for("text/zinc").expect("Zinc codec");
        let mut rows: Vec<_> = self.rows.iter().collect();
        rows.sort_by_key(|row| &row.id().expect("fixture ID").val);
        let mut hash = Sha256::new();
        for row in rows {
            for (name, value) in row.sorted_tags() {
                hash.update(name.as_bytes());
                hash.update(b"\t");
                hash.update(
                    zinc.encode_scalar(value)
                        .expect("fixture scalar")
                        .as_bytes(),
                );
                hash.update(b"\n");
            }
            hash.update(b"\n");
        }
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// All validation graphs are discarded before Criterion prepares timed ones.
    pub fn validate(&self) {
        for query in Query::ALL {
            let expected = self.expected_ids(query, false);
            let graph = self.graph(query, CacheState::Cold);
            assert_eq!(full_evaluator_ids(&graph, query), expected);
            for state in CacheState::ALL {
                let graph = self.graph(query, state);
                assert_eq!(
                    grid_ids(&graph.read(query.expression(), 0).unwrap()),
                    expected
                );
            }
        }
        let query = Query::NestedReference;
        let mut graph = self.graph(query, CacheState::ResultWarm);
        move_site_zero(&mut graph);
        let expected = self.expected_ids(query, true);
        assert_eq!(full_evaluator_ids(&graph, query), expected);
        assert_eq!(
            grid_ids(&graph.read(query.expression(), 0).unwrap()),
            expected
        );
        assert_eq!(graph.len(), self.len());
    }
}

pub fn grid_ids(grid: &HGrid) -> Vec<String> {
    let mut ids: Vec<_> = grid
        .rows
        .iter()
        .map(|row| row.id().expect("result ID").val.clone())
        .collect();
    ids.sort();
    ids
}

pub fn full_evaluator_ids(graph: &EntityGraph, query: Query) -> Vec<String> {
    let ast = filter::parse_filter(query.expression()).expect("fixture query");
    let resolve = |reference: &HRef| graph.get(&reference.val);
    let context = QueryContext::forward_only(&resolve);
    let mut ids: Vec<_> = graph
        .all()
        .into_iter()
        .filter(|row| filter::matches(&ast, row, Some(context)))
        .map(|row| row.id().expect("fixture ID").val.clone())
        .collect();
    ids.sort();
    ids
}

pub fn move_site_zero(graph: &mut EntityGraph) {
    let mut changes = HDict::new();
    changes.set("geoCity", Kind::Str("Seattle".into()));
    graph
        .update("site-0", changes)
        .expect("fixture site exists");
}

fn entity(id: String, markers: &[&str]) -> HDict {
    let mut row = HDict::new();
    row.set("id", reference(id));
    for marker in markers {
        row.set(*marker, Kind::Marker);
    }
    row
}

fn reference(id: String) -> Kind {
    Kind::Ref(HRef::from_val(id))
}
