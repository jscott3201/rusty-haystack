use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{Float, HDateTime, HRef, Kind, Number},
};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn point(kind: &str, zone: &str, unit: Option<&str>) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("p")));
    row.set("his", Kind::Marker);
    row.set("kind", Kind::Str(kind.into()));
    row.set("tz", Kind::Str(zone.into()));
    if let Some(unit) = unit {
        row.set("unit", Kind::Str(unit.into()));
    }
    row
}
fn graph() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    graph.add(point("Number", "UTC", Some("°F"))).unwrap();
    graph
}
fn context() -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "owner".into(),
        },
        Duration::from_secs(3),
    )
}
fn sample(second: i64, val: Kind) -> HistorySample {
    HistorySample {
        ts: HDateTime::new(
            chrono::DateTime::from_timestamp(1_717_200_000 + second, 0)
                .unwrap()
                .fixed_offset(),
            "UTC",
        ),
        val,
    }
}
fn num(value: f64) -> Kind {
    Kind::Number(Number::unitless(value))
}
fn request(store: &HisStore, operation: &str, samples: Vec<HistorySample>) -> HistoryWriteRequest {
    let state = store.state("p").unwrap();
    HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: state.authority,
            point: "p".into(),
            incarnation: state.incarnation,
            operation_id: operation.into(),
        },
        expected_generation: state.generation,
        samples,
    }
}
fn service(
    graph: SharedGraph,
    provider: Arc<dyn HistoryProvider>,
    policy: Arc<dyn HistoryMutationPolicy>,
) -> HistoryMutationService {
    let reads = ReadService::new(graph, Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let history = HistoryService::new(reads, provider, HistoryLimits::default()).unwrap();
    HistoryMutationService::new(history, policy, HistoryMutationLimits::default()).unwrap()
}
fn committed(outcome: HistoryWriteOutcome) -> HistoryWriteReceipt {
    match outcome {
        HistoryWriteOutcome::Committed(receipt) => receipt,
        other => panic!("expected committed: {other:?}"),
    }
}
fn rejected(outcome: HistoryWriteOutcome, expected: HistoryWriteRejection) {
    assert!(
        matches!(outcome, HistoryWriteOutcome::Rejected { reason, .. } if reason == expected),
        "{outcome:?}"
    );
}
struct Fixture {
    store: HisStore,
    mode: AtomicUsize,
    calls: AtomicUsize,
    held: Mutex<Vec<PreparedHistoryMutation>>,
}
impl Fixture {
    fn new(store: HisStore, mode: usize) -> Self {
        Self {
            store,
            mode: AtomicUsize::new(mode),
            calls: AtomicUsize::new(0),
            held: Mutex::new(vec![]),
        }
    }
}
impl HistoryProvider for Fixture {
    fn open(
        &self,
        id: String,
        start: chrono::DateTime<chrono::FixedOffset>,
        end: chrono::DateTime<chrono::FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        self.store.open(id, start, end, budget)
    }
    fn history_write_capability(&self) -> Option<HistoryWriteCapability> {
        (self.mode.load(Ordering::SeqCst) != 6).then(|| HistoryWriteCapability {
            store: self.store.clone(),
            qualification: HistoryReceiptQualification::ProviderProtocol,
        })
    }
    fn commit_history(&self, prepared: PreparedHistoryMutation) -> HistoryWriteOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let identity = prepared.identity().clone();
        match self.mode.load(Ordering::SeqCst) {
            0 => prepared.publish(),
            1 => {
                committed(prepared.publish());
                HistoryWriteOutcome::Unknown {
                    identity,
                    cause: HistoryWriteUnknown::Provider,
                }
            }
            2 => prepared.reject(HistoryWriteRejection::Provider),
            3 => {
                self.held.lock().push(prepared);
                HistoryWriteOutcome::Unknown {
                    identity,
                    cause: HistoryWriteUnknown::Pending,
                }
            }
            4 => {
                let mut identity = identity;
                identity.operation_id.push_str("-wrong");
                HistoryWriteOutcome::Rejected {
                    identity,
                    reason: HistoryWriteRejection::Provider,
                }
            }
            5 => HistoryWriteOutcome::Committed(HistoryWriteReceipt {
                identity,
                before_generation: 0,
                after_generation: 1,
                change_sequence: 1,
                submitted_samples: 1,
                unique_samples: 1,
                retained_samples: 1,
                evicted_samples: 0,
                qualification: HistoryReceiptQualification::ProviderProtocol,
            }),
            _ => prepared.reject(HistoryWriteRejection::Unsupported),
        }
    }
}
#[tokio::test]
async fn one_point_commit_upserts_retains_and_publishes_receipt_and_change_together() {
    let store = HisStore::with_retention(2).unwrap();
    let service = service(
        graph(),
        Arc::new(store.clone()),
        Arc::new(AllowAllHistoryMutations),
    );
    let request = request(
        &store,
        "batch",
        vec![
            sample(3, num(3.0)),
            sample(1, num(1.0)),
            sample(2, Kind::NA),
            sample(1, num(9.0)),
        ],
    );
    let receipt = committed(service.submit(context(), request.clone()).await.unwrap());
    assert_eq!(
        (receipt.before_generation, receipt.after_generation),
        (0, 1)
    );
    assert_eq!(
        (
            receipt.submitted_samples,
            receipt.unique_samples,
            receipt.retained_samples,
            receipt.evicted_samples
        ),
        (4, 3, 2, 1)
    );
    assert_eq!(
        store
            .read("p", None, None)
            .iter()
            .map(|item| item.val.clone())
            .collect::<Vec<_>>(),
        vec![Kind::NA, num(3.0)]
    );
    let changes = store.retained_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].sequence, receipt.change_sequence);
    assert_eq!(changes[0].operation, Some(request.identity.clone()));
    assert_eq!(changes[0].after, store.state("p").unwrap());
    assert_eq!(
        service
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        HistoryWriteOutcome::Committed(receipt.clone())
    );
    assert_eq!(
        committed(service.submit(context(), request).await.unwrap()),
        receipt
    );
    assert_eq!(store.retained_changes().len(), 1);
}
#[tokio::test]
async fn original_order_bits_units_and_expected_generation_bind_one_operation_id() {
    let store = HisStore::new();
    let service = service(
        graph(),
        Arc::new(store.clone()),
        Arc::new(AllowAllHistoryMutations),
    );
    let request = request(
        &store,
        "ordered",
        vec![sample(0, num(1.0)), sample(0, num(2.0))],
    );
    committed(service.submit(context(), request.clone()).await.unwrap());
    for variation in 0..5 {
        let mut changed = request.clone();
        match variation {
            0 => changed.samples.reverse(),
            1 => changed.expected_generation += 1,
            2 => changed.samples[0].val = Kind::Number(Number::new(1.0, Some("fahrenheit".into()))),
            3 => changed.samples[0].ts.tz_name = "GMT".into(),
            _ => changed.samples[0].val = num(-0.0),
        }
        rejected(
            service.submit(context(), changed).await.unwrap(),
            HistoryWriteRejection::Conflict,
        );
    }
    assert_eq!(store.state("p").unwrap().generation, 1);
}
#[tokio::test]
async fn mixed_unsupported_or_timezone_invalid_batch_has_no_sample_receipt_or_change_effect() {
    for bad in [
        Kind::Int(3),
        Kind::Float(Float::new(3.0)),
        Kind::Null,
        Kind::None,
        Kind::Remove,
        Kind::Bool(true),
        Kind::Str("wrong".into()),
        Kind::Ref(HRef::from_val("hidden")),
        Kind::Number(Number::new(3.0, Some("°C".into()))),
        Kind::Number(Number::new(3.0, Some("".into()))),
        Kind::Number(Number::new(3.0, Some("unknown-unit".into()))),
        Kind::Number(Number::new(f64::NAN, Some("°F".into()))),
        Kind::Number(Number::unitless(f64::from_bits(0x7ff8000000000001))),
        Kind::Number(Number::unitless(-f64::NAN)),
    ] {
        let store = HisStore::new();
        let service = service(
            graph(),
            Arc::new(store.clone()),
            Arc::new(AllowAllHistoryMutations),
        );
        let request = request(&store, "invalid", vec![sample(0, num(1.0)), sample(1, bad)]);
        rejected(
            service.submit(context(), request).await.unwrap(),
            HistoryWriteRejection::Unsupported,
        );
        assert_eq!(store.state("p").unwrap().generation, 0);
        assert!(store.is_empty("p"));
        assert_eq!(store.receipt_count(), 0);
        assert!(store.retained_changes().is_empty());
    }
    let store = HisStore::new();
    let service = service(
        graph(),
        Arc::new(store.clone()),
        Arc::new(AllowAllHistoryMutations),
    );
    let mut request = request(
        &store,
        "zone",
        vec![sample(0, num(1.0)), sample(1, num(2.0))],
    );
    request.samples[1].ts.tz_name = "GMT".into();
    rejected(
        service.submit(context(), request).await.unwrap(),
        HistoryWriteRejection::Invalid,
    );
    assert_eq!(store.receipt_count(), 0);
    assert!(store.retained_changes().is_empty());
}
#[tokio::test]
async fn aliases_na_canonical_nan_infinities_and_scalar_kinds_are_admitted_without_coercion() {
    for (kind, unit, values) in [
        (
            "Number",
            Some("fahrenheit"),
            vec![
                num(1.0),
                Kind::Number(Number::new(2.0, Some("°F".into()))),
                num(f64::NAN),
                num(f64::INFINITY),
                Kind::Number(Number::new(f64::NEG_INFINITY, Some("fahrenheit".into()))),
                Kind::NA,
            ],
        ),
        (
            "Number",
            Some("USD"),
            vec![Kind::Number(Number::new(2.0, Some("$".into()))), Kind::NA],
        ),
        (
            "Bool",
            None,
            vec![Kind::Bool(false), Kind::Bool(true), Kind::NA],
        ),
        ("Str", None, vec![Kind::Str("ready".into()), Kind::NA]),
    ] {
        let graph = SharedGraph::new(EntityGraph::new());
        graph.add(point(kind, "UTC", unit)).unwrap();
        let store = HisStore::new();
        let service = service(
            graph,
            Arc::new(store.clone()),
            Arc::new(AllowAllHistoryMutations),
        );
        let request = request(
            &store,
            "values",
            values
                .iter()
                .cloned()
                .enumerate()
                .map(|(i, val)| sample(i as i64, val))
                .collect(),
        );
        committed(service.submit(context(), request.clone()).await.unwrap());
        let stored = store.read("p", None, None);
        for (actual, expected) in stored.iter().zip(values) {
            match (&actual.val, expected) {
                (Kind::Number(a), Kind::Number(b)) => {
                    assert_eq!(a.val.to_bits(), b.val.to_bits());
                    assert_eq!(a.unit, b.unit);
                }
                (actual, expected) => assert_eq!(actual, &expected),
            }
        }
    }
}
#[tokio::test]
async fn timezone_overlap_accepts_both_explicit_offsets_but_gaps_and_aliases_reject() {
    for (zone, stamp, submitted_zone, accepted) in [
        ("New_York", "2024-11-03T01:30:00-04:00", "New_York", true),
        ("New_York", "2024-11-03T01:30:00-05:00", "New_York", true),
        ("New_York", "2024-03-10T02:30:00-05:00", "New_York", false),
        ("New_York", "2024-06-01T00:00:00Z", "New_York", false),
        ("UTC", "2024-06-01T00:00:00Z", "GMT", false),
        ("Calcutta", "2024-06-01T00:00:00+05:30", "Kolkata", false),
        (
            "New_York",
            "2024-06-01T00:00:00-04:00",
            "America/New_York",
            false,
        ),
        ("Rel", "2024-06-01T00:00:00Z", "Rel", false),
    ] {
        let graph = SharedGraph::new(EntityGraph::new());
        graph.add(point("Number", zone, Some("°F"))).unwrap();
        let store = HisStore::new();
        let service = service(
            graph,
            Arc::new(store.clone()),
            Arc::new(AllowAllHistoryMutations),
        );
        let request = request(
            &store,
            "time",
            vec![HistorySample {
                ts: HDateTime::new(
                    chrono::DateTime::parse_from_rfc3339(stamp).unwrap(),
                    submitted_zone,
                ),
                val: num(2.0),
            }],
        );
        let result = service.submit(context(), request).await.unwrap();
        if accepted {
            committed(result);
        } else {
            rejected(result, HistoryWriteRejection::Invalid);
            assert_eq!(store.receipt_count(), 0);
        }
    }
}
#[tokio::test]
async fn empty_scoped_batches_bind_rejection_while_native_empty_writes_do_nothing() {
    let store = HisStore::new();
    let service = service(
        graph(),
        Arc::new(store.clone()),
        Arc::new(AllowAllHistoryMutations),
    );
    let mut request = request(&store, "empty", vec![]);
    let before = store.state("p").unwrap();
    let outcome = service.submit(context(), request.clone()).await.unwrap();
    rejected(outcome.clone(), HistoryWriteRejection::EmptyBatch);
    assert_eq!(store.receipt_count(), 1);
    assert_eq!(
        service
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        outcome
    );
    assert_eq!(
        service.submit(context(), request.clone()).await.unwrap(),
        outcome
    );
    request.samples.push(sample(0, num(1.0)));
    rejected(
        service.submit(context(), request).await.unwrap(),
        HistoryWriteRejection::Conflict,
    );
    store.write("p", vec![]).unwrap();
    store.write("absent", vec![]).unwrap();
    assert_eq!(store.point_count(), 1);
    assert_eq!(store.state("p").unwrap(), before);
    assert!(store.retained_changes().is_empty());
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_submissions_commit_one_effect_and_reuse_the_same_zero_identity() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 0));
    let service = service(
        graph(),
        provider.clone(),
        Arc::new(AllowAllHistoryMutations),
    );
    let request = request(&store, "same-first", vec![sample(0, num(2.0))]);
    let mut tasks = vec![];
    for _ in 0..8 {
        let service = service.clone();
        let request = request.clone();
        tasks.push(tokio::spawn(async move {
            service.submit(context(), request).await.unwrap()
        }));
    }
    for task in tasks {
        let outcome = task.await.unwrap();
        assert!(
            matches!(
                outcome,
                HistoryWriteOutcome::Committed(_)
                    | HistoryWriteOutcome::Unknown {
                        cause: HistoryWriteUnknown::Pending,
                        ..
                    }
            ),
            "{outcome:?}"
        );
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.receipt_count(), 1);
    assert_eq!(store.state("p").unwrap().generation, 1);
    assert_eq!(store.retained_changes().len(), 1);
    let first = crate::request(&store, "first-of-two", vec![sample(1, num(3.0))]);
    let second = crate::request(&store, "second-of-two", vec![sample(2, num(4.0))]);
    let (a, b) = tokio::join!(
        service.submit(context(), first),
        service.submit(context(), second)
    );
    let outcomes = [a.unwrap(), b.unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, HistoryWriteOutcome::Committed(_)))
            .count(),
        1
    );
    assert_eq!(store.state("p").unwrap().generation, 2);
}
#[tokio::test]
async fn ambiguity_retained_authority_recreation_and_ephemeral_loss_never_replay() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 1));
    let graph = graph();
    let service1 = service(
        graph.clone(),
        provider.clone(),
        Arc::new(AllowAllHistoryMutations),
    );
    let request = request(&store, "lost", vec![sample(0, num(1.0))]);
    assert!(matches!(
        service1.submit(context(), request.clone()).await.unwrap(),
        HistoryWriteOutcome::Unknown { .. }
    ));
    let receipt = committed(
        service1
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
    );
    let service2 = service(
        graph.clone(),
        provider.clone(),
        Arc::new(AllowAllHistoryMutations),
    );
    assert_eq!(
        committed(
            service2
                .reconcile(context(), request.identity.clone())
                .await
                .unwrap()
        ),
        receipt
    );
    assert_eq!(
        committed(service2.submit(context(), request.clone()).await.unwrap()),
        receipt
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let fresh = service(
        graph,
        Arc::new(HisStore::new()),
        Arc::new(AllowAllHistoryMutations),
    );
    for outcome in [
        fresh
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        fresh.submit(context(), request).await.unwrap(),
    ] {
        assert!(matches!(
            outcome,
            HistoryWriteOutcome::Unknown {
                cause: HistoryWriteUnknown::Missing,
                ..
            }
        ));
    }
    assert_eq!(fresh.store().receipt_count(), 0);
}
#[tokio::test]
async fn recognized_receipts_survive_graph_removal_replacement_and_independent_history_resets() {
    let store = HisStore::new();
    let graph = graph();
    let service = service(
        graph.clone(),
        Arc::new(store.clone()),
        Arc::new(AllowAllHistoryMutations),
    );
    let request = request(&store, "old", vec![sample(0, num(1.0))]);
    let receipt = committed(service.submit(context(), request.clone()).await.unwrap());
    let before = store.state("p").unwrap();
    graph.remove("p").unwrap();
    assert_eq!(
        committed(service.submit(context(), request.clone()).await.unwrap()),
        receipt
    );
    graph.write(|graph| *graph = EntityGraph::new());
    assert_eq!(store.state("p").unwrap(), before);
    assert_eq!(
        committed(
            service
                .reconcile(context(), request.identity.clone())
                .await
                .unwrap()
        ),
        receipt
    );
    graph.add(point("Number", "UTC", Some("°F"))).unwrap();
    let reset = store.reset_point("p").unwrap();
    let reset2 = store.reset_point("p").unwrap();
    assert_ne!(reset.incarnation, before.incarnation);
    assert_ne!(reset2.incarnation, reset.incarnation);
    assert_eq!(reset2.generation, 0);
    assert_eq!(
        committed(service.submit(context(), request.clone()).await.unwrap()),
        receipt
    );
    let mut unrecognized = request;
    unrecognized.identity.operation_id = "stale-new".into();
    rejected(
        service.submit(context(), unrecognized).await.unwrap(),
        HistoryWriteRejection::Conflict,
    );
    assert!(store.is_empty("p"));
}
struct Permissions;
impl HistoryMutationPolicy for Permissions {
    fn authorize_intent(&self, p: &Principal, r: &HistoryWriteRequest) -> bool {
        r.identity.point == "p"
            && matches!(p, Principal::Authenticated { permissions, .. } if permissions.iter().any(|p| p == "write"))
    }
    fn authorize_schema(&self, p: &Principal, r: &HistoryWriteRequest, _: &HistorySchema) -> bool {
        self.authorize_intent(p, r)
    }
    fn authorize_reconcile(
        &self,
        p: &Principal,
        _: &HistoryOperationIdentity,
        original: Option<&HistoryWriteRequest>,
    ) -> bool {
        original.is_some_and(|request| self.authorize_intent(p, request))
    }
}
fn authenticated(permissions: &[&str]) -> ReadContext {
    ReadContext::with_timeout(
        Principal::authenticated("owner", permissions.iter().map(|p| (*p).into()).collect()),
        Duration::from_secs(3),
    )
}
#[tokio::test]
async fn current_original_intent_authorization_and_stable_principal_variant_subject_are_required() {
    let store = HisStore::new();
    let graph = graph();
    let service = service(
        graph.clone(),
        Arc::new(store.clone()),
        Arc::new(Permissions),
    );
    let request = request(&store, "permission", vec![sample(0, num(1.0))]);
    rejected(
        service
            .submit(authenticated(&["read"]), request.clone())
            .await
            .unwrap(),
        HistoryWriteRejection::Forbidden,
    );
    let receipt = committed(
        service
            .submit(authenticated(&["write"]), request.clone())
            .await
            .unwrap(),
    );
    graph.remove("p").unwrap();
    assert_eq!(
        committed(
            service
                .reconcile(authenticated(&["write", "read"]), request.identity.clone())
                .await
                .unwrap()
        ),
        receipt
    );
    assert!(matches!(
        service
            .reconcile(authenticated(&["read"]), request.identity.clone())
            .await,
        Err(ReadError::Forbidden)
    ));
    service.replace_policy(Arc::new(AllowAllHistoryMutations));
    assert!(matches!(
        service
            .reconcile(context(), request.identity)
            .await
            .unwrap(),
        HistoryWriteOutcome::Unknown {
            cause: HistoryWriteUnknown::Missing,
            ..
        }
    ));
}
#[tokio::test]
async fn provider_rejection_invalid_ack_and_missing_capability_preserve_actual_effect() {
    for mode in [2, 4, 5] {
        let store = HisStore::new();
        let provider = Arc::new(Fixture::new(store.clone(), mode));
        let service = service(graph(), provider, Arc::new(AllowAllHistoryMutations));
        let request = request(&store, "provider", vec![sample(0, num(1.0))]);
        let outcome = service.submit(context(), request.clone()).await.unwrap();
        if mode == 2 {
            rejected(outcome, HistoryWriteRejection::Provider);
        } else {
            assert!(matches!(
                outcome,
                HistoryWriteOutcome::Unknown {
                    cause: HistoryWriteUnknown::InvalidAcknowledgement,
                    ..
                }
            ));
        }
        assert!(store.is_empty("p"));
        assert!(store.retained_changes().is_empty());
    }
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 6));
    let history = HistoryService::new(
        ReadService::new(graph(), Arc::new(AllowAll), ReadLimits::default()).unwrap(),
        provider.clone(),
        HistoryLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        HistoryMutationService::new(
            history.clone(),
            Arc::new(AllowAllHistoryMutations),
            HistoryMutationLimits::default()
        ),
        Err(ReadError::Unavailable)
    ));
    provider.mode.store(0, Ordering::SeqCst);
    let service = HistoryMutationService::new(
        history,
        Arc::new(AllowAllHistoryMutations),
        HistoryMutationLimits::default(),
    )
    .unwrap();
    provider.mode.store(6, Ordering::SeqCst);
    rejected(
        service
            .submit(
                context(),
                request(&store, "read-only", vec![sample(0, num(1.0))]),
            )
            .await
            .unwrap(),
        HistoryWriteRejection::Unsupported,
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.receipt_count(), 0);
}
#[tokio::test]
async fn delayed_publication_rechecks_graph_policy_native_generation_reset_and_cancellation() {
    for action in 0..6 {
        let store = HisStore::new();
        let graph = graph();
        let provider = Arc::new(Fixture::new(store.clone(), 3));
        let service = service(
            graph.clone(),
            provider.clone(),
            Arc::new(AllowAllHistoryMutations),
        );
        let request = request(&store, "delayed", vec![sample(0, num(1.0))]);
        let context = context();
        let cancel = context.cancellation.clone();
        assert!(matches!(
            service.submit(context, request.clone()).await.unwrap(),
            HistoryWriteOutcome::Unknown {
                cause: HistoryWriteUnknown::Pending,
                ..
            }
        ));
        assert_eq!(service.read_service().load().admitted, 1);
        assert!(store.is_empty("p"));
        assert!(store.retained_changes().is_empty());
        match action {
            0 => {
                let mut changes = HDict::new();
                changes.set("tz", Kind::Str("GMT".into()));
                graph.update("p", changes).unwrap();
            }
            1 => service.replace_policy(Arc::new(AllowAllHistoryMutations)),
            2 => store
                .write(
                    "p",
                    vec![HisItem {
                        ts: sample(2, num(2.0)).ts.dt,
                        val: num(2.0),
                    }],
                )
                .unwrap(),
            3 => {
                store.reset_point("p").unwrap();
            }
            4 => cancel.cancel(),
            _ => graph.write(|graph| *graph = EntityGraph::new()),
        }
        let prepared = provider.held.lock().pop().unwrap();
        rejected(
            prepared.publish(),
            if action == 4 {
                HistoryWriteRejection::Cancelled
            } else {
                HistoryWriteRejection::Conflict
            },
        );
        assert_eq!(service.read_service().load().admitted, 0);
        assert_eq!(store.len("p"), usize::from(action == 2));
    }
}
#[tokio::test]
async fn point_receipt_series_and_input_capacity_fail_before_effect() {
    let store = HisStore::with_limits(HistoryStoreLimits {
        max_points: 1,
        receipt_capacity: 1,
        max_items_per_point: 2,
        ..HistoryStoreLimits::default()
    })
    .unwrap();
    let initial = store.state("p").unwrap();
    assert_eq!(initial, store.state("p").unwrap());
    assert_eq!(store.state("q"), Err(HistoryProviderError::Limit));
    let service = service(
        graph(),
        Arc::new(store.clone()),
        Arc::new(AllowAllHistoryMutations),
    );
    committed(
        service
            .submit(
                context(),
                request(&store, "first", vec![sample(0, num(1.0))]),
            )
            .await
            .unwrap(),
    );
    rejected(
        service
            .submit(
                context(),
                request(&store, "capacity", vec![sample(1, num(2.0))]),
            )
            .await
            .unwrap(),
        HistoryWriteRejection::Capacity,
    );
    let huge = request(
        &store,
        "huge",
        vec![sample(2, Kind::Str("x".repeat(1_000_000)))],
    );
    rejected(
        service.submit(context(), huge).await.unwrap(),
        HistoryWriteRejection::Limit,
    );
    assert_eq!(store.len("p"), 1);
    assert_eq!(store.state("p").unwrap().generation, 1);
}
#[tokio::test]
async fn retained_plan_holds_original_work_until_owner_sealed_publication_finishes() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 3));
    let builder = ApplicationBuilder::new(graph(), Arc::new(AllowAll), ReadLimits::default())
        .unwrap()
        .shutdown_policy(ShutdownPolicy {
            drain_timeout: Duration::from_millis(10),
            stop_timeout: Duration::from_millis(20),
        });
    let history = HistoryService::new(
        builder.handle().read_service(),
        provider.clone(),
        HistoryLimits::default(),
    )
    .unwrap();
    let service = HistoryMutationService::new(
        history.clone(),
        Arc::new(AllowAllHistoryMutations),
        HistoryMutationLimits::default(),
    )
    .unwrap();
    let owner = builder
        .owned_history(history)
        .unwrap()
        .history_mutations(service.clone())
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    let request = request(&store, "shutdown", vec![sample(0, num(1.0))]);
    service.submit(context(), request).await.unwrap();
    assert_eq!(service.read_service().load().admitted, 1);
    assert!(matches!(
        owner.close().await,
        Err(ApplicationError::ShutdownTimeout { .. })
    ));
    let plan = provider.held.lock().pop().unwrap();
    rejected(plan.publish(), HistoryWriteRejection::Cancelled);
    let report = owner.terminated().await;
    assert!(report.close.is_err());
    assert_eq!(service.read_service().load().admitted, 0);
    assert!(store.is_empty("p"));
}

