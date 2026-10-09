//! Wire-level fixtures for the deliberately bounded readById HTTP profile.
use haystack_app::{AllowAll, ApplicationBuilder, ReadLimits};
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind},
};
use haystack_server::HaystackServer;
use std::sync::Arc;

#[tokio::test]
async fn explicit_v5_reads_a_record_through_the_managed_service() {
    let graph = SharedGraph::new(EntityGraph::new());
    let mut record = HDict::new();
    record.set("id", Kind::Ref(HRef::from_val("a")));
    record.set("dis", Kind::Str("Alpha".into()));
    graph.add(record).unwrap();
    let application =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let listener = HaystackServer::new(graph)
        .with_scoped_reads(application.handle())
        .port(0)
        .into_listener();
    let owner = application
        .owned_resource(listener)
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    let address = owner.ready().await.unwrap().listeners[0].address;
    let response = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap()
        .post(format!("http://{address}/api/readById"))
        .header("Xeto-Version", "5")
        .header("Content-Type", "application/json")
        .body(r#"{"id":"a"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["xeto-version"], "5");
    assert_eq!(response.headers()["content-type"], "application/json");
    let value: serde_json::Value =
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(
        value["id"],
        serde_json::json!({"spec":"sys::Ref", "val":"a"})
    );
    assert_eq!(value["dis"], "Alpha");
    owner.close().await.unwrap();
    owner.terminated().await;
}

use haystack_app::{
    ApplicationOwner, PolicySnapshot, Principal, ReadError, ReadOperation, ReadPolicy, ReadService,
};
use haystack_core::{
    codecs::codec_for,
    kinds::{Float, NominalScalar, Number},
};
use haystack_server::auth::{
    AuthManager, AuthUser,
    users::{UserRecord, parse_password_hash},
};
use std::{collections::HashMap, time::Duration};

struct Rules;
impl ReadPolicy for Rules {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(Rules))
    }
}
impl PolicySnapshot for Rules {
    fn function(&self, _: &haystack_app::FunctionIdentity) -> bool {
        true
    }
    fn scope_key(&self) -> &str {
        "typed-fixture"
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, id: &str) -> bool {
        id != "denied"
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        tag != "secret"
    }
    fn reference(&self, id: &str) -> bool {
        id != "denied"
    }
    fn reference_display(&self, _: &str) -> bool {
        false
    }
    fn catalog(&self, _: haystack_app::CatalogKind, _: &str) -> bool {
        true
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}
fn auth() -> AuthManager {
    let users=HashMap::from([("user".into(),UserRecord { credentials: parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap(),permissions:vec!["read".into()] })]);
    let auth = AuthManager::new(users, Duration::from_secs(60));
    auth.inject_token(
        "fixture-token".into(),
        AuthUser {
            username: "user".into(),
            permissions: vec!["read".into()],
        },
    );
    auth
}
struct Fixture {
    base: String,
    owner: ApplicationOwner,
    service: ReadService,
    graph: SharedGraph,
    client: reqwest::Client,
}
impl Fixture {
    async fn start(authenticated: bool, limits: ReadLimits) -> Self {
        Self::with_policy(authenticated, limits, Arc::new(Rules)).await
    }
    async fn with_policy(
        authenticated: bool,
        limits: ReadLimits,
        policy: Arc<dyn ReadPolicy>,
    ) -> Self {
        let graph = SharedGraph::new(EntityGraph::new());
        for id in ["a", "denied", "x"] {
            let mut record = HDict::new();
            record.set(
                "id",
                Kind::Ref(HRef::new(id, Some("Private display".into()))),
            );
            record.set("dis", Kind::Str("Alpha".into()));
            record.set("site", Kind::Marker);
            record.set("secret", Kind::Str("masked".into()));
            record.set(
                "hidden",
                Kind::List(vec![Kind::Ref(HRef::from_val("denied"))]),
            );
            graph.add(record).unwrap();
        }
        let app = ApplicationBuilder::new(graph.clone(), policy, limits).unwrap();
        let service = app.handle().read_service();
        let mut server = HaystackServer::new(graph.clone())
            .with_scoped_reads(app.handle())
            .port(0);
        if authenticated {
            let auth = auth();
            auth.inject_token(
                "ops-fixture-token".into(),
                AuthUser {
                    username: "ops-only".into(),
                    permissions: vec!["read".into()],
                },
            );
            server = server.with_auth(auth);
        }
        let owner = app
            .owned_resource(server.into_listener())
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let addr = owner.ready().await.unwrap().listeners[0].address;
        Self {
            base: format!("http://{addr}/api"),
            owner,
            service,
            graph,
            client: haystack_client::ClientConfig::default()
                .build_reqwest_client()
                .unwrap(),
        }
    }
    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.get(format!("{}{path}", self.base))
    }
    fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.post(format!("{}{path}", self.base))
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
        assert_eq!(self.service.load().admitted, 0);
    }
}
async fn json_response(response: reqwest::Response, status: u16) -> serde_json::Value {
    assert_eq!(response.status(), status);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers()["xeto-version"], "5");
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}
async fn api_error(request: reqwest::RequestBuilder, status: u16, spec: &str) {
    let value = json_response(request.send().await.unwrap(), status).await;
    assert_eq!(value["spec"], format!("sys.api::{spec}"));
    assert_eq!(value["status"], status);
    assert!(value.get("errTrace").is_none());
}
#[tokio::test]
async fn raw_versions_precedence_duplicates_and_qualified_operation() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for version in [None, Some("4")] {
        let mut req = f.get("/readById?id=a");
        if let Some(v) = version {
            req = req.header("Xeto-Version", v);
        }
        let response = req.send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["xeto-version"], "4");
        assert_eq!(response.headers()["content-type"], "text/zinc");
        let grid = codec_for("text/zinc")
            .unwrap()
            .decode_grid(&response.text().await.unwrap())
            .unwrap();
        assert_eq!(grid.rows[0].id().unwrap().val, "a");
    }
    let value = json_response(
        f.get("/sys.api::readById?id=a&xeto-version=5&xeto-future=ignored")
            .header("Xeto-Version", "99")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(value["id"]["val"], "a");
    for req in [
        f.get("/readById?id=a&xeto-version=5&xeto-version=5"),
        f.get("/readById?id=a")
            .header("Xeto-Version", "5")
            .header("Xeto-Version", "5"),
        f.get("/readById?id=a").header("Xeto-Version", "6"),
        f.get("/readById?id=a&xeto-version="),
        f.get("/readById?id=a&xeto-version=5")
            .header("Xeto-Version", "4")
            .header("Xeto-Version", "4"),
    ] {
        api_error(req, 400, "UnsupportedVersionErr").await;
    }
    f.close().await;
}
#[tokio::test]
async fn raw_get_post_context_null_defaults_and_first_grid_row() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    let grid_null = f
        .post("/readById?xeto-version=5")
        .header("Content-Type", "text/zinc")
        .body("ver:\"3.0\"\nid,checked\n@a,N\n")
        .send()
        .await
        .unwrap();
    assert_eq!(json_response(grid_null, 200).await["id"]["val"], "a");
    for req in [
        f.get("/readById?xeto-version=5"),
        f.post("/readById?xeto-version=5")
            .header("Content-Type", "application/json")
            .body("{}"),
        f.post("/readById?xeto-version=5")
            .header("Content-Type", "application/json")
            .body(r#"{"id":null,"checked":null}"#),
        f.post("/readById?xeto-version=5")
            .header("Content-Type", "application/json")
            .body(""),
    ] {
        api_error(req, 404, "UnknownEntityErr").await;
    }
    for req in [f.get("/readById?xeto-version=5&id=a&checked=true"),f.get("/readById?xeto-version=5&id=%7B%22spec%22%3A%22sys%3A%3ARef%22%2C%22val%22%3A%22a%22%7D"),f.post("/readById?xeto-version=5").header("Content-Type","application/json").body(r#"{"id":"a","checked":null,"cursor":"unauthorized","select":["secret"]}"#),f.post("/readById?xeto-version=5").header("Content-Type","text/zinc").body("ver:\"3.0\" select:\"secret\" cursor:\"anything\"\nid,checked,select\n@a,T,\"secret\"\n@denied,T,\"secret\"\n")] {
        let value=json_response(req.send().await.unwrap(),200).await;
        assert_eq!(value["id"]["val"],"a");
        assert!(value.get("secret").is_none()); assert!(value.get("hidden").is_none());
        assert_eq!(value["id"].get("dis"),None); assert_eq!(value["dis"],"Alpha");
        assert!(value.get("complete").is_none()); assert!(value.get("cursor").is_none());
    }
    for body in [
        r#"{"id":5}"#,
        r#"{"id":true}"#,
        r#"{"id":"a","checked":1}"#,
        r#"{"id":"a","checked":"bad"}"#,
        r#"{"id":{"spec":"sys::Str","val":"a"}}"#,
        "{",
        "[]",
    ] {
        api_error(
            f.post("/readById?xeto-version=5")
                .header("Content-Type", "application/json")
                .body(body),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    api_error(
        f.get("/readById?xeto-version=5&id="),
        404,
        "UnknownEntityErr",
    )
    .await;
    for suffix in ["&checked=", "&id=[1]", "&id=%22a%22", "&id=%", "&id=%FF"] {
        api_error(
            f.get(&format!("/readById?xeto-version=5{suffix}")),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    f.close().await;
}
#[tokio::test]
async fn raw_missing_denied_are_indistinguishable_and_h4_errors_keep_status() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    let mut errors = vec![];
    for id in ["missing", "denied"] {
        errors.push(
            json_response(
                f.get(&format!("/readById?id={id}&xeto-version=5"))
                    .header("Accept", "text/zinc")
                    .send()
                    .await
                    .unwrap(),
                404,
            )
            .await,
        );
        assert_eq!(
            json_response(
                f.get(&format!("/readById?id={id}&checked=false&xeto-version=5"))
                    .send()
                    .await
                    .unwrap(),
                200
            )
            .await,
            serde_json::Value::Null
        );
        let response = f.get(&format!("/readById?id={id}")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["xeto-version"], "5");
        let grid = codec_for("text/zinc")
            .unwrap()
            .decode_grid(&response.text().await.unwrap())
            .unwrap();
        assert!(grid.meta.has("err"));
    }
    assert_eq!(errors[0], errors[1]);
    f.close().await;
}
#[tokio::test]
async fn raw_media_default_json_hayson_errors_and_reserved_filetype() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    let hayson = r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[{"name":"id"}],"rows":[{"id":{"_kind":"ref","val":"a"}}]}"#;
    for (version, ct, body) in [
        ("4", "application/json", hayson),
        ("5", "application/vnd.haystack+json;version=4", hayson),
        ("5", "application/json", r#"{"id":"a"}"#),
        ("5", "text/jeto", r#"{"id":"a"}"#),
    ] {
        let response = f
            .post("/readById")
            .header("Xeto-Version", version)
            .header("Content-Type", ct)
            .header("Accept", "application/vnd.haystack+json;version=4")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "application/json");
        let grid = codec_for("application/json")
            .unwrap()
            .decode_grid(&response.text().await.unwrap())
            .unwrap();
        assert_eq!(grid.rows[0].id().unwrap().val, "a");
    }
    let response = f
        .get("/readById?id=a&xeto-version=5&xeto-filetype=hayson")
        .header("Accept", "text/csv")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json");
    for ct in [
        None,
        Some("text/csv"),
        Some("application/vnd.haystack+json;version=5"),
    ] {
        let mut request = f
            .post("/readById?xeto-version=5")
            .header("Accept", "text/zinc")
            .body("{}");
        if let Some(ct) = ct {
            request = request.header("Content-Type", ct);
        }
        api_error(request, 415, "UnsupportedMediaTypeErr").await;
    }
    api_error(
        f.post("/readById?xeto-version=4")
            .header("Content-Type", "application/json")
            .body(r#"{"id":"a"}"#),
        400,
        "InvalidArgsErr",
    )
    .await;
    for accept in [
        "text/csv",
        "application/json;box=none",
        "application/json;box=unknown",
        "application/json;q=0",
        "application/vnd.haystack+json;version=5",
    ] {
        api_error(
            f.get("/readById?id=a&xeto-version=5")
                .header("Accept", accept),
            406,
            "NotAcceptableErr",
        )
        .await;
    }
    api_error(
        f.get("/readById?id=missing&xeto-version=5")
            .header("Accept", "text/zinc"),
        404,
        "UnknownEntityErr",
    )
    .await;
    f.close().await;
}
#[tokio::test]
async fn raw_authentication_precedes_version_resolution_and_preserves_h4_status() {
    let f = Fixture::start(true, ReadLimits::default()).await;
    for version in ["4", "5"] {
        api_error(
            f.get("/readById?id=a")
                .header("Xeto-Version", version)
                .header("Authorization", "BEARER authToken=bad"),
            if version == "5" { 403 } else { 401 },
            "AuthErr",
        )
        .await;
        api_error(
            f.get("/readById?id=a")
                .header("Xeto-Version", version)
                .header("Authorization", "broken"),
            if version == "5" { 400 } else { 401 },
            "AuthErr",
        )
        .await;
    }
    for (query, header, status) in [("4", "5", 401), ("5", "4", 403), ("%35", "4", 403)] {
        api_error(
            f.get(&format!("/readById?id=a&xeto-version={query}"))
                .header("Xeto-Version", header)
                .header("Authorization", "BEARER authToken=bad"),
            status,
            "AuthErr",
        )
        .await;
    }
    api_error(f.get("/readById?id=a&xeto-version=99"), 401, "AuthErr").await;
    api_error(
        f.get("/readById?id=a&xeto-version=99")
            .header("Authorization", "BEARER authToken=fixture-token"),
        400,
        "UnsupportedVersionErr",
    )
    .await;
    assert_eq!(
        json_response(
            f.get("/readById?id=a&xeto-version=5")
                .header("Authorization", "BEARER authToken=fixture-token")
                .send()
                .await
                .unwrap(),
            200
        )
        .await["id"]["val"],
        "a"
    );
    let response = f
        .get("/read")
        .header("Authorization", "BEARER authToken=bad")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.headers()["xeto-version"], "5");
    let response = f
        .get("/about")
        .header("Authorization", "HELLO username=dXNlcg")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(response.headers().contains_key("www-authenticate"));
    assert!(response.bytes().await.unwrap().is_empty());
    f.close().await;
}
#[tokio::test]
async fn raw_unknown_routes_methods_and_head_are_protocol_errors() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    api_error(
        f.get("/missing?xeto-version=5")
            .header("Accept", "text/zinc"),
        404,
        "UnknownFuncErr",
    )
    .await;
    for method in [
        reqwest::Method::PUT,
        reqwest::Method::DELETE,
        reqwest::Method::OPTIONS,
        reqwest::Method::PATCH,
    ] {
        api_error(
            f.client
                .request(method, format!("{}/readById?xeto-version=5", f.base)),
            501,
            "NotImplementedErr",
        )
        .await;
    }
    let response = f
        .client
        .head(format!("{}/readById?xeto-version=5", f.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 501);
    assert_eq!(response.headers()["xeto-version"], "5");
    assert!(response.bytes().await.unwrap().is_empty());
    f.close().await;
}
#[tokio::test]
async fn rich_values_are_exactly_supported_or_explicitly_rejected() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("rich")));
    row.set("integer", Kind::Int(i64::MAX));
    row.set("float", Kind::Float(Float::new(-0.0)));
    row.set("number", Kind::Number(Number::unitless(-0.0)));
    row.set("bytes", Kind::Buf(vec![0, 255]));
    row.set("none", Kind::None);
    row.set("list", Kind::List(vec![Kind::Null, Kind::Marker]));
    f.graph.add(row).unwrap();
    let value = json_response(
        f.get("/readById?id=rich&xeto-version=5")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(value["integer"].as_i64(), Some(i64::MAX));
    assert_eq!(value["number"]["spec"], "sys::Number");
    assert_eq!(
        value["number"]["val"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            .to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(
        value["float"].as_f64().unwrap().to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(value["bytes"]["val"], "AP8");
    assert_eq!(value["none"]["spec"], "sys::None");
    assert!(value["list"][0].is_null());
    for (id, bad) in [
        ("null", Kind::Null),
        (
            "nominal",
            Kind::Nominal(NominalScalar::new("acme::Token", "acme", "1.0", "x").unwrap()),
        ),
        ("nan", Kind::Float(Float::from_bits(0x7ff8000000000001))),
    ] {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val(id)));
        row.set("value", bad);
        f.graph.add(row).unwrap();
        api_error(
            f.get(&format!("/readById?id={id}&xeto-version=5")),
            406,
            "NotAcceptableErr",
        )
        .await;
    }
    api_error(
        f.get("/readById?id=rich&xeto-version=5")
            .header("Accept", "text/zinc"),
        406,
        "NotAcceptableErr",
    )
    .await;
    f.close().await;
}

#[tokio::test]
async fn gzip_and_auto_box_media_remain_inside_the_initial_profile() {
    use std::io::Read;
    let f = Fixture::start(false, ReadLimits::default()).await;
    let response = f
        .get("/readById?id=a&xeto-version=5")
        .header("Accept", "application/json;box=auto")
        .header("Accept-Encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert_eq!(
        response.headers()["vary"],
        "Accept, Xeto-Version, Accept-Encoding"
    );
    let bytes = response.bytes().await.unwrap();
    let mut body = String::new();
    flate2::read::GzDecoder::new(bytes.as_ref())
        .read_to_string(&mut body)
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["id"]["val"], "a");
    let response = f
        .get("/readById?id=a&xeto-version=5")
        .header("Accept-Encoding", "gzip;q=0")
        .send()
        .await
        .unwrap();
    assert!(response.headers().get("content-encoding").is_none());
    f.close().await;
}

#[tokio::test]
async fn legacy_h4_read_and_typed_read_share_policy_and_value_results() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    let legacy = f
        .post("/read")
        .header("Content-Type", "text/zinc")
        .header("Accept", "application/json")
        .body("ver:\"3.0\"\nid\n@a\n")
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), 200);
    let legacy = codec_for("application/json")
        .unwrap()
        .decode_grid(&legacy.text().await.unwrap())
        .unwrap();
    let typed = f
        .get("/readById?id=a&xeto-version=5")
        .header("Accept", "application/vnd.haystack+json")
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), 200);
    let typed = codec_for("application/json")
        .unwrap()
        .decode_grid(&typed.text().await.unwrap())
        .unwrap();
    assert_eq!(legacy.rows, typed.rows);
    assert_eq!(typed.rows.len(), 1);
    assert!(!typed.rows[0].has("secret"));
    assert!(!typed.rows[0].has("hidden"));
    assert!(typed.rows[0].id().unwrap().dis.is_none());
    f.close().await;
}

