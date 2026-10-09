mod activation;
mod nav;
mod system;
use crate::{
    budget::Budget,
    lifecycle::{ApplicationError, Lifecycle, WorkGuard},
    output,
    policy::{PolicySnapshot, ReadPolicy},
    sanitize::{self, View},
    types::*,
    wire,
};
pub use activation::{CatalogActivation, CatalogActivationError, CatalogActivationLimits};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use haystack_core::{
    data::HDict,
    filter::{self, CatalogKind, FilterError, FilterNode, FilterParseLimits},
    graph::SharedGraph,
    kinds::{Kind, Symbol},
    ontology::DefNamespace,
    xeto::Spec,
};
use hmac::{Hmac, KeyInit, Mac};
use parking_lot::{Mutex, MutexGuard};
use sha2::Sha256;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Instant,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLoad {
    pub admitted: usize,
    pub waiting: usize,
}

#[derive(Clone)]
pub struct ReadService {
    inner: Arc<Inner>,
}
struct Inner {
    /// Binding of the fixed handler inventory to the most recently observed
    /// graph catalog. Requests retain their own `Arc`; this is only a cache.
    registry: Mutex<Arc<crate::registry::Registry>>,
    system: system::SystemInfo,
    lifecycle: Option<Arc<Lifecycle>>,
    graph: SharedGraph,
    dataset: [u8; 16],
    policy: Arc<dyn ReadPolicy>,
    limits: Arc<ReadLimits>,
    active: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    cursor_key: [u8; 32],
    cursors: Mutex<CursorTable>,
}
#[derive(Default)]
struct CursorTable {
    entries: HashMap<[u8; 16], CursorRecord>,
    bytes: usize,
}
struct CursorRecord {
    identity: Arc<RequestIdentity>,
    stamp: Stamp,
    after: String,
    expires: Instant,
    bytes: usize,
}
struct Prepared {
    identity: Arc<RequestIdentity>,
    cursor: Option<String>,
    retained_estimate: usize,
}
enum Input {
    Request(ReadRequest),
    Wire {
        operation: ReadOperation,
        body: Vec<u8>,
        input: H4Codec,
        output: H4Codec,
    },
}
impl Input {
    fn operation(&self) -> ReadOperation {
        match self {
            Self::Request(r) => r.query.operation(),
            Self::Wire { operation, .. } => *operation,
        }
    }
}
struct CancelOnDrop {
    token: CancellationToken,
    armed: bool,
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}

