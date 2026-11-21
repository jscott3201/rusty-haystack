//! Local HTTP qualification of the explicitly selected command simulator.
use haystack_app::*;
use haystack_core::{
    codecs::jeto,
    data::{HDict, HGrid},
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, Number},
};
use haystack_server::{
    HaystackServer,
    auth::{
        AuthManager, AuthUser,
        users::{UserRecord, parse_password_hash},
    },
};
use std::{collections::HashMap, sync::Arc, time::Duration};
struct Running {
    url: String,
    owner: ApplicationOwner,
    graph: SharedGraph,
    http: reqwest::Client,
    clock: SimulatorClock,
    router: axum::Router,
}
impl Running {
    async fn start() -> Self {
        Self::configured(
            true,
            SimulatedCommandMode::Writable,
            Arc::new(AllowAll),
            Arc::new(AllowAllSimulatedCommands),
        )
        .await
    }
    async fn configured(
        selected: bool,
        mode: SimulatedCommandMode,
        read_policy: Arc<dyn ReadPolicy>,
        command_policy: Arc<dyn CommandPolicy>,
    ) -> Self {
        let graph = SharedGraph::new(EntityGraph::with_changelog_capacity(64));
        let mut point = HDict::new();
        point.set("id", Kind::Ref(HRef::from_val("p")));
        point.set("point", Kind::Marker);
        point.set("writable", Kind::Marker);
        point.set("kind", Kind::Str("Number".into()));
        point.set("unit", Kind::Str("°C".into()));
        point.set(
            "writeDef",
            Kind::Number(Number::new(20.0, Some("°C".into()))),
        );
        point.set("curVal", Kind::Number(Number::new(19.0, Some("°C".into()))));
        graph.add(point).unwrap();
        let builder =
            ApplicationBuilder::new(graph.clone(), read_policy, ReadLimits::default()).unwrap();
        for (id, kind) in [("b", "Bool"), ("s", "Str")] {
            let mut point = HDict::new();
            point.set("id", Kind::Ref(HRef::from_val(id)));
            point.set("point", Kind::Marker);
            point.set("writable", Kind::Marker);
            point.set("kind", Kind::Str(kind.into()));
            graph.add(point).unwrap();
        }
        let clock = SimulatorClock::manual();
        let simulator = SimulatedCommandService::with_clock(
            builder.handle().read_service(),
            command_policy,
            mode,
            CommandLimits::default(),
            clock.clone(),
        )
        .unwrap();
        let builder = if selected {
            builder.simulated_commands(simulator).unwrap()
        } else {
            builder
        };
        let auth = authentication();
        let router = HaystackServer::new(graph.clone())
            .with_scoped_reads(builder.handle())
            .with_auth(authentication())
            .into_external_router()
            .unwrap();
        let listener = HaystackServer::new(graph.clone())
            .with_scoped_reads(builder.handle())
            .with_auth(auth)
            .port(0)
            .into_listener();
        let owner = builder
            .owned_resource(listener)
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let address = owner.ready().await.unwrap().listeners[0].address;
        Self {
            url: format!("http://{address}/api"),
            owner,
            graph,
            clock,
            router,
            http: haystack_client::ClientConfig::default()
                .build_reqwest_client()
                .unwrap(),
        }
    }
    async fn invoke(&self, args: serde_json::Value) -> reqwest::Response {
        self.as_user("operator", args).await
    }
    async fn as_user(&self, user: &str, args: serde_json::Value) -> reqwest::Response {
        self.http
            .post(format!("{}/ph.api::pointWrite", self.url))
            .header("Authorization", format!("BEARER authToken={user}-login"))
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body(args.to_string())
            .send()
            .await
            .unwrap()
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
    }
}
async fn grid(response: reqwest::Response) -> HGrid {
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    let Kind::Grid(grid) = jeto::decode(
        text.as_bytes(),
        &jeto::Context::standard(),
        Some("sys::Grid"),
        jeto::Limits::default(),
    )
    .unwrap() else {
        panic!()
    };
    *grid
}
#[tokio::test]
async fn typed_point_write_inspects_seventeen_slots_and_preserves_graph_value() {
    let r = Running::start().await;
    let array = grid(r.invoke(serde_json::json!({"id":"p"})).await).await;
    assert_eq!(
        array
            .cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["level", "levelDis", "val", "who"]
    );
    assert_eq!(array.rows.len(), 17);
    assert_eq!(
        array.rows[16].get("val"),
        Some(&Kind::Number(Number::new(20.0, Some("°C".into()))))
    );
    assert_eq!(
        r.graph.read(|g| g.get("p").unwrap().get("curVal").cloned()),
        Some(Kind::Number(Number::new(19.0, Some("°C".into()))))
    );
    r.close().await;
}
fn boxed_number(value: &str) -> serde_json::Value {
    serde_json::json!({"spec":"sys::Number","val":value})
}
#[tokio::test]
async fn real_http_set_relinquish_types_and_monotonic_expiry() {
    use serde_json::json;
    let r = Running::start().await;
    assert!(
        grid(
            r.invoke(json!({"id":"p","level":16,"val":boxed_number("22")}))
                .await
        )
        .await
        .rows
        .is_empty()
    );
    assert!(grid(r.invoke(json!({"id":"p","level":8,"val":boxed_number("24°C"),"duration":boxed_number("10s")})).await).await.rows.is_empty());
    let rows = grid(r.invoke(json!({"id":"p","level":null})).await).await;
    assert_eq!(
        rows.rows[7].get("val"),
        Some(&Kind::Number(Number::new(24.0, Some("°C".into()))))
    );
    r.clock.advance(Duration::from_secs(10)).unwrap();
    let rows = grid(r.invoke(json!({"id":"p"})).await).await;
    assert!(rows.rows[7].get("val").is_none());
    assert!(rows.rows[15].get("val").is_some());
    grid(r.invoke(json!({"id":"p","level":16})).await).await;
    let rows = grid(r.invoke(json!({"id":"p"})).await).await;
    assert!(rows.rows[15].get("val").is_none());
    grid(r.invoke(json!({"id":"b","level":1,"val":true})).await).await;
    assert_eq!(
        grid(r.invoke(json!({"id":"b"})).await).await.rows[0].get("val"),
        Some(&Kind::Bool(true))
    );
    grid(
        r.invoke(json!({"id":"s","level":17,"val":"manual text"}))
            .await,
    )
    .await;
    assert_eq!(
        grid(r.invoke(json!({"id":"s"})).await).await.rows[16].get("val"),
        Some(&Kind::Str("manual text".into()))
    );
    r.close().await;
}
#[tokio::test]
async fn strict_http_inputs_and_post_only_inspection() {
    use serde_json::json;
    let r = Running::start().await;
    for args in [
        json!({"id":"p","level":0,"val":boxed_number("2")}),
        json!({"id":"p","level":18}),
        json!({"id":"p","level":8.1}),
        json!({"id":"p","level":boxed_number("8s")}),
        json!({"id":"p","level":8,"val":2}),
        json!({"id":"p","level":8,"val":2.5}),
        json!({"id":"p","level":8,"val":boxed_number("NaN")}),
        json!({"id":"p","level":8,"val":boxed_number("INF")}),
        json!({"id":"p","level":8,"val":boxed_number("2°F")}),
        json!({"id":"p","level":8,"val":boxed_number("2"),"duration":10}),
        json!({"id":"p","level":8,"val":boxed_number("2"),"duration":boxed_number("-1s")}),
        json!({"id":"p","level":8,"val":boxed_number("2"),"duration":boxed_number("2h")}),
        json!({"id":"p","level":7,"val":boxed_number("2"),"duration":boxed_number("1s")}),
        json!({"id":"p","level":8,"duration":boxed_number("1s")}),
        json!({"id":"p","duration":boxed_number("1s")}),
        json!({"id":"p","who":4}),
        json!({"id":"p","unexpected":true}),
    ] {
        let response = r.invoke(args.clone()).await;
        assert_eq!(
            response.status(),
            400,
            "{args}: {}",
            response.text().await.unwrap()
        );
    }
    let response = r
        .http
        .get(format!("{}/ph.api::pointWrite?id=p", r.url))
        .header("Authorization", "BEARER authToken=operator-login")
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 405);
    assert_eq!(response.headers()["allow"], "POST");
    let rows = grid(r.invoke(json!({"id":"p"})).await).await;
    assert!(rows.rows[..16].iter().all(|row| !row.has("val")));
    r.close().await;
}
#[tokio::test]
async fn independent_transport_permissions_absent_and_read_only_selection() {
    use serde_json::json;
    let r = Running::start().await;
    grid(r.as_user("reader", json!({"id":"p"})).await).await;
    assert_eq!(
        r.as_user(
            "reader",
            json!({"id":"p","level":8,"val":boxed_number("25")})
        )
        .await
        .status(),
        403
    );
    grid(
        r.as_user(
            "writer",
            json!({"id":"p","level":8,"val":boxed_number("25")}),
        )
        .await,
    )
    .await;
    assert_eq!(r.as_user("writer", json!({"id":"p"})).await.status(), 403);
    r.close().await;
    let r = Running::configured(
        false,
        SimulatedCommandMode::Writable,
        Arc::new(AllowAll),
        Arc::new(AllowAllSimulatedCommands),
    )
    .await;
    assert_eq!(r.invoke(json!({"id":"p"})).await.status(), 404);
    r.close().await;
    let r = Running::configured(
        true,
        SimulatedCommandMode::InspectOnly,
        Arc::new(AllowAll),
        Arc::new(AllowAllSimulatedCommands),
    )
    .await;
    grid(r.invoke(json!({"id":"p"})).await).await;
    assert_eq!(
        r.invoke(json!({"id":"p","level":8,"val":boxed_number("25")}))
            .await
            .status(),
        501
    );
    r.close().await;
}
#[tokio::test]
async fn v4_v5_qualified_zinc_and_simple_alias_preserve_number_cells_and_complete_input() {
    let r = Running::start().await;
    for (version, operation) in [
        ("4", "ph.api::pointWrite"),
        ("5", "ph.api::pointWrite"),
        ("5", "pointWrite"),
    ] {
        let response = r
            .http
            .post(format!("{}/{operation}", r.url))
            .header("Authorization", "BEARER authToken=operator-login")
            .header("Xeto-Version", version)
            .header("Content-Type", "text/zinc")
            .header("Accept", "text/zinc")
            .body("ver:\"3.0\"\nid,level,val\n@p,8,23°C\n")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    }
    for body in [
        "ver:\"3.0\"\nid,level,val\n@p,8,23°C,999\n",
        "ver:\"3.0\"\nid,level,val\n@p,8,23°C\n@p,1,99°C\n",
        "ver:\"3.0\"\nid,level,val\n@p,8,23°C garbage\n",
    ] {
        let response = r
            .http
            .post(format!("{}/ph.api::pointWrite", r.url))
            .header("Authorization", "BEARER authToken=operator-login")
            .header("Xeto-Version", "5")
            .header("Content-Type", "text/zinc")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{}", response.text().await.unwrap());
    }
    r.close().await;
}
#[derive(Clone, Copy)]
struct Visibility {
    hidden: bool,
    masked: bool,
    read: bool,
}
impl ReadPolicy for Visibility {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(*self))
    }
}
impl PolicySnapshot for Visibility {
    fn scope_key(&self) -> &str {
        "fixed-test-visibility"
    }
    fn function(&self, _: &FunctionIdentity) -> bool {
        true
    }
    fn operation(&self, op: ReadOperation) -> bool {
        op != ReadOperation::Read || self.read
    }
    fn entity(&self, id: &str) -> bool {
        !self.hidden || id != "p"
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        !self.masked || !matches!(tag, "val" | "who" | "levelDis")
    }
    fn reference(&self, _: &str) -> bool {
        true
    }
    fn reference_display(&self, _: &str) -> bool {
        true
    }
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        true
    }
    fn nominal_provenance(&self, _: &haystack_core::kinds::NominalScalar) -> bool {
        true
    }
}
#[derive(Clone, Copy)]
struct CommandRules {
    deny_point: bool,
}
impl CommandPolicy for CommandRules {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn CommandPolicySnapshot>, ReadError> {
        Ok(Arc::new(*self))
    }
}
impl CommandPolicySnapshot for CommandRules {
    fn scope_key(&self) -> &str {
        "fixed-test-command-rules"
    }
    fn action(&self, action: CommandAction) -> bool {
        self.deny_point || action == CommandAction::Inspect
    }
    fn point(&self, id: &str, _: CommandAction) -> bool {
        !self.deny_point || id != "p"
    }
}
#[tokio::test]
async fn action_point_and_disclosure_policies_are_independent_and_hide_values() {
    use serde_json::json;
    let r = Running::configured(
        true,
        SimulatedCommandMode::Writable,
        Arc::new(Visibility {
            hidden: false,
            masked: true,
            read: true,
        }),
        Arc::new(AllowAllSimulatedCommands),
    )
    .await;
    grid(
        r.invoke(
            json!({"id":"p","level":8,"val":boxed_number("23"),"who":"untrusted attribution"}),
        )
        .await,
    )
    .await;
    let rows = grid(r.invoke(json!({"id":"p"})).await).await;
    assert_eq!(
        rows.cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["level", "levelDis", "val", "who"]
    );
    assert!(
        rows.rows
            .iter()
            .all(|r| !r.has("val") && !r.has("who") && !r.has("levelDis"))
    );
    assert!(rows.meta.is_empty());
    r.close().await;
    for denied_command in [true, false] {
        let r = Running::configured(
            true,
            SimulatedCommandMode::Writable,
            Arc::new(Visibility {
                hidden: !denied_command,
                masked: false,
                read: true,
            }),
            if denied_command {
                Arc::new(CommandRules { deny_point: true })
            } else {
                Arc::new(AllowAllSimulatedCommands)
            },
        )
        .await;
        let hidden = r.invoke(json!({"id":"p"})).await;
        let missing = r.invoke(json!({"id":"missing"})).await;
        assert_eq!(hidden.status(), 404);
        assert_eq!(missing.status(), 404);
        assert_eq!(hidden.text().await.unwrap(), missing.text().await.unwrap());
        r.close().await;
    }
    let r = Running::configured(
        true,
        SimulatedCommandMode::Writable,
        Arc::new(AllowAll),
        Arc::new(CommandRules { deny_point: false }),
    )
    .await;
    assert_eq!(
        r.invoke(json!({"id":"p","level":8,"val":boxed_number("23")}))
            .await
            .status(),
        403
    );
    r.close().await;
    let r = Running::configured(
        true,
        SimulatedCommandMode::Writable,
        Arc::new(Visibility {
            hidden: false,
            masked: false,
            read: false,
        }),
        Arc::new(AllowAllSimulatedCommands),
    )
    .await;
    grid(
        r.as_user(
            "writer",
            json!({"id":"p","level":8,"val":boxed_number("23")}),
        )
        .await,
    )
    .await;
    assert_eq!(r.invoke(json!({"id":"p"})).await.status(), 403);
    r.close().await;
}
struct StopAtAcknowledgment {
    snapshots: std::sync::atomic::AtomicUsize,
    entered: parking_lot::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: parking_lot::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl ReadPolicy for StopAtAcknowledgment {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        // Lookup, schema authorization, and final pre-dispatch authorization
        // have completed. The fourth snapshot is the post-encoding disclosure.
        if self
            .snapshots
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            == 3
        {
            if let Some(entered) = self.entered.lock().take() {
                let _ = entered.send(());
            }
            let _ = self.release.lock().recv();
        }
        Ok(Arc::new(AllowAll))
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_http_outer_owner_stop_keeps_post_dispatch_unknown() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let policy = Arc::new(StopAtAcknowledgment {
        snapshots: std::sync::atomic::AtomicUsize::new(0),
        entered: parking_lot::Mutex::new(Some(entered_tx)),
        release: parking_lot::Mutex::new(release_rx),
    });
    let r = Running::configured(
        true,
        SimulatedCommandMode::Writable,
        policy,
        Arc::new(AllowAllSimulatedCommands),
    )
    .await;
    let request = r
        .http
        .post(format!("{}/ph.api::pointWrite", r.url))
        .header("Authorization", "BEARER authToken=operator-login")
        .header("Xeto-Version", "5")
        .header("Content-Type", "application/json")
        .body(serde_json::json!({"id":"p","level":8,"val":boxed_number("23")}).to_string());
    let request = tokio::spawn(async move { request.send().await });
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let owner = r.owner;
    let close = tokio::spawn(async move {
        let report = owner.close().await;
        let termination = owner.terminated().await;
        (report, termination)
    });
    let response = tokio::time::timeout(Duration::from_secs(3), request)
        .await
        .unwrap()
        .unwrap();
    if let Ok(response) = response {
        assert_eq!(response.status(), 504);
        if let Ok(text) = response.text().await {
            assert!(text.contains("Command outcome unknown"), "{text}");
            assert!(text.contains("sys.api::TimeoutErr"));
        }
    }
    // Owned I/O may close before the envelope arrives. Since dispatch was
    // observed, that transport failure is unknown rather than rejection.
    release_tx.send(()).unwrap();
    let (report, termination) = close.await.unwrap();
    assert!(report.unwrap().drain_expired);
    assert!(termination.cleanup_errors.is_empty());
}

fn authentication() -> AuthManager {
    let permissions = [
        ("operator", vec!["read", "write"]),
        ("reader", vec!["read"]),
        ("writer", vec!["write"]),
    ];
    let users = permissions.iter().map(|(name, permissions)| (name.to_string(), UserRecord { credentials: parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap(), permissions: permissions.iter().map(|p| p.to_string()).collect() })).collect::<HashMap<_,_>>();
    let auth = AuthManager::new(users, Duration::from_secs(60));
    for (name, permissions) in permissions {
        auth.inject_token(
            format!("{name}-login"),
            AuthUser {
                username: name.into(),
                permissions: permissions.into_iter().map(String::from).collect(),
            },
        );
    }
    auth
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outer_middleware_returns_unknown_when_transport_remains_available() {
    use tower::ServiceExt;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let policy = Arc::new(StopAtAcknowledgment {
        snapshots: std::sync::atomic::AtomicUsize::new(0),
        entered: parking_lot::Mutex::new(Some(entered_tx)),
        release: parking_lot::Mutex::new(release_rx),
    });
    let r = Running::configured(
        true,
        SimulatedCommandMode::Writable,
        policy,
        Arc::new(AllowAllSimulatedCommands),
    )
    .await;
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/api/ph.api::pointWrite")
        .header("Authorization", "BEARER authToken=operator-login")
        .header("Xeto-Version", "5")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({"id":"p","level":8,"val":boxed_number("23")}).to_string(),
        ))
        .unwrap();
    let router = r.router;
    let request = tokio::spawn(async move { router.oneshot(request).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    let owner = r.owner;
    let close = tokio::spawn(async move {
        let report = owner.close().await;
        let termination = owner.terminated().await;
        (report, termination)
    });
    let response = tokio::time::timeout(Duration::from_secs(3), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), 504);
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    assert!(body.contains("Command outcome unknown"));
    release_tx.send(()).unwrap();
    let (report, termination) = close.await.unwrap();
    assert!(report.unwrap().drain_expired);
    assert!(termination.cleanup_errors.is_empty());
}
#[tokio::test]
async fn hayson_grid_arguments_preserve_number_identity() {
    use haystack_core::{codecs::codec_for, data::HCol};
    let r = Running::start().await;
    let mut args = HGrid::new();
    args.meta.set("ver", Kind::Str("3.0".into()));
    args.cols = ["id", "level", "val"].into_iter().map(HCol::new).collect();
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("p")));
    row.set("level", Kind::Number(Number::unitless(8.0)));
    row.set("val", Kind::Number(Number::unitless(26.0)));
    args.rows.push(row);
    let body = codec_for("application/json")
        .unwrap()
        .encode_grid(&args)
        .unwrap();
    let response = r
        .http
        .post(format!("{}/ph.api::pointWrite", r.url))
        .header("Authorization", "BEARER authToken=operator-login")
        .header("Xeto-Version", "5")
        .header("Content-Type", "application/vnd.haystack+json")
        .header("Accept", "application/vnd.haystack+json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        codec_for("application/json")
            .unwrap()
            .decode_grid(&response.text().await.unwrap())
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        grid(r.invoke(serde_json::json!({"id":"p"})).await)
            .await
            .rows[7]
            .get("val"),
        Some(&Kind::Number(Number::new(26.0, Some("°C".into()))))
    );
    r.close().await;
}
#[tokio::test]
async fn body_poll_after_owner_close_suppresses_inspection_and_write_acknowledgment() {
    use tower::ServiceExt;
    for args in [
        serde_json::json!({"id":"p"}),
        serde_json::json!({"id":"p","level":8,"val":boxed_number("23")}),
    ] {
        let r = Running::start().await;
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/ph.api::pointWrite")
            .header("Authorization", "BEARER authToken=operator-login")
            .header("Xeto-Version", "5")
            .header("Content-Type", "application/json")
            .body(axum::body::Body::from(args.to_string()))
            .unwrap();
        let response = r.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
        r.close().await;
        assert!(
            axum::body::to_bytes(response.into_body(), 65536)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn http_retains_and_masks_last_controller_after_relinquishment_and_expiry() {
    use serde_json::json;
    for masked in [false, true] {
        let r = Running::configured(
            true,
            SimulatedCommandMode::Writable,
            Arc::new(Visibility {
                hidden: false,
                masked,
                read: true,
            }),
            Arc::new(AllowAllSimulatedCommands),
        )
        .await;
        grid(
            r.invoke(json!({"id":"p","level":8,"val":boxed_number("23"),"who":"Alice"}))
                .await,
        )
        .await;
        grid(
            r.invoke(json!({"id":"p","level":8,"val":null,"who":"Bob"}))
                .await,
        )
        .await;
        let relinquished = grid(r.invoke(json!({"id":"p"})).await).await;
        grid(r.invoke(json!({"id":"p","level":8,"val":boxed_number("24"),"who":"Carol","duration":boxed_number("10s")})).await).await;
        r.clock.advance(Duration::from_secs(10)).unwrap();
        let expired = grid(r.invoke(json!({"id":"p"})).await).await;
        r.close().await;
        for (rows, who) in [(relinquished, "Bob"), (expired, "Carol")] {
            assert!(!rows.rows[7].has("val"));
            if masked {
                assert!(!rows.rows[7].has("who"));
            } else {
                assert_eq!(rows.rows[7].get("who"), Some(&Kind::Str(who.into())));
            }
        }
    }
}