#[tokio::test]
async fn repair_null_id_obeys_checked_defaults_in_both_protocols() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for version in ["4", "5"] {
        for checked in [None, Some("null"), Some("true"), Some("false")] {
            for null_id in [false, true] {
                let mut args = serde_json::Map::new();
                if null_id {
                    args.insert("id".into(), serde_json::Value::Null);
                }
                if let Some(value) = checked {
                    args.insert("checked".into(), serde_json::from_str(value).unwrap());
                }
                // Jeto input is only available for v5; Zinc independently covers both versions.
                let zinc = format!(
                    "ver:\"3.0\"\nid,checked\nN,{}\n",
                    match checked {
                        Some("false") => "F",
                        Some("true") => "T",
                        _ => "N",
                    }
                );
                let mut requests = vec![
                    f.post(&format!("/readById?xeto-version={version}"))
                        .header("Content-Type", "text/zinc")
                        .body(zinc),
                ];
                if version == "5" {
                    requests.push(
                        f.post("/readById?xeto-version=5")
                            .header("Content-Type", "application/json")
                            .body(serde_json::Value::Object(args).to_string()),
                    );
                }
                for req in requests {
                    let response = req.send().await.unwrap();
                    if version == "5" {
                        let value = json_response(
                            response,
                            if checked == Some("false") { 200 } else { 404 },
                        )
                        .await;
                        if checked == Some("false") {
                            assert!(value.is_null());
                        } else {
                            assert_eq!(value["spec"], "sys.api::UnknownEntityErr");
                        }
                    } else {
                        assert_eq!(response.status(), 200);
                        let grid = codec_for("text/zinc")
                            .unwrap()
                            .decode_grid(&response.text().await.unwrap())
                            .unwrap();
                        assert_eq!(grid.meta.has("err"), checked != Some("false"));
                        assert!(grid.rows.is_empty());
                    }
                }
            }
        }
        for checked in ["", "&checked=true", "&checked=false"] {
            let response = f
                .get(&format!("/readById?xeto-version={version}{checked}"))
                .send()
                .await
                .unwrap();
            if version == "5" {
                let value = json_response(
                    response,
                    if checked == "&checked=false" {
                        200
                    } else {
                        404
                    },
                )
                .await;
                if checked == "&checked=false" {
                    assert!(value.is_null());
                } else {
                    assert_eq!(value["spec"], "sys.api::UnknownEntityErr");
                }
            } else {
                assert_eq!(response.status(), 200);
                let grid = codec_for("text/zinc")
                    .unwrap()
                    .decode_grid(&response.text().await.unwrap())
                    .unwrap();
                assert_eq!(grid.meta.has("err"), checked != "&checked=false");
            }
        }
    }
    f.close().await;
}