impl ReadService {
    /// Shares a graph and borrows the executing Tokio runtime. Creating another
    /// service creates another dataset/cursor authority; clones share authority.
    pub fn new(
        graph: SharedGraph,
        policy: Arc<dyn ReadPolicy>,
        limits: ReadLimits,
    ) -> Result<Self, ReadError> {
        limits.validate()?;
        let observation = bootstrap(&graph)?;
        let registry = Arc::new(crate::registry::Registry::bind(observation)?);
        Ok(Self {
            inner: Arc::new(Inner {
                registry: Mutex::new(registry),
                system: system::SystemInfo::new(false),
                lifecycle: None,
                graph,
                dataset: rand::random(),
                policy,
                active: Arc::new(Semaphore::new(limits.max_concurrent)),
                waiting: Arc::new(Semaphore::new(limits.max_queued)),
                limits: Arc::new(limits),
                cursor_key: rand::random(),
                cursors: Mutex::new(CursorTable::default()),
            }),
        })
    }
    pub(crate) fn managed(
        graph: SharedGraph,
        policy: Arc<dyn ReadPolicy>,
        limits: ReadLimits,
        lifecycle: Arc<Lifecycle>,
    ) -> Result<Self, ReadError> {
        let mut service = Self::new(graph, policy, limits)?;
        let inner = Arc::get_mut(&mut service.inner).expect("new service");
        inner.lifecycle = Some(lifecycle);
        inner.system = system::SystemInfo::new(true);
        Ok(service)
    }
    /// Whether both handles share this exact service, including policy, cursor
    /// authority and admission. Independently constructed services differ even
    /// when they refer to the same graph.
    pub fn same_service(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    /// Administrative graph handle for the owning application, outside the
    /// resource-policy API. Native callers already holding it remain trusted.
    pub fn graph(&self) -> SharedGraph {
        self.inner.graph.clone()
    }
    pub(crate) fn policy_snapshot(
        &self,
        principal: &Principal,
    ) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        self.inner.policy.snapshot(principal)
    }
    /// Retained view of the typed bindings for the graph's current catalog
    /// observation, for trusted transport configuration. Caller discovery
    /// uses the same per-observation entries after the request's policy
    /// snapshot. An unmanaged (replaced) graph has no executable functions.
    pub fn typed_functions(&self) -> crate::TypedFunctions {
        let observation = self
            .inner
            .graph
            .read(|graph| graph.activated_catalog().cloned());
        crate::TypedFunctions::new(observation.and_then(|o| self.inner.bound(o).ok()))
    }
    /// The fixed supported handler inventory as `(name, qname)` pairs. Routing
    /// derives from this bounded list, never from a parsed catalog; dispatch
    /// and discovery still consult each request's retained observation.
    pub fn supported_functions() -> impl Iterator<Item = (&'static str, &'static str)> {
        crate::registry::BINDINGS
            .iter()
            .map(|(qname, _)| (qname.rsplit("::").next().unwrap_or(qname), *qname))
    }
    pub fn limits(&self) -> &ReadLimits {
        &self.inner.limits
    }
    pub fn load(&self) -> ReadLoad {
        ReadLoad {
            admitted: self.inner.limits.max_concurrent - self.inner.active.available_permits(),
            waiting: self.inner.limits.max_queued - self.inner.waiting.available_permits(),
        }
    }
    pub async fn read(
        &self,
        context: ReadContext,
        request: ReadRequest,
    ) -> Result<ReadPage, ReadError> {
        self.run(context, Input::Request(request)).await
    }
    /// Decode under the same bounded admission/worker ownership as evaluation.
    /// Transport adapters must bound body collection before calling this method.
    pub async fn read_wire(
        &self,
        context: ReadContext,
        operation: ReadOperation,
        body: Vec<u8>,
        input: H4Codec,
        output: H4Codec,
    ) -> Result<ReadPage, ReadError> {
        self.run(
            context,
            Input::Wire {
                operation,
                body,
                input,
                output,
            },
        )
        .await
    }
    /// Register a logical request before body collection. Move this admission
    /// into `read_wire` after bounded collection; no second slot is acquired.
    pub async fn begin(&self, context: ReadContext) -> Result<ReadAdmission, ReadError> {
        let guard = self
            .inner
            .lifecycle
            .as_ref()
            .map(|life| life.admit().map_err(application_read_error))
            .transpose()?;
        self.begin_with_guard(context, guard, None, self.inner.limits.max_duration)
            .await
    }
    /// Continue an operation already admitted by this application's transport.
    /// The guard must belong to the exact application; no fresh admission is
    /// made during drain, and the registration moves into body/worker lifetime.
    pub async fn begin_admitted(
        &self,
        context: ReadContext,
        guard: WorkGuard,
    ) -> Result<ReadAdmission, ReadError> {
        if !self
            .inner
            .lifecycle
            .as_ref()
            .is_some_and(|life| Arc::ptr_eq(life, &guard.lifecycle))
        {
            return Err(ReadError::Forbidden);
        }
        self.begin_with_guard(context, Some(guard), None, self.inner.limits.max_duration)
            .await
    }
    /// Capture authenticated authority before waiting for an execution slot.
    /// The handle is validated once; body/queue/worker/disclosure share it.
    pub async fn begin_admitted_session(
        &self,
        context: ReadContext,
        guard: WorkGuard,
        session: crate::SubscriptionSession,
    ) -> Result<ReadAdmission, ReadError> {
        if !self
            .inner
            .lifecycle
            .as_ref()
            .is_some_and(|life| Arc::ptr_eq(life, &guard.lifecycle))
        {
            return Err(ReadError::Forbidden);
        }
        self.begin_with_guard(
            context,
            Some(guard),
            Some(session),
            self.inner.limits.max_duration,
        )
        .await
    }
    async fn begin_with_guard(
        &self,
        context: ReadContext,
        guard: Option<WorkGuard>,
        session: Option<crate::SubscriptionSession>,
        max_duration: std::time::Duration,
    ) -> Result<ReadAdmission, ReadError> {
        if session
            .as_ref()
            .is_some_and(|s| !s.matches(&context.principal) || !s.is_active())
        {
            return Err(ReadError::Forbidden);
        }
        let now = Instant::now();
        // Callers' limits are validated, but an unrepresentable instant must
        // never panic: it simply cannot tighten the caller's own deadline.
        let deadline = now
            .checked_add(max_duration)
            .map_or(context.deadline, |end| context.deadline.min(end));
        let cancel = context.cancellation.child_token();
        let mut budget = Budget::new(self.inner.limits.clone(), deadline, cancel);
        budget.owner_cancel = guard.as_ref().map(WorkGuard::cancellation);
        budget.owner_sealed = guard.as_ref().map(WorkGuard::closing);
        budget.session = session.clone().map(crate::budget::SessionFence::new);
        if session.is_some() {
            let bytes = context.principal.bytes();
            budget.charge(BudgetKind::Input, bytes)?;
            budget.charge(BudgetKind::Work, bytes.saturating_mul(4).saturating_add(1))?;
            budget.charge(
                BudgetKind::Retained,
                bytes.saturating_mul(4).saturating_add(256),
            )?;
        }
        budget.check()?;
        let permit = self.admit(&budget).await?;
        Ok(ReadAdmission {
            inner: self.inner.clone(),
            principal: Some(context.principal),
            session,
            budget: Some(budget),
            permit: Some(permit),
            guard,
        })
    }
    async fn run(&self, context: ReadContext, input: Input) -> Result<ReadPage, ReadError> {
        self.begin(context).await?.run(input).await
    }
    async fn admit(&self, budget: &Budget) -> Result<OwnedSemaphorePermit, ReadError> {
        budget.check()?;
        if let Ok(permit) = self.inner.active.clone().try_acquire_owned() {
            return Ok(permit);
        }
        let _queued = self
            .inner
            .waiting
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReadError::Capacity)?;
        tokio::select! {
            permit = self.inner.active.clone().acquire_owned() => { budget.check()?; permit.map_err(|_| ReadError::Unavailable) },
            _ = budget.cancelled() => Err(budget.check().err().unwrap_or(ReadError::Cancelled)),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(budget.deadline)) => Err(ReadError::Deadline),
        }
    }
}
/// Owns one logical request slot through body collection and worker execution.
/// Dropping before execution cancels collection and releases its slot; after the
/// handoff the awaiting caller, worker, and any deferred mutation plan share
/// ownership until each actually releases it. A ready result remains registered
/// through delivery, so drain cannot overtake a completed admitted request.
pub struct ReadAdmission {
    inner: Arc<Inner>,
    principal: Option<Principal>,
    session: Option<crate::SubscriptionSession>,
    budget: Option<Budget>,
    permit: Option<OwnedSemaphorePermit>,
    guard: Option<WorkGuard>,
}
/// Shared execution ownership, also retained by a provider-held mutation plan.
/// The original registration transfers here without an untracked handoff gap.
pub(crate) struct WorkLease {
    _permit: OwnedSemaphorePermit,
    _guard: Option<WorkGuard>,
}
impl Drop for ReadAdmission {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            budget.cancel.cancel();
        }
    }
}
impl ReadAdmission {
    /// Reserve raw bytes and transport copy/storage overhead before allocation.
    /// Calls accumulate across URI, headers, principal and streamed body frames.
    pub fn reserve_wire_input(&mut self, bytes: usize) -> Result<(), ReadError> {
        let budget = self.budget_mut();
        budget.charge(BudgetKind::Input, bytes)?;
        budget.charge(BudgetKind::Work, bytes.saturating_mul(4).saturating_add(1))?;
        budget.charge(
            BudgetKind::Retained,
            bytes.saturating_mul(4).saturating_add(256),
        )
    }
    /// Trusted transport authentication binds identity without replacing the
    /// original permit, deadline, cancellation or cumulative budget.
    pub fn bind_wire_principal(&mut self, principal: Principal) -> Result<(), ReadError> {
        self.reserve_wire_input(principal.bytes())?;
        self.principal = Some(principal);
        self.session = None;
        self.budget_mut().session = None;
        Ok(())
    }
    /// Bind the exact noncredential handle returned by trusted authentication.
    /// Request parameters cannot construct or select a session authority.
    pub fn bind_wire_session(
        &mut self,
        principal: Principal,
        session: crate::SubscriptionSession,
    ) -> Result<(), ReadError> {
        if !session.matches(&principal) || !session.is_active() {
            return Err(ReadError::Forbidden);
        }
        self.bind_wire_principal(principal)?;
        self.budget_mut().session = Some(crate::budget::SessionFence::new(session.clone()));
        self.session = Some(session);
        Ok(())
    }
    pub async fn invoke_wire(
        mut self,
        input: crate::TypedInvocationInput,
    ) -> Result<crate::TypedInvocationResponse, crate::ApiError> {
        let inner = self.inner.clone();
        let session = self.session.take();
        let response = self
            .run_task(move |principal, budget| {
                Ok(inner.execute_typed(principal, session, input, budget))
            })
            .await
            .map_err(crate::ApiError::from)??;
        response.disclosure.check()?;
        Ok(response)
    }
    pub fn deadline(&self) -> Instant {
        self.budget.as_ref().expect("live admission").deadline
    }
    pub fn check(&self) -> Result<(), ReadError> {
        self.budget.as_ref().expect("live admission").check()
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.budget.as_ref().expect("live admission").cancel.clone()
    }
    /// Wait for either caller cancellation or the application cooperative stop.
    pub async fn cancelled(&self) {
        self.budget
            .as_ref()
            .expect("live admission")
            .cancelled()
            .await;
    }
    pub async fn read(self, request: ReadRequest) -> Result<ReadPage, ReadError> {
        self.run(Input::Request(request)).await
    }
    pub async fn read_wire(
        self,
        operation: ReadOperation,
        body: Vec<u8>,
        input: H4Codec,
        output: H4Codec,
    ) -> Result<ReadPage, ReadError> {
        self.run(Input::Wire {
            operation,
            body,
            input,
            output,
        })
        .await
    }
    async fn run(self, input: Input) -> Result<ReadPage, ReadError> {
        let inner = self.inner.clone();
        self.run_task(move |principal, budget| inner.execute(principal, input, budget))
            .await
    }
    pub(crate) fn belongs_to(&self, service: &ReadService) -> bool {
        Arc::ptr_eq(&self.inner, &service.inner)
    }
    /// Transfers the original registration into a long-lived bounded session.
    pub(crate) fn into_session(mut self) -> Result<(Principal, Budget), ReadError> {
        let mut budget = self.budget.take().expect("live admission");
        budget.check()?;
        if budget.lease.is_none() {
            budget.lease = Some(Arc::new(WorkLease {
                _permit: self.permit.take().expect("live admission"),
                _guard: self.guard.take(),
            }));
        }
        Ok((self.principal.take().expect("live admission"), budget))
    }
    pub(crate) fn budget_mut(&mut self) -> &mut Budget {
        self.budget.as_mut().expect("live admission")
    }
    pub(crate) fn retain_work(&mut self) -> Result<Arc<WorkLease>, ReadError> {
        let budget = self.budget.as_mut().expect("live admission");
        budget.check()?;
        if budget.lease.is_none() {
            budget.lease = Some(Arc::new(WorkLease {
                _permit: self.permit.take().expect("live admission"),
                _guard: self.guard.take(),
            }));
        }
        Ok(budget.lease.as_ref().expect("installed lease").clone())
    }
    pub(crate) fn runtime(&self) -> tokio::runtime::Handle {
        self.inner
            .lifecycle
            .as_ref()
            .map_or_else(tokio::runtime::Handle::current, |life| life.runtime())
    }
    pub(crate) async fn run_task<T: Send + 'static>(
        self,
        task: impl FnOnce(Principal, &mut Budget) -> Result<T, ReadError> + Send + 'static,
    ) -> Result<T, ReadError> {
        self.run_task_fenced(None, task).await
    }
    /// As `run_task`, with an optional commit fence shared with the worker. A
    /// caller stop (deadline, cancellation, session revocation, owner stop)
    /// wins only if it abandons the fence before the worker commits. Once the
    /// worker has committed an effect, the caller awaits its actual outcome,
    /// so a published effect is never reported as a stop.
    pub(crate) async fn run_task_fenced<T: Send + 'static>(
        mut self,
        fence: Option<Arc<activation::CommitFence>>,
        task: impl FnOnce(Principal, &mut Budget) -> Result<T, ReadError> + Send + 'static,
    ) -> Result<T, ReadError> {
        enum Stop {
            Deadline,
            Cancelled,
            Forbidden,
            Owner,
        }
        let mut budget = self.budget.take().expect("live admission");
        let principal = self.principal.take().expect("live admission");
        let permit = self.permit.take().expect("live admission");
        let cancel = budget.cancel.clone();
        let deadline = budget.deadline;
        let owner_cancel = budget.owner_cancel.clone();
        let session = budget.session.clone();
        let mut drop_guard = CancelOnDrop {
            token: cancel.clone(),
            armed: true,
        };
        budget.check()?;
        let inner = self.inner.clone();
        budget.lease = Some(Arc::new(WorkLease {
            _permit: permit,
            _guard: self.guard.take(),
        }));
        // The worker and awaiting caller share the same registration through
        // completion handoff. Otherwise the worker can make drain observe zero
        // work and signal owner stop before its ready result reaches the caller.
        // On prompt cancellation/drop this copy releases while the worker's
        // capture still owns any queued or running work until actual exit.
        let _completion_lease = budget.lease.as_ref().expect("installed lease").clone();
        let mut job = spawn_worker(inner, principal, task, budget);
        let stopped = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(Stop::Deadline),
            _ = cancel.cancelled() => Err(Stop::Cancelled),
            _ = async { match &session { Some(session) => session.cancelled().await, None => std::future::pending::<()>().await } } => Err(Stop::Forbidden),
            // A completed result wins an owner stop racing delivery. The
            // caller's cancellation and absolute deadline keep their priority;
            // an in-flight worker still takes the prompt owner-stop branch.
            result = &mut job => Ok(result),
            _ = async {
                match owner_cancel { Some(token) => token.cancelled().await, None => std::future::pending::<()>().await }
            } => Err(Stop::Owner),
        };
        let result = match stopped {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => return Err(ReadError::Unavailable),
            // The worker passed its commit point: report what it actually did.
            Err(_) if fence.as_ref().is_some_and(|fence| !fence.abandon()) => match job.await {
                Ok(result) => result,
                Err(_) => return Err(ReadError::Unavailable),
            },
            Err(stop) => {
                if !matches!(stop, Stop::Cancelled) {
                    cancel.cancel();
                }
                job.abort();
                Err(match stop {
                    Stop::Deadline => ReadError::Deadline,
                    Stop::Cancelled | Stop::Owner => ReadError::Cancelled,
                    Stop::Forbidden => ReadError::Forbidden,
                })
            }
        };
        // A stop is a prompt request outcome, not proof that the worker exited.
        // Awaiting an aborted spawn_blocking job can wait indefinitely behind
        // unrelated runtime work. Its capture retains admission until release.
        drop_guard.armed = false;
        result
    }
}

