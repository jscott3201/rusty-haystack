use haystack_app::*;
use haystack_core::{
    codecs::entity::*,
    data::HDict,
    graph::{DiffOp, EntityGraph, EntityOperation, PreparedChange, SharedGraph},
    kinds::{HRef, Kind, NominalScalar},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn row(id: &str) -> HDict {
    let mut d = HDict::new();
    d.set("id", Kind::Ref(HRef::from_val(id)));
    d.set("site", Kind::Marker);
    d
}
fn context() -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "owner".into(),
        },
        Duration::from_secs(3),
    )
}
fn request(service: &MutationService, id: &str, ops: Vec<EntityOperation>) -> EntityBatchRequest {
    let state = service.store().graph().state();
    EntityBatchRequest {
        identity: OperationIdentity {
            operation_id: id.into(),
            dataset: service.store().dataset(),
            incarnation: state.incarnation,
        },
        expected_revision: state.revision,
        operations: ops,
    }
}
async fn managed(
    policy: Arc<dyn MutationPolicy>,
    limits: MutationLimits,
    provider: Arc<dyn MutationProvider>,
) -> (ApplicationOwner, MutationService) {
    let graph = SharedGraph::new(EntityGraph::new());
    graph.add(row("a")).unwrap();
    graph.add(row("b")).unwrap();
    let builder =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let service = MutationService::with_provider(
        builder.handle().read_service(),
        EphemeralMutationStore::new(graph),
        policy,
        limits,
        provider,
    )
    .unwrap();
    let early = builder.handle();
    let owner = builder
        .entity_mutations(service.clone())
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    assert!(early.mutation_service().is_some());
    (owner, service)
}
async fn close(owner: ApplicationOwner) {
    owner.close().await.unwrap();
    owner.terminated().await;
}
fn committed(o: MutationOutcome) -> EntityReceipt {
    match o {
        MutationOutcome::Committed(r) => r,
        other => panic!("expected committed: {other:?}"),
    }
}
async fn bootstrap(service: &MutationService) -> ChangesPage {
    service
        .changes(
            context(),
            ChangesRequest {
                cursor: None,
                max_diffs: 128,
            },
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn mixed_batch_is_atomic_and_retry_returns_original_after_removed_entity_and_later_native_writes()
 {
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        Arc::new(EphemeralProvider),
    )
    .await;
    let cursor = bootstrap(&service).await;
    let graph = service.store().graph();
    let mut patch = HDict::new();
    patch.set("label", Kind::Str("changed".into()));
    let r = request(
        &service,
        "mixed",
        vec![
            EntityOperation::Patch {
                id: "a".into(),
                changes: patch,
            },
            EntityOperation::Remove { id: "b".into() },
            EntityOperation::Add(row("c")),
        ],
    );
    let receipt = committed(service.submit(context(), r.clone()).await.unwrap());
    assert_eq!(receipt.after_revision, receipt.before_revision + 3);
    assert_eq!(receipt.qualification, ReceiptQualification::EphemeralMemory);
    graph.read(|g| {
        assert!(g.get("b").is_none());
        assert!(g.get("c").is_some());
        assert_eq!(
            g.get("a").unwrap().get("label"),
            Some(&Kind::Str("changed".into()))
        );
    });
    graph.add(row("native")).unwrap();
    assert_eq!(
        committed(service.submit(context(), r.clone()).await.unwrap()),
        receipt
    );
    let page = service
        .changes(
            context(),
            ChangesRequest {
                cursor: Some(cursor.cursor),
                max_diffs: 3,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.changes.len(), 3);
    assert_eq!(page.position, receipt.after_revision);
    assert!(!page.complete);
    assert!(page.changes.iter().all(|d| Some(d.span) == receipt.span));
    let page2 = service
        .changes(
            context(),
            ChangesRequest {
                cursor: Some(page.cursor),
                max_diffs: 3,
            },
        )
        .await
        .unwrap();
    assert_eq!(page2.changes.len(), 1);
    assert_eq!(page2.changes[0].id, "native");
    assert!(page2.complete);
    let mut different = r.clone();
    different.operations.reverse();
    assert!(matches!(
        service.submit(context(), different).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Conflict,
            ..
        }
    ));
    close(owner).await;
}
struct WritePermission;
impl MutationPolicy for WritePermission {
    fn authorize_intent(&self, p: &Principal, _: &EntityBatchRequest) -> bool {
        matches!(p,Principal::Authenticated{permissions,..} if permissions.iter().any(|p|p=="write"))
    }
    fn authorize_change(&self, _: &Principal, c: &PreparedChange) -> bool {
        c.id != "denied"
    }
    fn authorize_reconcile(
        &self,
        p: &Principal,
        _: &OperationIdentity,
        r: Option<&EntityBatchRequest>,
    ) -> bool {
        r.is_some_and(|r| self.authorize_intent(p, r))
    }
}
fn authenticated(permissions: &[&str]) -> ReadContext {
    ReadContext::with_timeout(
        Principal::authenticated(
            "same-subject",
            permissions.iter().map(|p| p.to_string()).collect(),
        ),
        Duration::from_secs(3),
    )
}
#[tokio::test]
async fn current_authority_is_separate_from_stable_receipt_principal_and_read_access() {
    let (owner, service) = managed(
        Arc::new(WritePermission),
        MutationLimits::default(),
        Arc::new(EphemeralProvider),
    )
    .await;
    let r = request(
        &service,
        "remove",
        vec![EntityOperation::Remove { id: "a".into() }],
    );
    assert!(matches!(
        service.submit(context(), r.clone()).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Forbidden,
            ..
        }
    ));
    assert_eq!(service.store().receipt_count(), 0);
    let receipt = committed(
        service
            .submit(authenticated(&["write"]), r.clone())
            .await
            .unwrap(),
    );
    assert_eq!(
        committed(
            service
                .reconcile(authenticated(&["extra", "write"]), r.identity.clone())
                .await
                .unwrap()
        ),
        receipt
    );
    assert_eq!(
        committed(
            service
                .submit(authenticated(&["extra", "write"]), r.clone())
                .await
                .unwrap()
        ),
        receipt
    );
    assert_eq!(
        service
            .reconcile(authenticated(&["read"]), r.identity.clone())
            .await,
        Err(ReadError::Forbidden)
    );
    assert!(matches!(
        service.submit(authenticated(&["read"]), r).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Forbidden,
            ..
        }
    ));
    close(owner).await;
}
#[tokio::test]
async fn invalid_denied_stale_limit_and_capacity_failures_leave_graph_and_feed_unchanged() {
    let (owner, service) = managed(
        Arc::new(WritePermission),
        MutationLimits {
            receipt_capacity: 1,
            ..MutationLimits::default()
        },
        Arc::new(EphemeralProvider),
    )
    .await;
    let graph = service.store().graph();
    let before = graph.state();
    for (id, ops) in [
        ("invalid", vec![EntityOperation::Add(row("a"))]),
        (
            "denied",
            vec![
                EntityOperation::Add(row("okay")),
                EntityOperation::Add(row("denied")),
            ],
        ),
    ] {
        let r = request(&service, id, ops);
        assert!(matches!(
            service.submit(authenticated(&["write"]), r).await.unwrap(),
            MutationOutcome::Rejected { .. }
        ));
        assert_eq!(graph.state(), before);
        assert_eq!(service.store().receipt_count(), 0);
    }
    let mut stale = request(&service, "stale", vec![EntityOperation::Add(row("new"))]);
    stale.expected_revision -= 1;
    assert!(matches!(
        service
            .submit(authenticated(&["write"]), stale)
            .await
            .unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Conflict,
            ..
        }
    ));
    assert_eq!(service.store().receipt_count(), 0);
    let mut large = row("large");
    large.set("text", Kind::Str("x".repeat(MAX_PAYLOAD_BYTES)));
    assert!(matches!(
        service
            .submit(
                authenticated(&["write"]),
                request(&service, "large", vec![EntityOperation::Add(large)])
            )
            .await
            .unwrap(),
        MutationOutcome::Rejected { .. }
    ));
    assert_eq!(graph.state(), before);
    assert_eq!(service.store().receipt_count(), 0);
    committed(
        service
            .submit(
                authenticated(&["write"]),
                request(
                    &service,
                    "first",
                    vec![EntityOperation::Remove { id: "a".into() }],
                ),
            )
            .await
            .unwrap(),
    );
    let head = graph.state();
    assert!(matches!(
        service
            .submit(
                authenticated(&["write"]),
                request(
                    &service,
                    "second",
                    vec![EntityOperation::Remove { id: "b".into() }]
                )
            )
            .await
            .unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Capacity,
            ..
        }
    ));
    assert_eq!(graph.state(), head);
    assert_eq!(service.store().receipt_count(), 1);
    close(owner).await;
}
struct ProbeProvider {
    mode: u8,
    calls: AtomicUsize,
}
impl MutationProvider for ProbeProvider {
    fn qualification(&self) -> ReceiptQualification {
        ReceiptQualification::ProviderProtocol
    }
    fn commit(&self, p: PreparedMutation) -> MutationOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let identity = p.identity().clone();
        match self.mode {
            0 => p.reject(RejectionReason::Provider),
            1 => {
                let _ = p.publish();
                MutationOutcome::Unknown {
                    identity,
                    cause: UnknownCause::Provider,
                }
            }
            _ => {
                drop(p);
                MutationOutcome::Unknown {
                    identity,
                    cause: UnknownCause::Pending,
                }
            }
        }
    }
}
#[tokio::test]
async fn provider_rejection_unknown_reply_and_abandoned_pending_have_distinct_reconciliation() {
    for mode in 0..3 {
        let provider = Arc::new(ProbeProvider {
            mode,
            calls: AtomicUsize::new(0),
        });
        let (owner, service) = managed(
            Arc::new(AllowAllMutations),
            MutationLimits::default(),
            provider.clone(),
        )
        .await;
        let r = request(
            &service,
            "provider",
            vec![EntityOperation::Remove { id: "a".into() }],
        );
        let before = service.store().graph().state().revision;
        let result = service.submit(context(), r.clone()).await.unwrap();
        let reconciled = service
            .reconcile(context(), r.identity.clone())
            .await
            .unwrap();
        match mode {
            0 => {
                assert!(matches!(
                    result,
                    MutationOutcome::Rejected {
                        reason: RejectionReason::Provider,
                        ..
                    }
                ));
                assert_eq!(result, reconciled);
                assert_eq!(service.store().graph().state().revision, before)
            }
            1 => {
                assert!(matches!(result, MutationOutcome::Unknown { .. }));
                assert_eq!(committed(reconciled).after_revision, before + 1)
            }
            _ => {
                assert!(matches!(
                    reconciled,
                    MutationOutcome::Unknown {
                        cause: UnknownCause::Pending,
                        ..
                    }
                ));
                assert_eq!(service.store().graph().state().revision, before)
            }
        }
        let _ = service.submit(context(), r).await.unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        close(owner).await;
    }
}
#[tokio::test]
async fn service_recreation_reconciles_surviving_store_but_new_store_reports_unknown() {
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        Arc::new(EphemeralProvider),
    )
    .await;
    let r = request(
        &service,
        "survive",
        vec![EntityOperation::Remove { id: "a".into() }],
    );
    let receipt = committed(service.submit(context(), r.clone()).await.unwrap());
    let store = service.store();
    close(owner).await;
    let builder =
        ApplicationBuilder::new(store.graph(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let replacement = MutationService::new(
        builder.handle().read_service(),
        store.clone(),
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
    )
    .unwrap();
    let owner = builder
        .entity_mutations(replacement.clone())
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    assert_eq!(
        committed(
            replacement
                .reconcile(context(), r.identity.clone())
                .await
                .unwrap()
        ),
        receipt
    );
    let fresh = MutationService::new(
        replacement.read_service(),
        EphemeralMutationStore::new(store.graph()),
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        fresh.reconcile(context(), r.identity).await.unwrap(),
        MutationOutcome::Unknown {
            cause: UnknownCause::Missing,
            ..
        }
    ));
    assert_ne!(fresh.store().dataset(), store.dataset());
    close(owner).await;
}
#[tokio::test]
async fn empty_patch_receipt_has_no_diff_equal_patch_does_and_whole_unit_limit_does_not_skip() {
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        Arc::new(EphemeralProvider),
    )
    .await;
    let start = bootstrap(&service).await;
    let empty = request(
        &service,
        "empty",
        vec![EntityOperation::Patch {
            id: "a".into(),
            changes: HDict::new(),
        }],
    );
    let receipt = committed(service.submit(context(), empty).await.unwrap());
    assert_eq!(receipt.before_revision, receipt.after_revision);
    assert_eq!(receipt.span, None);
    let mut equal = HDict::new();
    equal.set("site", Kind::Marker);
    let r = request(
        &service,
        "equal",
        vec![
            EntityOperation::Patch {
                id: "a".into(),
                changes: equal,
            },
            EntityOperation::Remove { id: "b".into() },
        ],
    );
    let receipt = committed(service.submit(context(), r).await.unwrap());
    assert_eq!(receipt.after_revision, receipt.before_revision + 2);
    assert!(matches!(
        service
            .changes(
                context(),
                ChangesRequest {
                    cursor: Some(start.cursor.clone()),
                    max_diffs: 1
                }
            )
            .await,
        Err(ReadError::UnitTooLarge)
    ));
    let page = service
        .changes(
            context(),
            ChangesRequest {
                cursor: Some(start.cursor),
                max_diffs: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.changes.len(), 2);
    assert!(page.complete);
    close(owner).await;
}
struct FilteredReads;
impl ReadPolicy for FilteredReads {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(Self))
    }
}
impl PolicySnapshot for FilteredReads {
    fn function(&self, _: &haystack_app::FunctionIdentity) -> bool {
        true
    }
    fn scope_key(&self) -> &str {
        "filtered-v1"
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, id: &str) -> bool {
        id != "private"
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        tag != "secret"
    }
    fn reference(&self, id: &str) -> bool {
        id != "private"
    }
    fn reference_display(&self, _: &str) -> bool {
        false
    }
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        false
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        false
    }
}
#[tokio::test]
async fn feed_sanitizes_current_preimages_removals_nested_references_and_denied_whole_units() {
    let graph = SharedGraph::new(EntityGraph::new());
    let builder = ApplicationBuilder::new(
        graph.clone(),
        Arc::new(FilteredReads),
        ReadLimits::default(),
    )
    .unwrap();
    let service = MutationService::new(
        builder.handle().read_service(),
        EphemeralMutationStore::new(graph.clone()),
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
    )
    .unwrap();
    let owner = builder
        .entity_mutations(service.clone())
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    let mut initial = row("a");
    initial.set("secret", Kind::Str("hidden".into()));
    initial.set(
        "nested",
        Kind::List(vec![Kind::Ref(HRef::from_val("private"))]),
    );
    initial.set(
        "visibleRef",
        Kind::Ref(HRef::new("target", Some("private display".into()))),
    );
    graph.add(initial).unwrap();
    let start = bootstrap(&service).await;
    graph.remove("a").unwrap();
    graph.add(row("private")).unwrap();
    let page = service
        .changes(
            context(),
            ChangesRequest {
                cursor: Some(start.cursor),
                max_diffs: 2,
            },
        )
        .await
        .unwrap();
    assert!(page.complete);
    assert_eq!(page.position, graph.state().revision);
    assert_eq!(page.changes.len(), 1);
    assert_eq!(page.changes[0].operation, DiffOp::Remove);
    let old = page.changes[0].previous.as_ref().unwrap();
    assert!(old.missing("secret"));
    assert!(old.missing("nested"));
    assert!(matches!(old.get("visibleRef"),Some(Kind::Ref(r)) if r.dis.is_none()));
    close(owner).await;
}
#[tokio::test]
async fn closed_owner_rejects_mutation_and_feed_admission() {
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        Arc::new(EphemeralProvider),
    )
    .await;
    let r = request(
        &service,
        "closed",
        vec![EntityOperation::Remove { id: "a".into() }],
    );
    close(owner).await;
    assert_eq!(service.submit(context(), r).await, Err(ReadError::Closed));
    assert!(matches!(
        service
            .changes(
                context(),
                ChangesRequest {
                    cursor: None,
                    max_diffs: 1
                }
            )
            .await,
        Err(ReadError::Closed)
    ));
}

