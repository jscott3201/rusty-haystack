//! Typed `ph.api::nav` over real HTTP (M2-PR05). The hierarchy, policy and
//! expectations below are project-owned fixtures written for this profile;
//! they are not generated from the implementation or copied from upstream.
use haystack_app::{
    AllowAll, ApplicationBuilder, ApplicationOwner, CatalogKind, FunctionIdentity, PolicySnapshot,
    Principal, ReadContext, ReadError, ReadLimits, ReadOperation, ReadPolicy, ReadService,
};
use haystack_core::{
    codecs::{codec_for, jeto},
    data::{HDict, HGrid},
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, NominalScalar},
    xeto::catalog::{ActivatedCatalog, Catalog},
};
use haystack_server::HaystackServer;
use std::{sync::Arc, time::Duration};

const NAV: &str = "/ph.api::nav";

/// Hidden entities, a referenceable-but-not-relatable site, and a masked
/// relation tag. Displays on references are never disclosed.
struct NavRules;
impl ReadPolicy for NavRules {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(NavRules))
    }
}
impl PolicySnapshot for NavRules {
    fn scope_key(&self) -> &str {
        "nav-fixture-v1"
    }
    fn function(&self, _: &FunctionIdentity) -> bool {
        true
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, id: &str) -> bool {
        !matches!(id, "s-hidden" | "e-hidden")
    }
    fn tag(&self, entity: &str, tag: &str) -> bool {
        tag != "secretRef" && !(entity == "s-site-tag-masked" && tag == "site")
    }
    fn reference(&self, id: &str) -> bool {
        self.entity(id) && id != "s-unrelatable"
    }
    fn reference_display(&self, _: &str) -> bool {
        false
    }
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        true
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}

fn record(id: &str, dis: &str, tags: &[(&str, Kind)]) -> HDict {
    let mut row = HDict::new();
    row.set(
        "id",
        Kind::Ref(HRef::new(id, Some(format!("{dis} display")))),
    );
    row.set("dis", Kind::Str(dis.into()));
    for (tag, value) in tags {
        row.set(*tag, value.clone());
    }
    row
}
fn to(id: &str) -> Kind {
    Kind::Ref(HRef::from_val(id))
}
/// Independently authored building hierarchy. Points are inserted out of
/// id order so result order cannot come from insertion order.
fn hierarchy() -> Vec<HDict> {
    let mut rows = vec![
        record("s-north", "North", &[("site", Kind::Marker)]),
        record("s-south", "South", &[("site", Kind::Marker)]),
        record("s-hidden", "Hidden", &[("site", Kind::Marker)]),
        record("s-unrelatable", "Unrelatable", &[("site", Kind::Marker)]),
        // Raw `site` tags that masking removes: the tag itself is denied, or
        // its value references a hidden entity. Neither is a visible site.
        record("s-site-tag-masked", "Masked tag", &[("site", Kind::Marker)]),
        record(
            "s-site-value-masked",
            "Masked value",
            &[("site", to("s-hidden"))],
        ),
        record(
            "e-ahu2",
            "AHU-2",
            &[("equip", Kind::Marker), ("siteRef", to("s-north"))],
        ),
        record(
            "e-ahu1",
            "AHU-1",
            &[("equip", Kind::Marker), ("siteRef", to("s-north"))],
        ),
        // Children that must not be disclosed under s-north.
        record(
            "e-hidden",
            "Hidden AHU",
            &[("equip", Kind::Marker), ("siteRef", to("s-north"))],
        ),
        record(
            "e-masked",
            "Masked",
            &[("equip", Kind::Marker), ("secretRef", to("s-north"))],
        ),
        // Only a nested reference: not a navigation relation.
        record(
            "e-nested",
            "Nested",
            &[("refs", Kind::List(vec![to("s-north")]))],
        ),
        // Under a referenceable-denied site and a missing target.
        record(
            "e-under-unrelatable",
            "Under",
            &[("siteRef", to("s-unrelatable"))],
        ),
        record("e-dangling", "Dangling", &[("siteRef", to("s-gone"))]),
        // A two-node cycle and a self reference.
        record("c-a", "Cycle A", &[("peerRef", to("c-b"))]),
        record(
            "c-b",
            "Cycle B",
            &[("peerRef", to("c-a")), ("altRef", to("c-a"))],
        ),
        record("c-self", "Self", &[("selfRef", to("c-self"))]),
    ];
    for n in [4, 7, 1, 6, 3, 5, 2] {
        rows.push(record(
            &format!("p-0{n}"),
            &format!("Point {n}"),
            &[("point", Kind::Marker), ("equipRef", to("e-ahu1"))],
        ));
    }
    rows
}