#[tokio::test]
async fn deferred_success_has_no_outward_change_or_committed_receipt_before_publish() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 3));
    let service = service(
        graph(),
        provider.clone(),
        Arc::new(AllowAllHistoryMutations),
    );
    let request = request(&store, "publish-later", vec![sample(0, num(1.0))]);
    let pending = service.submit(context(), request.clone()).await.unwrap();
    assert!(matches!(
        pending,
        HistoryWriteOutcome::Unknown {
            cause: HistoryWriteUnknown::Pending,
            ..
        }
    ));
    assert_eq!(
        service
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        pending
    );
    assert_eq!(store.state("p").unwrap().generation, 0);
    assert!(store.is_empty("p"));
    assert!(store.retained_changes().is_empty());
    let receipt = committed(provider.held.lock().pop().unwrap().publish());
    assert_eq!(
        service
            .reconcile(context(), request.identity)
            .await
            .unwrap(),
        HistoryWriteOutcome::Committed(receipt.clone())
    );
    assert_eq!(
        store.state("p").unwrap().generation,
        receipt.after_generation
    );
    assert_eq!(
        store.retained_changes()[0].sequence,
        receipt.change_sequence
    );
}
#[tokio::test]
async fn deferred_deadline_and_preparation_limits_never_publish_samples() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 3));
    let service = service(
        graph(),
        provider.clone(),
        Arc::new(AllowAllHistoryMutations),
    );
    let context = ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "owner".into(),
        },
        Duration::from_millis(30),
    );
    let request = request(&store, "deadline", vec![sample(0, num(1.0))]);
    service.submit(context, request).await.unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    rejected(
        provider.held.lock().pop().unwrap().publish(),
        HistoryWriteRejection::Deadline,
    );
    assert!(store.is_empty("p"));
    assert!(store.retained_changes().is_empty());
    let small = HistoryMutationService::new(
        service.history_service().clone(),
        Arc::new(AllowAllHistoryMutations),
        HistoryMutationLimits {
            max_prepared_bytes: 100,
            ..HistoryMutationLimits::default()
        },
    )
    .unwrap();
    rejected(
        small
            .submit(
                crate::context(),
                crate::request(&store, "prepared-size", vec![sample(0, num(1.0))]),
            )
            .await
            .unwrap(),
        HistoryWriteRejection::Limit,
    );
    store
        .write(
            "p",
            vec![
                HisItem {
                    ts: sample(0, num(1.0)).ts.dt,
                    val: num(1.0),
                },
                HisItem {
                    ts: sample(1, num(2.0)).ts.dt,
                    val: num(2.0),
                },
            ],
        )
        .unwrap();
    let narrow = HistoryMutationService::new(
        service.history_service().clone(),
        Arc::new(AllowAllHistoryMutations),
        HistoryMutationLimits {
            max_series_samples: 1,
            ..HistoryMutationLimits::default()
        },
    )
    .unwrap();
    rejected(
        narrow
            .submit(
                crate::context(),
                crate::request(&store, "series-size", vec![sample(2, num(3.0))]),
            )
            .await
            .unwrap(),
        HistoryWriteRejection::Limit,
    );
    assert_eq!(store.state("p").unwrap().generation, 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn number_points_require_valid_registered_unit_metadata_before_receipt_reservation() {
    for unit in [
        None,
        Some(Kind::Null),
        Some(Kind::Int(1)),
        Some(Kind::Str("".into())),
        Some(Kind::Str("unknown".into())),
    ] {
        let graph = graph();
        let mut changes = HDict::new();
        changes.set("unit", unit.unwrap_or(Kind::Remove));
        graph.update("p", changes).unwrap();
        let store = HisStore::new();
        let service = service(
            graph,
            Arc::new(store.clone()),
            Arc::new(AllowAllHistoryMutations),
        );
        rejected(
            service
                .submit(
                    context(),
                    request(&store, "schema", vec![sample(0, num(1.0))]),
                )
                .await
                .unwrap(),
            HistoryWriteRejection::Unsupported,
        );
        assert_eq!(store.receipt_count(), 0);
        assert!(store.retained_changes().is_empty());
    }
}
#[tokio::test]
async fn entity_and_history_operation_text_are_independent_domains() {
    use haystack_core::{codecs::entity, graph::EntityOperation};
    let graph = graph();
    let reads = ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let entities = MutationService::new(
        reads.clone(),
        EphemeralMutationStore::new(graph.clone()),
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
    )
    .unwrap();
    let store = HisStore::new();
    let history =
        HistoryService::new(reads, Arc::new(store.clone()), HistoryLimits::default()).unwrap();
    let service = HistoryMutationService::new(
        history,
        Arc::new(AllowAllHistoryMutations),
        HistoryMutationLimits::default(),
    )
    .unwrap();
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("other")));
    let state = graph.state();
    let identity = entity::OperationIdentity {
        operation_id: "same-text".into(),
        dataset: entities.store().dataset(),
        incarnation: state.incarnation,
    };
    let entity_outcome = entities
        .submit(
            context(),
            entity::EntityBatchRequest {
                identity: identity.clone(),
                expected_revision: state.revision,
                operations: vec![EntityOperation::Add(row)],
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        entity_outcome,
        entity::MutationOutcome::Committed(_)
    ));
    let request = request(&store, "same-text", vec![sample(0, num(1.0))]);
    let history_outcome = service.submit(context(), request.clone()).await.unwrap();
    committed(history_outcome.clone());
    assert_eq!(
        entities
            .reconcile(context(), identity.clone())
            .await
            .unwrap(),
        entity_outcome
    );
    assert_eq!(
        service
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        history_outcome
    );
    let mut wrong_authority = request.identity;
    wrong_authority.authority = identity.dataset;
    assert!(matches!(
        service.reconcile(context(), wrong_authority).await.unwrap(),
        HistoryWriteOutcome::Unknown {
            cause: HistoryWriteUnknown::Missing,
            ..
        }
    ));
}
#[tokio::test]
async fn history_mutation_selection_requires_the_exact_owned_read_provider_service() {
    let graph = graph();
    let builder =
        ApplicationBuilder::new(graph, Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let reads = builder.handle().read_service();
    let store = Arc::new(HisStore::new());
    let selected =
        HistoryService::new(reads.clone(), store.clone(), HistoryLimits::default()).unwrap();
    let other = HistoryService::new(reads, store, HistoryLimits::default()).unwrap();
    let mutations = HistoryMutationService::new(
        other,
        Arc::new(AllowAllHistoryMutations),
        HistoryMutationLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        builder
            .owned_history(selected)
            .unwrap()
            .history_mutations(mutations),
        Err(ReadError::Forbidden)
    ));
}

