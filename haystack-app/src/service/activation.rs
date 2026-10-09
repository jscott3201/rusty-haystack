//! Admitted, bounded catalog activation for the trusted owning application.
//!
//! The selected policy is reject-on-invalid-affected-data. Core performs
//! off-lock preparation, coherent chunked validation and the final
//! compare-and-publish; this module supplies the application's authority,
//! activation-specific limits, cancellation, owner sealing, fixed-handler
//! admission and the commit fence. Nothing here repairs data or inserts
//! defaults.
//!
//! Capacity: every attempt validates every associated record in the graph
//! (the union of old and candidate associations), in chunks of
//! `chunk_records` under separate read guards. Work is roughly proportional to
//! records plus selected slots and followed references; it is charged against
//! [`CatalogActivationLimits`], not per-request [`ReadLimits`]. Sustained
//! entity writes end in `Conflict` after `max_revalidations` restarts.
//! Incremental revalidation is a follow-up, not implemented here.
use super::*;
use haystack_core::xeto::catalog::{ActivatedCatalog, ActivationControl, ActivationError, Catalog};
use std::sync::atomic::{AtomicU8, Ordering};

/// A published activation. Runtime publication generation, selection digest
/// and the upstream source revision stored in nominal values stay distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogActivation {
    pub catalog_generation: u64,
    pub selection: String,
}

/// Failed activation; nothing was published. A caller stop that races the
/// worker's commit point is resolved by the commit fence: once the worker
/// commits, the awaiting caller reports its actual outcome instead of the
/// stop. Exceptions: a worker panic after commit is reported as
/// `Control(Unavailable)`, and a dropped activation future loses its outcome;
/// reconcile through the catalog generation or the typed-function selection
/// identity. `Rejected` displays without hidden identities; privileged detail
/// is read explicitly from it. `Unsupported` names the handler declaration
/// that the candidate cannot bind (a defensive guard: pinned profiles lacking
/// a handler are rejected earlier by provenance). Re-activating an identical
/// selection still publishes a new observation, advances the generation,
/// wakes once and makes in-flight retained typed requests `Unavailable`.
pub type CatalogActivationError = ActivationError<ReadError>;

/// Activation budgets, separate from per-request [`ReadLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogActivationLimits {
    /// Upper bound on the activation's own admission deadline when started
    /// through [`ReadService::activate_catalog_with`]. An existing
    /// [`ReadAdmission`] keeps its original deadline.
    pub max_duration: std::time::Duration,
    /// Cumulative validation work across all attempts.
    pub max_work: usize,
    /// Bytes retained while fitting one record; released per record.
    pub max_retained_bytes: usize,
    /// Records validated under one read guard.
    pub chunk_records: usize,
    /// Restarts after entity-only changes before reporting `Conflict`.
    pub max_revalidations: usize,
    /// Value depth while fitting nested Dicts and followed references,
    /// independent of [`ReadLimits::max_value_depth`]. The strict fitter has
    /// its own hard bound of 64.
    pub max_depth: usize,
}
impl Default for CatalogActivationLimits {
    fn default() -> Self {
        Self {
            max_duration: std::time::Duration::from_secs(60),
            max_work: 64 * 1024 * 1024,
            max_retained_bytes: 16 * 1024 * 1024,
            chunk_records: 256,
            max_revalidations: 3,
            max_depth: MAX_ACTIVATION_DEPTH,
        }
    }
}
/// Same ceiling as per-request deadlines; also keeps `now + max_duration`
/// representable.
const MAX_ACTIVATION_DURATION: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_ACTIVATION_DEPTH: usize = 64;
impl CatalogActivationLimits {
    pub fn validate(&self) -> Result<(), ReadError> {
        if self.max_duration.is_zero()
            || self.max_duration > MAX_ACTIVATION_DURATION
            || self.max_depth == 0
            || self.max_depth > MAX_ACTIVATION_DEPTH
            || self.max_work == 0
            || self.max_retained_bytes == 0
            || self.chunk_records == 0
            || self.chunk_records > 65_536
            || self.max_revalidations > 64
        {
            return Err(ReadError::InvalidLimits);
        }
        Ok(())
    }
}

