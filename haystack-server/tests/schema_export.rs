//! Project-owned schema export through the same admitted HTTP application.
use haystack_app::{
    AllowAll, ApplicationBuilder, ApplicationOwner, Principal, ReadContext, ReadLimits,
};
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind},
    xeto::catalog::Catalog,
};
use haystack_server::HaystackServer;
use std::{sync::Arc, time::Duration};

const ASSET: &str = include_str!("../examples/schema-library.xeto");
struct Fixture {
    owner: ApplicationOwner,
    url: String,
    http: reqwest::Client,
}
impl Fixture {
    async fn start() -> Self {
        let graph = SharedGraph::new(EntityGraph::new());
        let builder =
            ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
                .unwrap();
        let reads = builder.handle().read_service();
        let listener = HaystackServer::new(graph.clone())
            .with_scoped_reads(builder.handle())
            .port(0)
            .into_listener();
        let owner = builder
            .owned_resource(listener)
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let address = owner.ready().await.unwrap().listeners[0].address;
        reads
            .activate_catalog(
                ReadContext::with_timeout(
                    Principal::TrustedEmbedding {
                        subject: "fixture-owner".into(),
                    },
                    Duration::from_secs(5),
                ),
                Catalog::load_http_pinned()
                    .unwrap()
                    .with_project("fixture", ASSET, &[])
                    .unwrap(),
            )
            .await
            .unwrap();
        for (id, typed) in [("asset", true), ("dynamic", false)] {
            let mut row = HDict::new();
            row.set("id", Kind::Ref(HRef::from_val(id)));
            row.set("dis", Kind::Str("Alpha".into()));
            if typed {
                row.set("spec", Kind::Ref(HRef::from_val("fixture::Asset")));
            }
            graph.add(row).unwrap();
        }
        Self {
            owner,
            url: format!("http://{address}/api"),
            http: haystack_client::ClientConfig::default()
                .build_reqwest_client()
                .unwrap(),
        }
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
    }
}
#[tokio::test]
async fn project_schema_export_is_deterministic_and_preserves_contextual_read() {
    let f = Fixture::start().await;
    let response = f
        .http
        .get(format!("{}/rusty-haystack/openapi.json", f.url))
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.bytes().await.unwrap();
    let again = f
        .http
        .get(format!("{}/rusty-haystack/openapi.json", f.url))
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 200);
    assert_eq!(again.headers()["cache-control"], "private, no-store");
    assert_eq!(again.bytes().await.unwrap(), bytes);
    let typed = f
        .http
        .post(format!("{}/sys.api::readById", f.url))
        .header("Xeto-Version", "5")
        .header("Accept", "application/json;box=none")
        .header("Content-Type", "application/json")
        .body(r#"{"id":"asset"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(typed.status(), 200, "{}", typed.text().await.unwrap());
    let value: serde_json::Value = serde_json::from_slice(&typed.bytes().await.unwrap()).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"id":"asset","dis":"Alpha","spec":"fixture::Asset"})
    );
    f.close().await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let schema: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(schema["openapi"], "3.1.0");
    assert!(schema["paths"]["/api/sys.api::readById"]["post"].is_object());
    assert_eq!(
        schema["components"]["schemas"]["fixture_c_cAsset"]["type"],
        "object"
    );
}

#[tokio::test]
async fn schema_route_rejects_unsupported_controls_with_typed_errors() {
    let fixture = Fixture::start().await;
    let url = format!("{}/rusty-haystack/openapi.json", fixture.url);
    let response = fixture
        .http
        .post(&url)
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 405);
    assert_eq!(response.headers()["allow"], "GET");
    let error: serde_json::Value =
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
    assert_eq!(error["spec"], "sys.api::MethodNotAllowedErr");
    for request in [
        fixture.http.get(&url),
        fixture
            .http
            .get(format!("{url}?anything=value"))
            .header("Xeto-Version", "5"),
    ] {
        assert_eq!(request.send().await.unwrap().status(), 400);
    }
    let response = fixture
        .http
        .get(&url)
        .header("Xeto-Version", "5")
        .header("Accept-Encoding", "identity;q=0, gzip;q=0")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 406);
    fixture.close().await;
}
