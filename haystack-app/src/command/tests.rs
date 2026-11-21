use super::*;
use crate::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, Number},
};
fn point(id: &str, kind: &str) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row.set("point", Kind::Marker);
    row.set("writable", Kind::Marker);
    row.set("kind", Kind::Str(kind.into()));
    row
}
struct Fixture {
    service: SimulatedCommandService,
    session: SubscriptionSession,
    owner: Arc<ApplicationOwner>,
    graph: SharedGraph,
    clock: SimulatorClock,
}
impl Fixture {
    async fn start(limits: CommandLimits, mode: SimulatedCommandMode) -> Self {
        Self::configured(
            limits,
            mode,
            ReadLimits {
                max_concurrent: 1,
                ..ReadLimits::default()
            },
            Arc::new(AllowAllSimulatedCommands),
        )
        .await
    }
    async fn configured(
        limits: CommandLimits,
        mode: SimulatedCommandMode,
        reads: ReadLimits,
        policy: Arc<dyn CommandPolicy>,
    ) -> Self {
        Self::with_policies(limits, mode, reads, policy, Arc::new(AllowAll)).await
    }
    async fn with_policies(
        limits: CommandLimits,
        mode: SimulatedCommandMode,
        reads: ReadLimits,
        policy: Arc<dyn CommandPolicy>,
        read_policy: Arc<dyn ReadPolicy>,
    ) -> Self {
        let graph = SharedGraph::new(EntityGraph::with_changelog_capacity(16));
        let mut row = point("p", "Number");
        row.set("unit", Kind::Str("°C".into()));
        row.set("writeDef", number(20.0));
        graph.add(row).unwrap();
        graph.add(point("b", "Bool")).unwrap();
        graph.add(point("s", "Str")).unwrap();
        let builder = ApplicationBuilder::new(graph.clone(), read_policy, reads).unwrap();
        let clock = SimulatorClock::manual();
        let service = SimulatedCommandService::with_clock(
            builder.handle().read_service(),
            policy,
            mode,
            limits,
            clock.clone(),
        )
        .unwrap();
        let owner = builder
            .simulated_commands(service.clone())
            .unwrap()
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        owner.ready().await.unwrap();
        Self {
            service,
            session: SubscriptionSession::trusted("operator", Duration::from_secs(60)).unwrap(),
            owner: Arc::new(owner),
            graph,
            clock,
        }
    }
    async fn submit(&self, request: CommandRequest) -> CommandOutcome {
        self.service
            .submit(
                ReadContext::with_timeout(self.session.principal().clone(), Duration::from_secs(3)),
                self.session.clone(),
                request,
            )
            .await
    }
    async fn inspect(&self, id: &str) -> haystack_core::data::HGrid {
        match self.submit(CommandRequest::inspect(id)).await {
            CommandOutcome::Inspection(grid) => grid,
            other => panic!("{other:?}"),
        }
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
    }
}
fn number(value: f64) -> Kind {
    Kind::Number(Number::unitless(value))
}
fn cell(grid: &haystack_core::data::HGrid, level: usize) -> Option<&Kind> {
    grid.rows[level - 1].get("val")
}
fn accepted(outcome: CommandOutcome) {
    assert!(matches!(outcome, CommandOutcome::Accepted), "{outcome:?}");
}
fn rejected(outcome: CommandOutcome, error: ApiError) {
    assert!(
        matches!(&outcome, CommandOutcome::Rejected(e) if *e == error),
        "expected {error:?}, got {outcome:?}"
    );
}
#[tokio::test]
async fn priorities_normalization_relinquishment_and_nullable_default() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    let before = f.graph.read(|g| g.state());
    accepted(f.submit(CommandRequest::set("p", 8, number(23.0))).await);
    accepted(
        f.submit(CommandRequest::set("p", 4, number(24.0)).with_who("local label"))
            .await,
    );
    let rows = f.inspect("p").await;
    assert_eq!(
        cell(&rows, 8),
        Some(&Kind::Number(Number::new(23.0, Some("°C".into()))))
    );
    assert_eq!(rows.rows[7].get("who"), Some(&Kind::Str("operator".into())));
    assert_eq!(
        rows.rows[3].get("who"),
        Some(&Kind::Str("local label".into()))
    );
    assert_eq!(rows.rows.iter().position(|r| r.has("val")), Some(3));
    assert_eq!(f.service.inner.state.lock().points["p"].effective_level, 4);
    accepted(f.submit(CommandRequest::relinquish("p", 4)).await);
    assert_eq!(
        f.inspect("p").await.rows.iter().position(|r| r.has("val")),
        Some(7)
    );
    accepted(f.submit(CommandRequest::relinquish("p", 8)).await);
    assert_eq!(
        f.inspect("p").await.rows.iter().position(|r| r.has("val")),
        Some(16)
    );
    accepted(f.submit(CommandRequest::relinquish("p", 17)).await);
    assert!(f.inspect("p").await.rows.iter().all(|r| !r.has("val")));
    assert_eq!(f.service.inner.state.lock().points["p"].effective_level, 17);
    assert!(f.service.inner.state.lock().points["p"].effective.is_none());
    assert!(f.inspect("b").await.rows.iter().all(|r| !r.has("val")));
    assert_eq!(f.graph.read(|g| g.state()), before);
    f.close().await;
}
#[tokio::test]
async fn strict_value_units_duration_and_capacity_reject_without_effects() {
    let f = Fixture::start(
        CommandLimits {
            max_points: 1,
            max_value_bytes: 16,
            max_who_bytes: 16,
            ..CommandLimits::default()
        },
        SimulatedCommandMode::Writable,
    )
    .await;
    for request in [
        CommandRequest::set("p", 0, number(2.0)),
        CommandRequest::set("p", 18, number(2.0)),
        CommandRequest::set("p", 8, Kind::Int(2)),
        CommandRequest::set("p", 8, Kind::Float(haystack_core::kinds::Float::new(2.0))),
        CommandRequest::set("p", 8, number(f64::NAN)),
        CommandRequest::set("p", 8, number(f64::INFINITY)),
        CommandRequest::set("p", 8, Kind::Number(Number::new(3.0, Some("°F".into())))),
        CommandRequest::set("p", 7, number(2.0)).with_duration(Duration::from_secs(1)),
        CommandRequest::relinquish("p", 8).with_duration(Duration::from_secs(1)),
        CommandRequest::inspect("p").with_duration(Duration::from_secs(1)),
        CommandRequest::set("p", 8, number(2.0)).with_duration(Duration::ZERO),
        CommandRequest::set("p", 8, number(2.0)).with_duration(Duration::from_secs(3601)),
        CommandRequest::set("p", 8, number(2.0)).with_who("x".repeat(17)),
    ] {
        rejected(f.submit(request).await, ApiError::InvalidArgs);
    }
    assert!(f.service.inner.state.lock().points.is_empty());
    f.inspect("p").await;
    rejected(
        f.submit(CommandRequest::set("b", 8, Kind::Bool(true)))
            .await,
        ApiError::Unavailable,
    );
    f.close().await;
}
#[tokio::test]
async fn manual_expiry_renewal_replacement_and_session_independence() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    accepted(
        f.submit(CommandRequest::set("p", 8, number(22.0)).with_duration(Duration::from_secs(10)))
            .await,
    );
    f.clock.advance(Duration::from_secs(5)).unwrap();
    accepted(
        f.submit(CommandRequest::set("p", 8, number(23.0)).with_duration(Duration::from_secs(10)))
            .await,
    );
    f.clock.advance(Duration::from_secs(5)).unwrap();
    assert_eq!(
        cell(&f.inspect("p").await, 8),
        Some(&Kind::Number(Number::new(23.0, Some("°C".into()))))
    );
    f.clock.advance(Duration::from_secs(5)).unwrap();
    assert!(cell(&f.inspect("p").await, 8).is_none());
    accepted(
        f.submit(CommandRequest::set("p", 8, number(24.0)).with_duration(Duration::from_secs(10)))
            .await,
    );
    accepted(f.submit(CommandRequest::set("p", 8, number(25.0))).await);
    f.clock.advance(Duration::from_secs(10)).unwrap();
    assert!(cell(&f.inspect("p").await, 8).is_some());
    accepted(
        f.submit(CommandRequest::set("p", 8, number(26.0)).with_duration(Duration::from_secs(10)))
            .await,
    );
    f.session.close();
    f.clock.advance(Duration::from_secs(10)).unwrap();
    f.service.maintain();
    assert!(
        f.service.inner.state.lock().points["p"].slots[7]
            .value
            .is_none()
    );
    f.close().await;
}
#[tokio::test]
async fn schema_and_delete_recreate_fences_preserve_accepted_state() {
    for deleted in [false, true] {
        let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
        accepted(f.submit(CommandRequest::set("p", 8, number(22.0))).await);
        if deleted {
            let row = f.graph.remove("p").unwrap();
            f.graph.add(row).unwrap();
        } else {
            let mut changes = HDict::new();
            changes.set("writeDef", number(30.0));
            f.graph.update("p", changes).unwrap();
        }
        rejected(
            f.submit(CommandRequest::set("p", 8, number(40.0))).await,
            ApiError::Unavailable,
        );
        assert_eq!(
            f.service.inner.state.lock().points["p"].slots[7].value,
            Some(Kind::Number(Number::new(22.0, Some("°C".into()))))
        );
        f.close().await;
    }
}
#[tokio::test]
async fn read_only_owner_and_missing_point_remain_distinct() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::InspectOnly).await;
    f.inspect("p").await;
    rejected(
        f.submit(CommandRequest::set("p", 8, number(22.0))).await,
        ApiError::NotImplemented,
    );
    rejected(
        f.submit(CommandRequest::inspect("missing")).await,
        ApiError::UnknownEntity,
    );
    let wrong = ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(1));
    rejected(
        f.service
            .submit(wrong, f.session.clone(), CommandRequest::inspect("p"))
            .await,
        ApiError::Permission,
    );
    f.close().await;
}
#[test]
fn abandon_and_dispatch_have_one_winner() {
    for _ in 0..128 {
        let attempt = CommandAttempt::default();
        let other = attempt.clone();
        let worker = std::thread::spawn(move || other.dispatch());
        let result = attempt.classify(ApiError::Timeout);
        let dispatched = worker.join().unwrap().is_ok();
        assert_eq!(dispatched, result == ApiError::CommandUnknown);
        assert!(attempt.dispatch().is_err());
    }
}
#[tokio::test]
async fn cancellation_before_dispatch_is_rejected_but_after_dispatch_is_unknown() {
    for after in [false, true] {
        let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
        let token = CancellationToken::new();
        let hook_token = token.clone();
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || hook_token.cancel());
        if after {
            *f.service.inner.after_dispatch.lock() = Some(hook);
        } else {
            *f.service.inner.before_dispatch.lock() = Some(hook);
        }
        let result = f
            .service
            .submit(
                ReadContext::new(
                    f.session.principal().clone(),
                    Instant::now() + Duration::from_secs(3),
                    token,
                ),
                f.session.clone(),
                CommandRequest::set("p", 8, number(22.0)),
            )
            .await;
        assert_eq!(
            matches!(result, CommandOutcome::Unknown),
            after,
            "{result:?}"
        );
        // Admission with one permit is an actual worker-exit barrier.
        drop(
            f.service
                .read_service()
                .begin(ReadContext::with_timeout(
                    Principal::Anonymous,
                    Duration::from_secs(3),
                ))
                .await
                .unwrap(),
        );
        assert_eq!(f.service.inner.state.lock().points.contains_key("p"), after);
        f.close().await;
    }
}
#[tokio::test]
async fn stale_prepared_graph_rejects_and_post_dispatch_panic_is_unknown() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    let graph = f.graph.clone();
    *f.service.inner.before_dispatch.lock() = Some(Arc::new(move || {
        let mut change = HDict::new();
        change.set("writeDef", number(30.0));
        graph.update("p", change).unwrap();
    }));
    rejected(
        f.submit(CommandRequest::set("p", 8, number(22.0))).await,
        ApiError::Unavailable,
    );
    assert!(f.service.inner.state.lock().points.is_empty());
    *f.service.inner.before_dispatch.lock() = None;
    *f.service.inner.after_dispatch.lock() =
        Some(Arc::new(|| panic!("simulated lost acknowledgment")));
    assert!(matches!(
        f.submit(CommandRequest::set("p", 8, number(22.0))).await,
        CommandOutcome::Unknown
    ));
    drop(
        f.service
            .read_service()
            .begin(ReadContext::with_timeout(
                Principal::Anonymous,
                Duration::from_secs(3),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(
        f.service.inner.state.lock().points["p"].slots[7].generation,
        1
    );
    f.close().await;
}
#[tokio::test]
async fn prompt_unknown_retains_worker_admission_until_actual_exit() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    *f.service.inner.after_dispatch.lock() = Some(Arc::new(move || {
        if let Some(tx) = entered_tx.lock().take() {
            tx.send(()).unwrap();
        }
        let _ = release_rx.lock().recv();
    }));
    let service = f.service.clone();
    let session = f.session.clone();
    let cancel = CancellationToken::new();
    let call_cancel = cancel.clone();
    let call = tokio::spawn(async move {
        service
            .submit(
                ReadContext::new(
                    session.principal().clone(),
                    Instant::now() + Duration::from_secs(3),
                    call_cancel,
                ),
                session,
                CommandRequest::set("p", 8, number(22.0)),
            )
            .await
    });
    entered_rx.await.unwrap();
    cancel.cancel();
    assert!(matches!(call.await.unwrap(), CommandOutcome::Unknown));
    let second = f
        .service
        .read_service()
        .begin(ReadContext::with_timeout(
            Principal::Anonymous,
            Duration::from_millis(20),
        ))
        .await;
    assert!(matches!(second, Err(ReadError::Deadline)));
    release_tx.send(()).unwrap();
    drop(
        f.service
            .read_service()
            .begin(ReadContext::with_timeout(
                Principal::Anonymous,
                Duration::from_secs(3),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(
        f.service.inner.state.lock().points["p"].slots[7].generation,
        1
    );
    f.close().await;
}
fn wire_write() -> TypedInvocationInput {
    TypedInvocationInput {
        operation: "ph.api::pointWrite".into(),
        post: true,
        versions: vec!["5".into()],
        content_types: vec!["application/json".into()],
        accepts: vec!["application/json".into()],
        body: br#"{"id":"p","level":8,"val":{"spec":"sys::Number","val":"22"}}"#.to_vec(),
        ..Default::default()
    }
}
async fn bound(f: &Fixture) -> ReadAdmission {
    let mut admission = f
        .service
        .read_service()
        .begin(ReadContext::with_timeout(
            f.session.principal().clone(),
            Duration::from_secs(3),
        ))
        .await
        .unwrap();
    admission
        .bind_wire_session(f.session.principal().clone(), f.session.clone())
        .unwrap();
    admission
}
#[tokio::test]
async fn typed_encoding_loss_and_final_disclosure_keep_unknown_after_dispatch() {
    for failure in [
        "encode-cancel",
        "encode-panic",
        "final-session",
        "final-owner",
    ] {
        let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
        let mut admission = bound(&f).await;
        if failure == "encode-cancel" {
            let cancel = admission.cancellation();
            admission.budget_mut().typed_encode_hook = Some(Arc::new(move || cancel.cancel()));
        }
        if failure == "encode-panic" {
            admission.budget_mut().typed_encode_hook =
                Some(Arc::new(|| panic!("lost encoding acknowledgment")));
        }
        let attempt = CommandAttempt::default();
        let result = admission
            .invoke_wire_with_commands(
                wire_write(),
                None,
                None,
                None,
                Some(f.service.clone()),
                attempt,
            )
            .await;
        if failure.starts_with("encode") {
            assert!(matches!(result, Err(ApiError::CommandUnknown)));
        } else {
            let response = result.unwrap();
            if failure == "final-session" {
                f.session.close();
            } else {
                begin_close(&f.owner);
            }
            assert_eq!(response.disclosure.check(), Err(ApiError::CommandUnknown));
        }
        drop(
            f.service
                .read_service()
                .begin(ReadContext::with_timeout(
                    Principal::Anonymous,
                    Duration::from_secs(1),
                ))
                .await,
        );
        assert_eq!(
            f.service.inner.state.lock().points["p"].slots[7].generation,
            1
        );
        f.close().await;
    }
}
#[tokio::test]
async fn sealed_before_dispatch_and_exact_service_selection_never_accept() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    let admission = bound(&f).await;
    let owner = f.owner.clone();
    *f.service.inner.before_dispatch.lock() = Some(Arc::new(move || begin_close(&owner)));
    assert!(matches!(
        admission
            .invoke_wire_with_commands(
                wire_write(),
                None,
                None,
                None,
                Some(f.service.clone()),
                CommandAttempt::default()
            )
            .await,
        Err(ApiError::Unavailable)
    ));
    assert!(f.service.inner.state.lock().points.is_empty());
    f.close().await;
    let graph = SharedGraph::new(EntityGraph::new());
    let builder =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let foreign = ReadService::new(graph, Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let service = SimulatedCommandService::new(
        foreign,
        Arc::new(AllowAllSimulatedCommands),
        SimulatedCommandMode::Writable,
        CommandLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        builder.simulated_commands(service),
        Err(ReadError::Forbidden)
    ));
}
#[tokio::test]
async fn schema_change_then_restore_and_history_gap_still_reject_retained_identity() {
    for gap in [false, true] {
        let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
        accepted(f.submit(CommandRequest::set("p", 8, number(22.0))).await);
        for index in 0..if gap { 32 } else { 2 } {
            let mut changes = HDict::new();
            changes.set(
                if gap { "dis" } else { "writeDef" },
                if gap {
                    Kind::Str(index.to_string())
                } else {
                    number(if index == 0 { 30.0 } else { 20.0 })
                },
            );
            f.graph.update("p", changes).unwrap();
        }
        rejected(
            f.submit(CommandRequest::relinquish("p", 8)).await,
            ApiError::Unavailable,
        );
        assert!(
            f.service.inner.state.lock().points["p"].slots[7]
                .value
                .is_some()
        );
        f.close().await;
    }
}
#[tokio::test]
async fn duration_starts_at_acceptance_after_preparation() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    let clock = f.clock.clone();
    *f.service.inner.before_dispatch.lock() = Some(Arc::new(move || {
        clock.advance(Duration::from_secs(20)).unwrap()
    }));
    accepted(
        f.submit(CommandRequest::set("p", 8, number(22.0)).with_duration(Duration::from_secs(10)))
            .await,
    );
    *f.service.inner.before_dispatch.lock() = None;
    assert!(cell(&f.inspect("p").await, 8).is_some());
    f.clock.advance(Duration::from_secs(9)).unwrap();
    assert!(cell(&f.inspect("p").await, 8).is_some());
    f.clock.advance(Duration::from_secs(1)).unwrap();
    assert!(cell(&f.inspect("p").await, 8).is_none());
    f.close().await;
}
#[tokio::test]
async fn pending_and_retained_capacity_fail_before_dispatch() {
    let f = Fixture::configured(
        CommandLimits {
            max_pending: 1,
            ..CommandLimits::default()
        },
        SimulatedCommandMode::Writable,
        ReadLimits {
            max_concurrent: 2,
            ..ReadLimits::default()
        },
        Arc::new(AllowAllSimulatedCommands),
    )
    .await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    *f.service.inner.before_dispatch.lock() = Some(Arc::new(move || {
        if let Some(tx) = entered_tx.lock().take() {
            tx.send(()).unwrap();
            let _ = release_rx.lock().recv();
        }
    }));
    let service = f.service.clone();
    let session = f.session.clone();
    let first = tokio::spawn(async move {
        service
            .submit(
                ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(3)),
                session,
                CommandRequest::set("p", 8, number(22.0)),
            )
            .await
    });
    entered_rx.await.unwrap();
    rejected(
        f.submit(CommandRequest::set("p", 1, number(99.0))).await,
        ApiError::Unavailable,
    );
    assert!(f.service.inner.state.lock().points.is_empty());
    release_tx.send(()).unwrap();
    accepted(first.await.unwrap());
    assert_eq!(f.service.inner.state.lock().points["p"].effective_level, 8);
    f.close().await;
    let f = Fixture::start(
        CommandLimits {
            max_retained_bytes: 1,
            ..CommandLimits::default()
        },
        SimulatedCommandMode::Writable,
    )
    .await;
    rejected(
        f.submit(CommandRequest::set("p", 8, number(22.0))).await,
        ApiError::Unavailable,
    );
    assert!(f.service.inner.state.lock().points.is_empty());
    f.close().await;
}
struct Revocable(Arc<AtomicBool>);
impl CommandPolicy for Revocable {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn CommandPolicySnapshot>, ReadError> {
        Ok(Arc::new(Revoked(self.0.load(Ordering::Acquire))))
    }
}
struct Revoked(bool);
impl CommandPolicySnapshot for Revoked {
    fn scope_key(&self) -> &str {
        if self.0 { "revoked" } else { "authorized" }
    }
    fn action(&self, _: CommandAction) -> bool {
        !self.0
    }
    fn point(&self, _: &str, _: CommandAction) -> bool {
        !self.0
    }
}
#[tokio::test]
async fn current_command_policy_fences_both_sides_of_dispatch() {
    for after in [false, true] {
        let revoked = Arc::new(AtomicBool::new(false));
        let f = Fixture::configured(
            CommandLimits::default(),
            SimulatedCommandMode::Writable,
            ReadLimits::default(),
            Arc::new(Revocable(revoked.clone())),
        )
        .await;
        let hook: Arc<dyn Fn() + Send + Sync> =
            Arc::new(move || revoked.store(true, Ordering::Release));
        if after {
            *f.service.inner.after_dispatch.lock() = Some(hook);
        } else {
            *f.service.inner.before_dispatch.lock() = Some(hook);
        }
        let outcome = f.submit(CommandRequest::set("p", 8, number(22.0))).await;
        if after {
            assert!(matches!(outcome, CommandOutcome::Unknown));
        } else {
            rejected(outcome, ApiError::Permission);
        }
        assert_eq!(f.service.inner.state.lock().points.contains_key("p"), after);
        f.close().await;
    }
}
#[tokio::test]
async fn request_deadline_abandons_prepared_intent_but_keeps_dispatched_unknown() {
    for after in [false, true] {
        let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let entered_tx = Mutex::new(Some(entered_tx));
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            entered_tx.lock().take().unwrap().send(()).unwrap();
            let _ = release_rx.lock().recv();
        });
        if after {
            *f.service.inner.after_dispatch.lock() = Some(hook);
        } else {
            *f.service.inner.before_dispatch.lock() = Some(hook);
        }
        let service = f.service.clone();
        let session = f.session.clone();
        let call = tokio::spawn(async move {
            service
                .submit(
                    ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(1)),
                    session,
                    CommandRequest::set("p", 8, number(22.0)),
                )
                .await
        });
        entered_rx.await.unwrap();
        let result = call.await.unwrap();
        if after {
            assert!(matches!(result, CommandOutcome::Unknown));
        } else {
            rejected(result, ApiError::Timeout);
        }
        release_tx.send(()).unwrap();
        drop(
            f.service
                .read_service()
                .begin(ReadContext::with_timeout(
                    Principal::Anonymous,
                    Duration::from_secs(3),
                ))
                .await
                .unwrap(),
        );
        assert_eq!(f.service.inner.state.lock().points.contains_key("p"), after);
        f.close().await;
    }
}
#[tokio::test]
async fn output_budget_loss_is_unknown_after_local_acceptance() {
    let f = Fixture::configured(
        CommandLimits::default(),
        SimulatedCommandMode::Writable,
        ReadLimits {
            max_output_bytes: 1,
            ..ReadLimits::default()
        },
        Arc::new(AllowAllSimulatedCommands),
    )
    .await;
    let response = bound(&f)
        .await
        .invoke_wire_with_commands(
            wire_write(),
            None,
            None,
            None,
            Some(f.service.clone()),
            CommandAttempt::default(),
        )
        .await;
    assert!(matches!(response, Err(ApiError::CommandUnknown)));
    assert_eq!(
        f.service.inner.state.lock().points["p"].slots[7].generation,
        1
    );
    f.close().await;
}

