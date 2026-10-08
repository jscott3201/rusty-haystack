use super::*;
use haystack_app::{AllowAll, ReadLimits, ReadService};
use haystack_core::graph::EntityGraph;
use std::time::Duration;
use tower::ServiceExt;

fn app(duration: Duration, bytes: usize) -> (Router, ReadService) {
    let graph = SharedGraph::new(EntityGraph::new());
    let service = ReadService::new(
        graph.clone(),
        Arc::new(AllowAll),
        ReadLimits {
            max_concurrent: 1,
            max_queued: 0,
            max_duration: duration,
            max_input_bytes: bytes,
            ..ReadLimits::default()
        },
    )
    .unwrap();
    (
        HaystackServer::new(graph)
            .with_scoped_reads(service.clone())
            .build_router()
            .unwrap(),
        service,
    )
}
fn pending_request() -> Request<Body> {
    let body = Body::from_stream(futures_util::stream::pending::<
        Result<String, std::io::Error>,
    >());
    Request::post("/api/read").body(body).unwrap()
}
async fn wait_active(service: &ReadService) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while service.load().admitted == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn wait_idle(service: &ReadService) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while service.load().admitted != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn body_collection_is_admitted_and_abort_releases_its_single_slot() {
    let (router, service) = app(Duration::from_secs(2), 1024);
    let first = tokio::spawn(router.clone().oneshot(pending_request()));
    wait_active(&service).await;
    let response = router.clone().oneshot(pending_request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(service.load().admitted, 1);
    first.abort();
    let _ = first.await;
    wait_idle(&service).await;
    // A one-slot service completes body -> decode -> read without acquiring a
    // second permit, which would deadlock or report capacity here.
    let request = Request::post("/api/read")
        .body(Body::from("ver:\"3.0\"\nfilter\n\"site\"\n"))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    wait_idle(&service).await;
}
#[tokio::test]
async fn deadline_and_input_ceiling_apply_while_collecting_body() {
    let (router, service) = app(Duration::from_millis(50), 1024);
    let response = tokio::time::timeout(Duration::from_secs(1), router.oneshot(pending_request()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    wait_idle(&service).await;
    let (router, service) = app(Duration::from_secs(2), 16);
    let response = router
        .oneshot(
            Request::post("/api/read")
                .body(Body::from("x".repeat(17)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    wait_idle(&service).await;
}
