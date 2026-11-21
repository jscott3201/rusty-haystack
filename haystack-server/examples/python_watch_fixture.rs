//! Test-only first-party server controlled through a private stdin/stdout pipe.
//! No control routes are installed in the production server.
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{
        Request,
        ws::{Message as AxumMessage, WebSocketUpgrade},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use haystack_app::*;
use haystack_core::{
    codecs::{codec_for, subscription as wire},
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{Float, HRef, Kind, Number},
};
use haystack_server::{
    HaystackServer,
    auth::{
        AuthManager,
        users::{UserRecord, parse_password_hash},
    },
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{BufRead, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use tower::ServiceExt;

#[derive(Default)]
struct Control {
    log: Vec<Value>,
    sessions: Vec<String>, // Never serialized or printed.
    lose_create: usize,
    skip_create: usize,
    idle_create: usize,
    skip_unsub: usize,
    prepare_loss: bool,
    gate_create: bool,
    hold_create: bool,
    reject_ws: bool,
    committed: usize,
    lose_ack: usize,
    hold_poll: bool,
    ws_active: usize,
    deliveries: Vec<Value>,
}
#[derive(Default)]
struct Gate {
    armed: AtomicBool,
    entered: AtomicBool,
    released: Mutex<bool>,
    wake: parking_lot::Condvar,
}
impl Gate {
    fn release(&self) {
        *self.released.lock() = true;
        self.wake.notify_all();
    }
}
struct Policy(Arc<Gate>);
impl ReadPolicy for Policy {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        if self.0.armed.swap(false, Ordering::SeqCst) {
            self.0.entered.store(true, Ordering::SeqCst);
            let mut released = self.0.released.lock();
            let until = std::time::Instant::now() + Duration::from_secs(10);
            while !*released && std::time::Instant::now() < until {
                self.0
                    .wake
                    .wait_for(&mut released, Duration::from_millis(50));
            }
        }
        Ok(Arc::new(Self(self.0.clone())))
    }
}
impl PolicySnapshot for Policy {
    fn scope_key(&self) -> &str {
        "python-watch-fixture-v1"
    }
    fn function(&self, _: &FunctionIdentity) -> bool {
        true
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, id: &str) -> bool {
        id != "denied"
    }
    fn tag(&self, _: &str, _: &str) -> bool {
        true
    }
    fn reference(&self, id: &str) -> bool {
        id != "denied"
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
fn row(id: &str, value: f64) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row.set("point", Kind::Marker);
    row.set("value", Kind::Number(Number::unitless(value)));
    row
}
fn lost(parts: axum::http::response::Parts) -> Response {
    Response::from_parts(
        parts,
        Body::from_stream(futures_util::stream::once(async {
            Err::<String, _>(std::io::Error::other("fixture lost response"))
        })),
    )
}
fn receipt(outcome: &SubscriptionOutcome) -> Option<Value> {
    let SubscriptionOutcome::Delivery(d) = outcome else {
        return None;
    };
    let hex = |id: &[u8; 16]| id.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Some(
        json!({"key":d.watch.key,"authority":hex(&d.watch.authority),"watch_id":hex(&d.watch.watch),"dataset":hex(&d.dataset),"incarnation":hex(&d.incarnation),"catalog_generation":d.catalog_generation,"scope_generation":d.scope_generation,"token":hex(&d.token),"from_revision":d.from,"through":d.through,"initial":d.initial,"removed":d.removed}),
    )
}
// One front-door authority serves real HTTP and relays only scoped WS frames to
// the same first-party router/session. The test relay observes actual backend
// outcomes; it neither implements subscriptions nor synthesizes success replies.
async fn relay(
    upgrade: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    backend: String,
    control: Arc<Mutex<Control>>,
) -> Response {
    if control.lock().reject_ws {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut request = backend.into_client_request().unwrap();
    if let Some(value) = headers.get("authorization") {
        request.headers_mut().insert("authorization", value.clone());
    }
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", wire::WS_PROTOCOL.parse().unwrap());
    let Ok((mut upstream, response)) = tokio_tungstenite::connect_async(request).await else {
        return axum::http::StatusCode::BAD_GATEWAY.into_response();
    };
    assert_eq!(
        response.headers()["Sec-WebSocket-Protocol"],
        wire::WS_PROTOCOL
    );
    let session = {
        let mut c = control.lock();
        let credential = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let index = c.sessions.iter().position(|v| v == credential).unwrap();
        c.ws_active += 1;
        index
    };
    upgrade.protocols([wire::WS_PROTOCOL]).on_upgrade(move |mut downstream| async move {
        while let Some(Ok(message)) = downstream.next().await {
            let AxumMessage::Text(text) = message else { break };
            let frame: Value = serde_json::from_str(text.as_str()).unwrap();
            let request: SubscriptionRequest = wire::decode(frame["payload"].as_str().unwrap().as_bytes()).unwrap();
            {
                let mut c = control.lock();
                if c.log.len() < 512 { c.log.push(json!({"op":request.operation(),"key":request.watch().map(|w| &w.key),"session":session,"request":frame["payload"],"transport":"ws"})); }
            }
            if upstream.send(tungstenite::Message::Text(text.to_string().into())).await.is_err() { break; }
            let Some(Ok(tungstenite::Message::Text(text))) = upstream.next().await else { break };
            let frame: Value = serde_json::from_str(text.as_str()).unwrap();
            let outcome: SubscriptionOutcome = wire::decode(frame["payload"].as_str().unwrap().as_bytes()).unwrap();
            let lose = {
                let mut c = control.lock();
                if c.deliveries.len() < 512 && let Some(receipt) = receipt(&outcome) { c.deliveries.push(receipt); }
                if matches!(request, SubscriptionRequest::Acknowledge { .. }) && c.lose_ack > 0 { c.lose_ack -= 1; true } else { false }
            };
            if lose { break; }
            let mut disconnected = false;
            while matches!(request, SubscriptionRequest::Poll { .. }) && control.lock().hold_poll {
                tokio::select! { message = downstream.next() => { let _ = message; disconnected = true; break; }, _ = tokio::time::sleep(Duration::from_millis(5)) => {} }
            }
            if disconnected || downstream.send(AxumMessage::Text(text.to_string().into())).await.is_err() { break; }
        }
        let _ = upstream.close(None).await;
        let _ = downstream.close().await;
        control.lock().ws_active -= 1;
    }).into_response()
}
// A fixture-owned peer acknowledges an actual prepared delivery, then asks the
// same real router/session for the exact same Create. These auxiliary requests
// use authenticated in-process router calls; the tested client still uses HTTP.
// The returned Idle is the real response, not a synthesized outcome. Both
// auxiliary requests are labeled in the transcript, separate from client traffic.
async fn prepared_idle(
    router: Router,
    control: Arc<Mutex<Control>>,
    credential: axum::http::HeaderValue,
    create: &SubscriptionRequest,
    delivery: &std::sync::Arc<wire::SubscriptionDelivery>,
) -> Response {
    let acknowledgement = SubscriptionRequest::Acknowledge {
        watch: delivery.watch.clone(),
        scope_generation: delivery.scope_generation,
        token: delivery.token,
        through: delivery.through,
    };
    let mut returned = None;
    for request in [&acknowledgement, create] {
        let body = codec_for("text/zinc")
            .unwrap()
            .encode_grid(&wire::to_grid(request).unwrap())
            .unwrap();
        {
            let mut c = control.lock();
            let session = c
                .sessions
                .iter()
                .position(|value| value == credential.to_str().unwrap())
                .unwrap();
            if c.log.len() < 512 {
                c.log.push(json!({"op":request.operation(),"key":delivery.watch.key,"session":session,"fixture":true,"request":String::from_utf8(wire::encode(request).unwrap()).unwrap()}));
            }
        }
        let http_request = Request::builder()
            .method("POST")
            .uri(format!("/api/{}", request.operation()))
            .header("Authorization", credential.clone())
            .header("Content-Type", "text/zinc")
            .header("Accept", "text/zinc")
            .body(Body::from(body))
            .unwrap();
        let response =
            tokio::time::timeout(Duration::from_secs(2), router.clone().oneshot(http_request))
                .await
                .unwrap()
                .unwrap();
        let (parts, body) = response.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let bytes = to_bytes(body, wire::MAX_WIRE_BYTES).await.unwrap();
        assert!(bytes.len() <= wire::MAX_WIRE_BYTES);
        let grid = codec_for(headers.get("content-type").unwrap().to_str().unwrap())
            .unwrap()
            .decode_grid(std::str::from_utf8(&bytes).unwrap())
            .unwrap();
        let outcome: SubscriptionOutcome = wire::from_grid(&grid).unwrap();
        if matches!(request, SubscriptionRequest::Acknowledge { .. }) {
            assert!(matches!(outcome, SubscriptionOutcome::Acknowledged { .. }));
        } else {
            assert!(matches!(outcome, SubscriptionOutcome::Idle { .. }));
            let mut response = Response::builder()
                .status(status)
                .body(Body::from(bytes))
                .unwrap();
            *response.headers_mut() = headers;
            returned = Some(response);
        }
    }
    returned.unwrap()
}
fn print(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let graph = SharedGraph::new(EntityGraph::with_changelog_capacity(4));
    let mut rich = row("a", 1.0);
    rich.set("exact", Kind::Int(i64::MAX));
    rich.set("bits", Kind::Float(Float::from_bits(0x7ff8000000000042)));
    rich.set("buf", Kind::Buf(vec![0, 255]));
    rich.set("unit", Kind::Number(Number::new(21.5, Some("°C".into()))));
    rich.set("nothing", Kind::Null);
    rich.set("absent", Kind::None);
    rich.set("old", Kind::Str("remove me".into()));
    rich.set(
        "nested",
        Kind::List(vec![Kind::Dict(Box::new(row("nested", 7.0)))]),
    );
    graph.add(rich).unwrap();
    graph.add(row("b", 2.0)).unwrap();
    graph.add(row("denied", 99.0)).unwrap();
    // Parent read remains H4-compatible; watch data deliberately includes rich values.
    graph.add(row("parent", 3.0)).unwrap();
    let gate = Arc::new(Gate::default());
    let builder = ApplicationBuilder::new(
        graph.clone(),
        Arc::new(Policy(gate.clone())),
        ReadLimits::default(),
    )
    .unwrap();
    let service = StateSubscriptionService::new(
        builder.handle().read_service(),
        EntityMutationStore::new(graph.clone()),
        SubscriptionLimits {
            max_view_bytes: 4096,
            max_delivery_bytes: 8192,
            maintenance_interval: Duration::from_millis(10),
            ..SubscriptionLimits::default()
        },
    )
    .unwrap();
    let builder = builder.state_subscriptions(service.clone()).unwrap();
    let credentials = parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap();
    let auth = AuthManager::new(
        HashMap::from([(
            "user".into(),
            UserRecord {
                credentials,
                permissions: vec!["read".into()],
            },
        )]),
        Duration::from_secs(60),
    );
    let control = Arc::new(Mutex::new(Control::default()));
    let observed = control.clone();
    let middleware_gate = gate.clone();
    let owned_requests = Arc::new(Mutex::new(Vec::new()));
    let middleware_requests = owned_requests.clone();
    let native_router = HaystackServer::new(graph.clone())
        .with_scoped_reads(builder.handle())
        .with_auth(auth)
        .into_external_router()
        .unwrap();
    let idle_router = native_router.clone();
    let router = native_router.layer(middleware::from_fn(move |request: Request, next: Next| {
            let observed = observed.clone();
            let gate = middleware_gate.clone();
            let owned_requests = middleware_requests.clone();
            let idle_router = idle_router.clone();
            async move {
                let (parts, body) = request.into_parts();
                let op = parts.uri.path().rsplit('/').next().unwrap().to_owned();
                if op == "ws" && observed.lock().reject_ws { return Response::builder().status(503).body(Body::empty()).unwrap(); }
                let bytes = to_bytes(body, wire::MAX_WIRE_BYTES).await.unwrap();
                let codec = codec_for(parts.headers.get("content-type").and_then(|x| x.to_str().ok()).unwrap_or("text/zinc"));
                let request = codec.and_then(|codec| codec.decode_grid(std::str::from_utf8(&bytes).ok()?).ok()).and_then(|grid| wire::from_grid::<SubscriptionRequest>(&grid).ok());
                let create = matches!(request, Some(SubscriptionRequest::Create(_)));
                let skip = {
                    let mut c = observed.lock();
                    let session = parts.headers.get("authorization").and_then(|x| x.to_str().ok()).unwrap_or("").to_owned();
                    let session_index = if let Some(index) = c.sessions.iter().position(|x| x == &session) { index } else { let index = c.sessions.len(); c.sessions.push(session); index };
                    let key = match &request { Some(SubscriptionRequest::Create(x)) => Some(x.key.clone()), Some(x) => x.watch().map(|x| x.key.clone()), _ => None };
                    if c.log.len() < 512 { c.log.push(json!({"op":op,"key":key,"session":session_index,"fixture":parts.headers.contains_key("x-watch-fixture"),"request":request.as_ref().map(|x| String::from_utf8(wire::encode(x).unwrap()).unwrap())})); }
                    if op == "watchUnsub" && c.skip_unsub > 0 { c.skip_unsub -= 1; true } else if create && c.skip_create > 0 { c.skip_create -= 1; true } else { false }
                };
                if skip { return Response::builder().status(502).body(Body::empty()).unwrap(); }
                let prepare_loss = {
                    let mut c = observed.lock();
                    if create && (c.gate_create || c.prepare_loss) {
                        *gate.released.lock() = false; gate.entered.store(false, Ordering::SeqCst); gate.armed.store(true, Ordering::SeqCst); c.gate_create = false;
                    }
                    if create && c.prepare_loss { c.prepare_loss = false; true } else { false }
                };
                if prepare_loss {
                    let task = tokio::spawn(async move {
                        let response = next.run(Request::from_parts(parts, Body::from(bytes))).await;
                        let _ = to_bytes(response.into_body(), wire::MAX_WIRE_BYTES).await;
                    });
                    owned_requests.lock().push(task);
                    while !gate.entered.load(Ordering::SeqCst) { tokio::time::sleep(Duration::from_millis(5)).await; }
                    return Response::builder().status(502).body(Body::empty()).unwrap();
                }
                let idle_seed = {
                    let mut c = observed.lock();
                    if create && c.idle_create > 0 {
                        c.idle_create -= 1;
                        Some((parts.headers.get("authorization").unwrap().clone(), request.clone().unwrap()))
                    } else { None }
                };
                let response = next.run(Request::from_parts(parts, Body::from(bytes))).await;
                if !create { return response; }
                let (mut parts, body) = response.into_parts();
                let bytes = to_bytes(body, wire::MAX_WIRE_BYTES).await.unwrap();
                // A real, fully decoded Delivery is the commit receipt; registry
                // counts alone also include Preparing and are insufficient.
                let outcome = codec_for(parts.headers.get("content-type").and_then(|x| x.to_str().ok()).unwrap_or("text/zinc"))
                    .and_then(|codec| codec.decode_grid(std::str::from_utf8(&bytes).ok()?).ok())
                    .and_then(|grid| wire::from_grid::<SubscriptionOutcome>(&grid).ok());
                if let Some(outcome) = &outcome {
                    let mut c = observed.lock();
                    if matches!(outcome, SubscriptionOutcome::Delivery(_)) { c.committed += 1; }
                    if c.deliveries.len() < 512 && let Some(receipt) = receipt(outcome) { c.deliveries.push(receipt); }
                }
                if let Some((credential, create)) = idle_seed {
                    let Some(SubscriptionOutcome::Delivery(delivery)) = &outcome else { panic!("Idle seed requires prepared first-party delivery"); };
                    return prepared_idle(idle_router, observed.clone(), credential, &create, delivery).await;
                }
                while observed.lock().hold_create { tokio::time::sleep(Duration::from_millis(5)).await; }
                let lose = { let mut c = observed.lock(); if c.lose_create > 0 { c.lose_create -= 1; true } else { false } };
                if lose { parts.headers.insert("Content-Length", "100000".parse().unwrap()); lost(parts) } else { Response::from_parts(parts, Body::from(bytes)) }
            }
        }));
    let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_address = backend_listener.local_addr().unwrap();
    let (backend_stop, backend_stopped) = tokio::sync::oneshot::channel();
    let backend_router = router.clone();
    let backend_task = tokio::spawn(async move {
        axum::serve(backend_listener, backend_router)
            .with_graceful_shutdown(async {
                let _ = backend_stopped.await;
            })
            .await
            .unwrap();
    });
    let relay_control = control.clone();
    let backend_url = format!("ws://{backend_address}/api/ws");
    let router = if std::env::args().any(|arg| arg == "--direct") {
        router
    } else {
        Router::new()
            .route(
                "/api/ws",
                get(
                    move |upgrade: WebSocketUpgrade, headers: axum::http::HeaderMap| {
                        relay(upgrade, headers, backend_url.clone(), relay_control.clone())
                    },
                ),
            )
            .fallback_service(router)
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    print(json!({"url":format!("http://{address}/api")}));
    for line in std::io::stdin().lock().lines() {
        let command: Value = serde_json::from_str(&line.unwrap()).unwrap();
        match command["op"].as_str().unwrap() {
            "set" => {
                let mut c = control.lock();
                if let Some(n) = command["skip_unsub"].as_u64() {
                    c.skip_unsub = n as usize;
                }
                if let Some(b) = command["prepare_loss"].as_bool() {
                    c.prepare_loss = b;
                }
                if let Some(n) = command["lose_ack"].as_u64() {
                    c.lose_ack = n as usize;
                }
                if let Some(b) = command["hold_poll"].as_bool() {
                    c.hold_poll = b;
                }
                if let Some(n) = command["lose_create"].as_u64() {
                    c.lose_create = n as usize;
                }
                if let Some(n) = command["skip_create"].as_u64() {
                    c.skip_create = n as usize;
                }
                if let Some(n) = command["idle_create"].as_u64() {
                    c.idle_create = n as usize;
                }
                if let Some(b) = command["hold_create"].as_bool() {
                    c.hold_create = b;
                }
                if let Some(b) = command["reject_ws"].as_bool() {
                    c.reject_ws = b;
                }
            }
            "gate" => {
                *gate.released.lock() = false;
                gate.entered.store(false, Ordering::SeqCst);
                control.lock().gate_create = true;
            }
            "release" => gate.release(),
            "update" => {
                graph
                    .update(
                        command["id"].as_str().unwrap(),
                        row(
                            command["id"].as_str().unwrap(),
                            command["value"].as_f64().unwrap(),
                        ),
                    )
                    .unwrap();
            }
            "remove_tag" => {
                let mut changes = HDict::new();
                changes.set(command["tag"].as_str().unwrap(), Kind::Remove);
                graph
                    .update(command["id"].as_str().unwrap(), changes)
                    .unwrap();
            }
            "remove" => {
                graph.remove(command["id"].as_str().unwrap()).unwrap();
            }
            "overflow" => {
                let mut changes = HDict::new();
                changes.set("huge", Kind::Str("x".repeat(20_000)));
                graph.update("a", changes).unwrap();
            }
            "stats" => {}
            "stop" => break,
            _ => panic!("unknown fixture command"),
        }
        let c = control.lock();
        print(
            json!({"active":service.active_watches(),"bindings":service.binding_count(),"admitted":service.read_service().load().admitted,"committed":c.committed,"gate_entered":gate.entered.load(Ordering::SeqCst),"ws_active":c.ws_active,"deliveries":c.deliveries,"log":c.log}),
        );
    }
    gate.release();
    control.lock().hold_create = false;
    let requests = std::mem::take(&mut *owned_requests.lock());
    for request in requests {
        request.await.unwrap();
    }
    owner.close().await.unwrap();
    let _ = stop.send(());
    task.await.unwrap();
    let _ = backend_stop.send(());
    backend_task.await.unwrap();
    let backend_rebound = tokio::net::TcpListener::bind(backend_address)
        .await
        .unwrap();
    drop(backend_rebound);
    assert_eq!(service.active_watches(), 0);
    assert_eq!(service.binding_count(), 0);
    let rebound = tokio::net::TcpListener::bind(address).await.unwrap();
    drop(rebound);
    print(json!({"stopped":true,"active":0,"bindings":0,"listener_reused":true}));
}