/// Managed workers carry a tracker registration through the runtime's actual
/// closure lifetime; unmanaged workers keep their caller-runtime contract.
/// Neither abort nor a returned request error is a join receipt. Tokio may keep
/// an aborted closure in its blocking queue, so admission and registration
/// belong to the closure capture rather than the awaiting caller.
fn spawn_worker<T: Send + 'static>(
    inner: Arc<Inner>,
    principal: Principal,
    task: impl FnOnce(Principal, &mut Budget) -> Result<T, ReadError> + Send + 'static,
    mut budget: Budget,
) -> tokio::task::JoinHandle<Result<T, ReadError>> {
    let lifecycle = inner.lifecycle.clone();
    let work = move || {
        budget.check()?;
        task(principal, &mut budget)
    };
    match lifecycle {
        Some(life) => life.runtime().spawn_blocking(work),
        None => tokio::task::spawn_blocking(work),
    }
}

/// Install the pinned bootstrap observation on an unmanaged graph, deriving its
/// namespace from the graph's current one off-lock, or reuse the graph's
/// existing observation. Construction only: a graph later replaced by a raw
/// unmanaged graph is never silently re-bootstrapped by running services.
///
/// The pinned closure is admitted once. Publication compares only the inputs
/// the observation was derived from (incarnation, catalog generation, base
/// namespace identity), so concurrent entity writes never force a retry.
/// Installing it deliberately advances the graph's catalog generation once and
/// emits one catalog wake: the namespace gains the admitted declarations.
///
/// The pinned selection (embedded sources, strict compilation and callable
/// codec contexts) is admitted once per process and shared. Each installation
/// still derives its own namespace over the graph's base and is a distinct
/// observation, so identity-based currency checks stay per installation.
fn bootstrap(
    graph: &SharedGraph,
) -> Result<Arc<haystack_core::xeto::catalog::ActivatedCatalog>, ReadError> {
    bootstrap_with(graph, || {})
}
/// The process-wide admitted pinned bootstrap. Its inputs are embedded
/// sources, so the result never varies; a failure is a build defect and is
/// reported on every construction rather than retried.
fn pinned_bootstrap() -> Result<&'static haystack_core::xeto::catalog::ActivatedCatalog, ReadError>
{
    use haystack_core::xeto::catalog::{ActivatedCatalog, Catalog};
    static PINNED: std::sync::OnceLock<Option<ActivatedCatalog>> = std::sync::OnceLock::new();
    PINNED
        .get_or_init(|| {
            Catalog::load_http_pinned()
                .ok()
                .and_then(|catalog| ActivatedCatalog::new(catalog, None).ok())
        })
        .as_ref()
        .ok_or(ReadError::InvalidLimits)
}
fn bootstrap_with(
    graph: &SharedGraph,
    mut before_publish: impl FnMut(),
) -> Result<Arc<haystack_core::xeto::catalog::ActivatedCatalog>, ReadError> {
    use haystack_core::xeto::catalog::ActivatedCatalog;
    let mut prepared: Option<(Option<Arc<DefNamespace>>, Arc<ActivatedCatalog>)> = None;
    // Only a concurrent catalog change (namespace replacement or another
    // initializer) can force another attempt.
    for _ in 0..8 {
        let (state, current, base) = graph.read(|g| {
            (
                g.state(),
                g.activated_catalog().cloned(),
                g.namespace_arc().cloned(),
            )
        });
        if let Some(current) = current {
            return Ok(current);
        }
        let reusable = prepared
            .as_ref()
            .is_some_and(|(prior, _)| match (prior, &base) {
                (None, None) => true,
                (Some(prior), Some(base)) => Arc::ptr_eq(prior, base),
                _ => false,
            });
        // Derive only when the base differs from the prepared one (the first
        // attempt, or after a namespace replacement).
        if !reusable {
            let candidate =
                pinned_bootstrap()?.rebase(base.as_deref().cloned().unwrap_or_default());
            prepared = Some((base.clone(), Arc::new(candidate)));
        }
        let candidate = prepared.as_ref().expect("prepared candidate").1.clone();
        before_publish();
        if graph
            .write(|g| g.compare_initialize_catalog(state, base.as_ref(), candidate.clone()))
            .is_ok()
        {
            return Ok(candidate);
        }
    }
    Err(ReadError::Unavailable)
}

