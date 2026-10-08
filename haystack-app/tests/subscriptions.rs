use haystack_app::*;
use haystack_core::{
    codecs::subscription as wire,
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, Number},
};
use std::{sync::Arc, time::Duration};
fn row(id: &str, value: f64) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row.set("value", Kind::Number(Number::unitless(value)));
    row
}
fn graph() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    graph.add(row("a", 1.0)).unwrap();
    graph.add(row("b", 2.0)).unwrap();
    graph
}
fn service(graph: SharedGraph) -> StateSubscriptionService {
    StateSubscriptionService::new(
        ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap(),
        EphemeralMutationStore::new(graph),
        SubscriptionLimits::default(),
    )
    .unwrap()
}
fn session() -> SubscriptionSession {
    SubscriptionSession::trusted("owner", Duration::from_secs(10)).unwrap()
}
fn create(service: &StateSubscriptionService, key: &str, ids: &[&str]) -> SubscriptionRequest {
    SubscriptionRequest::Create(SubscriptionCreate {
        authority: service.authority(),
        key: key.into(),
        ids: ids.iter().map(|id| (*id).into()).collect(),
        lease_ms: 5000,
    })
}
async fn execute(
    service: &StateSubscriptionService,
    session: &SubscriptionSession,
    request: SubscriptionRequest,
) -> SubscriptionOutcome {
    service
        .execute(
            ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(3)),
            session.clone(),
            request,
        )
        .await
        .unwrap()
}
fn delivery(outcome: SubscriptionOutcome) -> Arc<SubscriptionDelivery> {
    match outcome {
        SubscriptionOutcome::Delivery(delivery) => delivery,
        other => panic!("expected delivery: {other:?}"),
    }
}
fn ack(delivery: &SubscriptionDelivery) -> SubscriptionRequest {
    SubscriptionRequest::Acknowledge {
        watch: delivery.watch.clone(),
        scope_generation: delivery.scope_generation,
        token: delivery.token,
        through: delivery.through,
    }
}
#[tokio::test]
async fn initial_create_retry_and_delivery_replay_are_fenced_until_explicit_ack() {
    let graph = graph();
    let service = service(graph.clone());
    let session = session();
    let request = create(&service, "known-create", &["a"]);
    let first = delivery(execute(&service, &session, request.clone()).await);
    assert!(first.initial);
    assert_eq!(first.from, graph.version());
    assert_eq!(first.through, first.from);
    assert_eq!(first.rows.len(), 1);
    graph.update("a", row("a", 9.0)).unwrap();
    let replay = delivery(execute(&service, &session, request).await);
    assert_eq!(
        wire::encode(&SubscriptionOutcome::Delivery(first.clone())).unwrap(),
        wire::encode(&SubscriptionOutcome::Delivery(replay)).unwrap()
    );
    assert_eq!(service.active_watches(), 1);
    assert_eq!(service.binding_count(), 1);
    assert!(matches!(
        execute(&service, &session, ack(&first)).await,
        SubscriptionOutcome::Acknowledged { .. }
    ));
    let changed = delivery(
        execute(
            &service,
            &session,
            SubscriptionRequest::Resume {
                watch: first.watch.clone(),
                scope_generation: 1,
                acknowledged: first.through,
            },
        )
        .await,
    );
    assert!(!changed.initial);
    assert_eq!(changed.from, first.through);
    assert_eq!(changed.through, graph.version());
    assert_eq!(
        changed.rows[0].get("value"),
        Some(&Kind::Number(Number::unitless(9.0)))
    );
    // A duplicate old ACK cannot consume a newer prepared delivery.
    execute(&service, &session, ack(&first)).await;
    let retry = delivery(
        execute(
            &service,
            &session,
            SubscriptionRequest::Poll {
                watch: first.watch.clone(),
            },
        )
        .await,
    );
    assert_eq!(retry.token, changed.token);
    execute(&service, &session, ack(&changed)).await;
    assert!(
        matches!(execute(&service,&session,SubscriptionRequest::Poll{watch:first.watch.clone()}).await,SubscriptionOutcome::Idle{acknowledged,..} if acknowledged==changed.through)
    );
    assert_eq!(service.read_service().load().admitted, 0);
}
#[tokio::test]
async fn caller_key_original_intent_sessions_and_new_authority_do_not_alias() {
    let graph = graph();
    let service = service(graph.clone());
    let session = session();
    let request = create(&service, "create-key", &["b", "a", "a"]);
    let first = delivery(execute(&service, &session, request.clone()).await);
    assert_eq!(first.rows.len(), 2);
    assert_eq!(
        execute(
            &service,
            &session,
            create(&service, "create-key", &["a", "b"])
        )
        .await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Conflict)
    );
    let other = SubscriptionSession::trusted("owner", Duration::from_secs(10)).unwrap();
    assert_eq!(
        execute(
            &service,
            &other,
            SubscriptionRequest::Poll {
                watch: first.watch.clone()
            }
        )
        .await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Forbidden)
    );
    let distinct = delivery(execute(&service, &other, request.clone()).await);
    assert_ne!(first.watch, distinct.watch);
    let replacement = StateSubscriptionService::new(
        service.read_service().clone(),
        service.entity_store(),
        SubscriptionLimits::default(),
    )
    .unwrap();
    assert_eq!(
        execute(&replacement, &session, request).await,
        SubscriptionOutcome::Resync(SubscriptionResync::Authority)
    );
    assert_eq!(replacement.binding_count(), 0);
    session.close();
    assert_eq!(
        execute(
            &service,
            &session,
            SubscriptionRequest::Poll {
                watch: first.watch.clone()
            }
        )
        .await,
        SubscriptionOutcome::Resync(SubscriptionResync::Revoked)
    );
}
#[tokio::test]
async fn membership_replacement_invalidates_old_ack_and_captures_new_initial() {
    let graph = graph();
    let service = service(graph.clone());
    let session = session();
    let original = create(&service, "scope", &["a"]);
    let first = delivery(execute(&service, &session, original.clone()).await);
    let second = delivery(
        execute(
            &service,
            &session,
            SubscriptionRequest::Replace {
                watch: first.watch.clone(),
                expected_scope_generation: 1,
                ids: vec!["b".into()],
            },
        )
        .await,
    );
    assert_eq!(second.scope_generation, 2);
    assert!(second.initial);
    assert_eq!(
        second.rows[0].get("id"),
        Some(&Kind::Ref(HRef::from_val("b")))
    );
    assert_eq!(
        execute(&service, &session, ack(&first)).await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Conflict)
    );
    assert_eq!(
        delivery(execute(&service, &session, original).await).token,
        second.token
    );
    let mut future = ack(&second);
    if let SubscriptionRequest::Acknowledge { through, .. } = &mut future {
        *through += 1;
    }
    assert_eq!(
        execute(&service, &session, future).await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Conflict)
    );
    execute(&service, &session, ack(&second)).await;
    graph.remove("b").unwrap();
    let removed = delivery(
        execute(
            &service,
            &session,
            SubscriptionRequest::Poll {
                watch: first.watch.clone(),
            },
        )
        .await,
    );
    assert_eq!(removed.removed, vec!["b"]);
    assert!(removed.rows.is_empty());
    let close = SubscriptionRequest::Unsubscribe {
        watch: first.watch.clone(),
    };
    assert_eq!(
        execute(&service, &session, close.clone()).await,
        execute(&service, &session, close).await
    );
    assert_eq!(service.active_watches(), 0);
    assert_eq!(service.binding_count(), 1);
}
#[tokio::test]
async fn retention_catalog_restore_and_lease_expiry_return_explicit_resync() {
    for reason in [
        SubscriptionResync::Gap,
        SubscriptionResync::Catalog,
        SubscriptionResync::Incarnation,
        SubscriptionResync::LeaseExpired,
    ] {
        let graph = SharedGraph::new(EntityGraph::with_changelog_capacity(2));
        graph.add(row("a", 1.0)).unwrap();
        let service = service(graph.clone());
        let session = session();
        let mut request = create(&service, "reason", &["a"]);
        if reason == SubscriptionResync::LeaseExpired
            && let SubscriptionRequest::Create(create) = &mut request
        {
            create.lease_ms = 20;
        }
        let initial = delivery(execute(&service, &session, request.clone()).await);
        execute(&service, &session, ack(&initial)).await;
        match reason {
            SubscriptionResync::Gap => {
                for value in 0..4 {
                    graph.update("a", row("a", value as f64)).unwrap();
                }
            }
            SubscriptionResync::Catalog => graph
                .write(|graph| graph.set_namespace(haystack_core::ontology::DefNamespace::new())),
            SubscriptionResync::Incarnation => graph.write(|graph| *graph = EntityGraph::new()),
            SubscriptionResync::LeaseExpired => tokio::time::sleep(Duration::from_millis(30)).await,
            _ => unreachable!(),
        }
        assert_eq!(
            execute(
                &service,
                &session,
                SubscriptionRequest::Poll {
                    watch: initial.watch.clone()
                }
            )
            .await,
            SubscriptionOutcome::Resync(reason)
        );
        assert_eq!(
            execute(&service, &session, request).await,
            SubscriptionOutcome::Resync(reason)
        );
        assert_eq!(service.active_watches(), 0);
        assert_eq!(service.binding_count(), 1);
    }
}
#[tokio::test]
async fn builder_requires_exact_read_and_entity_authority_in_both_orders_without_write_capability()
{
    for subscriptions_first in [false, true] {
        let graph = graph();
        let builder =
            ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
                .unwrap();
        let reads = builder.handle().read_service();
        let store = EphemeralMutationStore::new(graph.clone());
        let subscriptions = StateSubscriptionService::new(
            reads.clone(),
            store.clone(),
            SubscriptionLimits::default(),
        )
        .unwrap();
        let different = MutationService::new(
            reads.clone(),
            EphemeralMutationStore::new(graph),
            Arc::new(AllowAllMutations),
            MutationLimits::default(),
        )
        .unwrap();
        if subscriptions_first {
            assert!(
                builder
                    .state_subscriptions(subscriptions)
                    .unwrap()
                    .entity_mutations(different)
                    .is_err()
            );
        } else {
            assert!(
                builder
                    .entity_mutations(different)
                    .unwrap()
                    .state_subscriptions(subscriptions)
                    .is_err()
            );
        }
    }
    let graph = graph();
    let builder =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let early = builder.handle();
    let subscriptions = StateSubscriptionService::new(
        early.read_service(),
        EphemeralMutationStore::new(graph),
        SubscriptionLimits::default(),
    )
    .unwrap();
    let builder = builder.state_subscriptions(subscriptions.clone()).unwrap();
    assert!(early.subscription_service().is_some());
    assert!(early.mutation_service().is_none());
    let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    let session = session();
    let value = delivery(
        execute(
            &subscriptions,
            &session,
            create(&subscriptions, "read-only", &["a"]),
        )
        .await,
    );
    assert_eq!(value.rows.len(), 1);
    owner.close().await.unwrap();
    owner.terminated().await;
    assert_eq!(subscriptions.active_watches(), 0);
    assert_eq!(subscriptions.binding_count(), 0);
}