fn service_with_read_limits(
    graph: SharedGraph,
    provider: Arc<dyn HistoryProvider>,
    read_limits: ReadLimits,
    mutation_limits: HistoryMutationLimits,
) -> HistoryMutationService {
    let reads = ReadService::new(graph, Arc::new(AllowAll), read_limits).unwrap();
    let history = HistoryService::new(reads, provider, HistoryLimits::default()).unwrap();
    HistoryMutationService::new(history, Arc::new(AllowAllHistoryMutations), mutation_limits)
        .unwrap()
}
fn large_principal_context() -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "s".repeat(64_000),
        },
        Duration::from_secs(3),
    )
}
#[tokio::test]
async fn review_receipt_lookup_admits_principal_and_outcome_before_missing_or_recognized_copy() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 0));
    let graph = graph();
    let original = request(&store, "large-principal", vec![sample(0, num(1.0))]);
    committed(
        service(
            graph.clone(),
            provider.clone(),
            Arc::new(AllowAllHistoryMutations),
        )
        .submit(large_principal_context(), original.clone())
        .await
        .unwrap(),
    );
    let before = store.state("p").unwrap();
    let changes = store.retained_changes();
    for retained in [false, true] {
        let limits = if retained {
            ReadLimits {
                max_retained_bytes: 1,
                ..ReadLimits::default()
            }
        } else {
            ReadLimits {
                max_work: 1,
                ..ReadLimits::default()
            }
        };
        let narrow = service_with_read_limits(
            graph.clone(),
            provider.clone(),
            limits,
            HistoryMutationLimits::default(),
        );
        for recognized in [false, true] {
            let mut identity = original.identity.clone();
            if !recognized {
                identity.operation_id = "missing".into();
            }
            let result = narrow.reconcile(large_principal_context(), identity).await;
            assert!(
                matches!(result, Err(ReadError::Budget(_))),
                "retained={retained}, recognized={recognized}: {result:?}"
            );
        }
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.receipt_count(), 1);
    assert_eq!(store.state("p").unwrap(), before);
    assert_eq!(store.retained_changes(), changes);
}
#[tokio::test]
async fn review_submission_admits_large_principal_before_new_or_recognized_binding() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 0));
    let graph = graph();
    let original = request(&store, "large-principal", vec![sample(0, num(1.0))]);
    committed(
        service(
            graph.clone(),
            provider.clone(),
            Arc::new(AllowAllHistoryMutations),
        )
        .submit(large_principal_context(), original.clone())
        .await
        .unwrap(),
    );
    let before = store.state("p").unwrap();
    let changes = store.retained_changes();
    for retained in [false, true] {
        let limits = if retained {
            ReadLimits {
                max_retained_bytes: 64_000,
                ..ReadLimits::default()
            }
        } else {
            ReadLimits {
                max_work: 8_192,
                ..ReadLimits::default()
            }
        };
        let narrow = service_with_read_limits(
            graph.clone(),
            provider.clone(),
            limits,
            HistoryMutationLimits::default(),
        );
        for recognized in [false, true] {
            let submitted = if recognized {
                original.clone()
            } else {
                request(&store, "new-large-principal", vec![sample(1, num(2.0))])
            };
            let result = narrow.submit(large_principal_context(), submitted).await;
            assert!(
                matches!(
                    result,
                    Ok(HistoryWriteOutcome::Rejected {
                        reason: HistoryWriteRejection::Limit,
                        ..
                    }) | Err(ReadError::Budget(_))
                ),
                "retained={retained}, recognized={recognized}: {result:?}"
            );
        }
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.receipt_count(), 1);
    assert_eq!(store.state("p").unwrap(), before);
    assert_eq!(store.retained_changes(), changes);
}
#[tokio::test]
async fn review_existing_series_stops_for_work_before_a_late_invalid_unit() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 0));
    let mut items: Vec<_> = (0..200)
        .map(|second| HisItem {
            ts: sample(second, num(1.0)).ts.dt,
            val: num(1.0),
        })
        .collect();
    items.last_mut().unwrap().val =
        Kind::Number(Number::new(1.0, Some("invalid-unit-sentinel".into())));
    store.write("p", items).unwrap();
    let before = store.state("p").unwrap();
    let changes = store.retained_changes();
    let narrow = service_with_read_limits(
        graph(),
        provider.clone(),
        ReadLimits {
            max_work: 40_000,
            ..ReadLimits::default()
        },
        HistoryMutationLimits::default(),
    );
    rejected(
        narrow
            .submit(
                context(),
                request(&store, "bounded-scan", vec![sample(201, num(2.0))]),
            )
            .await
            .unwrap(),
        HistoryWriteRejection::Limit,
    );
    // With sufficient work the same source reaches the independent sentinel.
    rejected(
        service(
            graph(),
            provider.clone(),
            Arc::new(AllowAllHistoryMutations),
        )
        .submit(
            context(),
            request(&store, "sentinel-reached", vec![sample(201, num(2.0))]),
        )
        .await
        .unwrap(),
        HistoryWriteRejection::Unsupported,
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.receipt_count(), 0);
    assert_eq!(store.state("p").unwrap(), before);
    assert_eq!(store.retained_changes(), changes);
}
#[tokio::test]
async fn review_oversized_native_unit_is_bounded_before_registry_lookup() {
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 0));
    store
        .write(
            "p",
            vec![HisItem {
                ts: sample(0, num(1.0)).ts.dt,
                val: Kind::Number(Number::new(1.0, Some("x".repeat(2 * 1024 * 1024)))),
            }],
        )
        .unwrap();
    let before = store.state("p").unwrap();
    let changes = store.retained_changes();
    let narrow = service_with_read_limits(
        graph(),
        provider.clone(),
        ReadLimits::default(),
        HistoryMutationLimits {
            max_prepared_bytes: 4096,
            ..HistoryMutationLimits::default()
        },
    );
    rejected(
        narrow
            .submit(
                context(),
                request(&store, "oversized-unit", vec![sample(1, num(2.0))]),
            )
            .await
            .unwrap(),
        HistoryWriteRejection::Limit,
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.receipt_count(), 0);
    assert_eq!(store.state("p").unwrap(), before);
    assert_eq!(store.retained_changes(), changes);
}
#[tokio::test]
async fn review_scoped_admission_rejects_second_offsets_without_changing_native_history() {
    let graph = graph();
    graph.write(|graph| {
        graph
            .update("p", {
                let mut row = HDict::new();
                row.set("tz", Kind::Str("New_York".into()));
                row
            })
            .unwrap();
    });
    let store = HisStore::new();
    let provider = Arc::new(Fixture::new(store.clone(), 0));
    let utc = chrono::DateTime::parse_from_rfc3339("1880-06-01T00:00:00Z").unwrap();
    let offset = haystack_core::kinds::offset_at("New_York", utc).unwrap();
    assert_ne!(offset.local_minus_utc() % 60, 0);
    let timestamp = HDateTime::new(utc.with_timezone(&offset), "New_York");
    store
        .write(
            "p",
            vec![HisItem {
                ts: timestamp.dt,
                val: num(1.0),
            }],
        )
        .unwrap();
    let before = store.state("p").unwrap();
    let changes = store.retained_changes();
    let writes = service(graph, provider.clone(), Arc::new(AllowAllHistoryMutations));
    let submitted = request(
        &store,
        "second-offset",
        vec![HistorySample {
            ts: timestamp.clone(),
            val: num(2.0),
        }],
    );
    rejected(
        writes.submit(context(), submitted).await.unwrap(),
        HistoryWriteRejection::Unsupported,
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.receipt_count(), 0);
    assert_eq!(store.state("p").unwrap(), before);
    assert_eq!(store.retained_changes(), changes);
    assert_eq!(store.read("p", None, None)[0].ts, timestamp.dt);
    let read_request = HistoryReadRequest {
        id: "p".into(),
        range: "1880-05-31".into(),
    };
    let history = writes.history_service();
    let native = history
        .collect(context(), read_request.clone())
        .await
        .unwrap();
    assert_eq!(native.samples.len(), 1);
    assert_eq!(native.samples[0].ts, timestamp);
    for output in [H4Codec::Zinc, H4Codec::JsonV3, H4Codec::Json] {
        let body = haystack_core::codecs::history::encode_request(
            &read_request,
            haystack_core::codecs::codec_for("text/zinc").unwrap(),
        )
        .unwrap();
        let admission = history.read_service().begin(context()).await.unwrap();
        assert!(matches!(
            history
                .wire_admitted(admission, body, H4Codec::Zinc, output)
                .await,
            Err(ReadError::Projection)
        ));
    }
    assert_eq!(store.state("p").unwrap(), before);
    assert_eq!(store.read("p", None, None)[0].ts, timestamp.dt);
}