/// The retained observation must still be the graph's current one before any
/// graph evaluation; otherwise discovery, fitting and data could mix versions.
fn current_observation(
    graph: &haystack_core::graph::EntityGraph,
    registry: &crate::registry::Registry,
) -> Result<(), crate::ApiError> {
    match graph.activated_catalog() {
        Some(current) if Arc::ptr_eq(current, registry.observation()) => Ok(()),
        _ => Err(crate::ApiError::Unavailable),
    }
}

impl Inner {
    /// Bind (or reuse the cached binding of) one observation. Binding happens
    /// outside the cache lock; a stale overwrite only costs a later rebind.
    pub(crate) fn bound(
        &self,
        observation: Arc<haystack_core::xeto::catalog::ActivatedCatalog>,
    ) -> Result<Arc<crate::registry::Registry>, ReadError> {
        {
            let cached = self.registry.lock();
            if Arc::ptr_eq(cached.observation(), &observation) {
                return Ok(cached.clone());
            }
        }
        let registry = Arc::new(crate::registry::Registry::bind(observation)?);
        *self.registry.lock() = registry.clone();
        Ok(registry)
    }
    /// Capture the graph's current observation once per typed request.
    fn capture_registry(
        &self,
        budget: &mut Budget,
    ) -> Result<Arc<crate::registry::Registry>, crate::ApiError> {
        let observation = loop {
            if let Some(observation) = self.graph.read_for(budget.wait_quantum()?, |graph| {
                graph.activated_catalog().cloned()
            }) {
                break observation;
            }
        };
        let observation = observation.ok_or(crate::ApiError::Unavailable)?;
        self.bound(observation)
            .map_err(|_| crate::ApiError::Unavailable)
    }
    fn execute_typed(
        &self,
        principal: Principal,
        session: Option<crate::SubscriptionSession>,
        input: crate::TypedInvocationInput,
        budget: &mut Budget,
    ) -> Result<crate::TypedInvocationResponse, crate::ApiError> {
        use crate::{ApiError, typed_http};
        if principal.bytes() > budget.limits.max_input_bytes {
            return Err(ApiError::InvalidArgs);
        }
        budget.check()?;
        if session
            .as_ref()
            .is_some_and(|session| !session.matches(&principal) || !session.is_active())
        {
            return Err(ApiError::Permission);
        }
        // No token lookup can rebind an invocation to a replacement login.
        let envelope = typed_http::envelope(&input, budget)?;
        let policy = self.policy.snapshot(&principal)?;
        budget.check()?;
        if !policy.operation(ReadOperation::Read) {
            return Err(ApiError::Permission);
        }
        // One retained observation serves resolution, discovery, argument
        // decoding/defaults/fitting, metadata and result encoding. Graph
        // evaluation below first verifies it is still the graph's current one.
        let registry = self.capture_registry(budget)?;
        #[cfg(test)]
        if let Some(hook) = &budget.typed_lookup_hook {
            hook();
        }
        let entry = registry.resolve(&envelope.operation, policy.as_ref(), budget)?;
        entry.permits_method(input.post)?;
        let request = typed_http::decode(&input, &entry.wire, envelope, budget)?;
        // Reserve native binding copies before the binder constructs immutable
        // values. Collection arguments need the decoded expansion allowance.
        budget.charge(
            BudgetKind::Retained,
            input
                .body
                .len()
                .saturating_add(input.query.len())
                .saturating_mul(if entry.handler == crate::registry::Handler::ReadById {
                    4
                } else {
                    512
                })
                .saturating_add(4096),
        )?;
        let args = registry
            .catalog()
            .fit_arguments(&entry.identity.qname, &request.args)
            .map_err(|_| ApiError::InvalidArgs)?;
        let value = match entry.handler {
            crate::registry::Handler::Ops => registry.ops(policy.as_ref(), budget)?,
            crate::registry::Handler::About => self.about(&registry, &principal, budget)?,
            crate::registry::Handler::Libs => self.libraries(&registry, policy.as_ref(), budget)?,
            crate::registry::Handler::Filetypes => self.filetypes(request.version, budget)?,
            crate::registry::Handler::Nav => {
                self.nav(&registry, args.values(), policy.as_ref(), budget)?
            }
            crate::registry::Handler::Close => {
                if session.is_none() {
                    return Err(ApiError::AuthRequired);
                }
                Kind::None
            }
            handler @ (crate::registry::Handler::ReadByIds
            | crate::registry::Handler::Read
            | crate::registry::Handler::ReadAll) => {
                match self.system_read(&registry, handler, args.values(), policy.as_ref(), budget) {
                    Err(ApiError::UnknownEntity) => return typed_http::missing(&request, budget),
                    value => value?,
                }
            }
            crate::registry::Handler::ReadById => {
                let checked = matches!(args.values().get("checked"), Some(Kind::Bool(true)));
                match args.values().get("id") {
                    Some(Kind::Null) if checked => return typed_http::missing(&request, budget),
                    Some(Kind::Null) => Kind::Null,
                    Some(Kind::Ref(id)) => loop {
                        let wait = budget.wait_quantum()?;
                        if let Some(result) = self.graph.read_for(wait, |graph| {
                            current_observation(graph, &registry)?;
                            budget.charge(BudgetKind::Candidates, 1)?;
                            let mut view = View {
                                graph,
                                policy: policy.as_ref(),
                                budget,
                            };
                            Ok::<_, ApiError>(view.entity(&id.val)?)
                        }) {
                            match result? {
                                Some(row) => {
                                    break Kind::Dict(Box::new(
                                        Arc::try_unwrap(row).map_err(|_| ApiError::Internal)?,
                                    ));
                                }
                                None if checked => return typed_http::missing(&request, budget),
                                None => break Kind::Null,
                            }
                        }
                    },
                    _ => return Err(ApiError::Internal),
                }
            }
        };
        registry
            .catalog()
            .fit_result(&entry.identity.qname, &value)
            .map_err(|_| ApiError::Internal)?;
        let response = typed_http::encode(value, &request, &entry.wire, budget)?;
        if entry.handler == crate::registry::Handler::Close {
            budget.check()?;
            budget
                .session
                .as_ref()
                .ok_or(ApiError::AuthRequired)?
                .close()?;
        }
        budget.check()?;
        Ok(response)
    }
    fn execute(
        &self,
        principal: Principal,
        input: Input,
        budget: &mut Budget,
    ) -> Result<ReadPage, ReadError> {
        if let Principal::Authenticated { permissions, .. } = &principal
            && permissions.len() > budget.limits.max_ids
        {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        if principal.bytes() > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        budget.check()?;
        let policy = self.policy.snapshot(&principal)?;
        budget.check()?;
        if !policy.operation(input.operation()) {
            return Err(ReadError::Forbidden);
        }
        if policy.scope_key().len() > budget.limits.max_input_bytes {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let request = match input {
            Input::Request(request) => request,
            Input::Wire {
                operation,
                body,
                input,
                output,
            } => wire::decode(operation, &body, input, output, budget)?,
        };
        let prepared = normalize(request, principal, policy.as_ref(), budget)?;
        let prior = prepared
            .cursor
            .as_deref()
            .map(|c| self.lookup(c, budget))
            .transpose()?;
        if prior
            .as_ref()
            .is_some_and(|(identity, _, _)| identity.as_ref() != prepared.identity.as_ref())
        {
            return Err(ReadError::StaleCursor);
        }
        loop {
            let wait = budget.wait_quantum()?;
            if let Some(result) = self.graph.read_for(wait, |graph| {
                let stamp = Stamp {
                    dataset: self.dataset,
                    incarnation: graph.incarnation(),
                    entities: graph.version(),
                    catalog: graph.catalog_generation(),
                };
                if prior.as_ref().is_some_and(|(_, old, _)| *old != stamp) {
                    return Err(ReadError::StaleCursor);
                }
                let after = prior.as_ref().map(|(_, _, id)| id.as_str());
                let mut view = View {
                    graph,
                    policy: policy.as_ref(),
                    budget,
                };
                validate_query(&prepared.identity.query, &mut view)?;
                let (rows, complete, last) = produce(&prepared.identity, after, &mut view)?;
                let count = rows.len();
                let pending = if complete {
                    None
                } else {
                    Some(self.token(view.budget)?)
                };
                let cursor = pending.as_ref().map(|(_, token)| token.as_str());
                let grid = output::grid(rows, complete, cursor, view.budget)?;
                let output = output::encode(grid, prepared.identity.profile, view.budget)?;
                if let Some((nonce, _)) = &pending {
                    let after = last.ok_or(ReadError::Unavailable)?;
                    self.insert(
                        *nonce,
                        prepared.identity.clone(),
                        stamp,
                        after,
                        prepared.retained_estimate,
                        view.budget,
                    )?;
                }
                Ok(ReadPage {
                    output,
                    complete,
                    cursor: pending.map(|(_, token)| token),
                    row_count: count,
                })
            }) {
                return result;
            }
        }
    }
    fn lock_cursors<'a>(
        &'a self,
        budget: &Budget,
    ) -> Result<MutexGuard<'a, CursorTable>, ReadError> {
        loop {
            if let Some(guard) = self.cursors.try_lock_for(budget.wait_quantum()?) {
                return Ok(guard);
            }
        }
    }
    fn token(&self, budget: &mut Budget) -> Result<([u8; 16], String), ReadError> {
        budget.charge(BudgetKind::Retained, 256)?;
        let nonce: [u8; 16] = rand::random();
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.cursor_key).map_err(|_| ReadError::Unavailable)?;
        mac.update(&nonce);
        let mut bytes = Vec::with_capacity(48);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&mac.finalize().into_bytes());
        Ok((nonce, URL_SAFE_NO_PAD.encode(bytes)))
    }
    fn lookup(
        &self,
        token: &str,
        budget: &mut Budget,
    ) -> Result<(Arc<RequestIdentity>, Stamp, String), ReadError> {
        if token.len() != 64 {
            return Err(ReadError::StaleCursor);
        }
        budget.charge(BudgetKind::Retained, 256)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(token)
            .map_err(|_| ReadError::StaleCursor)?;
        if bytes.len() != 48 {
            return Err(ReadError::StaleCursor);
        }
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.cursor_key).map_err(|_| ReadError::Unavailable)?;
        mac.update(&bytes[..16]);
        mac.verify_slice(&bytes[16..])
            .map_err(|_| ReadError::StaleCursor)?;
        let nonce: [u8; 16] = bytes[..16].try_into().map_err(|_| ReadError::StaleCursor)?;
        let table = self.lock_cursors(budget)?;
        let record = table.entries.get(&nonce).ok_or(ReadError::StaleCursor)?;
        if Instant::now() >= record.expires {
            return Err(ReadError::StaleCursor);
        }
        Ok((
            record.identity.clone(),
            record.stamp.clone(),
            budget.copy_string(&record.after)?,
        ))
    }
    fn insert(
        &self,
        nonce: [u8; 16],
        identity: Arc<RequestIdentity>,
        stamp: Stamp,
        after: String,
        estimate: usize,
        budget: &mut Budget,
    ) -> Result<(), ReadError> {
        let bytes = estimate.saturating_add(after.len()).saturating_add(512);
        let mut table = self.lock_cursors(budget)?;
        budget.charge(BudgetKind::Work, table.entries.len())?;
        let now = Instant::now();
        table.entries.retain(|_, record| record.expires > now);
        table.bytes = table.entries.values().map(|r| r.bytes).sum();
        if table.entries.len() >= self.limits.cursor_capacity
            || bytes > self.limits.max_cursor_bytes.saturating_sub(table.bytes)
        {
            return Err(ReadError::Capacity);
        }
        budget.check()?;
        table.bytes += bytes;
        table.entries.insert(
            nonce,
            CursorRecord {
                identity,
                stamp,
                after,
                expires: now + self.limits.cursor_ttl,
                bytes,
            },
        );
        Ok(())
    }
}

