//! Actual HTTP submission, receipt lookup and bounded shared application state.
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    middleware::{self, Next},
    response::Response,
};
use haystack_app::*;
use haystack_client::{HaystackClient, transport::http::HttpTransport};
use haystack_core::{
    codecs::{codec_for, history_mutation as wire},
    data::{HDict, HGrid},
    graph::{EntityGraph, SharedGraph},
    kinds::{HDateTime, HRef, Kind, Number},
};
use haystack_server::HaystackServer;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn graph() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("p")));
    row.set("his", Kind::Marker);
    row.set("kind", Kind::Str("Number".into()));
    row.set("tz", Kind::Str("UTC".into()));
    row.set("unit", Kind::Str("°F".into()));
    graph.add(row).unwrap();
    graph
}
fn sample(second: i64, value: Kind) -> HistorySample {
    HistorySample {
        ts: HDateTime::new(
            chrono::DateTime::from_timestamp(1_717_200_000 + second, 123_456_789)
                .unwrap()
                .fixed_offset(),
            "UTC",
        ),
        val: value,
    }
}
fn request(store: &HisStore, operation: &str) -> HistoryWriteRequest {
    let state = store.state("p").unwrap();
    HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: state.authority,
            point: "p".into(),
            incarnation: state.incarnation,
            operation_id: operation.into(),
        },
        expected_generation: state.generation,
        samples: vec![
            sample(
                0,
                Kind::Number(Number::new(
                    f64::from_bits(0x3fd5555555555555),
                    Some("fahrenheit".into()),
                )),
            ),
            sample(1, Kind::NA),
        ],
    }
}
struct Running {
    url: String,
    owner: ApplicationOwner,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl Running {
    async fn start(
        graph: SharedGraph,
        builder: ApplicationBuilder,
        ack_mode: usize,
        writes: Arc<AtomicUsize>,
    ) -> Self {
        let mut router = HaystackServer::new(graph)
            .with_scoped_reads(builder.handle())
            .into_external_router()
            .unwrap();
        router = router.layer(middleware::from_fn(move |request: Request, next: Next| {
            let writes = writes.clone();
            async move {
                let submission = request.uri().path() == "/api/hisWrite";
                let response = next.run(request).await;
                if !submission {
                    return response;
                }
                writes.fetch_add(1, Ordering::SeqCst);
                if ack_mode == 0 {
                    return response;
                }
                let (mut parts, body) = response.into_parts();
                if ack_mode == 1 {
                    parts
                        .headers
                        .insert("Content-Length", "10000".parse().unwrap());
                    return Response::from_parts(
                        parts,
                        Body::from_stream(futures_util::stream::once(async {
                            Err::<String, _>(std::io::Error::other("injected lost history body"))
                        })),
                    );
                }
                let content_type = parts.headers.get("Content-Type").unwrap().to_str().unwrap();
                let codec = codec_for(content_type).unwrap();
                let bytes = to_bytes(body, wire::MAX_RECEIPT_BYTES).await.unwrap();
                let outcome = wire::decode_outcome(&bytes, codec).unwrap();
                let grid = if ack_mode == 2 {
                    HGrid::new()
                } else {
                    let mut outcome = outcome;
                    if ack_mode == 3
                        && let HistoryWriteOutcome::Committed(receipt) = &mut outcome
                    {
                        receipt.identity.operation_id.push_str("-wrong");
                    }
                    let mut grid = wire::outcome_grid(&outcome).unwrap();
                    grid.meta.set("err", Kind::Marker);
                    grid
                };
                let body = codec.encode_grid(&grid).unwrap();
                parts.headers.remove("Content-Length");
                Response::from_parts(parts, Body::from(body))
            }
        }));
        let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
        owner.ready().await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });
        Self {
            url: format!("http://{address}/api"),
            owner,
            stop,
            task,
        }
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
        let _ = self.stop.send(());
        self.task.await.unwrap();
    }
}
fn setup(
    selected: bool,
) -> (
    SharedGraph,
    ApplicationBuilder,
    HisStore,
    HistoryMutationService,
) {
    let graph = graph();
    let store = HisStore::new();
    let builder =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let history = HistoryService::new(
        builder.handle().read_service(),
        Arc::new(store.clone()),
        HistoryLimits::default(),
    )
    .unwrap();
    let service = HistoryMutationService::new(
        history.clone(),
        Arc::new(AllowAllHistoryMutations),
        HistoryMutationLimits::default(),
    )
    .unwrap();
    let builder = builder.owned_history(history).unwrap();
    let builder = if selected {
        builder.history_mutations(service.clone()).unwrap()
    } else {
        builder
    };
    (graph, builder, store, service)
}
fn client(url: &str, format: &str) -> HaystackClient<HttpTransport> {
    HaystackClient::from_transport(HttpTransport::with_format(url, "".into(), format))
}
fn anonymous() -> ReadContext {
    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(3))
}
fn committed(outcome: HistoryWriteOutcome) -> HistoryWriteReceipt {
    match outcome {
        HistoryWriteOutcome::Committed(receipt) => receipt,
        other => panic!("{other:?}"),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_http_and_embedding_share_exact_samples_receipts_and_capability_selection() {
    for format in ["text/zinc", "application/json;v=3", "application/json"] {
        let (graph, builder, store, service) = setup(true);
        let writes = Arc::new(AtomicUsize::new(0));
        let running = Running::start(graph, builder, 0, writes).await;
        let client = client(&running.url, format);
        let names = client
            .ops()
            .await
            .unwrap()
            .rows
            .into_iter()
            .filter_map(|row| match row.get("name") {
                Some(Kind::Str(value)) => Some(value.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            names.contains(&"hisRead".into())
                && names.contains(&"hisWrite".into())
                && names.contains(&"hisReceipt".into())
        );
        let request = request(&store, "http");
        let receipt = committed(client.his_write_scoped(&request).await.unwrap());
        assert_eq!(receipt.after_generation, 1);
        assert_eq!(store.retained_changes().len(), 1);
        assert_eq!(
            committed(
                service
                    .reconcile(anonymous(), request.identity.clone())
                    .await
                    .unwrap()
            ),
            receipt
        );
        assert_eq!(
            committed(client.reconcile_history(&request.identity).await.unwrap()),
            receipt
        );
        let stored = store.read("p", None, None);
        let Kind::Number(number) = &stored[0].val else {
            panic!()
        };
        assert_eq!(number.val.to_bits(), 0x3fd5555555555555);
        assert_eq!(number.unit.as_deref(), Some("fahrenheit"));
        assert_eq!(stored[0].ts.timestamp_subsec_nanos(), 123_456_789);
        assert_eq!(stored[1].val, Kind::NA);
        let read = client
            .his_read_scoped(&HistoryReadRequest {
                id: "p".into(),
                range: "2024-06-01".into(),
            })
            .await
            .unwrap();
        assert_eq!(read.samples.len(), 2);
        assert_eq!(read.metadata.history.generation, 1);
        let mut row = HDict::new();
        row.set("ts", Kind::DateTime(request.samples[0].ts.clone()));
        row.set("val", Kind::Number(Number::unitless(4.0)));
        assert!(client.his_write("p", vec![row]).await.is_err());
        assert_eq!(store.state("p").unwrap().generation, 1);
        running.close().await;
    }
    let (graph, builder, store, _) = setup(false);
    let running = Running::start(graph, builder, 0, Arc::new(AtomicUsize::new(0))).await;
    let client = client(&running.url, "text/zinc");
    let names = client.ops().await.unwrap().rows;
    assert!(!names.iter().any(|row| matches!(row.get("name"), Some(Kind::Str(value)) if value == "hisWrite" || value == "hisReceipt")));
    assert!(matches!(
        client
            .his_write_scoped(&request(&store, "unselected"))
            .await
            .unwrap(),
        HistoryWriteOutcome::Unknown { .. }
    ));
    assert_eq!(store.receipt_count(), 0);
    running.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_empty_wrong_and_err_marked_http_acknowledgements_never_trigger_replay() {
    for mode in 1..=4 {
        let (graph, builder, store, _) = setup(true);
        let writes = Arc::new(AtomicUsize::new(0));
        let running = Running::start(graph, builder, mode, writes.clone()).await;
        let client = client(&running.url, "application/json");
        let request = request(&store, "ambiguous");
        let outcome = client.his_write_scoped(&request).await.unwrap();
        if mode == 4 {
            committed(outcome);
        } else {
            assert!(
                matches!(outcome, HistoryWriteOutcome::Unknown { identity, .. } if identity == request.identity)
            );
        }
        let receipt = committed(client.reconcile_history(&request.identity).await.unwrap());
        assert_eq!(receipt.after_generation, 1);
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        assert_eq!(store.retained_changes().len(), 1);
        running.close().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn strict_h4_body_validation_and_negotiation_fail_before_any_effect() {
    let (graph, builder, store, _) = setup(true);
    let running = Running::start(graph, builder, 0, Arc::new(AtomicUsize::new(0))).await;
    let http = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    for format in ["text/zinc", "application/json;v=3", "application/json"] {
        let codec = codec_for(format).unwrap();
        let base = request(&store, "invalid");
        for mutation in 0..3 {
            let mut grid = wire::request_grid(&base).unwrap();
            match mutation {
                0 => {
                    grid.rows[1].remove_tag("val");
                }
                1 => {
                    grid.meta.remove_tag("historyWrite");
                }
                _ => {
                    grid.rows[1].set("val", Kind::Null);
                }
            }
            let response = http
                .post(format!("{}/hisWrite", running.url))
                .header("Content-Type", format)
                .header("Accept", format)
                .body(codec.encode_grid(&grid).unwrap())
                .send()
                .await
                .unwrap();
            if response.status().is_success() {
                let outcome =
                    wire::decode_outcome(&response.bytes().await.unwrap(), codec).unwrap();
                assert!(matches!(outcome, HistoryWriteOutcome::Rejected { .. }));
            } else {
                assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
            }
            assert_eq!(store.receipt_count(), 0);
            assert!(store.retained_changes().is_empty());
        }
    }
    let codec = codec_for("application/json").unwrap();
    let body = wire::encode_request(&request(&store, "bad-unit"), codec).unwrap();
    let body = String::from_utf8(body)
        .unwrap()
        .replace("\"unit\":\"fahrenheit\"", "\"unit\":null");
    let response = http
        .post(format!("{}/hisWrite", running.url))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    for (content_type, accept) in [
        ("text/csv", "text/zinc"),
        ("text/zinc", "text/csv"),
        ("application/json;v=5", "text/zinc"),
    ] {
        let response = http
            .post(format!("{}/hisWrite", running.url))
            .header("Content-Type", content_type)
            .header("Accept", accept)
            .body("irrelevant")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    assert_eq!(store.receipt_count(), 0);
    assert_eq!(store.state("p").unwrap().generation, 0);
    running.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unverified_custom_http_client_is_rejected_before_dispatch() {
    let (graph, builder, store, _) = setup(true);
    let writes = Arc::new(AtomicUsize::new(0));
    let running = Running::start(graph, builder, 0, writes.clone()).await;
    let transport = HttpTransport::with_bearer(
        &running.url,
        "".into(),
        haystack_client::ClientConfig::default()
            .build_reqwest_client()
            .unwrap(),
        "text/zinc",
    );
    let client = HaystackClient::from_transport(transport);
    assert!(
        client
            .his_write_scoped(&request(&store, "unsafe-client"))
            .await
            .is_err()
    );
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    running.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn special_number_units_are_preserved_before_http_admission() {
    let (graph, builder, store, _) = setup(true);
    let running = Running::start(graph, builder, 0, Arc::new(AtomicUsize::new(0))).await;
    let http = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    for format in ["text/zinc", "application/json;v=3", "application/json"] {
        let codec = codec_for(format).unwrap();
        let client = client(&running.url, format);
        for unit in ["°F", "fahrenheit"] {
            let mut request = request(&store, &format!("nan-{format}-{unit}"));
            let mut grid = wire::request_grid(&request).unwrap();
            grid.rows[0].set(
                "val",
                Kind::Number(Number::new(f64::NAN, Some(unit.into()))),
            );
            let before = store.state("p").unwrap();
            let receipts = store.receipt_count();
            let response = http
                .post(format!("{}/hisWrite", running.url))
                .header("Content-Type", format)
                .header("Accept", format)
                .body(codec.encode_grid(&grid).unwrap())
                .send()
                .await
                .unwrap();
            assert!(response.status().is_success());
            assert!(matches!(
                wire::decode_outcome(&response.bytes().await.unwrap(), codec).unwrap(),
                HistoryWriteOutcome::Rejected {
                    reason: HistoryWriteRejection::Unsupported,
                    ..
                }
            ));
            assert_eq!(store.state("p").unwrap(), before);
            assert_eq!(store.receipt_count(), receipts);
            for value in [f64::INFINITY, f64::NEG_INFINITY] {
                request = crate::request(&store, &format!("inf-{format}-{unit}-{value}"));
                request.samples[0].val = Kind::Number(Number::new(value, Some(unit.into())));
                committed(client.his_write_scoped(&request).await.unwrap());
                let stored = store.read("p", None, None);
                let Kind::Number(number) = &stored[0].val else {
                    panic!()
                };
                assert_eq!(number.val.to_bits(), value.to_bits());
                assert_eq!(number.unit.as_deref(), Some(unit));
            }
        }
    }
    running.close().await;
}
