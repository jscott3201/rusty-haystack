//! Typed watch dispatch retains the request's exact session, catalog and budget.
use super::*;
use crate::{
    ApiError, StateSubscriptionService, TypedInvocationResponse,
    registry::{Handler, Registry},
    subscription::watch::Request,
    typed_http,
};

pub(super) struct Prepared {
    registry: Arc<Registry>,
    function: &'static str,
    wire: typed_http::Request,
    request: Request,
    session: crate::SubscriptionSession,
    policy_scope: String,
}
impl Prepared {
    pub fn new(
        registry: Arc<Registry>,
        handler: Handler,
        mut wire: typed_http::Request,
        args: &HDict,
        session: crate::SubscriptionSession,
        policy_scope: &str,
        budget: &mut Budget,
    ) -> Result<Self, ApiError> {
        let (function, request) = Request::decode(handler, args, budget)?;
        wire.args = HDict::new();
        Ok(Self {
            registry,
            function,
            wire,
            request,
            session,
            policy_scope: budget.copy_string(policy_scope)?,
        })
    }
    pub async fn execute(
        self,
        mut admission: ReadAdmission,
        service: Option<StateSubscriptionService>,
    ) -> Result<TypedInvocationResponse, ApiError> {
        let service = service.ok_or(ApiError::Unavailable)?;
        admission.budget_mut().typed_catalog = Some(self.registry.observation().clone());
        admission
            .run_task(move |principal, budget| {
                Ok(service.typed_watch(
                    &principal,
                    &self.session,
                    self.request,
                    &self.policy_scope,
                    budget,
                    |grid, budget| {
                        let value = Kind::Grid(Box::new(grid));
                        self.registry
                            .catalog()
                            .fit_result(self.function, &value)
                            .map_err(|_| ApiError::Internal)?;
                        let profile = self
                            .registry
                            .observation()
                            .callable(self.function)
                            .ok_or(ApiError::Unavailable)?;
                        typed_http::encode(value, &self.wire, profile, budget)
                    },
                ))
            })
            .await
            .map_err(ApiError::from)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use haystack_core::{
        codecs::jeto,
        graph::{EntityGraph, SharedGraph},
        kinds::HRef,
        ontology::DefNamespace,
    };
    use std::time::Duration;
    fn setup(limits: ReadLimits) -> (ReadService, StateSubscriptionService, SubscriptionSession) {
        setup_with_policy(limits, Arc::new(AllowAll))
    }
    fn setup_with_policy(
        limits: ReadLimits,
        policy: Arc<dyn ReadPolicy>,
    ) -> (ReadService, StateSubscriptionService, SubscriptionSession) {
        let graph = SharedGraph::new(EntityGraph::new());
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("a")));
        row.set("dis", Kind::Str("A watched entity".into()));
        graph.add(row).unwrap();
        let reads = ReadService::new(graph.clone(), policy, limits).unwrap();
        let service = StateSubscriptionService::new(
            reads.clone(),
            EntityMutationStore::new(graph),
            SubscriptionLimits::default(),
        )
        .unwrap();
        (
            reads,
            service,
            SubscriptionSession::trusted("watcher", Duration::from_secs(30)).unwrap(),
        )
    }
    fn context(principal: Principal) -> ReadContext {
        ReadContext::with_timeout(principal, Duration::from_secs(3))
    }
    fn input(operation: &str, body: &str) -> TypedInvocationInput {
        TypedInvocationInput {
            operation: operation.into(),
            post: true,
            versions: vec!["5".into()],
            content_types: vec!["text/zinc".into()],
            accepts: vec!["application/json".into()],
            body: body.as_bytes().to_vec(),
            ..Default::default()
        }
    }
    fn create() -> TypedInvocationInput {
        input(
            "ph.api::watchSub",
            "ver:\"3.0\" watchDis:\"test\"\nid\n@a\n",
        )
    }
    async fn admission(reads: &ReadService, session: &SubscriptionSession) -> ReadAdmission {
        let mut admission = reads
            .begin(context(session.principal().clone()))
            .await
            .unwrap();
        admission
            .bind_wire_session(session.principal().clone(), session.clone())
            .unwrap();
        admission
    }
    async fn completed(reads: &ReadService) {
        // max_concurrent=1 makes this new admission an actual worker-exit
        // barrier after prompt cancellation, not a timing-based assertion.
        drop(reads.begin(context(Principal::Anonymous)).await.unwrap());
    }
    #[tokio::test]
    async fn catalog_fences_initial_create_at_lookup_and_again_after_output_preparation() {
        for after_lookup in [true, false] {
            let (reads, service, session) = setup(ReadLimits {
                max_concurrent: 1,
                ..ReadLimits::default()
            });
            let mut admission = admission(&reads, &session).await;
            let graph = reads.native_graph().unwrap();
            let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                graph.set_namespace(DefNamespace::new());
            });
            if after_lookup {
                admission.budget_mut().typed_lookup_hook = Some(hook);
            } else {
                admission.budget_mut().typed_encode_hook = Some(hook);
            }
            assert!(matches!(
                admission
                    .invoke_wire_with_services(create(), None, None, Some(service.clone()))
                    .await,
                Err(ApiError::Unavailable)
            ));
            completed(&reads).await;
            assert_eq!(service.active_watches(), 0);
            assert_eq!(service.binding_count(), 0);
        }
    }
    #[tokio::test]
    async fn output_budget_cancellation_and_exact_session_stop_cannot_publish_prepared_create() {
        for failure in ["output", "representation", "retained", "cancel", "session"] {
            let (reads, service, session) = setup(ReadLimits {
                max_concurrent: 1,
                max_output_bytes: if failure == "output" { 16 } else { 1_048_576 },
                ..ReadLimits::default()
            });
            let mut admission = admission(&reads, &session).await;
            if failure == "retained" {
                let observer = Arc::new(Mutex::new(admission.budget_mut().clone()));
                admission.budget_mut().typed_encode_hook = Some(Arc::new(move || {
                    let mut budget = observer.lock();
                    let remaining = budget.retained_remaining();
                    budget.charge(BudgetKind::Retained, remaining).unwrap();
                }));
            }
            if failure == "cancel" {
                let cancel = admission.budget_mut().cancel.clone();
                admission.budget_mut().typed_encode_hook = Some(Arc::new(move || cancel.cancel()));
            }
            if failure == "session" {
                let session = session.clone();
                admission.budget_mut().typed_encode_hook = Some(Arc::new(move || session.close()));
            }
            let mut request = create();
            if failure == "representation" {
                request.query = "box=none".into();
            }
            assert!(
                admission
                    .invoke_wire_with_services(request, None, None, Some(service.clone()))
                    .await
                    .is_err(),
                "{failure}"
            );
            completed(&reads).await;
            assert_eq!(service.active_watches(), 0, "{failure}");
            assert_eq!(service.binding_count(), 0, "{failure}");
        }
    }
    #[tokio::test]
    async fn strict_request_shape_errors_never_create_watch() {
        let (reads, service, session) = setup(ReadLimits {
            max_concurrent: 1,
            ..ReadLimits::default()
        });
        for body in [
            "ver:\"3.0\" watchDis:\"test\"\nid\n@a,@b\n",
            "ver:\"3.0\" watchDis:\"test\"\nid\n@a garbage\n",
        ] {
            assert!(matches!(
                admission(&reads, &session)
                    .await
                    .invoke_wire_with_services(
                        input("ph.api::watchSub", body),
                        None,
                        None,
                        Some(service.clone())
                    )
                    .await,
                Err(ApiError::InvalidArgs)
            ));
            assert_eq!(service.active_watches(), 0);
        }
        let response = admission(&reads, &session)
            .await
            .invoke_wire_with_services(create(), None, None, Some(service.clone()))
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
        assert_eq!(grid.rows.len(), 1);
        assert_eq!(service.active_watches(), 1);
    }
    struct ChangingPolicy(Arc<std::sync::atomic::AtomicBool>);
    struct DisjointSnapshot(bool);
    impl ReadPolicy for ChangingPolicy {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(DisjointSnapshot(
                self.0.load(std::sync::atomic::Ordering::Acquire),
            )))
        }
    }
    impl PolicySnapshot for DisjointSnapshot {
        fn scope_key(&self) -> &str {
            if self.0 {
                "entity-only-B"
            } else {
                "function-only-A"
            }
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            !self.0
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, id: &str) -> bool {
            self.0 || id != "secret"
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
    #[tokio::test]
    async fn disjoint_binding_and_capture_policy_scopes_cannot_publish_a_watch() {
        let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (reads, service, session) = setup_with_policy(
            ReadLimits {
                max_concurrent: 1,
                ..ReadLimits::default()
            },
            Arc::new(ChangingPolicy(changed.clone())),
        );
        let mut secret = HDict::new();
        secret.set("id", Kind::Ref(HRef::from_val("secret")));
        secret.set("dis", Kind::Str("fixture secret".into()));
        reads.native_graph().unwrap().add(secret).unwrap();
        let mut admission = admission(&reads, &session).await;
        // Binding retains immutable A; every later snapshot is immutable B.
        admission.budget_mut().typed_lookup_hook = Some(Arc::new(move || {
            changed.store(true, std::sync::atomic::Ordering::Release);
        }));
        let result = admission
            .invoke_wire_with_services(
                input(
                    "ph.api::watchSub",
                    "ver:\"3.0\" watchDis:\"test\"\nid\n@secret\n",
                ),
                None,
                None,
                Some(service.clone()),
            )
            .await;
        completed(&reads).await;
        assert_eq!(
            service.active_watches(),
            0,
            "disjoint function/entity permissions must not publish"
        );
        assert_eq!(service.binding_count(), 0);
        assert!(matches!(result, Err(ApiError::Permission)));
    }
}