fn request_bytes(request: &ReadRequest) -> usize {
    let query = match &request.query {
        ReadQuery::Ids(ids) => ids.iter().fold(0usize, |n, id| {
            n.saturating_add(id.len()).saturating_add(32)
        }),
        ReadQuery::Filter(s) | ReadQuery::Spec(s) => s.len(),
        ReadQuery::Nav(s)
        | ReadQuery::Definitions { filter: s }
        | ReadQuery::Specs { library: s } => s.as_ref().map_or(0, String::len),
        ReadQuery::Libraries => 0,
    };
    request.projection.iter().fold(
        query.saturating_add(request.cursor.as_ref().map_or(0, String::len)),
        |n, s| n.saturating_add(s.len()).saturating_add(32),
    )
}
fn normalize(
    mut request: ReadRequest,
    principal: Principal,
    policy: &dyn PolicySnapshot,
    budget: &mut Budget,
) -> Result<Prepared, ReadError> {
    if request.profile == OutputProfile::H4(H4Codec::Trio) {
        return Err(ReadError::InvalidQuery("codec cannot carry page metadata"));
    }
    if request.page_size == 0 || request.page_size > budget.limits.max_rows {
        return Err(ReadError::Budget(BudgetKind::Rows));
    }
    if request.projection.len() > budget.limits.max_ids {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    if let ReadQuery::Ids(ids) = &request.query
        && ids.len() > budget.limits.max_ids
    {
        return Err(ReadError::Budget(BudgetKind::Ids));
    }
    let bytes = request_bytes(&request)
        .saturating_add(principal.bytes())
        .saturating_add(policy.scope_key().len());
    if bytes > budget.limits.max_input_bytes {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    // Covers parser scalar/container expansion and retained normalized identity.
    let estimate = bytes.saturating_mul(512).saturating_add(1024);
    budget.charge(BudgetKind::Retained, estimate)?;
    budget.charge(BudgetKind::Work, bytes.saturating_add(1))?;
    request.projection.sort();
    request.projection.dedup();
    let query = match request.query {
        ReadQuery::Ids(mut ids) => {
            ids.sort();
            ids.dedup();
            NormalizedQuery::Ids(ids)
        }
        ReadQuery::Filter(filter) => {
            let filter = filter.trim();
            if filter == "*" {
                NormalizedQuery::Filter(None)
            } else {
                let limits = FilterParseLimits {
                    max_bytes: budget.limits.max_input_bytes,
                    max_nodes: budget.limits.max_ast_nodes,
                    max_depth: budget.limits.max_ast_depth,
                };
                let ast = filter::parse_filter_controlled(filter, limits, &mut || {
                    budget.check().map_err(|_| FilterError::Interrupted)
                })
                .map_err(|error| match error {
                    FilterError::Limit => ReadError::Budget(BudgetKind::Ast),
                    FilterError::Interrupted => {
                        budget.check().err().unwrap_or(ReadError::Cancelled)
                    }
                    FilterError::Parse { .. } => ReadError::InvalidQuery("invalid filter"),
                })?;
                NormalizedQuery::Filter(Some(ast))
            }
        }
        ReadQuery::Nav(s) => NormalizedQuery::Nav(s),
        ReadQuery::Definitions { filter } => NormalizedQuery::Definitions(filter),
        ReadQuery::Libraries => NormalizedQuery::Libraries,
        ReadQuery::Specs { library } => NormalizedQuery::Specs(library),
        ReadQuery::Spec(s) => NormalizedQuery::Spec(s),
    };
    let scope: Arc<str> = Arc::from(budget.copy_string(policy.scope_key())?);
    let identity = Arc::new(RequestIdentity {
        query,
        projection: request.projection,
        page_size: request.page_size,
        profile: request.profile,
        principal,
        scope,
    });
    Ok(Prepared {
        identity,
        cursor: request.cursor,
        retained_estimate: estimate,
    })
}
fn validate_query(query: &NormalizedQuery, view: &mut View<'_>) -> Result<(), ReadError> {
    fn ast(node: &FilterNode, view: &mut View<'_>, depth: usize) -> Result<(), ReadError> {
        view.budget.depth(depth)?;
        view.budget.charge(BudgetKind::Work, 1)?;
        match node {
            FilterNode::SpecMatch(term) => {
                if !filter::catalog_term_available(view.graph.namespace(), term, view)? {
                    return Err(ReadError::InvalidQuery("catalog term is unavailable"));
                }
            }
            FilterNode::And(l, r) | FilterNode::Or(l, r) => {
                ast(l, view, depth + 1)?;
                ast(r, view, depth + 1)?;
            }
            FilterNode::Cmp { val, .. } => sanitize::measure(val, view.budget, 1)?,
            _ => {}
        }
        Ok(())
    }
    if let NormalizedQuery::Filter(Some(node)) = query {
        ast(node, view, 1)?;
    }
    Ok(())
}
fn project(mut row: HDict, projection: &[String], budget: &mut Budget) -> Result<HDict, ReadError> {
    if !projection.is_empty() {
        let mut out = HDict::new();
        for tag in projection {
            budget.charge(BudgetKind::Work, 1)?;
            if let Some(value) = row.remove_tag(tag) {
                budget.charge(BudgetKind::Retained, 512)?;
                out.set(budget.copy_string(tag)?, value);
            }
        }
        row = out;
    }
    Ok(row)
}
fn nav_row(mut row: HDict, budget: &mut Budget) -> Result<HDict, ReadError> {
    let mut out = HDict::new();
    if let Some(Kind::Ref(id)) = row.remove_tag("id") {
        out.set("navId", Kind::Str(budget.copy_string(&id.val)?));
        out.set("id", Kind::Ref(id));
    }
    if let Some(dis) = row.remove_tag("dis") {
        out.set("dis", dis);
    }
    budget.charge(BudgetKind::Retained, 1536)?;
    Ok(out)
}
fn produce(
    identity: &RequestIdentity,
    after: Option<&str>,
    view: &mut View<'_>,
) -> Result<(Vec<HDict>, bool, Option<String>), ReadError> {
    match &identity.query {
        NormalizedQuery::Definitions(_)
        | NormalizedQuery::Libraries
        | NormalizedQuery::Specs(_)
        | NormalizedQuery::Spec(_) => return catalog_page(identity, after, view),
        _ => {}
    }
    let mut rows = Vec::new();
    let mut last = None;
    if let NormalizedQuery::Nav(Some(parent)) = &identity.query {
        if !view.policy.entity(parent) || !view.graph.contains(parent) {
            return Ok((rows, true, None));
        }
        // Charge raw inverse work once, including denied/wrong-relation edges.
        for _ in view.graph.incoming_edges(parent) {
            view.budget.charge(BudgetKind::Inverse, 1)?;
        }
    }
    let mut accept = |id: &str, view: &mut View<'_>| -> Result<bool, ReadError> {
        view.budget.charge(BudgetKind::Candidates, 1)?;
        let Some(row) = view.entity(id)? else {
            return Ok(false);
        };
        let matched = match &identity.query {
            NormalizedQuery::Filter(Some(node)) => {
                filter::matches_controlled(node, row.clone(), view.graph.namespace(), view)?
            }
            NormalizedQuery::Nav(None) => row.has("site"),
            NormalizedQuery::Nav(Some(parent)) => {
                let mut found = false;
                for (tag, value) in row.iter() {
                    view.budget.charge(BudgetKind::Work, 1)?;
                    if tag != "id" && matches!(value, Kind::Ref(r) if r.val == *parent) {
                        found = true;
                    }
                }
                found
            }
            _ => true,
        };
        if !matched {
            return Ok(false);
        }
        if rows.len() == identity.page_size {
            return Ok(true);
        }
        let row = Arc::try_unwrap(row).map_err(|_| ReadError::Unavailable)?;
        let row = if matches!(identity.query, NormalizedQuery::Nav(_)) {
            nav_row(row, view.budget)?
        } else {
            row
        };
        let row = project(row, &identity.projection, view.budget)?;
        view.budget.charge(BudgetKind::Retained, 128)?;
        rows.push(row);
        last = Some(view.budget.copy_string(id)?);
        Ok(false)
    };
    let mut more = false;
    match &identity.query {
        NormalizedQuery::Ids(ids) => {
            for id in ids
                .iter()
                .filter(|id| after.is_none_or(|after| id.as_str() > after))
            {
                if accept(id, view)? {
                    more = true;
                    break;
                }
            }
        }
        _ => {
            for (id, _) in view.graph.entities_after(after) {
                if accept(id, view)? {
                    more = true;
                    break;
                }
            }
        }
    }
    Ok((rows, !more, last))
}

fn catalog_page(
    identity: &RequestIdentity,
    after: Option<&str>,
    view: &mut View<'_>,
) -> Result<(Vec<HDict>, bool, Option<String>), ReadError> {
    let Some(ns) = view.graph.namespace() else {
        return if matches!(identity.query, NormalizedQuery::Spec(_)) {
            Err(ReadError::Unavailable)
        } else {
            Ok((Vec::new(), true, None))
        };
    };
    // Keep only page_size+1 borrowed names while scanning the unordered maps.
    // Work includes denied entries; no namespace helper materializes all names.
    let mut selected: BTreeMap<&str, CatalogRow<'_>> = BTreeMap::new();
    match &identity.query {
        NormalizedQuery::Definitions(filter) => {
            for (name, def) in ns.defs() {
                catalog_visit(name, view.budget)?;
                let allowed = view.policy.catalog(CatalogKind::Definition, name)
                    && view.policy.catalog(CatalogKind::Library, &def.lib)
                    && filter.as_ref().is_none_or(|f| name.contains(f));
                select_catalog(
                    &mut selected,
                    (name, CatalogRow::Definition(def)),
                    allowed,
                    after,
                    identity.page_size,
                    view.budget,
                )?;
            }
        }
        NormalizedQuery::Libraries => {
            for (name, lib) in ns.libs() {
                catalog_visit(name, view.budget)?;
                select_catalog(
                    &mut selected,
                    (name, CatalogRow::Library(lib)),
                    view.policy.catalog(CatalogKind::Library, name),
                    after,
                    identity.page_size,
                    view.budget,
                )?;
            }
        }
        NormalizedQuery::Specs(lib) => {
            for (name, spec) in ns.specs_map() {
                catalog_visit(name, view.budget)?;
                let allowed = view.policy.catalog(CatalogKind::Spec, name)
                    && view.policy.catalog(CatalogKind::Library, &spec.lib)
                    && lib.as_ref().is_none_or(|lib| *lib == spec.lib);
                select_catalog(
                    &mut selected,
                    (name, CatalogRow::Spec(spec, false)),
                    allowed,
                    after,
                    identity.page_size,
                    view.budget,
                )?;
            }
        }
        NormalizedQuery::Spec(name) => {
            let spec = ns
                .get_spec(name)
                .filter(|s| {
                    view.policy.catalog(CatalogKind::Spec, name)
                        && view.policy.catalog(CatalogKind::Library, &s.lib)
                })
                .ok_or(ReadError::Unavailable)?;
            catalog_visit(&spec.qname, view.budget)?;
            select_catalog(
                &mut selected,
                (&spec.qname, CatalogRow::Spec(spec, true)),
                true,
                after,
                identity.page_size,
                view.budget,
            )?;
        }
        _ => unreachable!(),
    }
    let complete = selected.len() <= identity.page_size;
    if !complete {
        selected.pop_last();
    }
    let mut rows = Vec::new();
    let mut last = None;
    for (name, item) in selected {
        let row = catalog_row(item, ns, view)?;
        let row = project(row, &identity.projection, view.budget)?;
        view.budget.charge(BudgetKind::Retained, 128)?;
        rows.push(row);
        last = Some(view.budget.copy_string(name)?);
    }
    Ok((rows, complete, last))
}
fn catalog_visit(name: &str, budget: &mut Budget) -> Result<(), ReadError> {
    budget.charge(BudgetKind::Candidates, 1)?;
    budget.charge(BudgetKind::Work, name.len().saturating_add(1))
}
fn select_catalog<'a>(
    selected: &mut BTreeMap<&'a str, CatalogRow<'a>>,
    (name, item): (&'a str, CatalogRow<'a>),
    allowed: bool,
    after: Option<&str>,
    limit: usize,
    budget: &mut Budget,
) -> Result<(), ReadError> {
    if !allowed || after.is_some_and(|after| name <= after) {
        return Ok(());
    }
    if selected.len() > limit
        && selected
            .last_key_value()
            .is_some_and(|(last, _)| name >= *last)
    {
        return Ok(());
    }
    budget.charge(BudgetKind::Retained, 128)?;
    selected.insert(name, item);
    if selected.len() > limit + 1 {
        selected.pop_last();
    }
    Ok(())
}
enum CatalogRow<'a> {
    Definition(&'a haystack_core::ontology::Def),
    Library(&'a haystack_core::ontology::Lib),
    Spec(&'a Spec, bool),
}
fn put(row: &mut HDict, tag: &str, value: Kind, budget: &mut Budget) -> Result<(), ReadError> {
    budget.charge(BudgetKind::Retained, 512)?;
    row.set(budget.copy_string(tag)?, value);
    Ok(())
}
fn text(row: &mut HDict, tag: &str, value: &str, budget: &mut Budget) -> Result<(), ReadError> {
    let value = budget.copy_string(value)?;
    put(row, tag, Kind::Str(value), budget)
}
fn catalog_row(
    item: CatalogRow<'_>,
    ns: &DefNamespace,
    view: &mut View<'_>,
) -> Result<HDict, ReadError> {
    let mut row = HDict::new();
    match item {
        CatalogRow::Definition(def) => {
            let symbol = Symbol::new(view.budget.copy_string(&def.symbol)?);
            put(&mut row, "def", Kind::Symbol(symbol), view.budget)?;
            let lib = Symbol::new(view.budget.copy_string(&def.lib)?);
            put(&mut row, "lib", Kind::Symbol(lib), view.budget)?;
            text(&mut row, "doc", &def.doc, view.budget)?;
        }
        CatalogRow::Library(lib) => {
            text(&mut row, "name", &lib.name, view.budget)?;
            text(&mut row, "version", &lib.version, view.budget)?;
        }
        CatalogRow::Spec(spec, detail) => {
            for (tag, value) in [
                ("qname", &spec.qname),
                ("name", &spec.name),
                ("lib", &spec.lib),
                ("doc", &spec.doc),
            ] {
                text(&mut row, tag, value, view.budget)?;
            }
            if let Some(base) = &spec.base
                && filter::catalog_term_available(Some(ns), base, view)?
            {
                text(&mut row, "base", base, view.budget)?;
            }
            if spec.is_abstract {
                put(&mut row, "abstract", Kind::Marker, view.budget)?;
            }
            if detail {
                let mut slots = Vec::new();
                for slot in &spec.slots {
                    view.budget.charge(BudgetKind::Work, 1)?;
                    view.budget.charge(BudgetKind::Retained, 128)?;
                    let mut entry = HDict::new();
                    text(&mut entry, "name", &slot.name, view.budget)?;
                    if slot.is_marker {
                        put(&mut entry, "marker", Kind::Marker, view.budget)?;
                    }
                    if slot.is_maybe() {
                        put(&mut entry, "maybe", Kind::Marker, view.budget)?;
                    }
                    if slot.is_query {
                        put(&mut entry, "query", Kind::Marker, view.budget)?;
                    }
                    if let Some(ty) = &slot.type_ref {
                        let primitive = matches!(
                            ty.as_str(),
                            "Str"
                                | "Number"
                                | "Ref"
                                | "Bool"
                                | "Date"
                                | "Time"
                                | "DateTime"
                                | "Uri"
                                | "Coord"
                                | "List"
                                | "Dict"
                                | "Grid"
                                | "Marker"
                                | "Query"
                        );
                        if primitive || filter::catalog_term_available(Some(ns), ty, view)? {
                            text(&mut entry, "type", ty, view.budget)?;
                        }
                    }
                    slots.push(Kind::Dict(Box::new(entry)));
                }
                put(&mut row, "slots", Kind::List(slots), view.budget)?;
            }
        }
    }
    Ok(row)
}

fn application_read_error(error: ApplicationError) -> ReadError {
    match error {
        ApplicationError::NotReady => ReadError::NotReady,
        _ => ReadError::Closed,
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use crate::AllowAll;
    use haystack_core::graph::EntityGraph;
    use std::{future::Future, task::Poll, time::Duration};

    #[test]
    fn second_review_completed_worker_wins_owner_stop_but_not_caller_cancellation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            for caller_stop in [false, true] {
                let reads = ReadService::new(
                    SharedGraph::new(EntityGraph::new()),
                    Arc::new(AllowAll),
                    ReadLimits::default(),
                )
                .unwrap();
                let context =
                    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(5));
                let caller = context.cancellation.clone();
                let stop = CancellationToken::new();
                let mut admission = reads.begin(context).await.unwrap();
                admission.budget_mut().owner_cancel = Some(stop.clone());
                let (entered_tx, entered_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel();
                let mut result = Box::pin(admission.run_task(move |_, _| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(7)
                }));
                assert!(
                    std::future::poll_fn(|cx| Poll::Ready(result.as_mut().poll(cx)))
                        .await
                        .is_pending()
                );
                entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                let sentinel = tokio::task::spawn_blocking(|| ());
                release_tx.send(()).unwrap();
                sentinel.await.unwrap();
                // One blocking thread makes this a receipt that the first
                // worker has exited and its join result is ready, without
                // polling the request future or relying on elapsed sleeps.
                stop.cancel();
                if caller_stop {
                    caller.cancel();
                }
                if caller_stop {
                    assert_eq!(result.await.unwrap_err(), ReadError::Cancelled);
                } else {
                    assert_eq!(result.await.unwrap(), 7);
                }
                assert_eq!(reads.load().admitted, 0);
            }
        });
    }
    #[test]
    fn second_review_owner_stop_still_returns_promptly_while_worker_is_running() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let reads = ReadService::new(
                SharedGraph::new(EntityGraph::new()),
                Arc::new(AllowAll),
                ReadLimits::default(),
            )
            .unwrap();
            let mut admission = reads
                .begin(ReadContext::with_timeout(
                    Principal::Anonymous,
                    Duration::from_secs(5),
                ))
                .await
                .unwrap();
            let stop = CancellationToken::new();
            admission.budget_mut().owner_cancel = Some(stop.clone());
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let mut result = Box::pin(admission.run_task(move |_, _| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(7)
            }));
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(result.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            stop.cancel();
            let outcome = result.await;
            let held = reads.load().admitted;
            release_tx.send(()).unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert_eq!(outcome.unwrap_err(), ReadError::Cancelled);
            assert_eq!(held, 1);
            assert_eq!(reads.load().admitted, 0);
        });
    }
}