struct Fixture {
    base: String,
    owner: ApplicationOwner,
    service: ReadService,
    client: reqwest::Client,
}
impl Fixture {
    async fn start(limits: ReadLimits) -> Self {
        Self::with_policy(limits, Arc::new(NavRules)).await
    }
    async fn with_policy(limits: ReadLimits, policy: Arc<dyn ReadPolicy>) -> Self {
        Self::with_rows(limits, policy, hierarchy()).await
    }
    async fn with_rows(limits: ReadLimits, policy: Arc<dyn ReadPolicy>, rows: Vec<HDict>) -> Self {
        let graph = SharedGraph::new(EntityGraph::new());
        for row in rows {
            graph.add(row).unwrap();
        }
        let app = ApplicationBuilder::new(graph.clone(), policy, limits).unwrap();
        let service = app.handle().read_service();
        let server = HaystackServer::new(graph)
            .with_scoped_reads(app.handle())
            .port(0);
        let owner = app
            .owned_resource(server.into_listener())
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let address = owner.ready().await.unwrap().listeners[0].address;
        Self {
            base: format!("http://{address}/api"),
            owner,
            service,
            client: haystack_client::ClientConfig::default()
                .build_reqwest_client()
                .unwrap(),
        }
    }
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.post(format!("{}{path}", self.base))
    }
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.get(format!("{}{path}", self.base))
    }
    /// Typed v5 call with the whole request grid as a Zinc body.
    async fn typed(&self, target: Option<&str>) -> (u16, String) {
        let response = self
            .post(NAV)
            .header("Xeto-Version", "5")
            .header("Content-Type", "text/zinc")
            .body(zinc_request(target))
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.text().await.unwrap())
    }
    /// The scoped H4 route, one page of at most `limit` rows.
    async fn h4(&self, target: Option<&str>, limit: usize, cursor: Option<&str>) -> HGrid {
        let mut meta = format!("ver:\"3.0\" limit:{limit}");
        if let Some(cursor) = cursor {
            meta.push_str(&format!(" cursor:\"{cursor}\""));
        }
        let body = match target {
            Some(id) => format!("{meta}\nnavId\n\"{id}\"\n"),
            None => format!("{meta}\nempty\n"),
        };
        let response = self
            .post("/nav")
            .header("Content-Type", "text/zinc")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        zinc(&response.text().await.unwrap())
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
        assert_eq!(self.service.load().admitted, 0);
    }
}
fn zinc_request(target: Option<&str>) -> String {
    match target {
        Some(id) => format!("ver:\"3.0\"\nnavId\n\"{id}\"\n"),
        None => "ver:\"3.0\"\nnavId\nN\n".into(),
    }
}
fn zinc(text: &str) -> HGrid {
    codec_for("text/zinc").unwrap().decode_grid(text).unwrap()
}
/// Decode a v5 Jeto response with the nav function's own result context.
fn typed_grid(body: &str) -> HGrid {
    let observation = ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap();
    let context = &observation.callable("ph.api::nav").unwrap().context;
    match jeto::decode(
        body.as_bytes(),
        context,
        Some("sys::Grid"),
        jeto::Limits::default(),
    )
    .unwrap()
    {
        Kind::Grid(grid) => *grid,
        other => panic!("nav returns a Grid: {other:?}"),
    }
}
fn nav_ids(grid: &HGrid) -> Vec<String> {
    grid.rows
        .iter()
        .map(|row| match row.get("navId") {
            Some(Kind::Str(id)) => id.clone(),
            other => panic!("navId: {other:?}"),
        })
        .collect()
}
fn col_names(grid: &HGrid) -> Vec<&str> {
    grid.cols.iter().map(|col| col.name.as_str()).collect()
}