// Polling close once requests the real owner transition without waiting for
// the very worker held at the test barrier. Cleanup is awaited by Fixture.
fn begin_close(owner: &ApplicationOwner) {
    use std::future::Future;
    let mut close = Box::pin(owner.close());
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let _ = close.as_mut().poll(&mut context);
}
#[tokio::test]
async fn bounded_maintenance_contention_waits_without_rejecting_a_valid_command() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    let service = f.service.clone();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = service.inner.state.lock();
        held_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(5));
    });
    held_rx.await.unwrap();
    let (prepared_tx, prepared_rx) = tokio::sync::oneshot::channel();
    let prepared_tx = Mutex::new(Some(prepared_tx));
    *f.service.inner.before_dispatch.lock() = Some(Arc::new(move || {
        if let Some(tx) = prepared_tx.lock().take() {
            tx.send(()).unwrap();
        }
    }));
    let service = f.service.clone();
    let session = f.session.clone();
    let mut call = tokio::spawn(async move {
        service
            .submit(
                ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(3)),
                session,
                CommandRequest::set("p", 8, number(22.0)),
            )
            .await
    });
    prepared_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut call)
            .await
            .is_err()
    );
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    accepted(call.await.unwrap());
    f.close().await;
}

// The read snapshot signals only after both final snapshots have captured their
// immutable decisions. Revocation then happens while publication is contended.
struct ContendedPolicies {
    command_revoked: AtomicBool,
    read_revoked: AtomicBool,
    final_snapshot: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}
