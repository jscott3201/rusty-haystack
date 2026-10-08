//! Owned listener/provider termination versus explicitly borrowed hosting.
use chrono::{DateTime, FixedOffset};
use futures_util::{SinkExt, StreamExt};
use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind},
};
use haystack_server::{
    HaystackServer,
    auth::{AuthManager, AuthUser},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

fn graph() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("a")));
    graph.add(row).unwrap();
    graph
}
fn builder(graph: &SharedGraph) -> ApplicationBuilder {
    ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
        .unwrap()
        .shutdown_policy(ShutdownPolicy {
            drain_timeout: Duration::ZERO,
            stop_timeout: Duration::from_secs(2),
        })
}
fn client() -> reqwest::Client {
    haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap()
}
#[derive(Clone, Copy)]
enum Init {
    Ready,
    Fail,
    Wait,
}
struct Provider {
    init: Init,
    initialized: AtomicUsize,
    held: AtomicUsize,
    closed: AtomicUsize,
    rolled_back: AtomicUsize,
    writes: AtomicUsize,
    entered: Semaphore,
}
impl Provider {
    fn new(init: Init) -> Arc<Self> {
        Arc::new(Self {
            init,
            initialized: AtomicUsize::new(0),
            held: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            rolled_back: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            entered: Semaphore::new(0),
        })
    }
}
impl HistoryProvider for Provider {
    fn initialize(&self) -> ResourceFuture<'_> {
        Box::pin(async move {
            self.held.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            match self.init {
                Init::Ready => {}
                Init::Fail => {
                    return Err(ApplicationError::resource(
                        "history",
                        "failure after acquisition",
                    ));
                }
                Init::Wait => std::future::pending::<()>().await,
            }
            self.initialized.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn rollback_initialize(&self) -> ResourceFuture<'_> {
        Box::pin(async move {
            self.held.fetch_sub(1, Ordering::SeqCst);
            self.rolled_back.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn close(&self) -> ResourceFuture<'_> {
        Box::pin(async move {
            self.held.fetch_sub(1, Ordering::SeqCst);
            self.closed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn open(
        &self,
        _: String,
        _: DateTime<FixedOffset>,
        _: DateTime<FixedOffset>,
        _: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        Box::pin(async { Err(HistoryProviderError::Failed) })
    }
    fn his_write(&self, _: &str, _: Vec<HisItem>) -> HistoryFuture<'_, ()> {
        Box::pin(async move {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
fn with_history(
    builder: ApplicationBuilder,
    provider: Arc<Provider>,
    owned: bool,
) -> ApplicationBuilder {
    let history = HistoryService::new(
        builder.handle().read_service(),
        provider,
        HistoryLimits::default(),
    )
    .unwrap();
    if owned {
        builder.owned_history(history).unwrap()
    } else {
        builder.borrowed_history(history).unwrap()
    }
}

#[tokio::test]
async fn repeated_scoped_owner_releases_listener_and_old_handles_stay_closed() {
    let mut port = 0;
    let mut old_services = vec![];
    for _ in 0..3 {
        let graph = graph();
        let application = builder(&graph);
        let handle = application.handle();
        let server = HaystackServer::new(graph)
            .with_scoped_reads(handle.clone())
            .port(port);
        let owner = application
            .owned_resource(server.into_listener())
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let ready = owner.ready().await.unwrap();
        let address = ready.listeners[0].address;
        port = address.port();
        let response = client()
            .post(format!("http://{address}/api/read"))
            .header("Content-Type", "text/zinc")
            .body("ver:\"3.0\"\nid\n@a\n")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert!(response.text().await.unwrap().contains("@a"));
        assert_eq!(
            handle
                .read_service()
                .read(
                    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(1)),
                    ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed)
                )
                .await
                .unwrap()
                .row_count,
            1
        );
        let (first, second) = tokio::join!(owner.close(), owner.close());
        assert_eq!(first, second);
        first.unwrap();
        assert!(owner.terminated().await.close.is_ok());
        assert_eq!(handle.outstanding_tasks(), 0);
        old_services.push(handle.read_service());
        for service in &old_services {
            assert_eq!(
                service
                    .read(
                        ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(1)),
                        ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed)
                    )
                    .await
                    .unwrap_err(),
                ReadError::Closed
            );
        }
    }
    assert_eq!(tokio::spawn(async { 42 }).await.unwrap(), 42);
}

#[tokio::test]
async fn owned_provider_initializes_and_closes_once_borrowed_provider_remains_usable() {
    for owned in [true, false] {
        let graph = graph();
        let provider = Provider::new(Init::Ready);
        let server = HaystackServer::new(graph.clone()).port(0);
        let owner = with_history(builder(&graph), provider.clone(), owned)
            .owned_resource(server.into_listener())
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        owner.ready().await.unwrap();
        assert_eq!(
            provider.initialized.load(Ordering::SeqCst),
            usize::from(owned)
        );
        owner.close().await.unwrap();
        owner.close().await.unwrap();
        assert!(owner.terminated().await.close.is_ok());
        assert_eq!(provider.closed.load(Ordering::SeqCst), usize::from(owned));
        assert_eq!(provider.rolled_back.load(Ordering::SeqCst), 0);
        assert_eq!(provider.held.load(Ordering::SeqCst), 0);
        if !owned {
            provider.his_write("a", vec![]).await.unwrap();
            assert_eq!(provider.writes.load(Ordering::SeqCst), 1);
        }
    }
}

#[tokio::test]
async fn bind_failure_closes_initialized_owned_provider_before_termination() {
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let graph = graph();
    let provider = Provider::new(Init::Ready);
    let server = HaystackServer::new(graph.clone()).port(occupied.local_addr().unwrap().port());
    let owner = with_history(builder(&graph), provider.clone(), true)
        .owned_resource(server.into_listener())
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    assert!(
        matches!(owner.ready().await,Err(ApplicationError::Resource { resource,.. }) if resource=="http bind")
    );
    assert!(owner.terminated().await.close.is_err());
    assert_eq!(provider.initialized.load(Ordering::SeqCst), 1);
    assert_eq!(provider.closed.load(Ordering::SeqCst), 1);
    assert_eq!(provider.held.load(Ordering::SeqCst), 0);
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    assert!(occupied.local_addr().is_ok());
}

#[tokio::test]
async fn partial_provider_failure_and_cancellation_release_acquisitions() {
    for init in [Init::Fail, Init::Wait] {
        let graph = graph();
        let provider = Provider::new(init);
        let server = HaystackServer::new(graph.clone()).port(0);
        let owner = with_history(builder(&graph), provider.clone(), true)
            .owned_resource(server.into_listener())
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        provider.entered.acquire().await.unwrap().forget();
        if matches!(init, Init::Wait) {
            owner.close().await.unwrap();
        } else {
            assert!(owner.ready().await.is_err());
        }
        let report = owner.terminated().await;
        assert_eq!(report.close.is_ok(), matches!(init, Init::Wait));
        assert_eq!(provider.initialized.load(Ordering::SeqCst), 0);
        assert_eq!(provider.closed.load(Ordering::SeqCst), 0);
        assert_eq!(provider.rolled_back.load(Ordering::SeqCst), 1);
        assert_eq!(provider.held.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn external_router_seals_builtins_without_closing_callers_listener_or_provider() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let graph = graph();
    let application = builder(&graph);
    let provider = Provider::new(Init::Ready);
    let application = with_history(application, provider.clone(), false);
    let router = HaystackServer::new(graph)
        .with_application(application.handle())
        .into_external_router()
        .unwrap();
    let external_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let owner = application
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    let request = || {
        Request::builder()
            .uri("/api/about")
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        router.clone().oneshot(request()).await.unwrap().status(),
        200
    );
    owner.close().await.unwrap();
    assert_eq!(router.oneshot(request()).await.unwrap().status(), 503);
    assert!(external_listener.local_addr().is_ok());
    provider.his_write("a", vec![]).await.unwrap();
    assert_eq!(provider.writes.load(Ordering::SeqCst), 1);
    assert_eq!(provider.initialized.load(Ordering::SeqCst), 0);
    assert_eq!(provider.closed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mismatched_managed_authority_is_refused_before_listener_start() {
    let graph = graph();
    let application = builder(&graph);
    let different = builder(&graph);
    let server = HaystackServer::new(graph)
        .with_scoped_reads(different.handle())
        .port(0);
    let owner = application
        .owned_resource(server.into_listener())
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    assert!(matches!(
        owner.ready().await,
        Err(ApplicationError::Configuration(_))
    ));
    assert!(owner.terminated().await.close.is_err());
    assert_eq!(owner.handle().outstanding_tasks(), 0);
}

#[tokio::test]
async fn legacy_websocket_connection_and_writer_finish_before_provider_close() {
    use tokio_tungstenite::tungstenite::{
        Message, client::IntoClientRequest, protocol::frame::coding::CloseCode,
    };
    let graph = graph();
    let application = builder(&graph);
    let handle = application.handle();
    let provider = Provider::new(Init::Ready);
    let auth = AuthManager::empty();
    auth.inject_token(
        "lifecycle-fixture".into(),
        AuthUser {
            username: "user".into(),
            permissions: vec!["read".into()],
        },
    );
    let server = HaystackServer::new(graph.clone())
        .with_application(handle.clone())
        .with_auth(auth)
        .port(0);
    let owner = with_history(application, provider.clone(), true)
        .owned_resource(server.into_listener())
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    let address = owner.ready().await.unwrap().listeners[0].address;
    let mut request = format!("ws://{address}/api/ws")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "Authorization",
        "BEARER authToken=lifecycle-fixture".parse().unwrap(),
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    ws.send(Message::Text(
        r#"{"reqId":"1","op":"watchSub","ids":["a"]}"#.into(),
    ))
    .await
    .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(response,Message::Text(ref text) if text.contains("watchId")),
        "{response:?}"
    );
    let (closed, frame) = tokio::join!(
        owner.close(),
        tokio::time::timeout(Duration::from_secs(2), ws.next())
    );
    closed.unwrap();
    assert!(
        matches!(frame.unwrap().unwrap().unwrap(),Message::Close(Some(frame)) if frame.code==CloseCode::Away)
    );
    assert!(owner.terminated().await.close.is_ok());
    assert_eq!(handle.outstanding_tasks(), 0);
    assert_eq!(provider.closed.load(Ordering::SeqCst), 1);
    assert_eq!(tokio::spawn(async { 42 }).await.unwrap(), 42);
}

struct DenyReads;
impl ReadPolicy for DenyReads {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Err(ReadError::Forbidden)
    }
}
#[tokio::test]
async fn attaching_lifecycle_preserves_scoped_policy_in_both_configuration_orders() {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    for scoped_first in [true, false] {
        let graph = graph();
        let application =
            ApplicationBuilder::new(graph.clone(), Arc::new(DenyReads), ReadLimits::default())
                .unwrap();
        let handle = application.handle();
        let server = HaystackServer::new(graph);
        let server = if scoped_first {
            server
                .with_scoped_reads(handle.clone())
                .with_application(handle.clone())
        } else {
            server
                .with_application(handle.clone())
                .with_scoped_reads(handle.clone())
        };
        let router = server.into_external_router().unwrap();
        let owner = application
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        owner.ready().await.unwrap();
        let read = Request::post("/api/read")
            .header("Content-Type", "text/zinc")
            .body(Body::from("ver:\"3.0\"\nid\n@a\n"))
            .unwrap();
        assert_eq!(router.clone().oneshot(read).await.unwrap().status(), 403);
        let history = Request::post("/api/hisRead").body(Body::empty()).unwrap();
        assert_eq!(router.oneshot(history).await.unwrap().status(), 404);
        owner.close().await.unwrap();
    }
}

#[test]
fn mismatched_lifecycle_attachment_is_rejected_in_both_configuration_orders() {
    for scoped_first in [true, false] {
        let graph = graph();
        let first = builder(&graph);
        let second = builder(&graph);
        let server = HaystackServer::new(graph.clone());
        let server = if scoped_first {
            server
                .with_scoped_reads(first.handle())
                .with_application(second.handle())
        } else {
            server
                .with_application(first.handle())
                .with_scoped_reads(second.handle())
        };
        let error = match server.into_external_router() {
            Ok(_) => panic!("different service authority must fail before publishing the router"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("exact application service"));
        assert!(graph.read(|graph| graph.namespace_arc().is_none()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_export_peer_does_not_hold_owned_connections_or_provider_open() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // Exceed the platform's ordinary send buffer and explicitly constrain the
    // peer's receive window. Reading only the headers then stalls Hyper's flush.
    const PAYLOAD_BYTES: usize = 32 * 1024 * 1024;
    let graph = graph();
    let mut row = HDict::new();
    row.set("payload", Kind::Str("x".repeat(PAYLOAD_BYTES)));
    graph.update("a", row).unwrap();
    let provider = Provider::new(Init::Ready);
    let application = builder(&graph).shutdown_policy(ShutdownPolicy {
        drain_timeout: Duration::from_millis(200),
        stop_timeout: Duration::from_secs(2),
    });
    let server = HaystackServer::new(graph).port(0);
    let owner = with_history(application, provider.clone(), true)
        .owned_resource(server.into_listener())
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    let address = owner.ready().await.unwrap().listeners[0].address;
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(1024).unwrap();
    assert!(socket.recv_buffer_size().unwrap() <= 64 * 1024);
    let mut peer = socket.connect(address).await.unwrap();
    peer.write_all(b"POST /api/export HTTP/1.1\r\nHost: localhost\r\nAccept: text/zinc\r\nContent-Length: 0\r\n\r\n")
        .await
        .unwrap();
    let headers = tokio::time::timeout(Duration::from_secs(10), async {
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            assert!(headers.len() < 4096, "bounded response headers");
            headers.push(peer.read_u8().await.unwrap());
        }
        String::from_utf8(headers).unwrap()
    })
    .await
    .expect("export response started");
    assert!(headers.starts_with("HTTP/1.1 200"));
    let length = headers
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .expect("export has a known response length");
    assert!(length > PAYLOAD_BYTES);
    let close = owner.close();
    tokio::pin!(close);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut close)
            .await
            .is_err(),
        "a blocked response must retain owned work during graceful drain"
    );
    assert_eq!(provider.closed.load(Ordering::SeqCst), 0);
    assert!(owner.handle().outstanding_tasks() > 0);
    let outcome = tokio::time::timeout(Duration::from_secs(4), &mut close).await;
    let termination = tokio::time::timeout(Duration::from_millis(500), owner.terminated()).await;
    // Capture every receipt while the peer remains open and does not read its
    // response body. Release it only now, also making RED cleanup bounded.
    let closed_before_peer_drop = provider.closed.load(Ordering::SeqCst);
    let outstanding_before_peer_drop = owner.handle().outstanding_tasks();
    drop(peer);
    tokio::time::timeout(Duration::from_secs(2), owner.terminated())
        .await
        .expect("test cleanup after releasing the peer");
    assert!(
        matches!(
            outcome,
            Ok(Ok(CloseReport {
                drain_expired: true
            }))
        ),
        "owned connection I/O did not stop: {outcome:?}"
    );
    assert!(
        termination.is_ok(),
        "termination required the peer to disconnect"
    );
    assert_eq!(closed_before_peer_drop, 1);
    assert_eq!(outstanding_before_peer_drop, 0);
    assert_eq!(provider.held.load(Ordering::SeqCst), 0);
    assert_eq!(tokio::spawn(async { 42 }).await.unwrap(), 42);
}