#[tokio::test]
async fn hierarchy_hides_masked_nodes_and_relations_in_id_order() {
    let f = Fixture::start(ReadLimits::default()).await;
    let (status, body) = f.typed(None).await;
    assert_eq!(status, 200, "{body}");
    let root = typed_grid(&body);
    // Policy-filtered sites only; the hidden site is absent.
    assert_eq!(nav_ids(&root), ["s-north", "s-south", "s-unrelatable"]);
    assert_eq!(col_names(&root), ["dis", "id", "navId"]);
    assert_eq!(root.meta.get("complete"), Some(&Kind::Bool(true)));
    let first = &root.rows[0];
    assert_eq!(first.get("id"), Some(&to("s-north")));
    assert_eq!(first.get("dis"), Some(&Kind::Str("North".into())));
    // Reference display is policy-gated and denied here.
    assert!(matches!(first.get("id"), Some(Kind::Ref(r)) if r.dis.is_none()));
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["spec"], "sys::Grid");
    assert_eq!(value["meta"], serde_json::json!({"complete": true}));

    // One inverse hop: hidden child, masked relation tag and a nested-only
    // reference are not children; insertion order does not leak.
    let (_, body) = f.typed(Some("s-north")).await;
    assert_eq!(nav_ids(&typed_grid(&body)), ["e-ahu1", "e-ahu2"]);
    let (_, body) = f.typed(Some("e-ahu1")).await;
    let points = typed_grid(&body);
    assert_eq!(
        nav_ids(&points),
        ["p-01", "p-02", "p-03", "p-04", "p-05", "p-06", "p-07"]
    );
    assert_eq!(points.meta.get("complete"), Some(&Kind::Bool(true)));
    f.close().await;
}

#[tokio::test]
async fn hidden_missing_unrelatable_and_unsupported_targets_are_one_empty_complete_page() {
    let f = Fixture::start(ReadLimits::default()).await;
    let mut bodies = Vec::new();
    // Hidden, missing, reference-denied, a path-like unsupported navId and
    // an existing childless leaf are indistinguishable: no existence oracle.
    for target in [
        "s-hidden",
        "s-gone",
        "s-unrelatable",
        "site/floors/1",
        "p-01",
        "e-hidden",
    ] {
        let (status, body) = f.typed(Some(target)).await;
        assert_eq!(status, 200, "{target}: {body}");
        bodies.push(body);
    }
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "{bodies:?}"
    );
    let empty = typed_grid(&bodies[0]);
    assert!(empty.rows.is_empty());
    assert_eq!(col_names(&empty), ["navId"]);
    assert_eq!(empty.meta.get("complete"), Some(&Kind::Bool(true)));
    // The version 4 shape of the same outcome is an ordinary empty grid,
    // never an error grid.
    let response = f
        .post(NAV)
        .header("Content-Type", "text/zinc")
        .body(zinc_request(Some("s-hidden")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["xeto-version"], "4");
    let legacy = zinc(&response.text().await.unwrap());
    assert!(legacy.meta.missing("err"));
    assert!(legacy.rows.is_empty());
    assert_eq!(legacy.meta.get("complete"), Some(&Kind::Bool(true)));
    f.close().await;
}