#[tokio::test]
async fn repair_accept_specific_quality_overrides_wildcards() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for version in ["4", "5"] {
        api_error(f.get(&format!("/readById?id=a&xeto-version={version}"))
            .header("Accept", "application/json;q=0,text/jeto;q=0,application/vnd.haystack+json;q=0,text/zinc;q=0,*/*;q=1"), 406, "NotAcceptableErr").await;
        let response = f
            .get(&format!("/readById?id=a&xeto-version={version}"))
            .header("Accept", "application/json;q=0,text/zinc;q=0.5,*/*;q=1")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/zinc");
    }
    f.close().await;
}

#[tokio::test]
async fn repair_vary_covers_negotiation_and_preserves_cors() {
    use haystack_server::cors::CorsPolicy;
    let graph = SharedGraph::new(EntityGraph::new());
    let app =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let server = HaystackServer::new(graph)
        .with_scoped_reads(app.handle())
        .with_cors(CorsPolicy::Allow(vec!["https://ops.example.com".into()]))
        .port(0);
    let owner = app
        .owned_resource(server.into_listener())
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    let address = owner.ready().await.unwrap().listeners[0].address;
    let client = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    for version in [None, Some("5")] {
        for media in [
            "application/json",
            "text/zinc",
            "application/vnd.haystack+json",
        ] {
            for encoding in ["identity", "gzip"] {
                let mut req = client
                    .get(format!("http://{address}/api/readById?checked=false"))
                    .header("Accept", media)
                    .header("Accept-Encoding", encoding)
                    .header("Origin", "https://ops.example.com");
                if let Some(v) = version {
                    req = req.header("Xeto-Version", v);
                }
                let response = req.send().await.unwrap();
                assert_eq!(response.status(), 200);
                let vary = response
                    .headers()
                    .get_all("vary")
                    .iter()
                    .flat_map(|v| v.to_str().unwrap().split(','))
                    .map(|v| v.trim().to_ascii_lowercase())
                    .collect::<Vec<_>>();
                for field in ["accept", "xeto-version", "accept-encoding", "origin"] {
                    assert!(vary.iter().any(|v| v == field), "missing {field}: {vary:?}");
                }
            }
        }
    }
    owner.close().await.unwrap();
    owner.terminated().await;
}