/// Resolves the race between a caller stop and the worker's commit point.
/// Exactly one side wins: the worker commits (and the caller must report its
/// outcome) or the caller abandons (and the worker must not publish).
pub(crate) struct CommitFence(AtomicU8);
const PENDING: u8 = 0;
const COMMITTED: u8 = 1;
const ABANDONED: u8 = 2;
impl CommitFence {
    fn new() -> Self {
        Self(AtomicU8::new(PENDING))
    }
    fn commit(&self) -> bool {
        self.0
            .compare_exchange(PENDING, COMMITTED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    /// `false` once the worker has committed.
    pub(crate) fn abandon(&self) -> bool {
        match self
            .0
            .compare_exchange(PENDING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(state) => state == ABANDONED,
        }
    }
}

struct Control<'a> {
    budget: &'a mut Budget,
    limits: CatalogActivationLimits,
    fence: Arc<CommitFence>,
    work: usize,
    retained: usize,
    /// The fixed inventory bound to the candidate before publication, so no
    /// fallible step follows the commit point.
    bound: Option<Arc<crate::registry::Registry>>,
}
impl ActivationControl for Control<'_> {
    type Error = ReadError;
    fn work(&mut self, amount: usize) -> Result<(), ReadError> {
        // Sealing is observed during validation too, so drain does not wait
        // for a whole graph scan that can no longer publish.
        publish_check(self.budget)?;
        self.work = self.work.saturating_add(amount);
        if self.work > self.limits.max_work {
            return Err(ReadError::Budget(BudgetKind::Work));
        }
        Ok(())
    }
    fn retain(&mut self, bytes: usize) -> Result<(), ReadError> {
        self.retained = self.retained.saturating_add(bytes);
        if self.retained > self.limits.max_retained_bytes {
            return Err(ReadError::Budget(BudgetKind::Retained));
        }
        Ok(())
    }
    fn release(&mut self, bytes: usize) {
        self.retained = self.retained.saturating_sub(bytes);
    }
    fn depth(&mut self, depth: usize) -> Result<(), ReadError> {
        // Activation's own bound, not the per-request value depth.
        self.budget.check()?;
        if depth > self.limits.max_depth {
            return Err(ReadError::Budget(BudgetKind::Depth));
        }
        Ok(())
    }
    fn wait(&mut self) -> Result<std::time::Duration, ReadError> {
        publish_check(self.budget)?;
        self.budget.wait_quantum()
    }
    fn chunk_records(&self) -> usize {
        self.limits.chunk_records
    }
    fn max_revalidations(&self) -> usize {
        self.limits.max_revalidations
    }
    fn admit(&mut self, candidate: &Arc<ActivatedCatalog>) -> Result<(), CatalogActivationError> {
        // The fixed supported inventory must bind before publication.
        let registry =
            crate::registry::Registry::bind_checked(candidate.clone()).map_err(|error| {
                ActivationError::Unsupported {
                    declaration: error.declaration,
                    reason: error.reason.into(),
                }
            })?;
        self.bound = Some(Arc::new(registry));
        Ok(())
    }
    fn publish(&mut self) -> Result<(), ReadError> {
        publish_check(self.budget)?;
        if !self.fence.commit() {
            return Err(ReadError::Cancelled);
        }
        #[cfg(test)]
        if let Some(hook) = &self.budget.activation_commit_hook {
            hook();
        }
        Ok(())
    }
}

/// Final authority check after the last lock wait. `Budget::check` covers the
/// deadline, caller/owner cancellation and session revocation, but not owner
/// sealing; an activation prepared before close began must not publish after.
fn publish_check(budget: &Budget) -> Result<(), ReadError> {
    budget.check()?;
    if budget
        .owner_sealed
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
    {
        return Err(ReadError::Cancelled);
    }
    Ok(())
}

impl ReadService {
    /// Admit and run one catalog activation as the trusted embedding with the
    /// default [`CatalogActivationLimits`].
    pub async fn activate_catalog(
        &self,
        context: ReadContext,
        catalog: Catalog,
    ) -> Result<CatalogActivation, CatalogActivationError> {
        self.activate_catalog_with(context, catalog, CatalogActivationLimits::default())
            .await
    }
    /// Admit and run one catalog activation as the trusted embedding. It
    /// shares this service's admission capacity, lifecycle and worker lease;
    /// its deadline is bounded by `limits.max_duration`, not
    /// [`ReadLimits::max_duration`].
    pub async fn activate_catalog_with(
        &self,
        context: ReadContext,
        catalog: Catalog,
        limits: CatalogActivationLimits,
    ) -> Result<CatalogActivation, CatalogActivationError> {
        limits.validate().map_err(ActivationError::Control)?;
        let guard = self
            .inner
            .lifecycle
            .as_ref()
            .map(|life| life.admit().map_err(application_read_error))
            .transpose()
            .map_err(ActivationError::Control)?;
        self.begin_with_guard(context, guard, None, limits.max_duration)
            .await
            .map_err(ActivationError::Control)?
            .activate_catalog(catalog, limits)
            .await
    }
}