#[tokio::test]
async fn reference_cycles_are_single_hop_and_terminate() {
    let f = Fixture::start(ReadLimits::default()).await;
    for (target, expected) in [
        ("c-a", vec!["c-b"]),
        ("c-b", vec!["c-a"]),
        ("c-self", vec!["c-self"]),
    ] {
        let (status, body) = f.typed(Some(target)).await;
        assert_eq!(status, 200);
        // c-b relates to c-a through two tags and still appears once.
        assert_eq!(nav_ids(&typed_grid(&body)), expected, "{target}");
    }
    f.close().await;
}

#[tokio::test]
async fn oversized_levels_truncate_deterministically_and_match_the_first_h4_page() {
    let f = Fixture::start(ReadLimits {
        max_rows: 5,
        ..ReadLimits::default()
    })
    .await;
    let (_, first) = f.typed(Some("e-ahu1")).await;
    let (_, again) = f.typed(Some("e-ahu1")).await;
    assert_eq!(first, again);
    let typed = typed_grid(&first);
    assert_eq!(nav_ids(&typed), ["p-01", "p-02", "p-03", "p-04", "p-05"]);
    assert_eq!(typed.meta.get("complete"), Some(&Kind::Bool(false)));
    assert!(typed.meta.missing("cursor"));
    // Scoped H4 with the same bound returns the same first page and then a
    // continuation; together they cover the whole level in id order.
    let page = f.h4(Some("e-ahu1"), 5, None).await;
    assert_eq!(page.rows, typed.rows);
    assert_eq!(page.meta.get("complete"), Some(&Kind::Bool(false)));
    let Some(Kind::Str(cursor)) = page.meta.get("cursor") else {
        panic!("H4 continuation")
    };
    let rest = f.h4(Some("e-ahu1"), 5, Some(cursor)).await;
    assert_eq!(nav_ids(&rest), ["p-06", "p-07"]);
    assert_eq!(rest.meta.get("complete"), Some(&Kind::Bool(true)));
    f.close().await;
}

#[tokio::test]
async fn typed_rows_columns_and_order_match_scoped_h4_navigation() {
    let f = Fixture::start(ReadLimits::default()).await;
    for target in [
        None,
        Some("s-north"),
        Some("e-ahu1"),
        Some("c-b"),
        Some("s-hidden"),
        Some("s-gone"),
        Some("s-unrelatable"),
    ] {
        let (_, body) = f.typed(target).await;
        let typed = typed_grid(&body);
        let h4 = f.h4(target, 100, None).await;
        assert_eq!(typed.rows, h4.rows, "{target:?}");
        assert_eq!(typed.meta.get("complete"), h4.meta.get("complete"));
        if !h4.rows.is_empty() {
            assert_eq!(col_names(&typed), col_names(&h4), "{target:?}");
        }
        // The typed version 4 bridge serves the same H4 rows and page marker.
        let response = f
            .post(NAV)
            .header("Content-Type", "text/zinc")
            .body(zinc_request(target))
            .send()
            .await
            .unwrap();
        let legacy = zinc(&response.text().await.unwrap());
        assert_eq!(legacy.rows, h4.rows, "{target:?}");
        assert_eq!(legacy.meta.get("complete"), h4.meta.get("complete"));
    }
    f.close().await;
}

