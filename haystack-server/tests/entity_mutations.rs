//! Real HTTP requests exercise the shared schema, authority and uncertain reply.
use axum::{
    body::Body,
    extract::Request,
    middleware::{self, Next},
    response::Response,
};
use haystack_app::*;
use haystack_client::{HaystackClient, transport::http::HttpTransport};
use haystack_core::{
    codecs::{
        codec_for,
        entity::{self, *},
    },
    data::HDict,
    graph::{EntityGraph, EntityOperation, PreparedChange, SharedGraph},
    kinds::{HRef, Kind},
};
use haystack_server::{
    HaystackServer,
    auth::{
        AuthManager, AuthUser,
        users::{UserRecord, parse_password_hash},
    },
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Running {
    url: String,
    owner: ApplicationOwner,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl Running {
    async fn start(
        builder: ApplicationBuilder,
        graph: SharedGraph,
        auth: Option<AuthManager>,
        lost: Option<Arc<AtomicUsize>>,
    ) -> Self {
        let mut server = HaystackServer::new(graph).with_scoped_reads(builder.handle());
        if let Some(auth) = auth {
            server = server.with_auth(auth)
        }
        let mut router = server.into_external_router().unwrap();
        if let Some(count) = lost {
            router = router.layer(middleware::from_fn(move |request: Request, next: Next| {
                let count = count.clone();
                async move {
                    let batch = request.uri().path() == "/api/entityBatch";
                    let response = next.run(request).await;
                    if batch {
                        count.fetch_add(1, Ordering::SeqCst);
                        let (mut parts, _) = response.into_parts();
                        parts
                            .headers
                            .insert("Content-Length", "10000".parse().unwrap());
                        Response::from_parts(
                            parts,
                            Body::from_stream(futures_util::stream::once(async {
                                Err::<String, _>(std::io::Error::other(
                                    "injected lost committed body",
                                ))
                            })),
                        )
                    } else {
                        response
                    }
                }
            }));
        }
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
            stop: Some(stop),
            task,
        }
    }
    async fn close(mut self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.await.unwrap();
    }
}
fn row(id: &str) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row
}
fn setup(policy: Arc<dyn MutationPolicy>) -> (SharedGraph, ApplicationBuilder, MutationService) {
    let graph = SharedGraph::new(EntityGraph::new());
    graph.add(row("old")).unwrap();
    let builder =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let service = MutationService::new(
        builder.handle().read_service(),
        EphemeralMutationStore::new(graph.clone()),
        policy,
        MutationLimits::default(),
    )
    .unwrap();
    let builder = builder.entity_mutations(service.clone()).unwrap();
    (graph, builder, service)
}
fn client(url: &str, token: &str) -> HaystackClient<HttpTransport> {
    HaystackClient::from_transport(HttpTransport::new(url, token.into()))
}
fn request(
    service: &MutationService,
    id: &str,
    operations: Vec<EntityOperation>,
) -> EntityBatchRequest {
    let state = service.store().graph().state();
    EntityBatchRequest {
        identity: OperationIdentity {
            operation_id: id.into(),
            dataset: service.store().dataset(),
            incarnation: state.incarnation,
        },
        expected_revision: state.revision,
        operations,
    }
}
fn auth() -> AuthManager {
    let record=UserRecord{credentials:parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap(),permissions:vec!["read".into()]};
    let manager = AuthManager::new(
        HashMap::from([("reader".into(), record)]),
        Duration::from_secs(60),
    );
    for (token, permissions) in [
        ("read-token", vec!["read".into()]),
        ("write-token", vec!["read".into(), "write".into()]),
    ] {
        manager.inject_token(
            token.into(),
            AuthUser {
                username: "person".into(),
                permissions,
            },
        );
    }
    manager
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_discovery_exact_values_batch_receipt_and_complete_unit_pages_share_embedding_state() {
    let (graph, builder, service) = setup(Arc::new(AllowAllMutations));
    let running = Running::start(builder, graph.clone(), None, None).await;
    let client = client(&running.url, "");
    let names: Vec<_> = client
        .ops()
        .await
        .unwrap()
        .rows
        .iter()
        .filter_map(|r| {
            r.get("name").and_then(|v| {
                if let Kind::Str(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            })
        })
        .collect();
    for name in ["entityBatch", "entityReceipt", "changes"] {
        assert!(names.contains(&name.into()));
    }
    for name in ["import", "export", "ws", "hisWrite"] {
        assert!(!names.contains(&name.into()));
    }
    let start = client
        .entity_changes(&ChangesRequest {
            cursor: None,
            max_diffs: 128,
        })
        .await
        .unwrap();
    let mut rich = row("new");
    rich.set("exact", Kind::Int(i64::MAX));
    rich.set("largePosition", Kind::Int((1i64 << 53) + 1));
    let r = request(
        &service,
        "http",
        vec![
            EntityOperation::Remove { id: "old".into() },
            EntityOperation::Add(rich),
        ],
    );
    let outcome = client.submit_entities(&r).await.unwrap();
    let MutationOutcome::Committed(receipt) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(receipt.before_revision, r.expected_revision);
    assert_eq!(
        client.reconcile_entity(&r.identity).await.unwrap(),
        MutationOutcome::Committed(receipt)
    );
    graph.add(row("native")).unwrap();
    assert!(
        client
            .entity_changes(&ChangesRequest {
                cursor: Some(start.cursor.clone()),
                max_diffs: 1
            })
            .await
            .is_err()
    );
    let page = client
        .entity_changes(&ChangesRequest {
            cursor: Some(start.cursor),
            max_diffs: 2,
        })
        .await
        .unwrap();
    assert!(!page.complete);
    assert_eq!(page.changes.len(), 2);
    assert_eq!(
        page.changes[1].changed.get("exact"),
        Some(&Kind::Int(i64::MAX))
    );
    assert_eq!(
        page.changes[1].changed.get("largePosition"),
        Some(&Kind::Int((1i64 << 53) + 1))
    );
    let page = client
        .entity_changes(&ChangesRequest {
            cursor: Some(page.cursor),
            max_diffs: 2,
        })
        .await
        .unwrap();
    assert!(page.complete);
    assert_eq!(page.changes[0].id, "native");
    running.close().await;
}
struct DenyTarget;
impl MutationPolicy for DenyTarget {
    fn authorize_intent(&self, _: &Principal, _: &EntityBatchRequest) -> bool {
        true
    }
    fn authorize_change(&self, _: &Principal, c: &PreparedChange) -> bool {
        c.id != "denied"
    }
    fn authorize_reconcile(
        &self,
        _: &Principal,
        _: &OperationIdentity,
        _: Option<&EntityBatchRequest>,
    ) -> bool {
        true
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coarse_write_permission_mixed_denial_and_malformed_legacy_shapes_have_no_effect() {
    let (graph, builder, service) = setup(Arc::new(DenyTarget));
    let running = Running::start(builder, graph.clone(), Some(auth()), None).await;
    let before = graph.state();
    let req = request(
        &service,
        "denied",
        vec![
            EntityOperation::Add(row("okay")),
            EntityOperation::Add(row("denied")),
        ],
    );
    let codec = codec_for("text/zinc").unwrap();
    let body = entity::encode_grid(&req, codec).unwrap();
    let http = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    let response = http
        .post(format!("{}/entityBatch", running.url))
        .header("Authorization", "BEARER authToken=read-token")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert_eq!(graph.state(), before);
    assert_eq!(service.store().receipt_count(), 0);
    let client = client(&running.url, "write-token");
    assert!(matches!(
        client.submit_entities(&req).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Forbidden,
            ..
        }
    ));
    assert_eq!(graph.state(), before);
    assert_eq!(service.store().receipt_count(), 0);
    for op in ["entityBatch", "entityReceipt", "changes"] {
        let response = http
            .post(format!("{}/{op}", running.url))
            .header("Authorization", "BEARER authToken=write-token")
            .body("ver:\"3.0\"\nsince\n0\n")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    assert_eq!(graph.state(), before);
    running.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_committed_http_response_reconciles_with_exactly_one_mutation_post() {
    let (graph, builder, service) = setup(Arc::new(AllowAllMutations));
    let count = Arc::new(AtomicUsize::new(0));
    let running = Running::start(builder, graph.clone(), None, Some(count.clone())).await;
    let client = client(&running.url, "");
    let r = request(
        &service,
        "lost",
        vec![EntityOperation::Remove { id: "old".into() }],
    );
    let result = client.submit_entities(&r).await.unwrap();
    assert!(matches!(result,MutationOutcome::Unknown{identity,..} if identity==r.identity));
    assert!(graph.read(|g| g.get("old").is_none()));
    assert!(matches!(
        client.reconcile_entity(&r.identity).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    running.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_scoped_profile_does_not_advertise_or_route_mutations() {
    let graph = SharedGraph::new(EntityGraph::new());
    let builder =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let running = Running::start(builder, graph, None, None).await;
    let client = client(&running.url, "");
    let ops = client.ops().await.unwrap();
    assert!(!ops.rows.iter().any(|row|matches!(row.get("name"),Some(Kind::Str(s)) if matches!(s.as_str(),"entityBatch"|"entityReceipt"|"changes"))));
    let http = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    assert_eq!(
        http.post(format!("{}/entityBatch", running.url))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    running.close().await;
}

struct CaptureRevision(std::sync::atomic::AtomicU64);
impl MutationPolicy for CaptureRevision {
    fn authorize_intent(&self, _: &Principal, r: &EntityBatchRequest) -> bool {
        self.0.store(r.expected_revision, Ordering::SeqCst);
        false
    }
    fn authorize_change(&self, _: &Principal, _: &PreparedChange) -> bool {
        false
    }
    fn authorize_reconcile(
        &self,
        _: &Principal,
        _: &OperationIdentity,
        _: Option<&EntityBatchRequest>,
    ) -> bool {
        false
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_http_carries_unsigned_revision_above_float_precision_without_rounding() {
    let policy = Arc::new(CaptureRevision(std::sync::atomic::AtomicU64::new(0)));
    let (graph, builder, service) = setup(policy.clone());
    let running = Running::start(builder, graph, None, None).await;
    let client = client(&running.url, "");
    let mut r = request(
        &service,
        "large-revision",
        vec![EntityOperation::Add(row("new"))],
    );
    r.expected_revision = u64::MAX;
    assert!(matches!(
        client.submit_entities(&r).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Forbidden,
            ..
        }
    ));
    assert_eq!(policy.0.load(Ordering::SeqCst), u64::MAX);
    running.close().await;
}