async fn custom_fallback(authenticated: bool) {
    use axum::{Router, http::StatusCode};
    let graph = SharedGraph::new(EntityGraph::new());
    let app =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let make_router = || {
        Router::new().fallback(|| async {
            (
                StatusCode::IM_A_TEAPOT,
                [("xeto-version", "vendor")],
                "owned fallback",
            )
        })
    };
    let untrusted = HaystackServer::new(graph.clone()).with_scoped_reads(app.handle());
    let untrusted = if authenticated {
        untrusted.with_authenticated_router(make_router())
    } else {
        untrusted.with_router(make_router())
    };
    assert!(untrusted.into_external_router().is_err());
    let server = HaystackServer::new(graph)
        .with_scoped_reads(app.handle())
        .with_trusted_external_routes()
        .with_auth(auth())
        .port(0);
    let server = if authenticated {
        server.with_authenticated_router(make_router())
    } else {
        server.with_router(make_router())
    };
    let owner = app
        .owned_resource(server.into_listener())
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    let address = owner.ready().await.unwrap().listeners[0].address;
    let client = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    for path in ["/vendor", "/api/vendor"] {
        for bearer in [false, true] {
            let mut req = client.get(format!("http://{address}{path}"));
            if bearer {
                req = req.header("Authorization", "BEARER authToken=fixture-token");
            }
            let response = req.send().await.unwrap();
            if authenticated && !bearer {
                assert_eq!(response.status(), 401);
            } else {
                assert_eq!(response.status(), 418);
                assert_eq!(response.headers()["xeto-version"], "vendor");
                assert_eq!(response.text().await.unwrap(), "owned fallback");
            }
        }
    }
    // The fallback does not replace the explicit built-in endpoint.
    let response = client
        .get(format!(
            "http://{address}/api/readById?checked=false&xeto-version=5"
        ))
        .header("Authorization", "BEARER authToken=fixture-token")
        .send()
        .await
        .unwrap();
    assert!(json_response(response, 200).await.is_null());
    owner.close().await.unwrap();
    owner.terminated().await;
}
#[tokio::test]
async fn repair_raw_custom_fallback_preserves_authority() {
    custom_fallback(false).await;
}
#[tokio::test]
async fn repair_authenticated_custom_fallback_preserves_authority() {
    custom_fallback(true).await;
}

#[tokio::test]
async fn repair_zinc_expanded_rows_are_charged_before_legacy_decode() {
    let body = format!("ver:\"3.0\"\n{}\n{}", "a".repeat(30), "T\n".repeat(900));
    assert!(body.len() < 1900);
    for (work, status) in [(12_000, 400), (100_000, 404)] {
        let f = Fixture::start(
            false,
            ReadLimits {
                max_work: work,
                max_retained_bytes: 32 * 1024 * 1024,
                ..ReadLimits::default()
            },
        )
        .await;
        api_error(
            f.post("/readById?xeto-version=5")
                .header("Content-Type", "text/zinc")
                .body(body.clone()),
            status,
            if status == 400 {
                "InvalidArgsErr"
            } else {
                "UnknownEntityErr"
            },
        )
        .await;
        f.close().await;
    }
}