#[tokio::test]
async fn whole_body_and_named_requests_fit_the_declared_grid_parameter() {
    let f = Fixture::start(ReadLimits::default()).await;
    let (_, expected) = f.typed(Some("s-north")).await;
    let named = |value: &str| {
        format!(
            r#"{{"req":{{"spec":"sys::Grid","cols":[{{"name":"navId"}}],"rows":[{{"navId":{value}}}]}}}}"#
        )
    };
    let hayson = r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[{"name":"navId"}],"rows":[{"navId":"s-north"}]}"#;
    for (media, body) in [
        // Grid formats post the request grid as the whole body.
        ("application/vnd.haystack+json", hayson.to_owned()),
        ("text/zinc", "ver:\"3.0\"\nnavId\n@s-north\n".to_owned()),
        // Named Jeto passes it as the req member, Str or boxed Ref navId.
        ("application/json", named(r#""s-north""#)),
        ("text/jeto", named(r#"{"spec":"sys::Ref","val":"s-north"}"#)),
    ] {
        let response = f
            .post(NAV)
            .header("Xeto-Version", "5")
            .header("Content-Type", media)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{media}");
        assert_eq!(response.headers()["content-type"], "application/json");
        assert_eq!(response.text().await.unwrap(), expected, "{media}");
    }
    // GET passes the named grid as JSON query text (noSideEffects).
    let query = "%7B%22cols%22%3A%5B%7B%22name%22%3A%22navId%22%7D%5D%2C%22rows%22%3A%5B%7B%22navId%22%3A%22s-north%22%7D%5D%7D";
    let response = f
        .get(&format!("{NAV}?req={query}"))
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), expected);
    // Empty grids in either convention select the root.
    let (_, root) = f.typed(None).await;
    for (media, body) in [
        ("text/zinc", "ver:\"3.0\"\nempty\n"),
        ("application/json", r#"{"req":{"cols":[],"rows":[]}}"#),
        (
            "application/json",
            r#"{"req":{"cols":[{"name":"navId"}],"rows":[{}]}}"#,
        ),
        (
            "application/json",
            r#"{"req":{"cols":[{"name":"navId"}],"rows":[{"navId":null}]}}"#,
        ),
    ] {
        let response = f
            .post(NAV)
            .header("Xeto-Version", "5")
            .header("Content-Type", media)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{body}");
        assert_eq!(response.text().await.unwrap(), root, "{body}");
    }
    f.close().await;
}

#[tokio::test]
async fn malformed_requests_and_lossy_output_are_explicit_errors() {
    let f = Fixture::start(ReadLimits::default()).await;
    let invalid = [
        // The required req argument is absent.
        ("application/json", "{}".to_owned()),
        ("text/zinc", String::new()),
        // A Jeto whole-grid body is not a named request.
        (
            "application/json",
            r#"{"spec":"sys::Grid","cols":[{"name":"navId"}],"rows":[]}"#.to_owned(),
        ),
        // navId is a request-grid column, never a named argument.
        ("application/json", r#"{"navId":"s-north"}"#.to_owned()),
        (
            "application/json",
            r#"{"req":{"cols":[],"rows":[]},"navId":"s-north"}"#.to_owned(),
        ),
        ("application/json", r#"{"req":"s-north"}"#.to_owned()),
        // Request-grid shape: kinds, extra columns, rows and H4 page controls.
        ("text/zinc", "ver:\"3.0\"\nnavId\n42\n".to_owned()),
        (
            "text/zinc",
            "ver:\"3.0\"\nnavId,limit\n\"s-north\",1\n".to_owned(),
        ),
        (
            "text/zinc",
            "ver:\"3.0\"\nnavId\n\"s-north\"\n\"s-south\"\n".to_owned(),
        ),
        (
            "text/zinc",
            "ver:\"3.0\" limit:1\nnavId\n\"s-north\"\n".to_owned(),
        ),
        (
            "text/zinc",
            "ver:\"3.0\" cursor:\"x\"\nnavId\nN\n".to_owned(),
        ),
    ];
    for (media, body) in invalid {
        let response = f
            .post(NAV)
            .header("Xeto-Version", "5")
            .header("Content-Type", media)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{media}: {body}");
        let value: serde_json::Value =
            serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(value["spec"], "sys.api::InvalidArgsErr", "{body}");
    }
    // GET without the required grid is the same missing-argument error.
    let response = f.get(NAV).header("Xeto-Version", "5").send().await.unwrap();
    assert_eq!(response.status(), 400);
    // box=none would erase the Ref identity of the id column: refused, not lossy.
    let response = f
        .post(NAV)
        .header("Xeto-Version", "5")
        .header("Content-Type", "text/zinc")
        .header("Accept", "application/json;box=none")
        .body(zinc_request(None))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 406);
    let value: serde_json::Value =
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(value["spec"], "sys.api::NotAcceptableErr");
    // An empty level has no Ref to erase, so the same mode stays exact.
    let response = f
        .post(NAV)
        .header("Xeto-Version", "5")
        .header("Content-Type", "text/zinc")
        .header("Accept", "application/json;box=none")
        .body(zinc_request(Some("s-gone")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    f.close().await;
}

#[tokio::test]
async fn typed_nav_is_served_at_its_qname_while_h4_nav_keeps_its_route() {
    let f = Fixture::start(ReadLimits::default()).await;
    // Explicit version 5 on the shared simple name still reaches scoped H4.
    for version in ["4", "5"] {
        let response = f
            .post("/nav")
            .header("Xeto-Version", version)
            .header("Content-Type", "text/zinc")
            .body(zinc_request(Some("s-north")))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/zinc");
        assert_eq!(response.headers()["xeto-version"], "4");
        let grid = zinc(&response.text().await.unwrap());
        assert_eq!(nav_ids(&grid), ["e-ahu1", "e-ahu2"]);
    }
    // The H4 route stays POST-only; the typed qname route admits GET.
    assert_eq!(f.get("/nav").send().await.unwrap().status(), 405);
    let (status, _) = f.typed(Some("s-north")).await;
    assert_eq!(status, 200);
    // Discovery: typed ops lists the qualified binding; H4 ops keeps one nav.
    let ops = f
        .get("/sys.api::ops")
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap();
    let ops: serde_json::Value = serde_json::from_slice(&ops.bytes().await.unwrap()).unwrap();
    let nav = ops["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["qname"] == "ph.api::nav")
        .expect("typed discovery");
    assert_eq!(nav["signature"], "(req: Grid) -> Grid");
    assert!(!nav["noSideEffects"].is_null());
    let legacy = zinc(&f.get("/ops").send().await.unwrap().text().await.unwrap());
    assert_eq!(
        legacy
            .rows
            .iter()
            .filter(|row| row.get("name") == Some(&Kind::Str("nav".into())))
            .count(),
        1
    );
    f.close().await;
}

#[tokio::test]
async fn catalog_activation_invalidates_h4_cursors_and_rebinds_typed_nav() {
    let f = Fixture::with_policy(
        ReadLimits {
            max_rows: 5,
            ..ReadLimits::default()
        },
        Arc::new(AllowAll),
    )
    .await;
    let page = f.h4(Some("e-ahu1"), 2, None).await;
    let Some(Kind::Str(cursor)) = page.meta.get("cursor") else {
        panic!("H4 continuation")
    };
    let cursor = cursor.clone();
    let generation = f.service.graph().state().catalog_generation;
    let published = f
        .service
        .activate_catalog(
            ReadContext::with_timeout(
                Principal::TrustedEmbedding {
                    subject: "owner".into(),
                },
                Duration::from_secs(5),
            ),
            Catalog::load_protocol_pinned().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(published.catalog_generation, generation + 1);
    // The shared read contract: a generation change makes the cursor stale.
    let response = f
        .post("/nav")
        .header("Content-Type", "text/zinc")
        .body(format!(
            "ver:\"3.0\" limit:2 cursor:\"{cursor}\"\nnavId\n\"e-ahu1\"\n"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    // A fresh typed request binds the new observation; nothing is stale.
    let (status, body) = f.typed(Some("e-ahu1")).await;
    assert_eq!(status, 200);
    assert_eq!(
        nav_ids(&typed_grid(&body)),
        ["p-01", "p-02", "p-03", "p-04", "p-05"]
    );
    f.close().await;
}

/// A mid-size point list under default limits. Each point carries a
/// realistic handful of tags, so masking every record would exceed the
/// retained budget long before the candidate bound.
fn point_list() -> Vec<HDict> {
    let mut rows = vec![
        record("site-a", "Site A", &[("site", Kind::Marker)]),
        record("site-b", "Site B", &[("site", Kind::Marker)]),
        record(
            "equip-1",
            "Equip 1",
            &[("equip", Kind::Marker), ("siteRef", to("site-b"))],
        ),
    ];
    for n in 0..4100 {
        let mut tags = vec![
            ("point", Kind::Marker),
            ("his", Kind::Marker),
            ("kind", Kind::Str("Number".into())),
            ("unit", Kind::Str("kW".into())),
            ("navName", Kind::Str(format!("point{n}"))),
            ("siteRef", to("site-a")),
        ];
        if n < 3000 {
            tags.push(("equipRef", to("equip-1")));
        }
        rows.push(record(&format!("pt-{n:04}"), &format!("Point {n}"), &tags));
    }
    rows
}

#[tokio::test]
async fn root_masks_only_possible_sites_and_capacity_failures_are_atomic() {
    let f = Fixture::with_rows(ReadLimits::default(), Arc::new(AllowAll), point_list()).await;
    // 4,100 non-site records stay within the candidate bound and are never
    // copied through the masked View, so the root is not a budget failure.
    let (status, body) = f.typed(None).await;
    assert_eq!(status, 200, "{body}");
    let root = typed_grid(&body);
    assert_eq!(nav_ids(&root), ["site-a", "site-b"]);
    assert_eq!(root.meta.get("complete"), Some(&Kind::Bool(true)));
    // 3,000 related children within the inbound-edge bound truncate to the
    // first max_rows (1,000) in id order rather than failing; the version 4
    // H4 encoding fits the default budgets.
    let response = f
        .post(NAV)
        .header("Content-Type", "text/zinc")
        .body(zinc_request(Some("equip-1")))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let level = zinc(&response.text().await.unwrap());
    assert_eq!(level.rows.len(), 1000);
    assert_eq!(nav_ids(&level)[0], "pt-0000");
    assert_eq!(nav_ids(&level)[999], "pt-0999");
    assert_eq!(level.meta.get("complete"), Some(&Kind::Bool(false)));
    // 4,100 inbound edges exceed max_inverse_edges (4,096) even though only
    // 1,000 rows would return: one atomic InvalidArgsErr, never partial rows.
    for (version, media) in [("5", "text/zinc"), ("4", "text/zinc")] {
        let response = f
            .post(NAV)
            .header("Xeto-Version", version)
            .header("Content-Type", media)
            .body(zinc_request(Some("site-a")))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "v{version}");
        assert_eq!(response.headers()["content-type"], "application/json");
        let value: serde_json::Value =
            serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "spec": "sys.api::InvalidArgsErr",
                "status": 400,
                "dis": "Invalid request arguments"
            })
        );
    }
    f.close().await;
    // Exact Jeto output is costlier per row (see docs/typed-http-read.md);
    // with a smaller page the same level truncates on the v5 path too.
    let f = Fixture::with_rows(
        ReadLimits {
            max_rows: 500,
            ..ReadLimits::default()
        },
        Arc::new(AllowAll),
        point_list(),
    )
    .await;
    let (status, body) = f.typed(Some("equip-1")).await;
    assert_eq!(status, 200);
    let level = typed_grid(&body);
    assert_eq!(level.rows.len(), 500);
    assert_eq!(nav_ids(&level)[499], "pt-0499");
    assert_eq!(level.meta.get("complete"), Some(&Kind::Bool(false)));
    f.close().await;
    // A retained budget smaller than the masked site rows fails the same way.
    let f = Fixture::with_rows(
        ReadLimits {
            max_retained_bytes: 96 * 1024,
            ..ReadLimits::default()
        },
        Arc::new(AllowAll),
        (0..64)
            .map(|n| record(&format!("s-{n:02}"), "Site", &[("site", Kind::Marker)]))
            .collect(),
    )
    .await;
    let response = f
        .post(NAV)
        .header("Xeto-Version", "5")
        .header("Content-Type", "text/zinc")
        .body(zinc_request(None))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let value: serde_json::Value =
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(value["spec"], "sys.api::InvalidArgsErr");
    assert!(value.get("rows").is_none());
    f.close().await;
}