#[tokio::test]
async fn bounded_original_bindings_survive_close_and_reject_before_initial_effects() {
    let graph = graph();
    let reads = ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let limits = SubscriptionLimits {
        max_watches: 1,
        max_watches_per_session: 1,
        max_bindings: 2,
        max_bindings_per_session: 2,
        ..SubscriptionLimits::default()
    };
    let service =
        StateSubscriptionService::new(reads, EphemeralMutationStore::new(graph), limits).unwrap();
    let session = session();
    let first_request = create(&service, "first", &["a"]);
    let first = delivery(execute(&service, &session, first_request.clone()).await);
    assert_eq!(
        execute(&service, &session, create(&service, "blocked", &["a"])).await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Capacity)
    );
    assert_eq!(service.binding_count(), 1);
    execute(
        &service,
        &session,
        SubscriptionRequest::Unsubscribe {
            watch: first.watch.clone(),
        },
    )
    .await;
    assert!(matches!(
        execute(&service, &session, first_request.clone()).await,
        SubscriptionOutcome::Closed { .. }
    ));
    let second = delivery(execute(&service, &session, create(&service, "second", &["a"])).await);
    execute(
        &service,
        &session,
        SubscriptionRequest::Unsubscribe {
            watch: second.watch.clone(),
        },
    )
    .await;
    assert_eq!(service.active_watches(), 0);
    assert_eq!(service.binding_count(), 2);
    assert_eq!(
        execute(&service, &session, create(&service, "third", &["a"])).await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Capacity)
    );
    assert!(matches!(
        execute(&service, &session, first_request).await,
        SubscriptionOutcome::Closed { .. }
    ));
    let fresh = crate::graph();
    let reads = ReadService::new(fresh.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let small = StateSubscriptionService::new(
        reads,
        EphemeralMutationStore::new(fresh),
        SubscriptionLimits {
            max_view_bytes: 32,
            ..SubscriptionLimits::default()
        },
    )
    .unwrap();
    assert_eq!(
        execute(&small, &session, create(&small, "too-large", &["a"])).await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Limit)
    );
    assert_eq!(small.binding_count(), 0);
    assert_eq!(small.retained_reservations(), 0);
}
#[tokio::test]
async fn preparing_sending_retrying_and_acknowledging_never_renew_the_lease() {
    for action in 0..3 {
        let service = service(graph());
        let session = session();
        let mut request = create(&service, "lease", &["a"]);
        if let SubscriptionRequest::Create(create) = &mut request {
            create.lease_ms = 100;
        }
        let initial = delivery(execute(&service, &session, request.clone()).await);
        tokio::time::sleep(Duration::from_millis(60)).await;
        let operation = match action {
            0 => request.clone(),
            1 => SubscriptionRequest::Poll {
                watch: initial.watch.clone(),
            },
            _ => ack(&initial),
        };
        execute(&service, &session, operation).await;
        tokio::time::sleep(Duration::from_millis(55)).await;
        assert_eq!(
            execute(&service, &session, request).await,
            SubscriptionOutcome::Resync(SubscriptionResync::LeaseExpired)
        );
    }
    let service = service(graph());
    let session = session();
    let mut request = create(&service, "renew", &["a"]);
    if let SubscriptionRequest::Create(create) = &mut request {
        create.lease_ms = 100;
    }
    let initial = delivery(execute(&service, &session, request).await);
    execute(
        &service,
        &session,
        SubscriptionRequest::Renew {
            watch: initial.watch.clone(),
            lease_ms: 1000,
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(110)).await;
    assert_eq!(
        delivery(
            execute(
                &service,
                &session,
                SubscriptionRequest::Poll {
                    watch: initial.watch.clone()
                }
            )
            .await
        )
        .token,
        initial.token
    );
}
#[tokio::test]
async fn authenticated_context_cannot_splice_another_permission_set_into_a_session() {
    let service = service(graph());
    let session = SubscriptionSession::authenticated(
        Principal::authenticated("owner", vec!["read".into()]),
        std::time::Instant::now() + Duration::from_secs(3),
    )
    .unwrap();
    let request = create(&service, "principal", &["a"]);
    let outcome = service
        .execute(
            ReadContext::with_timeout(
                Principal::authenticated("owner", vec!["admin".into()]),
                Duration::from_secs(2),
            ),
            session,
            request,
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Forbidden)
    );
    assert_eq!(service.binding_count(), 0);
}

