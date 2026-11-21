//! Single-point typed history adapters over the existing owned services.
use super::*;
use crate::{
    ApiError, HistoryMutationService, HistoryService, TypedInvocationResponse,
    registry::{Handler, Registry},
    typed_http,
};
use haystack_core::{
    codecs::{history, history_mutation},
    data::HGrid,
};

enum Operation {
    Read {
        request: history::HistoryReadRequest,
        scoped: bool,
    },
    Write(history_mutation::HistoryWriteRequest),
}
pub(super) struct Prepared {
    registry: Arc<Registry>,
    function: &'static str,
    request: typed_http::Request,
    operation: Operation,
}
impl Prepared {
    pub fn new(
        registry: Arc<Registry>,
        handler: Handler,
        mut request: typed_http::Request,
        args: &HDict,
        budget: &mut Budget,
    ) -> Result<Self, ApiError> {
        let Some(Kind::Grid(grid)) = args.get("req") else {
            return Err(ApiError::InvalidArgs);
        };
        // Binding already validated these native types. Consume only supported
        // structural column contexts, never arbitrary domain metadata. In
        // particular an Int/Float context cannot become Number by stripping it.
        let mut grid = (**grid).clone();
        normalize_columns(&mut grid, handler)?;
        let operation = if handler == Handler::HisRead {
            let scoped = grid.meta.has("history");
            let request = if scoped {
                history::request_from_grid(&grid)
            } else {
                history::legacy_request_from_grid(grid)
            }
            .map_err(|_| ApiError::InvalidArgs)?;
            Operation::Read { request, scoped }
        } else {
            Operation::Write(
                history_mutation::request_from_grid(&grid).map_err(|_| ApiError::InvalidArgs)?,
            )
        };
        request.args = HDict::new();
        budget.check()?;
        let name = if handler == Handler::HisRead {
            "ph.api::hisRead"
        } else {
            "ph.api::hisWrite"
        };
        Ok(Self {
            registry,
            function: name,
            request,
            operation,
        })
    }
    pub async fn execute(
        self,
        mut admission: ReadAdmission,
        history: Option<HistoryService>,
        mutations: Option<HistoryMutationService>,
    ) -> Result<TypedInvocationResponse, ApiError> {
        let Self {
            registry,
            function,
            request,
            operation,
        } = self;
        admission.budget_mut().typed_catalog = Some(registry.observation().clone());
        let inner = admission.inner.clone();
        match operation {
            Operation::Read {
                request: read,
                scoped,
            } => {
                let service = history.ok_or(ApiError::Unavailable)?;
                let mut session = service.open_typed(admission.stage()?, read).await?;
                let result = session.collect_borrowed().await?;
                session.release_completed();
                drop(session);
                if !scoped && result.terminal != history::HistoryTerminal::Complete {
                    return Err(ApiError::Unavailable);
                }
                admission
                    .run_task(move |_, budget| {
                        Ok((|| {
                            let mut grid = history::result_grid(&result)
                                .map_err(|_| ApiError::NotAcceptable)?;
                            if !scoped {
                                grid.meta.remove_tag("history");
                            }
                            finish(&inner, &registry, grid, &request, function, budget)
                        })())
                    })
                    .await
                    .map_err(ApiError::from)?
            }
            Operation::Write(write) => {
                let service = mutations.ok_or(ApiError::Unavailable)?;
                // Encode a bounded identity-only uncertainty envelope before
                // submission. Its reservation is cumulative and its disclosure
                // still uses the original deadline and session authority.
                // Reserve both bounded native outcome-grid constructions before
                // either identity/control copy and before crossing submission.
                let receipt_storage = history_mutation::MAX_RECEIPT_BYTES * 2;
                admission
                    .budget_mut()
                    .charge(BudgetKind::Retained, receipt_storage)?;
                admission
                    .budget_mut()
                    .charge(BudgetKind::Work, receipt_storage)?;
                let unknown = history_mutation::HistoryWriteOutcome::Unknown {
                    identity: write.identity.clone(),
                    cause: history_mutation::HistoryWriteUnknown::Provider,
                };
                let unknown_grid =
                    history_mutation::outcome_grid(&unknown).map_err(|_| ApiError::Internal)?;
                let fallback_registry = registry.clone();
                let fallback_request = typed_http::Request {
                    args: HDict::new(),
                    version: request.version,
                    output: request.output,
                    gzip: request.gzip,
                };
                let fallback = admission
                    .stage()?
                    .run_task(move |_, budget| {
                        let wire = fallback_registry
                            .observation()
                            .callable(function)
                            .ok_or(ReadError::Unavailable)?;
                        Ok(typed_http::encode(
                            Kind::Grid(Box::new(unknown_grid)),
                            &fallback_request,
                            wire,
                            budget,
                        ))
                    })
                    .await
                    .map_err(ApiError::from)??;
                let outcome = service.submit_admitted(admission.stage()?, write).await;
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        admission.budget.take();
                        return Ok(fallback);
                    }
                };
                // After crossing submission, every failure is ambiguous to the
                // caller, including projection, compression and catalog change.
                let result = admission
                    .run_task(move |_, budget| {
                        Ok((|| {
                            let grid = history_mutation::outcome_grid(&outcome)
                                .map_err(|_| ApiError::Internal)?;
                            finish(&inner, &registry, grid, &request, function, budget)
                        })())
                    })
                    .await;
                match result {
                    Ok(Ok(response)) => Ok(response),
                    Ok(Err(ApiError::Permission)) | Err(ReadError::Forbidden) => {
                        Err(ApiError::Permission)
                    }
                    _ => Ok(fallback),
                }
            }
        }
    }
}
fn finish(
    inner: &Inner,
    registry: &Registry,
    grid: HGrid,
    request: &typed_http::Request,
    function: &str,
    budget: &mut Budget,
) -> Result<TypedInvocationResponse, ApiError> {
    loop {
        if let Some(result) = inner.graph.read_for(budget.wait_quantum()?, |graph| {
            current_observation(graph, registry)
        }) {
            result?;
            break;
        }
    }
    let wire = registry
        .observation()
        .callable(function)
        .ok_or(ApiError::Internal)?;
    let value = Kind::Grid(Box::new(grid));
    registry
        .catalog()
        .fit_result(&wire.function, &value)
        .map_err(|_| ApiError::Internal)?;
    typed_http::encode(value, request, wire, budget)
}
fn normalize_columns(grid: &mut HGrid, handler: Handler) -> Result<(), ApiError> {
    for col in &mut grid.cols {
        if let Some(of) = col.meta.get("of") {
            let Kind::Ref(of) = of else {
                return Err(ApiError::InvalidArgs);
            };
            let allowed = match (handler, col.name.as_str()) {
                (Handler::HisRead, "id") => of.val == "sys::Ref",
                (Handler::HisRead, "range") => of.val == "sys::Str",
                (Handler::HisWrite, "ts") => of.val == "sys::DateTime",
                (Handler::HisWrite, "val") => {
                    matches!(of.val.as_str(), "sys::Number" | "sys::Bool" | "sys::Str")
                }
                _ => false,
            };
            if !allowed || of.dis.is_some() {
                return Err(ApiError::InvalidArgs);
            }
            col.meta.remove_tag("of");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use haystack_core::{
        codecs::{codec_for, history_mutation, jeto},
        graph::{EntityGraph, SharedGraph},
        kinds::{HDateTime, HRef, Number},
    };
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    fn graph() -> SharedGraph {
        let graph = SharedGraph::new(EntityGraph::new());
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("p")));
        row.set("his", Kind::Marker);
        row.set("kind", Kind::Str("Number".into()));
        row.set("unit", Kind::Str("°F".into()));
        row.set("tz", Kind::Str("UTC".into()));
        graph.add(row).unwrap();
        graph
    }
    fn request(store: &HisStore) -> HistoryWriteRequest {
        let state = store.state("p").unwrap();
        HistoryWriteRequest {
            identity: HistoryOperationIdentity {
                authority: state.authority,
                point: "p".into(),
                incarnation: state.incarnation,
                operation_id: "typed-write".into(),
            },
            expected_generation: state.generation,
            samples: vec![HistorySample {
                ts: HDateTime::new(
                    chrono::DateTime::from_timestamp(1_717_200_000, 0)
                        .unwrap()
                        .fixed_offset(),
                    "UTC",
                ),
                val: Kind::Number(Number::unitless(7.0)),
            }],
        }
    }
    fn input(request: &HistoryWriteRequest) -> TypedInvocationInput {
        TypedInvocationInput {
            operation: "ph.api::hisWrite".into(),
            post: true,
            versions: vec!["5".into()],
            content_types: vec!["text/zinc".into()],
            accepts: vec!["application/json".into()],
            body: history_mutation::encode_request(request, codec_for("text/zinc").unwrap())
                .unwrap(),
            ..Default::default()
        }
    }
    fn context(principal: Principal) -> ReadContext {
        ReadContext::with_timeout(principal, Duration::from_secs(3))
    }
    fn services() -> (
        ReadService,
        HistoryService,
        HistoryMutationService,
        HisStore,
    ) {
        let reads = ReadService::new(graph(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let store = HisStore::new();
        let history = HistoryService::new(
            reads.clone(),
            Arc::new(store.clone()),
            HistoryLimits::default(),
        )
        .unwrap();
        let writes = HistoryMutationService::new(
            history.clone(),
            Arc::new(AllowAllHistoryMutations),
            HistoryMutationLimits::default(),
        )
        .unwrap();
        (reads, history, writes, store)
    }
    #[tokio::test]
    async fn final_encoding_exhaustion_preserves_identity_unknown_and_committed_receipt() {
        let (reads, history, writes, store) = services();
        let request = request(&store);
        let mut admission = reads.begin(context(Principal::Anonymous)).await.unwrap();
        let observer = Arc::new(Mutex::new(admission.budget_mut().clone()));
        let calls = Arc::new(AtomicUsize::new(0));
        let hook_calls = calls.clone();
        admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
            if hook_calls.fetch_add(1, Ordering::SeqCst) == 1 {
                let mut budget = observer.lock();
                let remaining = budget.retained_remaining();
                budget.charge(BudgetKind::Retained, remaining).unwrap();
            }
        }));
        let response = admission
            .invoke_wire_with_history(input(&request), Some(history), Some(writes.clone()))
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let Kind::Grid(grid) = jeto::decode(
            &response.body,
            &jeto::Context::standard(),
            Some("sys::Grid"),
            jeto::Limits::default(),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            history_mutation::outcome_from_grid(&grid).unwrap(),
            HistoryWriteOutcome::Unknown {
                identity: request.identity.clone(),
                cause: HistoryWriteUnknown::Provider
            }
        );
        assert!(matches!(
            writes
                .reconcile(context(Principal::Anonymous), request.identity)
                .await
                .unwrap(),
            HistoryWriteOutcome::Committed(_)
        ));
        assert_eq!(store.state("p").unwrap().generation, 1);
        assert_eq!(reads.load().admitted, 0);
    }
    #[tokio::test]
    async fn revoked_session_cannot_disclose_unknown_but_reauthorized_receipt_resolves_commit() {
        let (reads, history, writes, store) = services();
        let request = request(&store);
        let session = SubscriptionSession::trusted("writer", Duration::from_secs(30)).unwrap();
        let principal = session.principal().clone();
        let mut admission = reads.begin(context(principal.clone())).await.unwrap();
        admission
            .bind_wire_session(principal.clone(), session.clone())
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
            if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                session.close();
            }
        }));
        assert!(matches!(
            admission
                .invoke_wire_with_history(input(&request), Some(history), Some(writes.clone()))
                .await,
            Err(ApiError::Permission)
        ));
        assert!(matches!(
            writes
                .reconcile(context(principal), request.identity)
                .await
                .unwrap(),
            HistoryWriteOutcome::Committed(_)
        ));
        assert_eq!(store.state("p").unwrap().generation, 1);
    }
    struct Delayed {
        store: HisStore,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    impl HistoryProvider for Delayed {
        fn open(
            &self,
            id: String,
            start: chrono::DateTime<chrono::FixedOffset>,
            end: chrono::DateTime<chrono::FixedOffset>,
            budget: HistoryPullBudget,
        ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                self.store.open(id, start, end, budget).await
            })
        }
    }
    fn read_input() -> TypedInvocationInput {
        TypedInvocationInput {
            operation: "ph.api::hisRead".into(),
            post: true,
            versions: vec!["5".into()],
            content_types: vec!["text/zinc".into()],
            accepts: vec!["application/json".into()],
            body: b"ver:\"3.0\"\nid,range\n@p,\"2024-06-01\"\n".to_vec(),
            ..Default::default()
        }
    }
    #[tokio::test]
    async fn original_deadline_and_session_expiry_retain_work_until_provider_finishes() {
        for expiry in [true, false] {
            let reads =
                ReadService::new(graph(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let provider = Arc::new(Delayed {
                store: HisStore::new(),
                entered: entered.clone(),
                release: release.clone(),
            });
            let history =
                HistoryService::new(reads.clone(), provider, HistoryLimits::default()).unwrap();
            let session = SubscriptionSession::trusted(
                "reader",
                if expiry {
                    Duration::from_millis(150)
                } else {
                    Duration::from_secs(30)
                },
            )
            .unwrap();
            let mut admission = reads
                .begin(ReadContext::with_timeout(
                    session.principal().clone(),
                    if expiry {
                        Duration::from_secs(3)
                    } else {
                        Duration::from_millis(150)
                    },
                ))
                .await
                .unwrap();
            admission
                .bind_wire_session(session.principal().clone(), session)
                .unwrap();
            let task = tokio::spawn(async move {
                admission
                    .invoke_wire_with_history(read_input(), Some(history), None)
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), entered.notified())
                .await
                .unwrap();
            let response = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            assert!(response.is_err());
            assert_eq!(
                reads.load().admitted,
                1,
                "provider still owns the original admission"
            );
            release.notify_one();
            tokio::time::timeout(Duration::from_secs(2), async {
                while reads.load().admitted != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
    }
    struct HiddenPoint;
    impl ReadPolicy for HiddenPoint {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(Self))
        }
    }
    impl PolicySnapshot for HiddenPoint {
        fn scope_key(&self) -> &str {
            "hidden-point-v1"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            true
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, id: &str) -> bool {
            id != "p"
        }
        fn tag(&self, _: &str, _: &str) -> bool {
            true
        }
        fn reference(&self, _: &str) -> bool {
            true
        }
        fn reference_display(&self, _: &str) -> bool {
            false
        }
        fn catalog(&self, _: CatalogKind, _: &str) -> bool {
            true
        }
        fn nominal_provenance(&self, _: &haystack_core::kinds::NominalScalar) -> bool {
            true
        }
    }
    struct UnreachableProvider(Arc<AtomicUsize>);
    impl HistoryProvider for UnreachableProvider {
        fn open(
            &self,
            _: String,
            _: chrono::DateTime<chrono::FixedOffset>,
            _: chrono::DateTime<chrono::FixedOffset>,
            _: HistoryPullBudget,
        ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(HistoryProviderError::Failed) })
        }
    }
    #[tokio::test]
    async fn missing_denied_and_multipoint_requests_do_not_open_read_only_provider() {
        let reads =
            ReadService::new(graph(), Arc::new(HiddenPoint), ReadLimits::default()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let history = HistoryService::new(
            reads.clone(),
            Arc::new(UnreachableProvider(calls.clone())),
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
        for id in ["p", "missing"] {
            let mut input = read_input();
            input.body = format!("ver:\"3.0\"\nid,range\n@{id},\"2024-06-01\"\n").into_bytes();
            assert!(matches!(
                reads
                    .begin(context(Principal::Anonymous))
                    .await
                    .unwrap()
                    .invoke_wire_with_history(input, Some(history.clone()), None)
                    .await,
                Err(ApiError::Unavailable)
            ));
        }
        let mut multi = read_input();
        multi.body = b"ver:\"3.0\" range:\"2024-06-01\"\nid\n@p\n@missing\n".to_vec();
        assert!(matches!(
            reads
                .begin(context(Principal::Anonymous))
                .await
                .unwrap()
                .invoke_wire_with_history(multi, Some(history.clone()), None)
                .await,
            Err(ApiError::InvalidArgs)
        ));
        let mut write = read_input();
        write.operation = "ph.api::hisWrite".into();
        assert!(matches!(
            reads
                .begin(context(Principal::Anonymous))
                .await
                .unwrap()
                .invoke_wire_with_history(write, Some(history), None)
                .await,
            Err(ApiError::UnknownFunction(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn catalog_rebase_between_binding_and_submission_cannot_publish() {
        let (reads, history, writes, store) = services();
        let request = request(&store);
        let mut admission = reads.begin(context(Principal::Anonymous)).await.unwrap();
        let graph = reads.native_graph().unwrap();
        let calls = AtomicUsize::new(0);
        admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                graph.set_namespace(DefNamespace::new());
            }
        }));
        let response = admission
            .invoke_wire_with_history(input(&request), Some(history), Some(writes.clone()))
            .await
            .unwrap();
        let Kind::Grid(grid) = jeto::decode(
            &response.body,
            &jeto::Context::standard(),
            Some("sys::Grid"),
            jeto::Limits::default(),
        )
        .unwrap() else {
            panic!()
        };
        assert!(matches!(
            history_mutation::outcome_from_grid(&grid).unwrap(),
            HistoryWriteOutcome::Unknown { .. }
        ));
        assert_eq!(store.state("p").unwrap().generation, 0);
        assert!(matches!(
            writes
                .reconcile(context(Principal::Anonymous), request.identity)
                .await
                .unwrap(),
            HistoryWriteOutcome::Unknown {
                cause: HistoryWriteUnknown::Missing,
                ..
            }
        ));
    }
    struct RebaseAfterCommit {
        store: HisStore,
        graph: SharedGraph,
    }
    impl HistoryProvider for RebaseAfterCommit {
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
            self.store.history_write_capability()
        }
        fn commit_history(&self, prepared: PreparedHistoryMutation) -> HistoryWriteOutcome {
            let outcome = self.store.commit_history(prepared);
            self.graph.set_namespace(DefNamespace::new());
            outcome
        }
    }
    #[tokio::test]
    async fn catalog_rebase_after_publication_is_unknown_and_receipt_stays_committed() {
        let reads = ReadService::new(graph(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let store = HisStore::new();
        let provider = Arc::new(RebaseAfterCommit {
            store: store.clone(),
            graph: reads.native_graph().unwrap(),
        });
        let history =
            HistoryService::new(reads.clone(), provider, HistoryLimits::default()).unwrap();
        let writes = HistoryMutationService::new(
            history.clone(),
            Arc::new(AllowAllHistoryMutations),
            HistoryMutationLimits::default(),
        )
        .unwrap();
        let request = request(&store);
        let response = reads
            .begin(context(Principal::Anonymous))
            .await
            .unwrap()
            .invoke_wire_with_history(input(&request), Some(history), Some(writes.clone()))
            .await
            .unwrap();
        let Kind::Grid(grid) = jeto::decode(
            &response.body,
            &jeto::Context::standard(),
            Some("sys::Grid"),
            jeto::Limits::default(),
        )
        .unwrap() else {
            panic!()
        };
        assert!(matches!(
            history_mutation::outcome_from_grid(&grid).unwrap(),
            HistoryWriteOutcome::Unknown { .. }
        ));
        assert_eq!(store.state("p").unwrap().generation, 1);
        assert!(matches!(
            writes
                .reconcile(context(Principal::Anonymous), request.identity)
                .await
                .unwrap(),
            HistoryWriteOutcome::Committed(_)
        ));
    }
    struct RevokeBeforeCommit {
        store: HisStore,
        session: SubscriptionSession,
        before_commit: Option<Mutex<std::sync::mpsc::Receiver<()>>>,
        completed: Mutex<Option<tokio::sync::oneshot::Sender<HistoryWriteOutcome>>>,
    }
    impl HistoryProvider for RevokeBeforeCommit {
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
            self.store.history_write_capability()
        }
        fn commit_history(&self, prepared: PreparedHistoryMutation) -> HistoryWriteOutcome {
            self.session.close();
            if let Some(release) = &self.before_commit {
                // Dropping the sender also releases the worker if the test
                // panics, so runtime shutdown cannot hang on this fixture.
                let _ = release.lock().recv();
            }
            let outcome = self.store.commit_history(prepared);
            let _ = self
                .completed
                .lock()
                .take()
                .expect("one commit attempt")
                .send(outcome.clone());
            outcome
        }
    }
    #[tokio::test]
    async fn provider_cannot_publish_after_synchronous_session_revocation() {
        revocation_before_commit(false).await;
    }
    #[tokio::test]
    async fn session_revocation_can_return_while_provider_receipt_is_pending() {
        revocation_before_commit(true).await;
    }
    async fn revocation_before_commit(paused: bool) {
        let reads = ReadService::new(graph(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let store = HisStore::new();
        let session = SubscriptionSession::trusted("writer", Duration::from_secs(30)).unwrap();
        let principal = session.principal().clone();
        let (release, before_commit) = std::sync::mpsc::channel();
        let (completed, completion) = tokio::sync::oneshot::channel();
        let provider = Arc::new(RevokeBeforeCommit {
            store: store.clone(),
            session: session.clone(),
            before_commit: paused.then_some(Mutex::new(before_commit)),
            completed: Mutex::new(Some(completed)),
        });
        let history =
            HistoryService::new(reads.clone(), provider, HistoryLimits::default()).unwrap();
        let writes = HistoryMutationService::new(
            history.clone(),
            Arc::new(AllowAllHistoryMutations),
            HistoryMutationLimits::default(),
        )
        .unwrap();
        let request = request(&store);
        let mut admission = reads.begin(context(principal.clone())).await.unwrap();
        admission
            .bind_wire_session(principal.clone(), session)
            .unwrap();
        assert!(
            admission
                .invoke_wire_with_history(input(&request), Some(history), Some(writes.clone()))
                .await
                .is_err()
        );
        if paused {
            // A prompt request error does not join its blocking provider. The
            // reservation and work lease remain live until that attempt ends.
            assert_eq!(reads.load().admitted, 1);
            assert_eq!(store.state("p").unwrap().generation, 0);
            assert!(store.is_empty("p"));
            assert_eq!(
                writes
                    .reconcile(context(principal.clone()), request.identity.clone())
                    .await
                    .unwrap(),
                HistoryWriteOutcome::Unknown {
                    identity: request.identity.clone(),
                    cause: HistoryWriteUnknown::Pending,
                }
            );
            release.send(()).unwrap();
        }
        // The unpaused case still attempts publication immediately after
        // closing the session, independently of cancellation propagation. Wait
        // for that attempt before requiring its receipt to be terminal.
        let outcome = tokio::time::timeout(Duration::from_secs(3), completion)
            .await
            .expect("provider completion")
            .expect("provider outcome");
        assert_eq!(store.state("p").unwrap().generation, 0);
        assert!(store.is_empty("p"));
        let receipt = writes
            .reconcile(context(principal), request.identity)
            .await
            .unwrap();
        assert_eq!(receipt, outcome);
        assert!(
            matches!(
                receipt,
                HistoryWriteOutcome::Rejected {
                    reason: HistoryWriteRejection::Forbidden | HistoryWriteRejection::Cancelled,
                    ..
                }
            ),
            "receipt after provider completion: {receipt:?}"
        );
    }
}