#[tokio::test]
async fn contextual_jeto_boxing_modes_preserve_exact_http_results_and_reject_loss() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for accept in ["application/json;box=all", "text/jeto;box=all"] {
        let response = f
            .get("/readById?id=a&xeto-version=5")
            .header("Accept", accept)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.headers()["vary"],
            "Accept, Xeto-Version, Accept-Encoding"
        );
        let json = json_response(response, 200).await;
        assert_eq!(json["id"], serde_json::json!({"spec":"sys::Ref","val":"a"}));
        assert_eq!(
            json["dis"],
            serde_json::json!({"spec":"sys::Str","val":"Alpha"})
        );
        assert_eq!(
            json["site"],
            serde_json::json!({"spec":"sys::Marker","val":"✓"})
        );
    }
    assert!(
        json_response(
            f.get("/readById?checked=false&xeto-version=5")
                .header("Accept", "application/json;box=none")
                .send()
                .await
                .unwrap(),
            200
        )
        .await
        .is_null()
    );
    api_error(
        f.get("/readById?id=a&xeto-version=5")
            .header("Accept", "application/json;box=none"),
        406,
        "NotAcceptableErr",
    )
    .await;
    // A specific exclusion wins over wildcard acceptance for all three modes.
    for mode in ["auto", "none", "all"] {
        api_error(
            f.get("/readById?checked=false&xeto-version=5")
                .header(
                    "Accept",
                    format!("application/json;q=0, application/json;box={mode};q=0, */*;q=1"),
                )
                .header("Accept", "text/zinc;q=0"),
            406,
            "NotAcceptableErr",
        )
        .await;
    }
    let json=json_response(f.post("/readById?xeto-version=5").header("Content-Type","application/json;box=all").header("Accept","application/json;box=all").body(r#"{"id":{"spec":"sys::Ref","val":"a","dis":"incoming"},"checked":{"spec":"sys::Bool","val":"true"}}"#).send().await.unwrap(),200).await;
    assert_eq!(json["id"]["val"], "a");
    assert!(json["id"].get("dis").is_none());
    f.close().await;
}
#[tokio::test]
async fn contextual_jeto_http_uses_temporals_specials_and_nested_grid_scope() {
    use haystack_core::{
        codecs::{jeto, typed},
        data::{HCol, HGrid},
        kinds::Uri,
    };
    let f = Fixture::start(false, ReadLimits::default()).await;
    let mut meta = HDict::new();
    meta.set("title", Kind::Str("Nested".into()));
    let mut colmeta = HDict::new();
    colmeta.set("of", Kind::Ref(HRef::from_val("sys::Number")));
    let mut gridrow = HDict::new();
    gridrow.set("value", Kind::Number(Number::unitless(42.0)));
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("contextual")));
    row.set(
        "date",
        Kind::Date(chrono::NaiveDate::from_ymd_opt(2024, 11, 26).unwrap()),
    );
    row.set(
        "time",
        Kind::Time(chrono::NaiveTime::from_hms_opt(14, 30, 0).unwrap()),
    );
    row.set("uri", Kind::Uri(Uri::new("https://example.test/")));
    row.set("positiveInf", Kind::Float(Float::new(f64::INFINITY)));
    row.set(
        "canonicalNaN",
        Kind::Float(Float::from_bits(0x7ff8000000000000)),
    );
    row.set(
        "grid",
        Kind::Grid(Box::new(HGrid::from_parts(
            meta,
            vec![HCol::with_meta("value", colmeta)],
            vec![gridrow],
        ))),
    );
    let expected = Kind::Dict(Box::new(row.clone()));
    f.graph.add(row).unwrap();
    for mode in ["auto", "all"] {
        let response = f
            .get("/readById?id=contextual&xeto-version=5")
            .header("Accept", format!("application/json;box={mode}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = response.bytes().await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["grid"]["spec"], "sys::Grid");
        assert_eq!(json["grid"]["cols"][0]["name"], "value");
        assert_eq!(json["grid"]["cols"][0]["of"], "sys::Number");
        assert_eq!(
            json["date"],
            serde_json::json!({"spec":"sys::Date","val":"2024-11-26"})
        );
        assert_eq!(
            json["canonicalNaN"],
            serde_json::json!({"spec":"sys::Float","val":"NaN"})
        );
        let decoded = jeto::decode(
            &body,
            &jeto::Context::standard(),
            Some("sys::Dict"),
            jeto::Limits::default(),
        )
        .unwrap();
        assert_eq!(
            typed::encode(&decoded).unwrap(),
            typed::encode(&expected).unwrap()
        );
    }
    f.close().await;
}
#[tokio::test]
async fn contextual_jeto_request_validates_every_member_before_binding_declared_parameters() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for body in [
        r#"{"id":"a","id":"x"}"#,
        r#"{"id":"a","extra":{"spec":"missing::Type","val":"bad"}}"#,
        r#"{"id":"a","extra":[null,{"spec":"sys::Int","val":42}]}"#,
        r#"{"id":{"spec":"sys::Str","val":"a"},"checked":true}"#,
        r#"{"id":"a","checked":{"spec":"sys::Number","val":"1"}}"#,
    ] {
        api_error(
            f.post("/readById?xeto-version=5")
                .header("Content-Type", "application/json")
                .body(body),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    for body in [
        r#"{"id":"a","checked":"true","extra":{"x":[1,2,null]}}"#,
        r#"{"id":"a","checked":true}"#,
    ] {
        assert_eq!(
            json_response(
                f.post("/readById?xeto-version=5")
                    .header("Content-Type", "application/json")
                    .body(body)
                    .send()
                    .await
                    .unwrap(),
                200
            )
            .await["id"]["val"],
            "a"
        );
    }
    f.close().await;
}

#[tokio::test]
async fn v5_ops_discovers_the_executable_profile_and_rejects_arguments() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for path in ["/ops", "/sys.api::ops"] {
        let value = json_response(
            f.get(path)
                .header("Xeto-Version", "5")
                .send()
                .await
                .unwrap(),
            200,
        )
        .await;
        let rows = value["rows"].as_array().expect("ops returns a Grid");
        assert_eq!(
            rows.iter()
                .map(|row| row["qname"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "ph.api::nav",
                "sys.api::about",
                "sys.api::close",
                "sys.api::filetypes",
                "sys.api::libs",
                "sys.api::ops",
                "sys.api::read",
                "sys.api::readAll",
                "sys.api::readById",
                "sys.api::readByIds"
            ]
        );
        for row in rows {
            assert!(row["signature"].as_str().is_some());
            assert_eq!(
                !row["noSideEffects"].is_null(),
                row["qname"] != "sys.api::close"
            );
        }
    }
    for body in [
        r#"{"returns":null}"#,
        r#"{"unexpected":null}"#,
        r#"{"unexpected":true}"#,
    ] {
        api_error(
            f.post("/ops")
                .header("Xeto-Version", "5")
                .header("Content-Type", "application/json")
                .body(body),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    api_error(
        f.get("/ops?returns=null").header("Xeto-Version", "5"),
        400,
        "InvalidArgsErr",
    )
    .await;
    f.close().await;
}

#[tokio::test]
async fn v5_ops_authentication_precedes_protocol_validation_and_h4_stays_public() {
    let f = Fixture::start(true, ReadLimits::default()).await;
    for path in ["/ops", "/ops?xeto-version=4"] {
        assert_eq!(f.get(path).send().await.unwrap().status(), 200);
    }
    api_error(f.get("/ops").header("Xeto-Version", "5"), 403, "AuthErr").await;
    api_error(f.get("/ops?xeto-version=bad"), 401, "AuthErr").await;
    api_error(f.get("/ops?xeto-version=4&xeto-version=4"), 401, "AuthErr").await;
    api_error(
        f.get("/ops?xeto-version=bad")
            .header("Authorization", "BEARER authToken=fixture-token"),
        400,
        "UnsupportedVersionErr",
    )
    .await;
    let value = json_response(
        f.get("/ops?xeto%2Dversion=5")
            .header("Xeto-Version", "4")
            .header("Authorization", "BEARER authToken=fixture-token")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(value["rows"].as_array().unwrap().len(), 10);
    f.close().await;
}

struct FunctionPolicy {
    denied: Arc<std::sync::atomic::AtomicBool>,
    snapshots: Arc<std::sync::atomic::AtomicUsize>,
}
struct FunctionRules {
    read_by_id: bool,
    ops: bool,
}
impl ReadPolicy for FunctionPolicy {
    fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        self.snapshots
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let read_by_id = matches!(principal, Principal::Authenticated { subject, .. } if subject == "user")
            && !self.denied.load(std::sync::atomic::Ordering::SeqCst);
        Ok(Arc::new(FunctionRules {
            read_by_id,
            ops: true,
        }))
    }
}
impl PolicySnapshot for FunctionRules {
    fn scope_key(&self) -> &str {
        if self.read_by_id {
            "read-allowed"
        } else {
            "ops-only"
        }
    }
    fn function(&self, function: &haystack_app::FunctionIdentity) -> bool {
        assert_eq!(function.library_version, "5.0.0");
        assert_eq!(
            function.revision,
            "873b922451d3ef4c0c9c08ef3daa542f352d69f3"
        );
        let library = function.qname.split("::").next().unwrap();
        assert_eq!(
            function.source_path,
            format!("src/xeto/{library}/funcs.xeto")
        );
        (self.ops && function.qname == "sys.api::ops")
            || (self.read_by_id && function.qname == "sys.api::readById")
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, _: &str) -> bool {
        true
    }
    fn tag(&self, _: &str, _: &str) -> bool {
        true
    }
    fn reference(&self, _: &str) -> bool {
        true
    }
    fn reference_display(&self, _: &str) -> bool {
        true
    }
    fn catalog(&self, _: haystack_app::CatalogKind, _: &str) -> bool {
        true
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}
#[tokio::test]
async fn ops_and_invocation_share_explicit_function_policy_with_one_fresh_snapshot() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let denied = Arc::new(AtomicBool::new(false));
    let snapshots = Arc::new(AtomicUsize::new(0));
    let f = Fixture::with_policy(
        true,
        ReadLimits {
            max_concurrent: 1,
            max_queued: 0,
            ..ReadLimits::default()
        },
        Arc::new(FunctionPolicy {
            denied: denied.clone(),
            snapshots: snapshots.clone(),
        }),
    )
    .await;
    for (token, expected) in [
        ("fixture-token", vec!["sys.api::ops", "sys.api::readById"]),
        ("ops-fixture-token", vec!["sys.api::ops"]),
    ] {
        let value = json_response(
            f.get("/ops")
                .header("Xeto-Version", "5")
                .header("Authorization", format!("BEARER authToken={token}"))
                .send()
                .await
                .unwrap(),
            200,
        )
        .await;
        assert_eq!(
            value["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["qname"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
    }
    assert_eq!(snapshots.load(Ordering::SeqCst), 2);
    api_error(
        f.get("/readById?id=a")
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=ops-fixture-token"),
        404,
        "UnknownFuncErr",
    )
    .await;
    let entity = json_response(
        f.get("/sys.api::readById?id=a")
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=fixture-token")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(entity["dis"], "Alpha");
    denied.store(true, Ordering::SeqCst);
    api_error(
        f.get("/sys.api::readById?id=a")
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=fixture-token"),
        404,
        "UnknownFuncErr",
    )
    .await;
    assert_eq!(snapshots.load(Ordering::SeqCst), 5);
    f.close().await;
}

#[tokio::test]
async fn ops_zero_arguments_accept_supported_containers_and_preserve_null_key_rejection() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for (media, body) in [
        ("application/json", "{}"),
        ("application/json", r#"{"spec":"sys::Dict"}"#),
        (
            "application/vnd.haystack+json",
            r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[],"rows":[{}]}"#,
        ),
        ("text/zinc", "ver:\"3.0\"\nempty\n"),
    ] {
        let value = json_response(
            f.post("/ops?xeto-ignored=ignored")
                .header("Xeto-Version", "5")
                .header("Content-Type", media)
                .body(body)
                .send()
                .await
                .unwrap(),
            200,
        )
        .await;
        assert_eq!(value["rows"].as_array().unwrap().len(), 10);
    }
    for (media, body) in [
        (
            "application/vnd.haystack+json",
            r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[{"name":"returns"}],"rows":[{"returns":null}]}"#,
        ),
        (
            "application/vnd.haystack+json",
            r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[{"name":"unexpected"}],"rows":[{"unexpected":null}]}"#,
        ),
        ("text/zinc", "ver:\"3.0\"\nreturns\nN\n"),
    ] {
        api_error(
            f.post("/ops")
                .header("Xeto-Version", "5")
                .header("Content-Type", media)
                .body(body),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    f.close().await;
}

#[tokio::test]
async fn ops_duplicate_version_headers_and_query_precedence_cannot_bypass_authentication() {
    let f = Fixture::start(true, ReadLimits::default()).await;
    api_error(
        f.get("/ops").header("Xeto-Version", "unsupported"),
        401,
        "AuthErr",
    )
    .await;
    api_error(
        f.get("/ops")
            .header("Xeto-Version", "4")
            .header("Xeto-Version", "4"),
        401,
        "AuthErr",
    )
    .await;
    api_error(
        f.get("/ops?xeto-version=4")
            .header("Xeto-Version", "4")
            .header("Xeto-Version", "4")
            .header("Authorization", "BEARER authToken=fixture-token"),
        400,
        "UnsupportedVersionErr",
    )
    .await;
    api_error(
        f.get("/ops?xeto-version=4&xeto-version=4")
            .header("Authorization", "BEARER authToken=fixture-token"),
        400,
        "UnsupportedVersionErr",
    )
    .await;
    api_error(
        f.get("/ops?xeto%2Dversion=5").header("Xeto-Version", "4"),
        403,
        "AuthErr",
    )
    .await;
    let legacy = f
        .get("/ops?xeto%2Dversion=4")
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), 200);
    assert_eq!(legacy.headers()["xeto-version"], "4");
    let grid = codec_for("text/zinc")
        .unwrap()
        .decode_grid(&legacy.text().await.unwrap())
        .unwrap();
    assert!(grid.col("name").is_some() && grid.col("summary").is_some());
    f.close().await;
}

#[tokio::test]
async fn ops_generated_grid_limits_return_one_error_without_partial_rows() {
    for limits in [
        ReadLimits {
            max_rows: 1,
            ..ReadLimits::default()
        },
        ReadLimits {
            max_candidates: 1,
            ..ReadLimits::default()
        },
        ReadLimits {
            max_output_bytes: 128,
            ..ReadLimits::default()
        },
    ] {
        let f = Fixture::start(false, limits).await;
        let response = json_response(
            f.get("/ops")
                .header("Xeto-Version", "5")
                .send()
                .await
                .unwrap(),
            400,
        )
        .await;
        assert_eq!(response["spec"], "sys.api::InvalidArgsErr");
        assert!(response.get("rows").is_none());
        f.close().await;
    }
}

struct ReadByIdOnly;
impl ReadPolicy for ReadByIdOnly {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(FunctionRules {
            read_by_id: true,
            ops: false,
        }))
    }
}

#[tokio::test]
async fn resolver_candidate_limit_counts_hidden_entries_before_a_null_read() {
    for max_candidates in [1, 10] {
        let f = Fixture::with_policy(
            true,
            ReadLimits {
                max_candidates,
                ..ReadLimits::default()
            },
            Arc::new(ReadByIdOnly),
        )
        .await;
        // An absent nullable id with checked=false does no graph lookup. Only
        // all ten registry entries, including policy-hidden entries, spend candidates.
        let response = f
            .get("/readById?checked=false")
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=fixture-token")
            .send()
            .await
            .unwrap();
        let value = json_response(response, if max_candidates == 1 { 400 } else { 200 }).await;
        if max_candidates == 1 {
            assert_eq!(
                value,
                serde_json::json!({
                    "spec": "sys.api::InvalidArgsErr",
                    "status": 400,
                    "dis": "Invalid request arguments"
                })
            );
        } else {
            assert_eq!(value, serde_json::Value::Null);
            // With enough scan budget, direct invocation confirms ops is hidden.
            api_error(
                f.get("/ops")
                    .header("Xeto-Version", "5")
                    .header("Authorization", "BEARER authToken=fixture-token"),
                404,
                "UnknownFuncErr",
            )
            .await;
        }
        f.close().await;
    }
}

#[tokio::test]
async fn system_read_by_ids_preserves_positions_duplicates_and_literal_missing_zinc_rows() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    let value = json_response(
        f.post("/readByIds")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(r#"{"ids":["x","a","x","missing","denied"],"checked":false}"#)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    let rows = value["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows.iter()
            .map(|row| row["id"]["val"].as_str())
            .collect::<Vec<_>>(),
        [Some("x"), Some("a"), Some("x"), None, None]
    );
    assert!(
        rows[3]
            .as_object()
            .unwrap()
            .values()
            .all(serde_json::Value::is_null)
    );
    assert!(
        rows[4]
            .as_object()
            .unwrap()
            .values()
            .all(serde_json::Value::is_null)
    );
    let response = f
        .post("/readByIds")
        .header("Xeto-Version", "5")
        .header("Content-Type", "application/json")
        .header("Accept", "text/zinc")
        .body(r#"{"ids":["missing","denied","missing"],"checked":false}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    assert_eq!(
        text.lines().skip(1).collect::<Vec<_>>(),
        ["id", "N", "N", "N"]
    );
    let empty = json_response(
        f.post("/readByIds")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(r#"{"ids":[]}"#)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert!(empty["rows"].as_array().unwrap().is_empty());
    for id in ["missing", "denied"] {
        api_error(
            f.post("/readByIds")
                .header("Xeto-Version", "5")
                .header("Content-Type", "application/json")
                .body(format!(r#"{{"ids":["a","{id}"]}}"#)),
            404,
            "UnknownEntityErr",
        )
        .await;
    }
    f.close().await;
}

#[tokio::test]
async fn system_filter_reads_authorize_before_limits_and_sort_only_selected_rows() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for (id, dis) in [("aa", "Zulu"), ("ab", "Beta"), ("ac", "Aardvark")] {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val(id)));
        row.set("dis", Kind::Str(dis.into()));
        row.set("pr09", Kind::Marker);
        f.graph.add(row).unwrap();
    }
    let first = json_response(
        f.get("/read?filter=site")
            .header("Xeto-Version", "5")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(first["id"]["val"], "a");
    let authorized = json_response(
        f.post("/readAll")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(r#"{"filter":"site","opts":{"limit":2}}"#)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(
        authorized["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"]["val"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a", "x"]
    );
    let ties = json_response(
        f.post("/readAll")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(r#"{"filter":"site","opts":{"limit":2,"sort":false}}"#)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(
        ties["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"]["val"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a", "x"]
    );
    let sorted = json_response(
        f.post("/readAll")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(r#"{"filter":"pr09","opts":{"limit":2,"sort":false}}"#)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(
        sorted["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"]["val"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ab", "aa"]
    );
    for body in [
        r#"{"filter":"site","opts":{"search":null}}"#,
        r#"{"filter":"site","opts":{"gridMeta":{}}}"#,
        r#"{"filter":"site","opts":{"unknown":null}}"#,
        r#"{"filter":"site","opts":{"limit":1.5}}"#,
    ] {
        api_error(
            f.post("/readAll")
                .header("Xeto-Version", "5")
                .header("Content-Type", "application/json")
                .body(body),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    api_error(
        f.get("/readAll?filter=site&opts=%7B%22unknown%22%3Anull%7D")
            .header("Xeto-Version", "5"),
        400,
        "InvalidArgsErr",
    )
    .await;
    assert_eq!(
        json_response(
            f.get("/read?filter=missingMarker&checked=false")
                .header("Xeto-Version", "5")
                .send()
                .await
                .unwrap(),
            200
        )
        .await,
        serde_json::Value::Null
    );
    api_error(
        f.get("/read?filter=missingMarker")
            .header("Xeto-Version", "5"),
        404,
        "UnknownEntityErr",
    )
    .await;
    f.close().await;
}

#[tokio::test]
async fn system_filter_explicit_string_post_is_not_relabelled() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for operation in ["read", "readAll"] {
        api_error(
            f.post(&format!("/{operation}"))
                .header("Xeto-Version", "5")
                .header("Content-Type", "application/json")
                .body(r#"{"filter":{"spec":"sys::Str","val":"site"}}"#),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    f.close().await;
}

#[tokio::test]
async fn system_filter_explicit_string_get_is_not_relabelled() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for operation in ["read", "readAll"] {
        api_error(
            f.get(&format!(
                "/{operation}?filter=%7B%22spec%22%3A%22sys%3A%3AStr%22%2C%22val%22%3A%22site%22%7D"
            ))
            .header("Xeto-Version", "5"),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    f.close().await;
}

#[tokio::test]
async fn system_filter_mixed_query_and_grid_does_not_promote_query_text() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for version in ["4", "5"] {
        for body in [
            r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[],"rows":[{}]}"#,
            r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[],"rows":[]}"#,
        ] {
            api_error(
                f.post("/sys.api::readAll?filter=%7B%22spec%22%3A%22sys%3A%3AStr%22%2C%22val%22%3A%22site%22%7D")
                    .header("Xeto-Version", version)
                    .header("Content-Type", "application/vnd.haystack+json")
                    .body(body),
                400,
                "InvalidArgsErr",
            )
            .await;
        }
    }
    f.close().await;
}

#[tokio::test]
async fn system_filter_contextual_boxed_and_legacy_text_remain_valid() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for request in [
        f.post("/readAll")
            .header("Content-Type", "application/json")
            .body(r#"{"filter":"site"}"#),
        f.post("/readAll")
            .header("Content-Type", "application/json")
            .body(r#"{"filter":{"spec":"sys::Filter","val":"site"}}"#),
        f.get("/readAll?filter=site"),
        f.get("/readAll?filter=%7B%22spec%22%3A%22sys%3A%3AFilter%22%2C%22val%22%3A%22site%22%7D"),
    ] {
        let value = json_response(
            request.header("Xeto-Version", "5").send().await.unwrap(),
            200,
        )
        .await;
        assert_eq!(value["rows"].as_array().unwrap().len(), 2);
        assert_eq!(value["rows"][0]["id"]["val"], "a");
        assert_eq!(value["rows"][1]["id"]["val"], "x");
    }
    // The bridge follows actual Grid media, including Grid input under v5.
    // These bytes are authored directly, without using the native encoder.
    for version in ["4", "5"] {
        for (media, body) in [
            ("text/zinc", "ver:\"3.0\"\nfilter\n\"site\"\n"),
            (
                "application/vnd.haystack+json",
                r#"{"_kind":"grid","meta":{"ver":"3.0"},"cols":[{"name":"filter"}],"rows":[{"filter":"site"}]}"#,
            ),
        ] {
            let response = f
                .post("/sys.api::readAll")
                .header("Xeto-Version", version)
                .header("Content-Type", media)
                .header("Accept", "text/zinc")
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200, "version={version} media={media}");
            let body = response.text().await.unwrap();
            assert!(body.contains("@a"), "{body}");
            assert!(body.contains("@x"), "{body}");
            assert!(!body.contains("@denied"), "{body}");
        }
    }
    f.close().await;
}

#[tokio::test]
async fn system_opts_structural_dict_post_preserves_limit() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    let value = json_response(
        f.post("/readAll")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(r#"{"filter":"site","opts":{"spec":"sys::Dict","limit":1}}"#)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(value["rows"].as_array().unwrap().len(), 1);
    assert_eq!(value["rows"][0]["id"]["val"], "a");
    f.close().await;
}

#[tokio::test]
async fn system_opts_structural_dict_get_preserves_limit() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    let value = json_response(
        f.get("/readAll?filter=site&opts=%7B%22spec%22%3A%22sys%3A%3ADict%22%2C%22limit%22%3A1%7D")
            .header("Xeto-Version", "5")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(value["rows"].as_array().unwrap().len(), 1);
    assert_eq!(value["rows"][0]["id"]["val"], "a");
    f.close().await;
}

#[tokio::test]
async fn system_opts_malformed_spec_and_unknown_null_remain_invalid() {
    let f = Fixture::start(false, ReadLimits::default()).await;
    for opts in [
        r#"{"spec":null,"limit":1}"#,
        r#"{"spec":1,"limit":1}"#,
        r#"{"spec":"sys::Str","limit":1}"#,
        r#"{"spec":"sys::Grid","limit":1}"#,
        r#"{"spec":"missing::Dict","limit":1}"#,
        r#"{"spec":"rusty.http::Arguments","limit":1}"#,
        r#"{"spec":"rusty.call::Arguments","limit":1}"#,
        r#"{"spec":{"spec":"sys::Ref","val":"sys::Dict"},"limit":1}"#,
        r#"{"spec":"sys::Dict","unknown":null}"#,
    ] {
        api_error(
            f.post("/readAll")
                .header("Xeto-Version", "5")
                .header("Content-Type", "application/json")
                .body(format!(r#"{{"filter":"site","opts":{opts}}}"#)),
            400,
            "InvalidArgsErr",
        )
        .await;
        let mut url = reqwest::Url::parse(&format!("{}/readAll", f.base)).unwrap();
        url.query_pairs_mut()
            .append_pair("filter", "site")
            .append_pair("opts", opts);
        api_error(
            f.client.get(url).header("Xeto-Version", "5"),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    f.close().await;
}

#[tokio::test]
async fn system_metadata_is_typed_truthful_and_legacy_mapping_is_deliberate() {
    let f = Fixture::start(true, ReadLimits::default()).await;
    let token = "BEARER authToken=fixture-token";
    let info = json_response(
        f.get("/about")
            .header("Xeto-Version", "5")
            .header("Authorization", token)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(info["serverName"], "rusty-haystack");
    assert_eq!(info["tz"], "UTC");
    assert_eq!(info["protocolVersions"], serde_json::json!(["4", "5"]));
    assert_eq!(info["productVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["whoami"], "user");
    assert!(info["serverTime"].as_str().unwrap().contains('T'));
    assert!(info["serverBootTime"].as_str().unwrap().contains('T'));
    let again = json_response(
        f.get("/about")
            .header("Xeto-Version", "5")
            .header("Authorization", token)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(again["serverBootTime"], info["serverBootTime"]);
    let libs = json_response(
        f.get("/libs")
            .header("Xeto-Version", "5")
            .header("Authorization", token)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(
        libs["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ph.api", "sys", "sys.api"]
    );
    assert_eq!(libs["rows"][0]["version"], "5.0.0");
    for path in ["/about", "/libs"] {
        api_error(
            f.get(path)
                .header("Xeto-Version", "5")
                .header("Authorization", token)
                .header("Accept", "text/zinc"),
            406,
            "NotAcceptableErr",
        )
        .await;
    }
    let legacy = f
        .get("/sys.api::about")
        .header("Xeto-Version", "4")
        .header("Authorization", token)
        .header("Accept", "text/zinc")
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), 200);
    let legacy = codec_for("text/zinc")
        .unwrap()
        .decode_grid(&legacy.text().await.unwrap())
        .unwrap();
    assert_eq!(legacy.rows[0].get("tz"), Some(&Kind::Str("UTC".into())));
    assert!(matches!(
        legacy.rows[0].get("serverBootTime"),
        Some(Kind::DateTime(_))
    ));
    let types = json_response(
        f.get("/filetypes")
            .header("Xeto-Version", "5")
            .header("Authorization", token)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(
        types["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["hayson", "jeto", "zinc"]
    );
    assert!(
        types["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["canRead"] == true && row["canWrite"] == true)
    );
    let legacy = f
        .get("/filetypes")
        .header("Xeto-Version", "4")
        .header("Authorization", token)
        .header("Accept", "text/zinc")
        .send()
        .await
        .unwrap();
    assert_eq!(legacy.status(), 200);
    let text = legacy.text().await.unwrap();
    assert!(text.contains("^filetype:zinc"));
    assert!(text.contains("text/zinc"));
    assert!(!text.contains("canRead") && !text.contains("fileSpec"));
    f.close().await;
}

#[tokio::test]
async fn system_limits_charge_positions_and_selected_rows_without_partial_results() {
    let f = Fixture::start(
        false,
        ReadLimits {
            max_rows: 1,
            ..ReadLimits::default()
        },
    )
    .await;
    for body in [
        r#"{"ids":["a","a"]}"#,
        r#"{"ids":["missing","denied"],"checked":false}"#,
    ] {
        api_error(
            f.post("/readByIds")
                .header("Xeto-Version", "5")
                .header("Content-Type", "application/json")
                .body(body),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    api_error(
        f.get("/readAll?filter=site").header("Xeto-Version", "5"),
        400,
        "InvalidArgsErr",
    )
    .await;
    api_error(
        f.get("/filetypes").header("Xeto-Version", "5"),
        400,
        "InvalidArgsErr",
    )
    .await;
    for limit in ["1", "1.0", "0"] {
        let value = json_response(
            f.post("/readAll")
                .header("Xeto-Version", "5")
                .header("Content-Type", "application/json")
                .body(format!(r#"{{"filter":"site","opts":{{"limit":{limit}}}}}"#))
                .send()
                .await
                .unwrap(),
            200,
        )
        .await;
        assert_eq!(
            value["rows"].as_array().unwrap().len(),
            usize::from(limit != "0")
        );
    }
    for limit in [
        "-1",
        "2",
        "1.5",
        r#"{"spec":"sys::Float","val":"INF"}"#,
        r#"{"spec":"sys::Number","val":"1kW"}"#,
    ] {
        api_error(
            f.post("/readAll")
                .header("Xeto-Version", "5")
                .header("Content-Type", "application/json")
                .body(format!(r#"{{"filter":"site","opts":{{"limit":{limit}}}}}"#)),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    f.close().await;
    // Resolution spends one candidate per fixed binding, then the read spends
    // one per requested position. Derive the bound from the inventory so the
    // single-position control fits exactly and only the duplicate exceeds it.
    let bindings = ReadService::supported_functions().count();
    let f = Fixture::start(
        false,
        ReadLimits {
            max_candidates: bindings + 1,
            ..ReadLimits::default()
        },
    )
    .await;
    let control = json_response(
        f.post("/readByIds")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(r#"{"ids":["a"]}"#)
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(control["rows"].as_array().unwrap().len(), 1);
    api_error(
        f.post("/readByIds")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(r#"{"ids":["a","a"]}"#),
        400,
        "InvalidArgsErr",
    )
    .await;
    f.close().await;
}

struct LibraryRules;
impl ReadPolicy for LibraryRules {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(Self))
    }
}
impl PolicySnapshot for LibraryRules {
    fn scope_key(&self) -> &str {
        "library-fixture"
    }
    fn function(&self, _: &haystack_app::FunctionIdentity) -> bool {
        true
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, _: &str) -> bool {
        true
    }
    fn tag(&self, _: &str, _: &str) -> bool {
        true
    }
    fn reference(&self, _: &str) -> bool {
        true
    }
    fn reference_display(&self, _: &str) -> bool {
        true
    }
    fn catalog(&self, kind: haystack_app::CatalogKind, name: &str) -> bool {
        kind != haystack_app::CatalogKind::Library || name == "sys.api"
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}
#[tokio::test]
async fn system_libraries_respect_visibility_and_h4_keeps_name_version_columns() {
    let f = Fixture::with_policy(false, ReadLimits::default(), Arc::new(LibraryRules)).await;
    let value = json_response(
        f.get("/libs")
            .header("Xeto-Version", "5")
            .send()
            .await
            .unwrap(),
        200,
    )
    .await;
    assert_eq!(value["rows"].as_array().unwrap().len(), 1);
    assert_eq!(value["rows"][0]["name"], "sys.api");
    let response = f
        .get("/sys.api::libs")
        .header("Xeto-Version", "4")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let grid = codec_for("text/zinc")
        .unwrap()
        .decode_grid(&response.text().await.unwrap())
        .unwrap();
    assert_eq!(
        grid.cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["name", "version"]
    );
    assert_eq!(grid.rows.len(), 1);
    assert_eq!(
        grid.rows[0].get("version"),
        Some(&Kind::Str("5.0.0".into()))
    );
    for path in [
        "/sys.api::about?filter=site",
        "/sys.api::libs?limit=1",
        "/filetypes?limit=1",
    ] {
        api_error(
            f.get(path).header("Xeto-Version", "4"),
            400,
            "InvalidArgsErr",
        )
        .await;
    }
    f.close().await;
}