struct ScopePolicy {
    mode: std::sync::atomic::AtomicUsize,
}
struct ScopeSnapshot {
    mode: usize,
}
impl ReadPolicy for ScopePolicy {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(ScopeSnapshot {
            mode: self.mode.load(std::sync::atomic::Ordering::SeqCst),
        }))
    }
}
impl PolicySnapshot for ScopeSnapshot {
    fn function(&self, _: &haystack_app::FunctionIdentity) -> bool {
        true
    }
    // Deliberately stable key exercises defensive retained-value reauthorization,
    // including a policy implementation that failed its versioning obligation.
    fn scope_key(&self) -> &str {
        "scope-test"
    }
    fn operation(&self, _: ReadOperation) -> bool {
        self.mode != 4
    }
    fn entity(&self, id: &str) -> bool {
        !(self.mode == 1 && id == "a") && !(self.mode == 2 && id == "b")
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        !(self.mode == 3 && tag == "value")
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
#[tokio::test]
async fn revoked_entities_tags_references_and_operations_never_replay_or_emit_removed_ids() {
    for mode in 1..=4 {
        let graph = graph();
        let mut linked = row("a", 1.0);
        linked.set("related", Kind::Ref(HRef::from_val("b")));
        graph.update("a", linked).unwrap();
        let policy = Arc::new(ScopePolicy {
            mode: std::sync::atomic::AtomicUsize::new(0),
        });
        let reads = ReadService::new(graph.clone(), policy.clone(), ReadLimits::default()).unwrap();
        let service = StateSubscriptionService::new(
            reads,
            EphemeralMutationStore::new(graph.clone()),
            SubscriptionLimits::default(),
        )
        .unwrap();
        let session = session();
        let initial =
            delivery(execute(&service, &session, create(&service, "revoke", &["a"])).await);
        assert!(initial.rows[0].has("related"));
        policy.mode.store(mode, std::sync::atomic::Ordering::SeqCst);
        graph.remove("a").unwrap();
        assert_eq!(
            execute(
                &service,
                &session,
                SubscriptionRequest::Poll {
                    watch: initial.watch.clone()
                }
            )
            .await,
            SubscriptionOutcome::Resync(SubscriptionResync::Policy)
        );
        assert_eq!(service.active_watches(), 0);
    }
}

struct Gate {
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    released: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            armed: std::sync::atomic::AtomicBool::new(true),
            entered: tokio::sync::Notify::new(),
            released: std::sync::Mutex::new(false),
            wake: std::sync::Condvar::new(),
        })
    }
    fn block_once(&self) {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.notify_one();
            let released = self.released.lock().unwrap();
            let (released, timeout) = self
                .wake
                .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
                .unwrap();
            assert!(
                *released && !timeout.timed_out(),
                "test gate must be released"
            );
        }
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
struct GateRelease(Arc<Gate>);
impl Drop for GateRelease {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct GatedPolicy {
    gate: Arc<Gate>,
    in_graph: bool,
}
impl ReadPolicy for GatedPolicy {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        if !self.in_graph {
            self.gate.block_once()
        }
        Ok(Arc::new(Self {
            gate: self.gate.clone(),
            in_graph: self.in_graph,
        }))
    }
}
impl PolicySnapshot for GatedPolicy {
    fn function(&self, _: &haystack_app::FunctionIdentity) -> bool {
        true
    }
    fn scope_key(&self) -> &str {
        "gated"
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, _: &str) -> bool {
        if self.in_graph {
            self.gate.block_once()
        }
        true
    }
    fn tag(&self, _: &str, _: &str) -> bool {
        true
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_create_reserves_one_binding_before_snapshot_and_never_duplicates() {
    let graph = graph();
    let gate = Gate::new();
    let _release = GateRelease(gate.clone());
    let reads = ReadService::new(
        graph.clone(),
        Arc::new(GatedPolicy {
            gate: gate.clone(),
            in_graph: false,
        }),
        ReadLimits::default(),
    )
    .unwrap();
    let service = StateSubscriptionService::new(
        reads,
        EphemeralMutationStore::new(graph.clone()),
        SubscriptionLimits::default(),
    )
    .unwrap();
    let session = session();
    let request = create(&service, "concurrent", &["a"]);
    let worker = {
        let service = service.clone();
        let session = session.clone();
        let request = request.clone();
        tokio::spawn(async move { execute(&service, &session, request).await })
    };
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    assert_eq!(service.binding_count(), 1);
    assert_eq!(service.read_service().load().admitted, 1);
    assert_eq!(
        execute(&service, &session, request.clone()).await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Pending)
    );
    assert_eq!(
        execute(&service, &session, create(&service, "concurrent", &["b"])).await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Conflict)
    );
    graph.update("a", row("a", 7.0)).unwrap();
    gate.release();
    let first = delivery(worker.await.unwrap());
    assert_eq!(first.through, graph.version());
    assert_eq!(
        first.rows[0].get("value"),
        Some(&Kind::Number(Number::unitless(7.0)))
    );
    assert_eq!(
        delivery(execute(&service, &session, request).await).token,
        first.token
    );
    assert_eq!(service.binding_count(), 1);
    assert_eq!(service.read_service().load().admitted, 0);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_projection_and_feed_head_share_one_guard_with_concurrent_commit() {
    let graph = graph();
    let before = graph.version();
    let gate = Gate::new();
    let _release = GateRelease(gate.clone());
    let reads = ReadService::new(
        graph.clone(),
        Arc::new(GatedPolicy {
            gate: gate.clone(),
            in_graph: true,
        }),
        ReadLimits::default(),
    )
    .unwrap();
    let service = StateSubscriptionService::new(
        reads,
        EphemeralMutationStore::new(graph.clone()),
        SubscriptionLimits::default(),
    )
    .unwrap();
    let session = session();
    let worker = {
        let service = service.clone();
        let session = session.clone();
        tokio::spawn(async move {
            execute(&service, &session, create(&service, "capture-race", &["a"])).await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), gate.entered.notified())
        .await
        .unwrap();
    assert!(
        graph.write_for(Duration::ZERO, |_| ()).is_none(),
        "projection callback is inside the graph read guard"
    );
    let committing = {
        let graph = graph.clone();
        tokio::task::spawn_blocking(move || graph.update("a", row("a", 7.0)).unwrap())
    };
    gate.release();
    let initial = delivery(worker.await.unwrap());
    committing.await.unwrap();
    let after = graph.version();
    assert_eq!(after, before + 1);
    let value = initial.rows[0].get("value");
    assert!(
        (initial.through == before && value == Some(&Kind::Number(Number::unitless(1.0))))
            || (initial.through == after && value == Some(&Kind::Number(Number::unitless(7.0))))
    );
    assert_eq!(initial.from, initial.through);
    execute(&service, &session, ack(&initial)).await;
    let resumed = execute(
        &service,
        &session,
        SubscriptionRequest::Resume {
            watch: initial.watch.clone(),
            scope_generation: 1,
            acknowledged: initial.through,
        },
    )
    .await;
    if initial.through == before {
        let changed = delivery(resumed);
        assert_eq!(changed.through, after);
        assert_eq!(
            changed.rows[0].get("value"),
            Some(&Kind::Number(Number::unitless(7.0)))
        )
    } else {
        assert!(matches!(resumed, SubscriptionOutcome::Idle { .. }))
    }
}
#[tokio::test]
async fn complete_commit_units_are_never_split_when_a_poll_limit_is_too_small() {
    use haystack_core::graph::{BatchLimits, EntityOperation};
    for limit in [1, 2] {
        let graph = graph();
        let reads =
            ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let service = StateSubscriptionService::new(
            reads,
            EphemeralMutationStore::new(graph.clone()),
            SubscriptionLimits {
                max_change_diffs: limit,
                ..SubscriptionLimits::default()
            },
        )
        .unwrap();
        let session = session();
        let first =
            delivery(execute(&service, &session, create(&service, "unit", &["a", "b"])).await);
        execute(&service, &session, ack(&first)).await;
        graph.write(|graph| {
            let prepared = graph
                .prepare_batch(
                    graph.version(),
                    &[
                        EntityOperation::Remove { id: "a".into() },
                        EntityOperation::Patch {
                            id: "b".into(),
                            changes: row("b", 9.0),
                        },
                    ],
                    BatchLimits::default(),
                )
                .unwrap();
            graph.apply_prepared(prepared).unwrap();
        });
        let outcome = execute(
            &service,
            &session,
            SubscriptionRequest::Poll {
                watch: first.watch.clone(),
            },
        )
        .await;
        if limit == 1 {
            assert_eq!(
                outcome,
                SubscriptionOutcome::Resync(SubscriptionResync::Overflow)
            );
        } else {
            let changed = delivery(outcome);
            assert_eq!(changed.from, first.through);
            assert_eq!(changed.through, first.through + 2);
            assert_eq!(changed.removed, vec!["a"]);
            assert_eq!(changed.rows.len(), 1);
        }
    }
}

#[tokio::test]
async fn review_replacement_membership_growth_is_reserved_before_publication() {
    let graph = SharedGraph::new(EntityGraph::new());
    let limits = SubscriptionLimits {
        max_view_bytes: 1,
        max_delivery_bytes: 4096,
        max_retained_bytes: 20 * 1024,
        ..SubscriptionLimits::default()
    };
    let service = StateSubscriptionService::new(
        ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap(),
        EphemeralMutationStore::new(graph),
        limits.clone(),
    )
    .unwrap();
    let session = session();
    let original = create(&service, "membership-bound", &["missing"]);
    let initial = delivery(execute(&service, &session, original.clone()).await);
    let before = service.retained_reservations();
    let ids: Vec<_> = (0..50)
        .map(|n| format!("{n:04}{}", "x".repeat(996)))
        .collect();
    let request = SubscriptionRequest::Replace {
        watch: initial.watch.clone(),
        expected_scope_generation: 1,
        ids: ids.clone(),
    };
    assert!(wire::encode(&request).unwrap().len() < 65_536);
    assert_eq!(
        execute(&service, &session, request).await,
        SubscriptionOutcome::Rejected(SubscriptionRejection::Capacity)
    );
    assert_eq!(service.retained_reservations(), before);
    assert_eq!(
        *delivery(
            execute(
                &service,
                &session,
                SubscriptionRequest::Poll {
                    watch: initial.watch.clone()
                }
            )
            .await
        ),
        *initial
    );
    let grown = delivery(
        execute(
            &service,
            &session,
            SubscriptionRequest::Replace {
                watch: initial.watch.clone(),
                expected_scope_generation: 1,
                ids: ids[..2].to_vec(),
            },
        )
        .await,
    );
    assert_eq!(grown.scope_generation, 2);
    let reserved = service.retained_reservations();
    assert!(reserved > before && reserved <= limits.max_retained_bytes);
    assert_eq!(
        *delivery(execute(&service, &session, original).await),
        *grown
    );
    assert!(matches!(
        execute(
            &service,
            &session,
            SubscriptionRequest::Unsubscribe {
                watch: initial.watch.clone()
            }
        )
        .await,
        SubscriptionOutcome::Closed { .. }
    ));
    assert_eq!(service.binding_count(), 1);
    assert_eq!(service.retained_reservations(), reserved);
}

#[tokio::test]
async fn review_execution_resource_exhaustion_is_terminal_overflow_native_and_wire() {
    for wire_request in [false, true] {
        for kind in [BudgetKind::Retained, BudgetKind::Work, BudgetKind::Values] {
            let graph = SharedGraph::new(EntityGraph::with_changelog_capacity(16_000));
            let mut read_limits = ReadLimits::default();
            match kind {
                BudgetKind::Retained => read_limits.max_retained_bytes = 65_536,
                BudgetKind::Work => read_limits.max_work = 20_000,
                BudgetKind::Values => read_limits.max_value_nodes = 1,
                _ => unreachable!(),
            }
            let reads = ReadService::new(graph.clone(), Arc::new(AllowAll), read_limits).unwrap();
            let service = StateSubscriptionService::new(
                reads.clone(),
                EphemeralMutationStore::new(graph.clone()),
                SubscriptionLimits {
                    max_change_diffs: 16_000,
                    ..SubscriptionLimits::default()
                },
            )
            .unwrap();
            let session = session();
            let original = create(&service, "resource-bound", &["missing"]);
            let initial = delivery(execute(&service, &session, original.clone()).await);
            execute(&service, &session, ack(&initial)).await;
            if kind == BudgetKind::Work {
                graph.add(row("outside", 0.0)).unwrap();
                for n in 0..12_000 {
                    graph
                        .update("outside", row("outside", f64::from(n)))
                        .unwrap();
                }
            } else {
                let mut entity = row("missing", 1.0);
                if kind == BudgetKind::Retained {
                    entity.set("blob", Kind::Str("x".repeat(40 * 1024)));
                }
                graph.add(entity).unwrap();
            }
            let poll = SubscriptionRequest::Poll {
                watch: initial.watch.clone(),
            };
            let result = if wire_request {
                let codec = haystack_core::codecs::codec_for("text/zinc").unwrap();
                let admission = reads
                    .begin(ReadContext::with_timeout(
                        session.principal().clone(),
                        Duration::from_secs(3),
                    ))
                    .await
                    .unwrap();
                let response = service
                    .wire_admitted(
                        admission,
                        session.clone(),
                        SubscriptionWireRequest {
                            operation: "watchPoll",
                            body: wire::encode_grid(&poll, codec).unwrap(),
                            input: H4Codec::Zinc,
                            output: H4Codec::Zinc,
                            legacy_allowed: false,
                        },
                    )
                    .await;
                response
                    .map(|bytes| wire::decode_grid::<SubscriptionOutcome>(&bytes, codec).unwrap())
            } else {
                service
                    .execute(
                        ReadContext::with_timeout(
                            session.principal().clone(),
                            Duration::from_secs(3),
                        ),
                        session.clone(),
                        poll,
                    )
                    .await
            };
            assert_eq!(
                result,
                Ok(SubscriptionOutcome::Resync(SubscriptionResync::Overflow)),
                "wire={wire_request} budget={kind:?}"
            );
            assert_eq!(service.active_watches(), 0);
            assert_eq!(service.binding_count(), 1);
            assert_eq!(
                execute(&service, &session, original).await,
                SubscriptionOutcome::Resync(SubscriptionResync::Overflow)
            );
            assert_eq!(
                execute(&service, &session, ack(&initial)).await,
                SubscriptionOutcome::Resync(SubscriptionResync::Overflow)
            );
            assert_eq!(
                execute(
                    &service,
                    &session,
                    SubscriptionRequest::Resume {
                        watch: initial.watch.clone(),
                        scope_generation: initial.scope_generation,
                        acknowledged: initial.through
                    }
                )
                .await,
                SubscriptionOutcome::Resync(SubscriptionResync::Overflow)
            );
        }
    }
}

#[tokio::test]
async fn review_input_errors_deadlines_and_cancellation_do_not_invalidate_a_watch() {
    let service = service(graph());
    let session = session();
    let initial =
        delivery(execute(&service, &session, create(&service, "input-errors", &["a"])).await);
    let invalid = SubscriptionRequest::Replace {
        watch: initial.watch.clone(),
        expected_scope_generation: 1,
        ids: vec!["a".into(); wire::MAX_IDS + 1],
    };
    assert!(matches!(
        service
            .execute(
                ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(1)),
                session.clone(),
                invalid
            )
            .await,
        Err(ReadError::InvalidQuery(_))
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let poll = SubscriptionRequest::Poll {
        watch: initial.watch.clone(),
    };
    assert_eq!(
        service
            .execute(
                ReadContext::new(
                    session.principal().clone(),
                    std::time::Instant::now() + Duration::from_secs(1),
                    cancel
                ),
                session.clone(),
                poll.clone()
            )
            .await,
        Err(ReadError::Cancelled)
    );
    assert_eq!(
        service
            .execute(
                ReadContext::new(
                    session.principal().clone(),
                    std::time::Instant::now() - Duration::from_secs(1),
                    CancellationToken::new()
                ),
                session.clone(),
                poll.clone()
            )
            .await,
        Err(ReadError::Deadline)
    );
    assert_eq!(*delivery(execute(&service, &session, poll).await), *initial);
    assert_eq!(service.active_watches(), 1);
}