struct HoldFirst {
    calls: AtomicUsize,
    entered: tokio::sync::Semaphore,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl MutationProvider for HoldFirst {
    fn qualification(&self) -> ReceiptQualification {
        ReceiptQualification::ProviderProtocol
    }
    fn commit(&self, prepared: PreparedMutation) -> MutationOutcome {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.entered.add_permits(1);
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }
        prepared.publish()
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reversed_provider_completion_and_native_writer_use_committed_graph_order_without_feed_holes()
 {
    let (tx, rx) = std::sync::mpsc::channel();
    let provider = Arc::new(HoldFirst {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Semaphore::new(0),
        release: std::sync::Mutex::new(rx),
    });
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        provider.clone(),
    )
    .await;
    let start = bootstrap(&service).await;
    let first = request(
        &service,
        "allocated-first",
        vec![EntityOperation::Add(row("first"))],
    );
    let second = request(
        &service,
        "committed-first",
        vec![EntityOperation::Add(row("second"))],
    );
    let clone = service.clone();
    let task = tokio::spawn(async move { clone.submit(context(), first).await.unwrap() });
    provider.entered.acquire().await.unwrap().forget();
    let receipt = committed(service.submit(context(), second).await.unwrap());
    service.store().graph().add(row("native")).unwrap();
    tx.send(()).unwrap();
    assert!(matches!(
        task.await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Conflict,
            ..
        }
    ));
    let page = service
        .changes(
            context(),
            ChangesRequest {
                cursor: Some(start.cursor),
                max_diffs: 128,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.changes
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>(),
        vec!["second", "native"]
    );
    assert_eq!(page.changes[0].revision, receipt.after_revision);
    assert_eq!(page.changes[1].revision, receipt.after_revision + 1);
    assert!(page.complete);
    assert!(service.store().graph().read(|g| g.get("first").is_none()));
    close(owner).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_readers_observe_only_complete_before_or_after_batch() {
    use std::sync::atomic::AtomicBool;
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        Arc::new(EphemeralProvider),
    )
    .await;
    let graph = service.store().graph();
    let stop = Arc::new(AtomicBool::new(false));
    let saw_before = Arc::new(AtomicBool::new(false));
    let saw_after = Arc::new(AtomicBool::new(false));
    let flags = (stop.clone(), saw_before.clone(), saw_after.clone());
    let reader = std::thread::spawn(move || {
        while !flags.0.load(Ordering::SeqCst) {
            graph.read(|g| {
                let present = (
                    g.get("a").is_some(),
                    g.get("b").is_some(),
                    g.get("c").is_some(),
                );
                match present {
                    (true, true, false) => flags.1.store(true, Ordering::SeqCst),
                    (false, false, true) => flags.2.store(true, Ordering::SeqCst),
                    other => panic!("partial batch: {other:?}"),
                }
            });
        }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !saw_before.load(Ordering::SeqCst) {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    committed(
        service
            .submit(
                context(),
                request(
                    &service,
                    "atomic-read",
                    vec![
                        EntityOperation::Remove { id: "a".into() },
                        EntityOperation::Remove { id: "b".into() },
                        EntityOperation::Add(row("c")),
                    ],
                ),
            )
            .await
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while !saw_after.load(Ordering::SeqCst) {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    stop.store(true, Ordering::SeqCst);
    reader.join().unwrap();
    close(owner).await;
}
#[tokio::test]
async fn feed_cursor_rejects_principal_catalog_and_every_graph_replacement_incarnation() {
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        Arc::new(EphemeralProvider),
    )
    .await;
    let start = bootstrap(&service).await;
    let other = ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "other".into(),
        },
        Duration::from_secs(2),
    );
    assert!(matches!(
        service
            .changes(
                other,
                ChangesRequest {
                    cursor: Some(start.cursor.clone()),
                    max_diffs: 128
                }
            )
            .await,
        Err(ReadError::StaleCursor)
    ));
    let graph = service.store().graph();
    graph.write(|g| g.set_namespace(haystack_core::ontology::DefNamespace::new()));
    assert!(matches!(
        service
            .changes(
                context(),
                ChangesRequest {
                    cursor: Some(start.cursor),
                    max_diffs: 128
                }
            )
            .await,
        Err(ReadError::StaleCursor)
    ));
    for count in [0, 0, 3] {
        let start = bootstrap(&service).await;
        let mut replacement = EntityGraph::new();
        for n in 0..count {
            replacement.add(row(&format!("r{n}"))).unwrap();
        }
        let retired = graph.write(|g| std::mem::replace(g, replacement));
        assert!(matches!(
            service
                .changes(
                    context(),
                    ChangesRequest {
                        cursor: Some(start.cursor.clone()),
                        max_diffs: 128
                    }
                )
                .await,
            Err(ReadError::StaleCursor)
        ));
        graph.write(|g| *g = retired);
        assert!(matches!(
            service
                .changes(
                    context(),
                    ChangesRequest {
                        cursor: Some(start.cursor),
                        max_diffs: 128
                    }
                )
                .await,
            Err(ReadError::StaleCursor)
        ));
    }
    close(owner).await;
}
#[tokio::test]
async fn service_unit_must_fit_feed_byte_limit_before_receipt_or_entity_effect() {
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits {
            page_bytes: 8192,
            ..MutationLimits::default()
        },
        Arc::new(EphemeralProvider),
    )
    .await;
    let graph = service.store().graph();
    let before = graph.state();
    let mut large = row("large");
    large.set("text", Kind::Str("x".repeat(12_000)));
    assert!(matches!(
        service
            .submit(
                context(),
                request(
                    &service,
                    "oversized-unit",
                    vec![EntityOperation::Add(large)]
                )
            )
            .await
            .unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Limit,
            ..
        }
    ));
    assert_eq!(graph.state(), before);
    assert_eq!(service.store().receipt_count(), 0);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_caller_keeps_worker_owned_and_reconciles_guaranteed_precommit_rejection() {
    let (tx, rx) = std::sync::mpsc::channel();
    let provider = Arc::new(HoldFirst {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Semaphore::new(0),
        release: std::sync::Mutex::new(rx),
    });
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        provider.clone(),
    )
    .await;
    let before = service.store().graph().state();
    let r = request(
        &service,
        "cancelled",
        vec![EntityOperation::Remove { id: "a".into() }],
    );
    let identity = r.identity.clone();
    let clone = service.clone();
    let task = tokio::spawn(async move { clone.submit(context(), r).await });
    provider.entered.acquire().await.unwrap().forget();
    task.abort();
    let _ = task.await;
    assert_eq!(service.read_service().load().admitted, 1);
    assert!(owner.handle().outstanding_tasks() > 0);
    tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while service.read_service().load().admitted != 0 {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert_eq!(service.store().graph().state(), before);
    assert!(matches!(
        service.reconcile(context(), identity).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Cancelled,
            ..
        }
    ));
    close(owner).await;
}

struct DeferredProvider(std::sync::Mutex<Option<PreparedMutation>>);
impl MutationProvider for DeferredProvider {
    fn qualification(&self) -> ReceiptQualification {
        ReceiptQualification::ProviderProtocol
    }
    fn commit(&self, prepared: PreparedMutation) -> MutationOutcome {
        let identity = prepared.identity().clone();
        *self.0.lock().unwrap() = Some(prepared);
        MutationOutcome::Unknown {
            identity,
            cause: UnknownCause::Pending,
        }
    }
}
#[tokio::test]
async fn deferred_plan_rechecks_policy_generation_after_provider_returns() {
    let provider = Arc::new(DeferredProvider(std::sync::Mutex::new(None)));
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        provider.clone(),
    )
    .await;
    let graph = service.store().graph();
    let before = graph.state();
    let r = request(
        &service,
        "deferred-policy",
        vec![EntityOperation::Remove { id: "a".into() }],
    );
    assert!(matches!(
        service.submit(context(), r).await.unwrap(),
        MutationOutcome::Unknown {
            cause: UnknownCause::Pending,
            ..
        }
    ));
    service.replace_policy(Arc::new(WritePermission));
    let result = provider.0.lock().unwrap().take().unwrap().publish();
    assert!(
        matches!(
            result,
            MutationOutcome::Rejected {
                reason: RejectionReason::Conflict,
                ..
            }
        ),
        "{result:?}"
    );
    assert_eq!(graph.state(), before);
    close(owner).await;
}
#[tokio::test]
async fn deferred_plan_retains_work_and_admission_and_rejects_after_owner_seal() {
    use futures_util::FutureExt;
    let provider = Arc::new(DeferredProvider(std::sync::Mutex::new(None)));
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        provider.clone(),
    )
    .await;
    let before = service.store().graph().state();
    let r = request(
        &service,
        "deferred-close",
        vec![EntityOperation::Remove { id: "a".into() }],
    );
    assert!(matches!(
        service.submit(context(), r).await.unwrap(),
        MutationOutcome::Unknown { .. }
    ));
    assert_eq!(service.read_service().load().admitted, 1);
    assert!(owner.handle().outstanding_tasks() > 0);
    assert!(owner.close().now_or_never().is_none());
    let result = provider.0.lock().unwrap().take().unwrap().publish();
    assert!(matches!(
        result,
        MutationOutcome::Rejected {
            reason: RejectionReason::Cancelled,
            ..
        }
    ));
    assert_eq!(service.store().graph().state(), before);
    close(owner).await;
}

#[tokio::test]
async fn deferred_plan_can_publish_once_and_reconcile_after_provider_returns() {
    let provider = Arc::new(DeferredProvider(std::sync::Mutex::new(None)));
    let (owner, service) = managed(
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        provider.clone(),
    )
    .await;
    let graph = service.store().graph();
    let before = graph.state();
    let r = request(
        &service,
        "deferred-commit",
        vec![EntityOperation::Remove { id: "a".into() }],
    );
    let identity = r.identity.clone();
    assert!(matches!(
        service.submit(context(), r).await.unwrap(),
        MutationOutcome::Unknown {
            cause: UnknownCause::Pending,
            ..
        }
    ));
    assert_eq!(service.read_service().load().admitted, 1);
    assert_eq!(graph.state(), before);
    let result = provider.0.lock().unwrap().take().unwrap().publish();
    assert!(matches!(result, MutationOutcome::Committed(_)));
    assert_eq!(graph.state().revision, before.revision + 1);
    assert_eq!(service.read_service().load().admitted, 0);
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    assert_eq!(
        service.reconcile(context(), identity).await.unwrap(),
        result
    );
    close(owner).await;
}