impl CommandPolicy for ContendedPolicies {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn CommandPolicySnapshot>, ReadError> {
        Ok(Arc::new(Revoked(
            self.command_revoked.load(Ordering::Acquire),
        )))
    }
}
impl ReadPolicy for ContendedPolicies {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        let snapshot = Arc::new(RevokedRead(self.read_revoked.load(Ordering::Acquire)));
        if let Some(tx) = self.final_snapshot.lock().take() {
            tx.send(()).unwrap();
        }
        Ok(snapshot)
    }
}
struct RevokedRead(bool);
impl PolicySnapshot for RevokedRead {
    fn scope_key(&self) -> &str {
        if self.0 {
            "read-revoked"
        } else {
            "read-authorized"
        }
    }
    fn function(&self, _: &FunctionIdentity) -> bool {
        !self.0
    }
    fn operation(&self, _: ReadOperation) -> bool {
        !self.0
    }
    fn entity(&self, _: &str) -> bool {
        !self.0
    }
    fn tag(&self, _: &str, _: &str) -> bool {
        !self.0
    }
    fn reference(&self, _: &str) -> bool {
        !self.0
    }
    fn reference_display(&self, _: &str) -> bool {
        !self.0
    }
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        !self.0
    }
    fn nominal_provenance(&self, _: &haystack_core::kinds::NominalScalar) -> bool {
        !self.0
    }
}
async fn policy_revocation_during_contention(read: bool, graph_retry: bool, typed: bool) {
    let policies = Arc::new(ContendedPolicies {
        command_revoked: AtomicBool::new(false),
        read_revoked: AtomicBool::new(false),
        final_snapshot: Mutex::new(None),
    });
    let f = Fixture::with_policies(
        CommandLimits::default(),
        SimulatedCommandMode::Writable,
        ReadLimits {
            max_concurrent: 1,
            ..ReadLimits::default()
        },
        policies.clone(),
        policies.clone(),
    )
    .await;
    accepted(f.submit(CommandRequest::set("p", 8, number(21.0))).await);
    let old_value = f.service.inner.state.lock().points["p"].slots[7]
        .value
        .clone();
    let old_generation = f.service.inner.state.lock().points["p"].slots[7].generation;
    let dispatched = Arc::new(AtomicBool::new(false));
    let dispatched_hook = dispatched.clone();
    *f.service.inner.after_dispatch.lock() = Some(Arc::new(move || {
        dispatched_hook.store(true, Ordering::Release);
    }));
    let (prepared_tx, prepared_rx) = tokio::sync::oneshot::channel();
    let prepared_tx = Mutex::new(Some(prepared_tx));
    let (prepare_release_tx, prepare_release_rx) = std::sync::mpsc::channel();
    let prepare_release_rx = Mutex::new(prepare_release_rx);
    let (snapshot_tx, snapshot_rx) = tokio::sync::oneshot::channel();
    let snapshot_tx = Mutex::new(Some(snapshot_tx));
    let snapshot_policies = policies.clone();
    *f.service.inner.before_dispatch.lock() = Some(Arc::new(move || {
        prepared_tx.lock().take().unwrap().send(()).unwrap();
        prepare_release_rx
            .lock()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        *snapshot_policies.final_snapshot.lock() = snapshot_tx.lock().take();
    }));
    let (retry_tx, retry_rx) = tokio::sync::oneshot::channel();
    let retry_tx = Mutex::new(Some(retry_tx));
    let (retry_release_tx, retry_release_rx) = std::sync::mpsc::channel();
    let retry_release_rx = Mutex::new(retry_release_rx);
    let retry_hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Some(tx) = retry_tx.lock().take() {
            tx.send(()).unwrap();
            retry_release_rx
                .lock()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    });
    if graph_retry {
        *f.service.inner.publication_retry.lock() = Some(retry_hook);
    } else {
        *f.service.inner.store_retry.lock() = Some(retry_hook);
    }
    let service = f.service.clone();
    let session = f.session.clone();
    let admission = if typed { Some(bound(&f).await) } else { None };
    let typed_attempt = CommandAttempt::default();
    let call_attempt = typed_attempt.clone();
    let call = tokio::spawn(async move {
        if let Some(admission) = admission {
            match admission
                .invoke_wire_with_commands(
                    wire_write(),
                    None,
                    None,
                    None,
                    Some(service),
                    call_attempt.clone(),
                )
                .await
            {
                Ok(_) => CommandOutcome::Accepted,
                Err(error) => match call_attempt.classify(error) {
                    ApiError::CommandUnknown => CommandOutcome::Unknown,
                    error => CommandOutcome::Rejected(error),
                },
            }
        } else {
            service
                .submit(
                    ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(3)),
                    session,
                    CommandRequest::set("p", 8, number(99.0)),
                )
                .await
        }
    });
    prepared_rx.await.unwrap();
    let holder_service = f.service.clone();
    let graph = f.graph.clone();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        if graph_retry {
            graph.write(|_| {
                held_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            });
        } else {
            let _guard = holder_service.inner.state.lock();
            held_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    });
    held_rx.await.unwrap();
    prepare_release_tx.send(()).unwrap();
    snapshot_rx.await.unwrap();
    // Snapshot capture alone cannot prove contention: the worker could be
    // descheduled before its try-lock. Wait for that acquisition to fail, then
    // keep the worker at the retry barrier through revocation and holder exit.
    retry_rx.await.unwrap();
    if read {
        policies.read_revoked.store(true, Ordering::Release);
    } else {
        policies.command_revoked.store(true, Ordering::Release);
    }
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    retry_release_tx.send(()).unwrap();
    let result = call.await.unwrap();
    // Preserve cleanup even when the pre-repair assertion fails.
    let did_dispatch = dispatched.load(Ordering::Acquire);
    let (generation, value) = {
        let state = f.service.inner.state.lock();
        (
            state.points["p"].slots[7].generation,
            state.points["p"].slots[7].value.clone(),
        )
    };
    assert_eq!(f.service.inner.pending.load(Ordering::Acquire), 0);
    drop(
        f.service
            .read_service()
            .begin(ReadContext::with_timeout(
                f.session.principal().clone(),
                Duration::from_secs(1),
            ))
            .await
            .unwrap(),
    );
    f.close().await;
    rejected(result, ApiError::Permission);
    assert!(!did_dispatch);
    if typed {
        assert_eq!(typed_attempt.0.load(Ordering::Acquire), 2);
    }
    assert_eq!(generation, old_generation);
    assert_eq!(value, old_value);
}
#[tokio::test]
async fn command_policy_revoked_during_store_wait_rejects_without_dispatch() {
    for typed in [false, true] {
        policy_revocation_during_contention(false, false, typed).await;
    }
}
#[tokio::test]
async fn read_policy_revoked_during_store_wait_rejects_without_dispatch() {
    for typed in [false, true] {
        policy_revocation_during_contention(true, false, typed).await;
    }
}
#[tokio::test]
async fn command_policy_revoked_during_graph_retry_rejects_without_dispatch() {
    for typed in [false, true] {
        policy_revocation_during_contention(false, true, typed).await;
    }
}
#[tokio::test]
async fn read_policy_revoked_during_graph_retry_rejects_without_dispatch() {
    for typed in [false, true] {
        policy_revocation_during_contention(true, true, typed).await;
    }
}
#[tokio::test]
async fn relinquishment_records_controller_and_expiry_retains_controller() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    accepted(
        f.submit(CommandRequest::set("p", 8, number(23.0)).with_who("Alice"))
            .await,
    );
    accepted(
        f.submit(CommandRequest::relinquish("p", 8).with_who("Bob"))
            .await,
    );
    let relinquished = f.inspect("p").await;
    accepted(
        f.submit(
            CommandRequest::set("p", 8, number(24.0))
                .with_who("Carol")
                .with_duration(Duration::from_secs(10)),
        )
        .await,
    );
    f.clock.advance(Duration::from_secs(10)).unwrap();
    let expired = f.inspect("p").await;
    f.close().await;
    assert!(cell(&relinquished, 8).is_none());
    assert_eq!(
        relinquished.rows[7].get("who"),
        Some(&Kind::Str("Bob".into()))
    );
    assert!(cell(&expired, 8).is_none());
    assert_eq!(expired.rows[7].get("who"), Some(&Kind::Str("Carol".into())));
}

#[tokio::test]
async fn retained_controller_bytes_enforce_store_capacity_even_on_null_slots() {
    let f = Fixture::start(CommandLimits::default(), SimulatedCommandMode::Writable).await;
    f.inspect("p").await;
    let base_bytes = f.service.inner.state.lock().bytes;
    f.close().await;
    let f = Fixture::start(
        CommandLimits {
            max_retained_bytes: base_bytes + 3,
            ..CommandLimits::default()
        },
        SimulatedCommandMode::Writable,
    )
    .await;
    accepted(
        f.submit(CommandRequest::relinquish("p", 8).with_who("Bob"))
            .await,
    );
    let before = f.service.inner.state.lock().bytes;
    let outcome = f
        .submit(CommandRequest::relinquish("p", 9).with_who("x"))
        .await;
    let after = f.service.inner.state.lock().bytes;
    let generation = f.service.inner.state.lock().points["p"].slots[8].generation;
    f.close().await;
    rejected(outcome, ApiError::Unavailable);
    assert_eq!(before, base_bytes + 3);
    assert_eq!(after, before);
    assert_eq!(generation, 0);
}
