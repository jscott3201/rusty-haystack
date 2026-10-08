//! Adapter-level pending streams cannot be expressed by a finite HTTP fixture.
use super::*;
use haystack_app::{
    AllowAll, ApplicationBuilder, ApplicationOwner, ReadLimits, ReadService, ShutdownPolicy,
};
use haystack_core::graph::EntityGraph;
use std::time::Duration;
use tower::ServiceExt;

async fn app(
    duration: Duration,
    bytes: usize,
    queued: usize,
) -> (Router, ReadService, ApplicationOwner) {
    let graph = SharedGraph::new(EntityGraph::new());
    let app = ApplicationBuilder::new(
        graph.clone(),
        Arc::new(AllowAll),
        ReadLimits {
            max_concurrent: 1,
            max_queued: queued,
            max_duration: duration,
            max_input_bytes: bytes,
            ..ReadLimits::default()
        },
    )
    .unwrap()
    .shutdown_policy(ShutdownPolicy {
        drain_timeout: Duration::ZERO,
        stop_timeout: Duration::from_secs(1),
    });
    let service = app.handle().read_service();
    let router = HaystackServer::new(graph)
        .with_scoped_reads(app.handle())
        .into_external_router()
        .unwrap();
    let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    (router, service, owner)
}
fn request(body: Body) -> Request<Body> {
    Request::post("/api/readById?xeto-version=5")
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}
fn pending() -> Request<Body> {
    request(Body::from_stream(futures_util::stream::pending::<
        Result<String, std::io::Error>,
    >()))
}
async fn load(service: &ReadService, admitted: usize, waiting: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while service.load().admitted != admitted || service.load().waiting != waiting {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn error(response: Response, status: StatusCode, spec: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(response.headers()["xeto-version"], "5");
    assert_eq!(response.headers()["content-type"], "application/json");
    let value: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["spec"], format!("sys.api::{spec}"));
}
#[tokio::test]
async fn typed_pending_body_holds_one_slot_and_abort_releases_it() {
    let (router, service, owner) = app(Duration::from_secs(2), 1024, 0).await;
    let task = tokio::spawn(router.clone().oneshot(pending()));
    load(&service, 1, 0).await;
    error(
        router.clone().oneshot(pending()).await.unwrap(),
        StatusCode::SERVICE_UNAVAILABLE,
        "UnavailableErr",
    )
    .await;
    assert_eq!(service.load().admitted, 1);
    task.abort();
    let _ = task.await;
    load(&service, 0, 0).await;
    let response = router
        .oneshot(request(Body::from(r#"{"checked":false}"#)))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    load(&service, 0, 0).await;
    owner.close().await.unwrap();
    owner.terminated().await;
}
#[tokio::test]
async fn typed_queue_is_bounded_and_a_dropped_waiter_does_not_cancel_the_active_body() {
    let (router, service, owner) = app(Duration::from_secs(3), 1024, 1).await;
    let active = tokio::spawn(router.clone().oneshot(pending()));
    load(&service, 1, 0).await;
    let waiting = tokio::spawn(
        router
            .clone()
            .oneshot(request(Body::from(r#"{"checked":false}"#))),
    );
    load(&service, 1, 1).await;
    error(
        router.oneshot(pending()).await.unwrap(),
        StatusCode::SERVICE_UNAVAILABLE,
        "UnavailableErr",
    )
    .await;
    waiting.abort();
    let _ = waiting.await;
    load(&service, 1, 0).await;
    active.abort();
    let _ = active.await;
    load(&service, 0, 0).await;
    owner.close().await.unwrap();
    owner.terminated().await;
}
#[tokio::test]
async fn typed_deadline_and_cumulative_raw_input_ceiling_cover_streams_uri_and_headers() {
    let (router, service, owner) = app(Duration::from_millis(50), 1024, 0).await;
    error(
        router.oneshot(pending()).await.unwrap(),
        StatusCode::GATEWAY_TIMEOUT,
        "TimeoutErr",
    )
    .await;
    load(&service, 0, 0).await;
    owner.close().await.unwrap();
    let (router, service, owner) = app(Duration::from_secs(1), 512, 0).await;
    // Each frame is individually small; the cumulative source ceiling applies.
    let body = Body::from_stream(futures_util::stream::iter(
        (0..20).map(|_| Ok::<_, std::io::Error>("x".repeat(32))),
    ));
    error(
        router.clone().oneshot(request(body)).await.unwrap(),
        StatusCode::BAD_REQUEST,
        "InvalidArgsErr",
    )
    .await;
    error(
        router
            .clone()
            .oneshot(
                Request::get(format!(
                    "/api/readById?xeto-version=5&id={}",
                    "a".repeat(513)
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
        "InvalidArgsErr",
    )
    .await;
    error(
        router
            .oneshot(
                Request::get("/api/readById?xeto-version=5")
                    .header("accept", "x".repeat(513))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
        "InvalidArgsErr",
    )
    .await;
    load(&service, 0, 0).await;
    owner.close().await.unwrap();
    owner.terminated().await;
}
#[tokio::test]
async fn typed_owner_close_stops_pending_body_and_future_admission_with_current_json_error() {
    let (router, service, owner) = app(Duration::from_secs(5), 1024, 0).await;
    let active = tokio::spawn(router.clone().oneshot(pending()));
    load(&service, 1, 0).await;
    owner.close().await.unwrap();
    owner.terminated().await;
    error(
        active.await.unwrap().unwrap(),
        StatusCode::SERVICE_UNAVAILABLE,
        "UnavailableErr",
    )
    .await;
    error(
        router
            .oneshot(request(Body::from(r#"{"checked":false}"#)))
            .await
            .unwrap(),
        StatusCode::SERVICE_UNAVAILABLE,
        "UnavailableErr",
    )
    .await;
    load(&service, 0, 0).await;
}
#[tokio::test]
async fn typed_cors_preflight_allows_version_and_exposes_it_before_auth() {
    let graph = SharedGraph::new(EntityGraph::new());
    let app =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let password_hash = crate::auth::users::hash_password("fixture-password");
    let auth = AuthManager::from_toml_str(&format!(
        "[users.user]\npassword_hash = \"{password_hash}\"\npermissions = [\"read\"]\n"
    ))
    .unwrap();
    assert!(auth.is_enabled());
    let router = HaystackServer::new(graph)
        .with_scoped_reads(app.handle())
        .with_auth(auth)
        .with_cors(CorsPolicy::Allow(vec!["https://ops.example.com".into()]))
        .into_external_router()
        .unwrap();
    let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/readById")
                .header("origin", "https://ops.example.com")
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "xeto-version,content-type,authorization",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(
        response.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("xeto-version")
    );
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "https://ops.example.com"
    );
    let response = router
        .oneshot(
            Request::get("/api/readById?xeto-version=5")
                .header("origin", "https://ops.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert_eq!(
        response.headers()["access-control-expose-headers"],
        "xeto-version"
    );
    owner.close().await.unwrap();
    owner.terminated().await;
}

#[tokio::test]
async fn trusted_custom_routes_keep_their_owned_body_status_and_version_headers() {
    let graph = SharedGraph::new(EntityGraph::new());
    let app =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let raw = Router::new().route(
        "/api/vendorRaw",
        axum::routing::get(|| async { (StatusCode::IM_A_TEAPOT, "owned raw") }),
    );
    let authenticated = Router::new().route(
        "/api/vendorAuth",
        axum::routing::get(|| async { ([("xeto-version", "vendor")], "owned auth") }),
    );
    let router = HaystackServer::new(graph)
        .with_scoped_reads(app.handle())
        .with_trusted_external_routes()
        .with_router(raw)
        .with_authenticated_router(authenticated)
        .into_external_router()
        .unwrap();
    let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    // Routers with no custom fallback keep the built-in default through both
    // merges; their default 404 must not swallow typed dispatch.
    error(
        router
            .clone()
            .oneshot(Request::get("/api/absent").body(Body::empty()).unwrap())
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
        "UnknownFuncErr",
    )
    .await;
    error(
        router
            .clone()
            .oneshot(Request::get("/absent").body(Body::empty()).unwrap())
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
        "InvalidPathErr",
    )
    .await;
    let response = router
        .clone()
        .oneshot(Request::get("/api/vendorRaw").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::IM_A_TEAPOT);
    assert!(response.headers().get("xeto-version").is_none());
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap(),
        "owned raw"
    );
    let response = router
        .oneshot(Request::get("/api/vendorAuth").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["xeto-version"], "vendor");
    assert_eq!(
        axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap(),
        "owned auth"
    );
    owner.close().await.unwrap();
    owner.terminated().await;
}