impl ReadAdmission {
    /// Run a catalog activation under this admission's original deadline,
    /// cancellation, session and lifecycle registration, with its own
    /// activation work/retention limits. Prompt caller cancellation does not
    /// release the worker's lease before it exits, and a stop never hides a
    /// publication the worker already committed.
    pub async fn activate_catalog(
        self,
        catalog: Catalog,
        limits: CatalogActivationLimits,
    ) -> Result<CatalogActivation, CatalogActivationError> {
        limits.validate().map_err(ActivationError::Control)?;
        let inner = self.inner.clone();
        let fence = Arc::new(CommitFence::new());
        let worker_fence = fence.clone();
        self.run_task_fenced(Some(fence), move |principal, budget| {
            Ok(inner.activate(principal, catalog, limits, worker_fence, budget))
        })
        .await
        .map_err(ActivationError::Control)?
    }
}

impl Inner {
    fn activate(
        &self,
        principal: Principal,
        catalog: Catalog,
        limits: CatalogActivationLimits,
        fence: Arc<CommitFence>,
        budget: &mut Budget,
    ) -> Result<CatalogActivation, CatalogActivationError> {
        if !matches!(principal, Principal::TrustedEmbedding { .. }) {
            return Err(ActivationError::Control(ReadError::Forbidden));
        }
        publish_check(budget).map_err(ActivationError::Control)?;
        let expected = loop {
            let wait = budget.wait_quantum().map_err(ActivationError::Control)?;
            if let Some(current) = self
                .graph
                .read_for(wait, |graph| graph.activated_catalog().cloned())
            {
                break current;
            }
        }
        .ok_or(ActivationError::Unmanaged)?;
        let mut control = Control {
            budget,
            limits,
            fence,
            work: 0,
            retained: 0,
            bound: None,
        };
        let (next, state) = self
            .graph
            .activate_catalog(&expected, catalog, &mut control)?;
        // After the commit point: only infallible handoff remains. Fresh
        // requests capture the new observation and reuse this binding.
        if let Some(registry) = control.bound.take() {
            *self.registry.lock() = registry;
        }
        Ok(CatalogActivation {
            catalog_generation: state.catalog_generation,
            selection: next.selection_identity().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AllowAll, ApiError, ApplicationBuilder, FunctionIdentity, ShutdownPolicy,
        TypedInvocationInput,
    };
    use haystack_core::{graph::EntityGraph, kinds::HRef, xeto::catalog::PINNED_XETO_REVISION};
    use std::time::Duration;

    // Project-owned fixture declarations; not attributed to upstream.
    const PROJECT: &str = r#"
pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"},{lib:"ph",versions:"5.0.0"},{lib:"ph.protocols",versions:"5.0.0"}}>
Owner: sys::Entity { name:Str }
AddressFeature: ph::Feature { addr:ph.protocols::ModbusAddr }
Asset: sys::Entity {
  details:AddressFeature
  owner:Ref?<of:Owner>
  label:Str?
}
Pump: Asset { pump }
"#;
    fn project() -> Catalog {
        Catalog::load_protocol_pinned()
            .unwrap()
            .with_project("fixture", PROJECT, &[("pump", "fixture::Pump")])
            .unwrap()
    }
    fn nominal(name: &str, text: &str) -> Kind {
        Kind::Nominal(
            NominalScalar::new(
                name,
                "https://github.com/Project-Haystack/xeto",
                PINNED_XETO_REVISION,
                text,
            )
            .unwrap(),
        )
    }
    use haystack_core::kinds::NominalScalar;
    fn owner() -> HDict {
        let mut owner = HDict::new();
        owner.set("id", Kind::Ref(HRef::from_val("owner-1")));
        owner.set("spec", Kind::Ref(HRef::from_val("fixture::Owner")));
        owner.set("name", Kind::Str("Owner".into()));
        owner
    }
    fn pump(id: &str, bit: i64) -> HDict {
        let mut addr = HDict::new();
        addr.set(
            "spec",
            Kind::Ref(HRef::from_val("ph.protocols::ModbusAddr")),
        );
        addr.set("addr", Kind::Str("400001".into()));
        addr.set("encoding", nominal("ph.protocols::ModbusEncoding", "f4"));
        addr.set("access", nominal("ph.protocols::ModbusAccess", "r"));
        addr.set("bitIndex", Kind::Int(bit));
        let mut details = HDict::new();
        details.set("spec", Kind::Ref(HRef::from_val("fixture::AddressFeature")));
        details.set("addr", Kind::Dict(Box::new(addr)));
        let mut pump = HDict::new();
        pump.set("id", Kind::Ref(HRef::from_val(id)));
        pump.set("spec", Kind::Ref(HRef::from_val("fixture::Pump")));
        pump.set("pump", Kind::Marker);
        pump.set("details", Kind::Dict(Box::new(details)));
        pump.set("owner", Kind::Ref(HRef::from_val("owner-1")));
        pump
    }
    fn graph(rows: &[HDict]) -> SharedGraph {
        let graph = SharedGraph::new(EntityGraph::new());
        for row in rows {
            graph.add(row.clone()).unwrap();
        }
        graph
    }
    fn trusted() -> ReadContext {
        ReadContext::with_timeout(
            Principal::TrustedEmbedding {
                subject: "owner".into(),
            },
            Duration::from_secs(5),
        )
    }
    fn anonymous() -> ReadContext {
        ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(5))
    }
    fn read_by_id(id: &str) -> TypedInvocationInput {
        TypedInvocationInput {
            operation: "readById".into(),
            versions: vec!["5".into()],
            post: true,
            content_types: vec!["application/json".into()],
            body: format!(r#"{{"id":"{id}"}}"#).into_bytes(),
            ..Default::default()
        }
    }
    fn observation(graph: &SharedGraph) -> usize {
        graph.read(|g| Arc::as_ptr(g.activated_catalog().unwrap()) as usize)
    }

    /// Hides one entity from callers; integrity validation still sees it.
    struct Hide(&'static str);
    impl ReadPolicy for Hide {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(Hide(self.0)))
        }
    }
    impl PolicySnapshot for Hide {
        fn scope_key(&self) -> &str {
            "hide-fixture"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            true
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, id: &str) -> bool {
            id != self.0
        }
        fn tag(&self, _: &str, _: &str) -> bool {
            true
        }
        fn reference(&self, id: &str) -> bool {
            id != self.0
        }
        fn reference_display(&self, id: &str) -> bool {
            id != self.0
        }
        fn catalog(&self, kind: CatalogKind, name: &str) -> bool {
            AllowAll.catalog(kind, name)
        }
        fn nominal_provenance(&self, _: &NominalScalar) -> bool {
            true
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activation_publishes_selected_semantics_for_fresh_typed_requests() {
        let graph = graph(&[owner(), pump("pump-1", 0)]);
        let reads =
            ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let before = reads
            .typed_functions()
            .selection_identity()
            .unwrap()
            .to_owned();
        // The bootstrap observation cannot represent selected nominal values.
        let invoke = |input: TypedInvocationInput| {
            let reads = reads.clone();
            async move {
                reads
                    .begin(anonymous())
                    .await
                    .unwrap()
                    .invoke_wire(input)
                    .await
            }
        };
        assert!(invoke(read_by_id("pump-1")).await.is_err());
        // Callers cannot activate catalogs.
        assert!(matches!(
            reads.activate_catalog(anonymous(), project()).await,
            Err(ActivationError::Control(ReadError::Forbidden))
        ));
        let generation = graph.state().catalog_generation;
        let published = reads.activate_catalog(trusted(), project()).await.unwrap();
        assert_eq!(published.catalog_generation, generation + 1);
        assert_ne!(published.selection, before);
        assert_ne!(published.selection, PINNED_XETO_REVISION);
        assert_eq!(
            reads.typed_functions().selection_identity(),
            Some(published.selection.as_str())
        );
        let body = invoke(read_by_id("pump-1")).await.unwrap().body;
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("\"f4\"") && text.contains("400001"), "{text}");
        // Only libraries admitted by the current observation are advertised.
        let libs = invoke(TypedInvocationInput {
            operation: "libs".into(),
            versions: vec!["5".into()],
            ..Default::default()
        })
        .await
        .unwrap();
        let libs = String::from_utf8(libs.body).unwrap();
        for name in ["\"ph.protocols\"", "\"fixture\"", "\"sys.api\""] {
            assert!(libs.contains(name), "{name}: {libs}");
        }
        assert!(
            !libs.contains("sys.refs"),
            "dependency-only identity: {libs}"
        );
        // Stored data was neither repaired nor relabeled.
        assert_eq!(graph.get("pump-1").unwrap(), pump("pump-1", 0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn controlled_spec_match_uses_strict_selected_fitting_and_masked_targets() {
        let graph = graph(&[owner(), pump("pump-1", 0)]);
        let reads =
            ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        reads.activate_catalog(trusted(), project()).await.unwrap();
        // Writes are not repaired or validated by activation; strict matching
        // still never treats an invalid record as a selected Pump.
        graph.add(pump("pump-bad", 16)).unwrap();
        // Typed Jeto grids deliberately refuse a structural `spec` column
        // (documented known limit), so the controlled filter is observed
        // through the native typed page.
        let typed_grid = reads
            .begin(anonymous())
            .await
            .unwrap()
            .invoke_wire(TypedInvocationInput {
                operation: "readAll".into(),
                versions: vec!["5".into()],
                post: true,
                content_types: vec!["application/json".into()],
                body: br#"{"filter":"fixture::Pump"}"#.to_vec(),
                ..Default::default()
            })
            .await;
        assert!(matches!(typed_grid, Err(ApiError::NotAcceptable)));
        let read_all = |reads: ReadService| async move {
            let page = reads
                .read(
                    anonymous(),
                    ReadRequest::new(
                        ReadQuery::Filter("fixture::Pump".into()),
                        OutputProfile::Typed,
                    ),
                )
                .await
                .unwrap();
            let ReadOutput::Typed(grid) = page.output else {
                panic!("typed output")
            };
            grid.rows
                .iter()
                .map(|row| row.id().map(|id| id.val.clone()).unwrap_or_default())
                .collect::<Vec<_>>()
        };
        assert_eq!(read_all(reads.clone()).await, ["pump-1"]);
        // A visible dangling reference never fits the target-constrained slot.
        let mut dangling = pump("pump-dangling", 0);
        dangling.set("owner", Kind::Ref(HRef::from_val("missing")));
        graph.add(dangling).unwrap();
        assert_eq!(read_all(reads.clone()).await, ["pump-1"]);
        // Matching uses the caller's masked view: a reference to a hidden
        // entity is removed before fitting (here a nullable slot), and the
        // hidden target is never fetched or disclosed.
        let masked = ReadService::new(
            graph.clone(),
            Arc::new(Hide("owner-1")),
            ReadLimits::default(),
        )
        .unwrap();
        assert_eq!(read_all(masked).await, ["pump-1"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hidden_invalid_data_rejects_activation_without_public_disclosure() {
        let graph = graph(&[owner(), pump("pump-1", 0), pump("secret-pump", 16)]);
        let reads = ReadService::new(
            graph.clone(),
            Arc::new(Hide("secret-pump")),
            ReadLimits::default(),
        )
        .unwrap();
        let state = graph.state();
        let current = observation(&graph);
        let mut wakes = graph.subscribe_wakes();
        let Err(ActivationError::Rejected(rejection)) =
            reads.activate_catalog(trusted(), project()).await
        else {
            panic!("hidden invalid data must participate in integrity validation")
        };
        for public in [rejection.to_string(), format!("{rejection:?}")] {
            assert!(
                !public.contains("secret") && !public.contains("fixture"),
                "{public}"
            );
        }
        assert_eq!(rejection.privileged().0, "secret-pump");
        assert_eq!(graph.state(), state);
        assert_eq!(observation(&graph), current);
        assert!(wakes.try_recv().is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn retained_request_rejects_stale_lookup_and_old_results_encode_with_old_context() {
        let graph = graph(&[owner(), pump("pump-1", 0)]);
        let reads =
            ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let pause = || {
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let release = std::sync::Mutex::new(release_rx);
            let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                entered_tx.send(()).unwrap();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            });
            (entered_rx, release_tx, hook)
        };
        // 1. Captured before activation, paused before graph evaluation: the
        //    retained observation is no longer current, so it never evaluates.
        let (entered, release, hook) = pause();
        let mut admission = reads.begin(anonymous()).await.unwrap();
        admission.budget_mut().typed_lookup_hook = Some(hook);
        let stale = tokio::spawn(admission.invoke_wire(read_by_id("owner-1")));
        tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(3)))
            .await
            .unwrap()
            .unwrap();
        let next = reads.activate_catalog(trusted(), project()).await.unwrap();
        release.send(()).unwrap();
        assert!(matches!(stale.await.unwrap(), Err(ApiError::Unavailable)));

        // 2. An owned result captured under the current observation encodes
        //    with that retained context even if another activation publishes.
        let (entered, release, hook) = pause();
        let mut admission = reads.begin(anonymous()).await.unwrap();
        admission.budget_mut().typed_encode_hook = Some(hook);
        let retained = tokio::spawn(admission.invoke_wire(read_by_id("pump-1")));
        tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(3)))
            .await
            .unwrap()
            .unwrap();
        let unrelated = r#"pragma: Lib <version:"1.0.0", depends:{{lib:"sys",versions:"5.0.0"}}>
Tag: sys::Dict { note:Str? }
"#;
        let additive = project().with_project("extra", unrelated, &[]).unwrap();
        let newer = reads.activate_catalog(trusted(), additive).await.unwrap();
        assert_ne!(newer.selection, next.selection);
        release.send(()).unwrap();
        let old = retained.await.unwrap().unwrap().body;
        // A fresh call uses the new observation; additive same-origin
        // contexts preserve the selected values byte-for-byte.
        let fresh = reads
            .begin(anonymous())
            .await
            .unwrap()
            .invoke_wire(read_by_id("pump-1"))
            .await
            .unwrap()
            .body;
        assert_eq!(old, fresh);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owner_sealing_after_admission_prevents_publication() {
        let graph = graph(&[owner(), pump("pump-1", 0)]);
        let app = ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
            .unwrap()
            .shutdown_policy(ShutdownPolicy {
                drain_timeout: Duration::from_secs(2),
                stop_timeout: Duration::from_secs(1),
            });
        let handle = app.handle();
        let reads = handle.read_service();
        let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
        owner.ready().await.unwrap();
        let state = graph.state();
        let current = observation(&graph);
        let admission = reads.begin(trusted()).await.unwrap();
        let closing = tokio::spawn(async move { owner.close().await });
        // Admission is sealed (not yet stopped) while this registered
        // activation still runs within the drain allowance.
        handle.closing().cancelled().await;
        assert!(!handle.cancellation().is_cancelled());
        let result = admission
            .activate_catalog(project(), CatalogActivationLimits::default())
            .await;
        assert!(
            matches!(result, Err(ActivationError::Control(ReadError::Cancelled))),
            "{result:?}"
        );
        assert_eq!(graph.state(), state);
        assert_eq!(observation(&graph), current);
        closing.await.unwrap().unwrap();
    }

    /// Entity allow-list; catalog visibility is unrestricted.
    struct Allow(&'static [&'static str]);
    impl ReadPolicy for Allow {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(Allow(self.0)))
        }
    }
    impl PolicySnapshot for Allow {
        fn scope_key(&self) -> &str {
            "allow-fixture"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            true
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, id: &str) -> bool {
            self.0.contains(&id)
        }
        fn tag(&self, _: &str, _: &str) -> bool {
            true
        }
        fn reference(&self, id: &str) -> bool {
            self.0.contains(&id)
        }
        fn reference_display(&self, id: &str) -> bool {
            self.0.contains(&id)
        }
        fn catalog(&self, kind: CatalogKind, name: &str) -> bool {
            AllowAll.catalog(kind, name)
        }
        fn nominal_provenance(&self, _: &NominalScalar) -> bool {
            true
        }
    }
    /// Every entity visible; one library's catalog names are hidden.
    struct HideLibrary(&'static str);
    impl ReadPolicy for HideLibrary {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(HideLibrary(self.0)))
        }
    }
    impl PolicySnapshot for HideLibrary {
        fn scope_key(&self) -> &str {
            "hide-library-fixture"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            true
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, _: &str) -> bool {
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
        fn catalog(&self, kind: CatalogKind, name: &str) -> bool {
            match kind {
                CatalogKind::Library => name != self.0,
                _ => name
                    .split_once("::")
                    .is_none_or(|(library, _)| library != self.0),
            }
        }
        fn nominal_provenance(&self, _: &NominalScalar) -> bool {
            true
        }
    }
    async fn read_body(reads: &ReadService, id: &str) -> String {
        let body = reads
            .begin(anonymous())
            .await
            .unwrap()
            .invoke_wire(read_by_id(id))
            .await
            .unwrap()
            .body;
        String::from_utf8(body).unwrap()
    }
    async fn pumps(reads: &ReadService) -> Vec<String> {
        let page = reads
            .read(
                anonymous(),
                ReadRequest::new(
                    ReadQuery::Filter("fixture::Pump".into()),
                    OutputProfile::Typed,
                ),
            )
            .await
            .unwrap();
        let ReadOutput::Typed(grid) = page.output else {
            panic!("typed output")
        };
        grid.rows
            .iter()
            .map(|row| row.id().map(|id| id.val.clone()).unwrap_or_default())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn structural_spec_references_follow_catalog_not_entity_visibility() {
        // An entity allow-list keeps catalog annotations: nested structural
        // specs no longer strip `details`, so the record still fits.
        let graph = graph(&[owner(), pump("pump-1", 0)]);
        let allowed = ReadService::new(
            graph.clone(),
            Arc::new(Allow(&["pump-1"])),
            ReadLimits::default(),
        )
        .unwrap();
        allowed
            .activate_catalog(trusted(), project())
            .await
            .unwrap();
        let body = read_body(&allowed, "pump-1").await;
        for expected in [
            "\"details\"",
            "fixture::Pump",
            "fixture::AddressFeature",
            "400001",
        ] {
            assert!(body.contains(expected), "{expected}: {body}");
        }
        assert!(
            !body.contains("owner-1"),
            "entity references keep entity policy: {body}"
        );
        assert_eq!(pumps(&allowed).await, ["pump-1"]);
        // A catalog-hidden declaration name is never disclosed through a
        // structural annotation, at the top level or nested.
        let hidden = ReadService::new(
            graph.clone(),
            Arc::new(HideLibrary("fixture")),
            ReadLimits::default(),
        )
        .unwrap();
        for id in ["pump-1", "owner-1"] {
            let body = read_body(&hidden, id).await;
            assert!(!body.contains("fixture"), "{body}");
            assert!(body.contains(id), "{body}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn caller_stop_after_the_commit_point_reports_the_published_activation() {
        let graph = graph(&[owner(), pump("pump-1", 0)]);
        let reads =
            ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        let cancel = CancellationToken::new();
        let mut admission = reads
            .begin(ReadContext::new(
                Principal::TrustedEmbedding {
                    subject: "owner".into(),
                },
                Instant::now() + Duration::from_secs(5),
                cancel.clone(),
            ))
            .await
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release = std::sync::Mutex::new(release_rx);
        admission.budget_mut().activation_commit_hook = Some(Arc::new(move || {
            entered_tx.send(()).unwrap();
            release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }));
        let activation =
            tokio::spawn(admission.activate_catalog(project(), CatalogActivationLimits::default()));
        tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(3)))
            .await
            .unwrap()
            .unwrap();
        // The caller stops while the worker is past its commit point. The
        // biased stop arm wins every poll from here on, so the fence decides.
        cancel.cancel();
        release_tx.send(()).unwrap();
        let published = activation.await.unwrap().unwrap();
        assert_eq!(
            graph.state().catalog_generation,
            published.catalog_generation
        );
        assert!(graph.read(|g| g.namespace().unwrap().get_spec("fixture::Pump").is_some()));
        assert_eq!(
            reads.typed_functions().selection_identity(),
            Some(published.selection.as_str())
        );
    }

    #[test]
    fn commit_fence_admits_exactly_one_side() {
        let fence = CommitFence::new();
        assert!(fence.commit());
        assert!(!fence.abandon(), "a committed effect cannot be abandoned");
        let fence = CommitFence::new();
        assert!(fence.abandon());
        assert!(fence.abandon());
        assert!(!fence.commit(), "an abandoned activation cannot commit");
    }

    #[test]
    fn bootstrap_publishes_despite_entity_writes_and_compiles_once_per_base() {
        let graph = graph(&[owner()]);
        let writer = graph.clone();
        let mut attempts = 0;
        let observation = bootstrap_with(&graph, || {
            attempts += 1;
            writer.add(pump(&format!("p{attempts}"), 0)).unwrap();
        })
        .unwrap();
        assert_eq!(attempts, 1, "entity writes never force another attempt");
        assert!(graph.read(|g| Arc::ptr_eq(g.activated_catalog().unwrap(), &observation)));
        // Installing the bootstrap deliberately advances the generation once.
        assert_eq!(graph.state().catalog_generation, 1);
        // A namespace replacement does force exactly one rebuilt attempt.
        let graph = self::graph(&[]);
        let writer = graph.clone();
        let mut attempts = 0;
        let observation = bootstrap_with(&graph, || {
            attempts += 1;
            if attempts == 1 {
                writer.set_namespace(haystack_core::ontology::DefNamespace::new());
            }
        })
        .unwrap();
        assert_eq!(attempts, 2);
        assert!(graph.read(|g| Arc::ptr_eq(g.activated_catalog().unwrap(), &observation)));
        // The admitted pinned selection is shared process-wide, but every
        // installation is its own observation with its own derived namespace.
        let other = bootstrap(&self::graph(&[])).unwrap();
        assert!(!Arc::ptr_eq(&other, &observation));
        assert!(!Arc::ptr_eq(other.namespace(), observation.namespace()));
        assert!(std::ptr::eq(other.catalog(), observation.catalog()));
        assert!(std::ptr::eq(
            other.catalog(),
            pinned_bootstrap().unwrap().catalog()
        ));
        assert_eq!(other.selection_identity(), observation.selection_identity());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activation_limits_and_errors_are_specific() {
        let rows: Vec<HDict> = std::iter::once(owner())
            .chain((0..20).map(|n| pump(&format!("pump-{n:02}"), 0)))
            .collect();
        let graph = graph(&rows);
        // Tiny per-request budgets do not constrain activation validation.
        let reads = ReadService::new(
            graph.clone(),
            Arc::new(AllowAll),
            ReadLimits {
                max_work: 1_000,
                ..ReadLimits::default()
            },
        )
        .unwrap();
        let generation = graph.state().catalog_generation;
        assert!(matches!(
            reads
                .activate_catalog_with(
                    trusted(),
                    project(),
                    CatalogActivationLimits {
                        max_work: 64,
                        ..CatalogActivationLimits::default()
                    }
                )
                .await,
            Err(ActivationError::Control(ReadError::Budget(
                BudgetKind::Work
            )))
        ));
        assert!(matches!(
            reads
                .activate_catalog_with(
                    trusted(),
                    project(),
                    CatalogActivationLimits {
                        chunk_records: 0,
                        ..CatalogActivationLimits::default()
                    }
                )
                .await,
            Err(ActivationError::Control(ReadError::InvalidLimits))
        ));
        assert_eq!(graph.state().catalog_generation, generation);
        // The narrower readById-only selection restates admitted upstream
        // declarations differently, so provenance rejects it before binding.
        assert!(matches!(
            reads
                .activate_catalog(trusted(), Catalog::load_pinned().unwrap())
                .await,
            Err(ActivationError::Catalog(_))
        ));
        // A candidate lacking a supported handler names that declaration.
        let narrow =
            Arc::new(ActivatedCatalog::new(Catalog::load_pinned().unwrap(), None).unwrap());
        let mut budget = Budget::new(
            Arc::new(ReadLimits::default()),
            Instant::now() + Duration::from_secs(5),
            CancellationToken::new(),
        );
        let mut control = Control {
            budget: &mut budget,
            limits: CatalogActivationLimits::default(),
            fence: Arc::new(CommitFence::new()),
            work: 0,
            retained: 0,
            bound: None,
        };
        match control.admit(&narrow) {
            Err(ActivationError::Unsupported {
                declaration,
                reason,
            }) => {
                assert_eq!(declaration, "sys.api::ops");
                assert!(reason.contains("not admitted"), "{reason}");
            }
            other => panic!("expected Unsupported: {other:?}"),
        }
        assert!(control.bound.is_none());
        assert_eq!(graph.state().catalog_generation, generation);
        let first = reads.activate_catalog(trusted(), project()).await.unwrap();
        assert_eq!(graph.state().catalog_generation, generation + 1);
        // An identical selection still publishes a new observation.
        let mut wakes = graph.subscribe_wakes();
        let again = reads.activate_catalog(trusted(), project()).await.unwrap();
        assert_eq!(again.selection, first.selection);
        assert_eq!(again.catalog_generation, generation + 2);
        assert!(matches!(
            wakes.try_recv(),
            Ok(haystack_core::graph::GraphWake::Catalog(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activation_duration_is_capped_and_never_overflows() {
        let graph = graph(&[owner(), pump("pump-1", 0)]);
        let reads =
            ReadService::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
        for (max_duration, valid) in [
            (Duration::MAX, false),
            (Duration::from_secs(60) + Duration::from_nanos(1), false),
            (Duration::from_secs(60), true),
        ] {
            let limits = CatalogActivationLimits {
                max_duration,
                ..CatalogActivationLimits::default()
            };
            assert_eq!(limits.validate().is_ok(), valid, "{max_duration:?}");
            if !valid {
                assert!(matches!(
                    reads
                        .activate_catalog_with(trusted(), project(), limits)
                        .await,
                    Err(ActivationError::Control(ReadError::InvalidLimits))
                ));
            }
        }
        // The admission deadline arithmetic itself cannot panic: an
        // unrepresentable bound leaves the caller's own deadline in place.
        let context = trusted();
        let deadline = context.deadline;
        let admission = reads
            .begin_with_guard(context, None, None, Duration::MAX)
            .await
            .unwrap();
        assert_eq!(admission.deadline(), deadline);
        drop(admission);
        reads
            .activate_catalog_with(
                trusted(),
                project(),
                CatalogActivationLimits {
                    max_duration: Duration::from_secs(60),
                    ..CatalogActivationLimits::default()
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activation_depth_is_independent_of_request_value_depth() {
        // Pump -> details -> addr nests Dicts and owner follows a reference,
        // so fitting needs more than a shallow per-request value depth.
        let graph = graph(&[owner(), pump("pump-1", 0)]);
        let reads = ReadService::new(
            graph.clone(),
            Arc::new(AllowAll),
            ReadLimits {
                max_value_depth: 2,
                ..ReadLimits::default()
            },
        )
        .unwrap();
        let generation = graph.state().catalog_generation;
        assert!(matches!(
            reads
                .activate_catalog_with(
                    trusted(),
                    project(),
                    CatalogActivationLimits {
                        max_depth: 2,
                        ..CatalogActivationLimits::default()
                    }
                )
                .await,
            Err(ActivationError::Control(ReadError::Budget(
                BudgetKind::Depth
            )))
        ));
        for max_depth in [0, 65] {
            assert!(
                CatalogActivationLimits {
                    max_depth,
                    ..CatalogActivationLimits::default()
                }
                .validate()
                .is_err()
            );
        }
        assert_eq!(graph.state().catalog_generation, generation);
        assert_eq!(CatalogActivationLimits::default().max_depth, 64);
        reads.activate_catalog(trusted(), project()).await.unwrap();
        assert_eq!(graph.state().catalog_generation, generation + 1);
    }

    #[test]
    fn final_publication_check_rejects_owner_sealing_that_budget_check_admits() {
        let mut budget = Budget::new(
            Arc::new(ReadLimits::default()),
            Instant::now() + Duration::from_secs(5),
            CancellationToken::new(),
        );
        let sealed = CancellationToken::new();
        budget.owner_sealed = Some(sealed.clone());
        publish_check(&budget).unwrap();
        sealed.cancel();
        // The ordinary request check deliberately does not observe sealing.
        budget.check().unwrap();
        assert_eq!(publish_check(&budget), Err(ReadError::Cancelled));
    }
}
