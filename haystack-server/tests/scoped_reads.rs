//! Actual HTTP consumers decode the same bounded H4 output as embedded callers.
use chrono::{DateTime, FixedOffset};
use haystack_app::*;
use haystack_core::{
    codecs::codec_for,
    data::{HDict, HGrid},
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, NominalScalar, Number},
    ontology::DefNamespace,
};
use haystack_server::{
    HaystackServer, HistoryProvider,
    actions::{ActionHandler, ActionRegistry},
    auth::{
        AuthManager, AuthUser,
        users::{UserRecord, parse_password_hash},
    },
    his_store::HisItem,
};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::sync::oneshot;

struct Server {
    url: String,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}
impl Server {
    fn start(server: HaystackServer) -> Self {
        let (stop, mut stopped) = oneshot::channel();
        let (tx, rx) = mpsc::channel();
        let task = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move { tokio::select! { _=&mut stopped=>{}, result=server.port(0).run_reporting_addr(move|a|tx.send(a).unwrap())=>result.unwrap(), } });
        });
        let address = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        Self {
            url: format!("http://{address}/api"),
            stop: Some(stop),
            task: Some(task),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let result = task.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}
struct Rules(Arc<AtomicUsize>);
struct Snapshot {
    authenticated: bool,
}
impl ReadPolicy for Rules {
    fn snapshot(&self, p: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(Snapshot {
            authenticated: matches!(p,Principal::Authenticated{subject,permissions} if subject=="user" && permissions==&["read".to_string()]),
        }))
    }
}
impl PolicySnapshot for Snapshot {
    fn scope_key(&self) -> &str {
        if self.authenticated {
            "user-v1"
        } else {
            "anonymous-v1"
        }
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, id: &str) -> bool {
        id != "denied" && (self.authenticated || id != "private")
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        tag != "secret"
    }
    fn reference(&self, id: &str) -> bool {
        self.entity(id)
    }
    fn reference_display(&self, _: &str) -> bool {
        false
    }
    fn catalog(&self, _: CatalogKind, name: &str) -> bool {
        !name.starts_with("hidden")
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}
fn fixture() -> (SharedGraph, ReadService, Arc<AtomicUsize>) {
    let mut ns = DefNamespace::new();
    ns.load_xeto_str("Thing: Dict {\n  site\n}\n", "visible")
        .unwrap();
    ns.load_xeto_str("Secret: Dict {\n  site\n}\n", "hidden")
        .unwrap();
    let graph = SharedGraph::new(EntityGraph::with_namespace(ns));
    for id in ["a", "z", "private", "denied"] {
        let mut row = HDict::new();
        row.set(
            "id",
            Kind::Ref(HRef::new(id, Some("private display".into()))),
        );
        row.set("site", Kind::Marker);
        row.set("nullField", Kind::Null);
        row.set("signedZero", Kind::Number(Number::unitless(-0.0)));
        row.set("secret", Kind::Str("private value".into()));
        row.set(
            "hiddenReference",
            Kind::List(vec![Kind::Ref(HRef::from_val("denied"))]),
        );
        if id == "a" {
            row.set("parentRef", Kind::Ref(HRef::from_val("z")));
        }
        graph.add(row).unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let service = ReadService::new(
        graph.clone(),
        Arc::new(Rules(calls.clone())),
        ReadLimits::default(),
    )
    .unwrap();
    (graph, service, calls)
}
fn auth() -> AuthManager {
    let users=HashMap::from([("user".into(),UserRecord { credentials: parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap(),permissions:vec!["read".into()] })]);
    let manager = AuthManager::new(users, Duration::from_secs(60));
    manager.inject_token(
        "fixture-token".into(),
        AuthUser {
            username: "user".into(),
            permissions: vec!["read".into()],
        },
    );
    manager
}
fn context(principal: Principal) -> ReadContext {
    ReadContext::with_timeout(principal, Duration::from_secs(5))
}
fn decode(bytes: &[u8], codec: H4Codec) -> HGrid {
    codec_for(codec.mime())
        .unwrap()
        .decode_grid(std::str::from_utf8(bytes).unwrap())
        .unwrap()
}
async fn compare(
    server: &Server,
    svc: &ReadService,
    authenticated: bool,
    endpoint: &str,
    query: ReadQuery,
    body: &str,
    codec: H4Codec,
) -> HGrid {
    let principal = if authenticated {
        Principal::authenticated("user", vec!["read".into()])
    } else {
        Principal::Anonymous
    };
    let page = svc
        .read(
            context(principal),
            ReadRequest::new(query, OutputProfile::H4(codec)),
        )
        .await
        .unwrap();
    assert!(page.complete);
    let ReadOutput::H4 { body: embedded, .. } = page.output else {
        panic!()
    };
    let client = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    let mut request = client
        .post(format!("{}/{endpoint}", server.url))
        .header("Content-Type", "text/zinc")
        .header("Accept", codec.mime())
        .body(body.to_owned());
    if authenticated {
        request = request.header("Authorization", "BEARER authToken=fixture-token");
    }
    let response = request.send().await.unwrap();
    assert_eq!(response.status(), 200, "{endpoint}");
    assert_eq!(response.headers()["Content-Type"], codec.mime());
    let received = response.bytes().await.unwrap();
    let actual = decode(&received, codec);
    assert_eq!(actual, decode(&embedded, codec), "{endpoint}, {codec:?}");
    actual
}
#[tokio::test]
async fn real_http_and_embedded_reads_share_identity_policy_refs_and_catalogs() {
    for authenticated in [false, true] {
        let (graph, svc, _) = fixture();
        let mut builder = HaystackServer::new(graph).with_scoped_reads(svc.clone());
        if authenticated {
            builder = builder.with_auth(auth());
        }
        let server = Server::start(builder);
        for codec in [H4Codec::Zinc, H4Codec::Json, H4Codec::JsonV3] {
            let ids = vec!["z", "a", "denied", "missing", "private", "a"];
            let body = format!(
                "ver:\"3.0\"\nid\n{}\n",
                ids.iter()
                    .map(|id| format!("@{id}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let grid = compare(
                &server,
                &svc,
                authenticated,
                "read",
                ReadQuery::Ids(ids.into_iter().map(str::to_owned).collect()),
                &body,
                codec,
            )
            .await;
            assert_eq!(grid.len(), if authenticated { 3 } else { 2 });
            assert!(grid.rows.iter().all(|r| r.missing("secret")
                && r.missing("hiddenReference")
                && r.id().unwrap().dis.is_none()));
            for filter in ["not secret", "parentRef->site", "secret"] {
                compare(
                    &server,
                    &svc,
                    authenticated,
                    "read",
                    ReadQuery::Filter(filter.into()),
                    &format!("ver:\"3.0\"\nfilter\n\"{filter}\"\n"),
                    codec,
                )
                .await;
            }
            compare(
                &server,
                &svc,
                authenticated,
                "nav",
                ReadQuery::Nav(Some("z".into())),
                "ver:\"3.0\"\nnavId\n\"z\"\n",
                codec,
            )
            .await;
            compare(
                &server,
                &svc,
                authenticated,
                "defs",
                ReadQuery::Definitions {
                    filter: Some("site".into()),
                },
                "ver:\"3.0\"\nfilter\n\"site\"\n",
                codec,
            )
            .await;
            compare(
                &server,
                &svc,
                authenticated,
                "libs",
                ReadQuery::Libraries,
                "",
                codec,
            )
            .await;
            compare(
                &server,
                &svc,
                authenticated,
                "specs",
                ReadQuery::Specs { library: None },
                "",
                codec,
            )
            .await;
            compare(
                &server,
                &svc,
                authenticated,
                "spec",
                ReadQuery::Spec("visible::Thing".into()),
                "ver:\"3.0\"\nqname\n\"visible::Thing\"\n",
                codec,
            )
            .await;
        }
        let client = haystack_client::ClientConfig::default()
            .build_reqwest_client()
            .unwrap();
        let mut errors = Vec::new();
        for qname in ["hidden::Secret", "missing::Secret"] {
            let mut request = client
                .post(format!("{}/spec", server.url))
                .body(format!("ver:\"3.0\"\nqname\n\"{qname}\"\n"));
            if authenticated {
                request = request.header("Authorization", "BEARER authToken=fixture-token");
            }
            let response = request.send().await.unwrap();
            assert_eq!(response.status(), 404);
            errors.push(response.bytes().await.unwrap());
        }
        assert_eq!(errors[0], errors[1]);
    }
}
struct CountHistory(Arc<AtomicUsize>);
impl HistoryProvider for CountHistory {
    fn his_read(
        &self,
        _: &str,
        _: Option<DateTime<FixedOffset>>,
        _: Option<DateTime<FixedOffset>>,
    ) -> Pin<Box<dyn Future<Output = Vec<HisItem>> + Send + '_>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { vec![] })
    }
    fn his_write(&self, _: &str, _: Vec<HisItem>) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }
}
struct CountAction(Arc<AtomicUsize>);
impl ActionHandler for CountAction {
    fn name(&self) -> &str {
        "action"
    }
    fn invoke(&self, _: &HDict, _: &str, _: &HDict) -> Result<HGrid, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(HGrid::new())
    }
}
#[tokio::test]
async fn scoped_capabilities_disable_every_bypass_before_decode_or_provider_invocation() {
    let (graph, svc, policy_calls) = fixture();
    let providers = Arc::new(AtomicUsize::new(0));
    let mut actions = ActionRegistry::new();
    actions.register(Box::new(CountAction(providers.clone())));
    let before = graph.read(|g| (g.version(), g.catalog_generation()));
    let server = Server::start(
        HaystackServer::new(graph.clone())
            .with_scoped_reads(svc)
            .with_actions(actions)
            .with_history_provider(Box::new(CountHistory(providers.clone()))),
    );
    let client = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    let disabled = [
        "watchSub",
        "watchPoll",
        "watchUnsub",
        "pointWrite",
        "hisRead",
        "hisWrite",
        "invokeAction",
        "import",
        "export",
        "validate",
        "loadLib",
        "unloadLib",
        "exportLib",
        "changes",
    ];
    for endpoint in disabled {
        let response = client
            .post(format!("{}/{endpoint}", server.url))
            .body("not a valid grid")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{endpoint}");
    }
    let response = client
        .get(format!("{}/ws", server.url))
        .header("Connection", "upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert_eq!(providers.load(Ordering::SeqCst), 0);
    assert_eq!(policy_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        before,
        graph.read(|g| (g.version(), g.catalog_generation()))
    );
    let response = client
        .get(format!("{}/ops", server.url))
        .send()
        .await
        .unwrap();
    let grid = decode(&response.bytes().await.unwrap(), H4Codec::Zinc);
    let mut names = grid
        .rows
        .iter()
        .map(|row| match row.get("name") {
            Some(Kind::Str(s)) => s.as_str(),
            _ => panic!("name"),
        })
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            "about", "close", "defs", "formats", "libs", "nav", "ops", "read", "spec", "specs"
        ]
    );
    let response = client
        .get(format!("{}/formats", server.url))
        .send()
        .await
        .unwrap();
    let grid = decode(&response.bytes().await.unwrap(), H4Codec::Zinc);
    assert_eq!(grid.len(), 3);
    assert!(
        grid.rows
            .iter()
            .all(|r| r.get("mime") != Some(&Kind::Str("text/trio".into())))
    );
    let response = client
        .post(format!("{}/read", server.url))
        .header("Accept", "text/trio")
        .body("not a grid")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(policy_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn scoped_custom_routes_require_explicit_external_authority_before_binding() {
    use axum::{Router, routing::get};
    for authenticated in [false, true] {
        let (graph, svc, _) = fixture();
        let before = graph.read(|g| g.catalog_generation());
        let custom = Router::new().route("/external", get(|| async { "external" }));
        let builder = HaystackServer::new(graph.clone()).with_scoped_reads(svc);
        let builder = if authenticated {
            builder.with_authenticated_router(custom)
        } else {
            builder.with_router(custom)
        };
        let result = builder
            .port(0)
            .run_reporting_addr(|_| panic!("must reject before bind"))
            .await;
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(before, graph.read(|g| g.catalog_generation()));
    }
    let (graph, svc, _) = fixture();
    let server = Server::start(
        HaystackServer::new(graph)
            .with_scoped_reads(svc)
            .with_router(Router::new().route("/external", get(|| async { "external" })))
            .with_trusted_external_routes(),
    );
    let response = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap()
        .get(server.url.replace("/api", "/external"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "external");
}

#[tokio::test]
async fn wire_metadata_continuations_can_move_between_http_and_embedding() {
    let (graph, service, _) = fixture();
    let mut extra = HDict::new();
    extra.set("id", Kind::Ref(HRef::from_val("b")));
    extra.set("site", Kind::Marker);
    graph.add(extra).unwrap();
    let server = Server::start(HaystackServer::new(graph).with_scoped_reads(service.clone()));
    let client = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    for codec in [H4Codec::Zinc, H4Codec::Json, H4Codec::JsonV3] {
        let first = client
            .post(format!("{}/read", server.url))
            .header("Accept", codec.mime())
            .body("ver:\"3.0\" limit:1\nfilter\n\"not secret\"\n")
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), 200);
        let first = decode(&first.bytes().await.unwrap(), codec);
        assert_eq!(first.rows[0].id().unwrap().val, "a");
        assert_eq!(first.meta.get("complete"), Some(&Kind::Bool(false)));
        let Some(Kind::Str(cursor)) = first.meta.get("cursor") else {
            panic!("continuation metadata")
        };
        let mut request = ReadRequest::new(
            ReadQuery::Filter("not  secret".into()),
            OutputProfile::H4(codec),
        );
        request.page_size = 1;
        request.cursor = Some(cursor.clone());
        let second = service
            .read(context(Principal::Anonymous), request)
            .await
            .unwrap();
        assert!(!second.complete);
        let ReadOutput::H4 { body, .. } = second.output else {
            panic!()
        };
        assert_eq!(decode(&body, codec).rows[0].id().unwrap().val, "b");
        let third = client
            .post(format!("{}/read", server.url))
            .header("Accept", codec.mime())
            .body(format!(
                "ver:\"3.0\" limit:1 cursor:\"{}\"\nfilter\n\"not secret\"\n",
                second.cursor.unwrap()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(third.status(), 200);
        let third = decode(&third.bytes().await.unwrap(), codec);
        assert_eq!(third.rows[0].id().unwrap().val, "z");
        assert_eq!(third.meta.get("complete"), Some(&Kind::Bool(true)));
        assert!(third.meta.missing("cursor"));
    }
}