#[cfg(test)]
mod registry_dispatch_tests {
    use super::*;
    use crate::{AllowAll, FunctionIdentity, TypedInvocationInput};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Policy(Arc<AtomicUsize>);
    impl ReadPolicy for Policy {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(Self(self.0.clone())))
        }
    }
    impl PolicySnapshot for Policy {
        fn scope_key(&self) -> &str {
            "method-side-effect-fixture"
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            true
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, _: &str) -> bool {
            self.0.fetch_add(1, Ordering::SeqCst);
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
            AllowAll.catalog(kind, name)
        }
        fn nominal_provenance(&self, _: &haystack_core::kinds::NominalScalar) -> bool {
            true
        }
    }
    #[tokio::test]
    async fn get_without_marker_stops_before_handler_while_post_executes() {
        let graph = SharedGraph::new(haystack_core::graph::EntityGraph::new());
        let mut record = HDict::new();
        record.set("id", Kind::Ref(haystack_core::kinds::HRef::from_val("a")));
        graph.add(record).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut service = ReadService::new(
            graph,
            Arc::new(Policy(calls.clone())),
            ReadLimits::default(),
        )
        .unwrap();
        Arc::get_mut(Arc::get_mut(&mut service.inner).unwrap().registry.get_mut())
            .unwrap()
            .disable_read_get_for_test();
        let context =
            || ReadContext::with_timeout(Principal::Anonymous, std::time::Duration::from_secs(1));
        let input = TypedInvocationInput {
            operation: "readById".into(),
            versions: vec!["5".into()],
            query: "id=a".into(),
            ..Default::default()
        };
        assert!(matches!(
            service
                .begin(context())
                .await
                .unwrap()
                .invoke_wire(input)
                .await,
            Err(crate::ApiError::MethodNotAllowed)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let input = TypedInvocationInput {
            operation: "readById".into(),
            versions: vec!["5".into()],
            post: true,
            content_types: vec!["application/json".into()],
            body: br#"{"id":"a"}"#.to_vec(),
            ..Default::default()
        };
        assert!(
            service
                .begin(context())
                .await
                .unwrap()
                .invoke_wire(input)
                .await
                .is_ok()
        );
        // View may consult entity policy again while sanitizing the id Ref.
        // The positive control proves execution, without prescribing callbacks.
        assert!(calls.load(Ordering::SeqCst) > 0);
    }
}
