//! Explicit ownership for application work, resources and shutdown.
use crate::{HistoryService, MutationService, ReadError, ReadLimits, ReadPolicy, ReadService};
use futures_util::FutureExt;
use haystack_core::graph::SharedGraph;
use parking_lot::Mutex;
use std::{
    future::Future,
    net::SocketAddr,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{runtime::Handle, sync::watch, task::AbortHandle};
use tokio_util::{
    sync::CancellationToken,
    task::{TaskTracker, task_tracker::TaskTrackerToken},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationState {
    Starting,
    Running,
    Closing,
    Closed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApplicationError {
    #[error("application is not ready")]
    NotReady,
    #[error("application is closed to new work")]
    Closed,
    #[error("invalid application configuration: {0}")]
    Configuration(&'static str),
    #[error("application builder was dropped before start")]
    Abandoned,
    #[error("application startup deadline exceeded")]
    StartupTimeout,
    #[error("resource {resource}: {message}")]
    Resource { resource: String, message: String },
    #[error("owned task {0} failed")]
    Task(String),
    #[error("application shutdown timed out in {phase:?}; {outstanding} owned tasks remain")]
    ShutdownTimeout {
        phase: ShutdownPhase,
        outstanding: usize,
    },
}
impl ApplicationError {
    pub fn resource(resource: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Resource {
            resource: resource.into(),
            message: message.into(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPhase {
    Tasks,
    Resources,
}

#[derive(Debug, Clone, Copy)]
pub struct ShutdownPolicy {
    pub drain_timeout: Duration,
    pub stop_timeout: Duration,
}
impl Default for ShutdownPolicy {
    fn default() -> Self {
        Self {
            drain_timeout: Duration::from_secs(1),
            stop_timeout: Duration::from_secs(4),
        }
    }
}
impl ShutdownPolicy {
    fn validate(self) -> Result<(), ApplicationError> {
        let limit = Duration::from_secs(3600);
        if self.drain_timeout > limit || self.stop_timeout.is_zero() || self.stop_timeout > limit {
            Err(ApplicationError::Configuration(
                "shutdown durations must be finite and stop must be positive",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerInfo {
    pub resource: String,
    pub address: SocketAddr,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyInfo {
    pub listeners: Vec<ListenerInfo>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseReport {
    /// The cooperative-stop phase was needed after the drain allowance expired.
    pub drain_expired: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminationReport {
    /// Preserves the shared close outcome, including an earlier timeout even if
    /// work subsequently finished. Receiving this report proves cleanup ended.
    pub close: Result<CloseReport, ApplicationError>,
    pub cleanup_errors: Vec<ApplicationError>,
}

pub type ResourceFuture<'a, T = ()> =
    Pin<Box<dyn Future<Output = Result<T, ApplicationError>> + Send + 'a>>;

/// A resource explicitly transferred to the application. Borrowed resources
/// must not be registered here. Hooks execute once, on the selected runtime.
///
/// Initialization must be cancellation safe: dropping its future must stop
/// that attempt. Record any partial acquisition in `self` or an RAII guard.
/// `rollback_start` is invoked after a failed/cancelled attempt and must release
/// those partial acquisitions; an unsuccessful return is not cleanup evidence.
/// Successfully initialized resources instead receive `close`, in reverse
/// order, only after owned work has finished. Cleanup futures are retained and
/// observed even when the public close deadline expires.
pub trait ApplicationResource: Send + 'static {
    fn name(&self) -> &str;
    fn initialize(&mut self, context: ResourceContext) -> ResourceFuture<'_, ReadyInfo>;
    fn rollback_start(&mut self) -> ResourceFuture<'_>;
    fn close(&mut self) -> ResourceFuture<'_>;
}

/// Configuration is consumed by `start`. The early handle exists so adapters
/// can share the exact service/cursor authority; it rejects work until ready.
pub struct ApplicationBuilder {
    parts: Option<BuildParts>,
    startup_timeout: Duration,
    shutdown: ShutdownPolicy,
}
struct BuildParts {
    application: ApplicationHandle,
    resources: Vec<Box<dyn ApplicationResource>>,
    history_resource: Option<Box<dyn ApplicationResource>>,
}
impl ApplicationBuilder {
    pub fn new(
        graph: SharedGraph,
        policy: Arc<dyn ReadPolicy>,
        limits: ReadLimits,
    ) -> Result<Self, ReadError> {
        let lifecycle = Lifecycle::new();
        let reads = ReadService::managed(graph, policy, limits, lifecycle.clone())?;
        Ok(Self {
            parts: Some(BuildParts {
                application: ApplicationHandle {
                    lifecycle,
                    reads,
                    mutations: Arc::new(Mutex::new(None)),
                    history: Arc::new(Mutex::new(None)),
                    history_mutations: Arc::new(Mutex::new(None)),
                    subscriptions: Arc::new(Mutex::new(None)),
                },
                resources: vec![],
                history_resource: None,
            }),
            startup_timeout: Duration::from_secs(30),
            shutdown: ShutdownPolicy::default(),
        })
    }
    /// Configure the bounded label reported by typed `about`. The boot timestamp
    /// is captured once when this owner starts on its selected runtime.
    pub fn server_name(self, name: impl Into<String>) -> Result<Self, ReadError> {
        self.parts
            .as_ref()
            .expect("unconsumed builder")
            .application
            .reads
            .set_server_name(name.into())?;
        Ok(self)
    }
    pub fn handle(&self) -> ApplicationHandle {
        self.parts
            .as_ref()
            .expect("unconsumed builder")
            .application
            .clone()
    }
    /// Attach an opt-in mutation service sharing this exact managed admission,
    /// graph and read-policy authority. Early handle clones see the selection.
    pub fn entity_mutations(self, service: MutationService) -> Result<Self, ReadError> {
        let app = &self.parts.as_ref().expect("unconsumed builder").application;
        if !service.read_service().same_service(&app.reads) {
            return Err(ReadError::Forbidden);
        }
        if app
            .subscriptions
            .lock()
            .as_ref()
            .is_some_and(|subscriptions| !subscriptions.entity_store().same_store(&service.store()))
        {
            return Err(ReadError::Forbidden);
        }
        let mut selected = app.mutations.lock();
        if selected.is_some() {
            return Err(ReadError::InvalidQuery("mutation service already selected"));
        }
        *selected = Some(service);
        drop(selected);
        Ok(self)
    }
    /// Select one shared subscription owner without granting mutation capability.
    /// Existing and later mutation selection must retain its exact entity store.
    pub fn state_subscriptions(
        self,
        service: crate::StateSubscriptionService,
    ) -> Result<Self, ReadError> {
        let app = &self.parts.as_ref().expect("unconsumed builder").application;
        if !service.read_service().same_service(&app.reads)
            || app
                .mutations
                .lock()
                .as_ref()
                .is_some_and(|mutations| !mutations.store().same_store(&service.entity_store()))
        {
            return Err(ReadError::Forbidden);
        }
        let mut selected = app.subscriptions.lock();
        if selected.is_some() {
            return Err(ReadError::InvalidQuery("subscriptions already selected"));
        }
        *selected = Some(service);
        drop(selected);
        Ok(self)
    }
    /// Select authorized history writes after the exact history service. Its
    /// provider remains initialized/closed once by the existing history owner.
    pub fn history_mutations(
        self,
        service: crate::HistoryMutationService,
    ) -> Result<Self, ReadError> {
        let app = &self.parts.as_ref().expect("unconsumed builder").application;
        if !app
            .history
            .lock()
            .as_ref()
            .is_some_and(|history| history.same_selection(service.history_service()))
        {
            return Err(ReadError::Forbidden);
        }
        let mut selected = app.history_mutations.lock();
        if selected.is_some() {
            return Err(ReadError::InvalidQuery(
                "history mutations already selected",
            ));
        }
        *selected = Some(service);
        drop(selected);
        Ok(self)
    }
    /// Transfer history provider lifecycle ownership to the application. Its
    /// initialization precedes all listener resources regardless of call order.
    pub fn owned_history(self, service: HistoryService) -> Result<Self, ReadError> {
        self.select_history(service, true)
    }
    /// Borrow an externally managed history provider; no lifecycle hooks run.
    pub fn borrowed_history(self, service: HistoryService) -> Result<Self, ReadError> {
        self.select_history(service, false)
    }
    fn select_history(mut self, service: HistoryService, owned: bool) -> Result<Self, ReadError> {
        let parts = self.parts.as_mut().expect("unconsumed builder");
        if !service
            .read_service()
            .same_service(&parts.application.reads)
        {
            return Err(ReadError::Forbidden);
        }
        let mut selected = parts.application.history.lock();
        if selected.is_some() {
            return Err(ReadError::InvalidQuery("history service already selected"));
        }
        if owned {
            parts.history_resource = Some(Box::new(HistoryResource(service.provider())));
        }
        *selected = Some(service);
        drop(selected);
        Ok(self)
    }
    pub fn owned_resource(mut self, resource: impl ApplicationResource) -> Self {
        self.parts
            .as_mut()
            .expect("unconsumed builder")
            .resources
            .push(Box::new(resource));
        self
    }
    pub fn shutdown_policy(mut self, policy: ShutdownPolicy) -> Self {
        self.shutdown = policy;
        self
    }
    pub fn startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }
    /// Borrows the runtime. The caller must keep it running until termination.
    pub fn start(mut self, runtime: &Handle) -> Result<ApplicationOwner, ApplicationError> {
        self.shutdown.validate()?;
        if self.startup_timeout.is_zero() || self.startup_timeout > Duration::from_secs(3600) {
            return Err(ApplicationError::Configuration(
                "startup timeout must be positive and at most one hour",
            ));
        }
        let mut parts = self.parts.take().expect("unconsumed builder");
        parts.application.reads.start_system_clock();
        if let Some(subscriptions) = parts.application.subscription_service() {
            parts
                .resources
                .insert(0, Box::new(subscriptions.resource()));
        }
        if let Some(history) = parts.history_resource.take() {
            parts.resources.insert(0, history);
        }
        let owner = ApplicationOwner {
            application: parts.application.clone(),
        };
        *parts.application.lifecycle.runtime.lock() = Some(runtime.clone());
        let deadline = Instant::now() + self.startup_timeout;
        runtime.spawn(coordinate(parts, self.shutdown, deadline));
        Ok(owner)
    }
}
impl Drop for ApplicationBuilder {
    fn drop(&mut self) {
        if let Some(parts) = &self.parts {
            let error = ApplicationError::Abandoned;
            parts.application.lifecycle.fail(error.clone());
            parts.application.lifecycle.finish(Err(error), vec![]);
        }
    }
}

/// Sole shutdown owner. Dropping it requests shutdown without blocking or
/// claiming completion. Keep it and await `close`/`terminated` for evidence.
/// Handles and service clones do not own shutdown.
pub struct ApplicationOwner {
    application: ApplicationHandle,
}
impl ApplicationOwner {
    pub fn handle(&self) -> ApplicationHandle {
        self.application.clone()
    }
    pub fn state(&self) -> ApplicationState {
        self.application.state()
    }
    pub async fn ready(&self) -> Result<ReadyInfo, ApplicationError> {
        self.application.ready().await
    }
    pub async fn close(&self) -> Result<CloseReport, ApplicationError> {
        self.application.lifecycle.seal();
        self.application.lifecycle.close_result().await
    }
    pub async fn terminated(&self) -> TerminationReport {
        self.application.terminated().await
    }
}
impl Drop for ApplicationOwner {
    fn drop(&mut self) {
        self.application.lifecycle.seal();
    }
}

#[derive(Clone)]
pub struct ApplicationHandle {
    pub(crate) lifecycle: Arc<Lifecycle>,
    reads: ReadService,
    mutations: Arc<Mutex<Option<MutationService>>>,
    history: Arc<Mutex<Option<HistoryService>>>,
    history_mutations: Arc<Mutex<Option<crate::HistoryMutationService>>>,
    subscriptions: Arc<Mutex<Option<crate::StateSubscriptionService>>>,
}
impl ApplicationHandle {
    pub fn subscription_service(&self) -> Option<crate::StateSubscriptionService> {
        self.subscriptions.lock().clone()
    }
    pub fn history_mutation_service(&self) -> Option<crate::HistoryMutationService> {
        self.history_mutations.lock().clone()
    }
    pub fn history_service(&self) -> Option<HistoryService> {
        self.history.lock().clone()
    }
    pub fn mutation_service(&self) -> Option<MutationService> {
        self.mutations.lock().clone()
    }
    pub fn read_service(&self) -> ReadService {
        self.reads.clone()
    }
    pub fn state(&self) -> ApplicationState {
        self.lifecycle.control.lock().state
    }
    pub fn outstanding_tasks(&self) -> usize {
        self.lifecycle.tasks.len()
    }
    pub fn same_application(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lifecycle, &other.lifecycle)
    }
    pub async fn ready(&self) -> Result<ReadyInfo, ApplicationError> {
        let mut changed = self.lifecycle.changed.subscribe();
        loop {
            {
                let control = self.lifecycle.control.lock();
                if let Some(error) = &control.failure {
                    return Err(error.clone());
                }
                match control.state {
                    ApplicationState::Running => {
                        return Ok(control.ready.clone().expect("running is ready"));
                    }
                    ApplicationState::Starting => {}
                    _ => return Err(ApplicationError::Closed),
                }
            }
            let _ = changed.changed().await;
        }
    }
    /// Registers before collecting an adapter request body or accepting an
    /// upgrade. Checking Running and registration is atomic with close sealing.
    pub fn admit(&self) -> Result<WorkGuard, ApplicationError> {
        self.lifecycle.admit()
    }
    /// Fires when admission seals, allowing an owned listener to stop accepting.
    pub fn closing(&self) -> CancellationToken {
        self.lifecycle.sealed.clone()
    }
    /// Fires after the drain allowance, asking admitted work to stop.
    pub fn cancellation(&self) -> CancellationToken {
        self.lifecycle.stop.clone()
    }
    pub async fn terminated(&self) -> TerminationReport {
        let mut changed = self.lifecycle.changed.subscribe();
        loop {
            if let Some(report) = self.lifecycle.control.lock().termination.clone() {
                return report;
            }
            let _ = changed.changed().await;
        }
    }
}

/// A registered operation. Child registrations represent continuation of the
/// same admitted work and remain valid during drain. Keep the parent until the
/// child is registered so closing cannot observe a false empty task set.
pub struct WorkGuard {
    pub(crate) lifecycle: Arc<Lifecycle>,
    _token: TaskTrackerToken,
}
impl WorkGuard {
    pub(crate) fn closing(&self) -> CancellationToken {
        self.lifecycle.sealed.clone()
    }
    pub fn child(&self) -> Self {
        Self {
            lifecycle: self.lifecycle.clone(),
            _token: self.lifecycle.tasks.token(),
        }
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.lifecycle.stop.clone()
    }
}

#[derive(Clone)]
pub struct ResourceContext {
    application: ApplicationHandle,
    startup_deadline: Instant,
}
impl ResourceContext {
    pub fn application(&self) -> ApplicationHandle {
        self.application.clone()
    }
    pub fn startup_deadline(&self) -> Instant {
        self.startup_deadline
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.application.closing()
    }
    /// Starts a background resource task before readiness. The application
    /// observes its future's actual destruction, including panic or abort.
    /// A failed task seals the application and initiates cleanup.
    pub fn spawn(
        &self,
        name: impl Into<String>,
        future: impl Future<Output = Result<(), ApplicationError>> + Send + 'static,
    ) -> Result<AbortHandle, ApplicationError> {
        let life = &self.application.lifecycle;
        let control = life.control.lock();
        if control.state != ApplicationState::Starting {
            return Err(ApplicationError::Closed);
        }
        let name = name.into();
        let report_to = life.clone();
        let runtime = life.runtime();
        let task = life.tasks.spawn_on(
            async move {
                match AssertUnwindSafe(future).catch_unwind().await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => report_to.fail(error),
                    Err(_) => report_to.fail(ApplicationError::Task(name)),
                }
            },
            &runtime,
        );
        Ok(task.abort_handle())
    }
}

pub(crate) struct Lifecycle {
    control: Mutex<Control>,
    changed: watch::Sender<u64>,
    pub(crate) tasks: TaskTracker,
    pub(crate) stop: CancellationToken,
    sealed: CancellationToken,
    runtime: Mutex<Option<Handle>>,
}
struct Control {
    state: ApplicationState,
    close_started: Option<Instant>,
    ready: Option<ReadyInfo>,
    failure: Option<ApplicationError>,
    close: Option<Result<CloseReport, ApplicationError>>,
    termination: Option<TerminationReport>,
}
impl Lifecycle {
    fn new() -> Arc<Self> {
        let (changed, _) = watch::channel(0);
        Arc::new(Self {
            control: Mutex::new(Control {
                state: ApplicationState::Starting,
                close_started: None,
                ready: None,
                failure: None,
                close: None,
                termination: None,
            }),
            changed,
            tasks: TaskTracker::new(),
            stop: CancellationToken::new(),
            sealed: CancellationToken::new(),
            runtime: Mutex::new(None),
        })
    }
    fn notify(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub(crate) fn runtime(&self) -> Handle {
        self.runtime
            .lock()
            .as_ref()
            .expect("started runtime")
            .clone()
    }
    pub(crate) fn admit(self: &Arc<Self>) -> Result<WorkGuard, ApplicationError> {
        let control = self.control.lock();
        match control.state {
            ApplicationState::Running => Ok(WorkGuard {
                lifecycle: self.clone(),
                _token: self.tasks.token(),
            }),
            ApplicationState::Starting => Err(ApplicationError::NotReady),
            _ => Err(ApplicationError::Closed),
        }
    }
    fn seal(&self) {
        let mut control = self.control.lock();
        if control.close_started.is_none() {
            control.close_started = Some(Instant::now());
            if control.state != ApplicationState::Failed {
                control.state = ApplicationState::Closing;
            }
            self.tasks.close();
            self.sealed.cancel();
            self.notify();
        }
    }
    fn fail(&self, error: ApplicationError) {
        {
            let mut control = self.control.lock();
            if control.termination.is_some() {
                return;
            }
            control.failure.get_or_insert(error);
            control.state = ApplicationState::Failed;
        }
        self.seal();
        self.notify();
    }
    fn running(&self, ready: ReadyInfo) {
        let mut control = self.control.lock();
        if control.state == ApplicationState::Starting {
            control.ready = Some(ready);
            control.state = ApplicationState::Running;
            self.notify();
        }
    }
    fn timed_out(&self, phase: ShutdownPhase) {
        let error = ApplicationError::ShutdownTimeout {
            phase,
            outstanding: self.tasks.len(),
        };
        self.fail(error.clone());
        let mut control = self.control.lock();
        control.close.get_or_insert(Err(error));
        self.notify();
    }
    fn finish(
        &self,
        result: Result<CloseReport, ApplicationError>,
        cleanup_errors: Vec<ApplicationError>,
    ) {
        let mut control = self.control.lock();
        let close = control.close.get_or_insert(result).clone();
        control.state = if close.is_ok() {
            ApplicationState::Closed
        } else {
            ApplicationState::Failed
        };
        control.termination = Some(TerminationReport {
            close,
            cleanup_errors,
        });
        self.notify();
    }
    async fn close_result(&self) -> Result<CloseReport, ApplicationError> {
        let mut changed = self.changed.subscribe();
        loop {
            if let Some(result) = self.control.lock().close.clone() {
                return result;
            }
            let _ = changed.changed().await;
        }
    }
}

async fn coordinate(mut parts: BuildParts, policy: ShutdownPolicy, startup_deadline: Instant) {
    let life = parts.application.lifecycle.clone();
    let context = ResourceContext {
        application: parts.application.clone(),
        startup_deadline,
    };
    let mut initialized = 0;
    let mut partial = None;
    let mut ready = ReadyInfo::default();
    for (index, resource) in parts.resources.iter_mut().enumerate() {
        if life.sealed.is_cancelled() {
            break;
        }
        let name = resource.name().to_string();
        let initialized_result = tokio::select! {
            biased;
            _ = life.sealed.cancelled() => None,
            _ = tokio::time::sleep_until(startup_deadline.into()) => Some(Err(ApplicationError::StartupTimeout)),
            result = AssertUnwindSafe(async { resource.initialize(context.clone()).await }).catch_unwind() => Some(result.unwrap_or_else(|_| Err(ApplicationError::resource(name, "initialization panicked")))),
        };
        match initialized_result {
            Some(Ok(info)) => {
                ready.listeners.extend(info.listeners);
                initialized += 1;
            }
            result => {
                partial = Some(index);
                if let Some(Err(error)) = result {
                    life.fail(error);
                }
                break;
            }
        }
    }
    if initialized == parts.resources.len() {
        life.running(ready);
    }
    life.sealed.cancelled().await;
    let start = life
        .control
        .lock()
        .close_started
        .expect("sealed close time");
    let drain_end = start + policy.drain_timeout;
    let stop_end = drain_end + policy.stop_timeout;
    let drain_expired = tokio::time::timeout_at(drain_end.into(), life.tasks.wait())
        .await
        .is_err();
    life.stop.cancel();
    if tokio::time::timeout_at(stop_end.into(), life.tasks.wait())
        .await
        .is_err()
    {
        life.timed_out(ShutdownPhase::Tasks);
        // A timeout is not a join receipt. Keep ownership until actual exit.
        life.tasks.wait().await;
    }
    let mut cleanup_errors = vec![];
    if let Some(index) = partial {
        let resource = &mut parts.resources[index];
        let name = resource.name().to_string();
        cleanup(
            &life,
            stop_end,
            &mut cleanup_errors,
            name,
            Box::pin(async { resource.rollback_start().await }),
        )
        .await;
    }
    for resource in parts.resources[..initialized].iter_mut().rev() {
        let name = resource.name().to_string();
        cleanup(
            &life,
            stop_end,
            &mut cleanup_errors,
            name,
            Box::pin(async { resource.close().await }),
        )
        .await;
    }
    // Release resources before publishing termination, even when handles remain.
    drop(parts.resources);
    let result = match life.control.lock().failure.clone() {
        Some(error) => Err(error),
        None => Ok(CloseReport { drain_expired }),
    };
    life.finish(result, cleanup_errors);
}

async fn cleanup(
    life: &Lifecycle,
    deadline: Instant,
    errors: &mut Vec<ApplicationError>,
    name: String,
    future: ResourceFuture<'_>,
) {
    let future = AssertUnwindSafe(future).catch_unwind();
    tokio::pin!(future);
    let result = match tokio::time::timeout_at(deadline.into(), &mut future).await {
        Ok(result) => result,
        Err(_) => {
            life.timed_out(ShutdownPhase::Resources);
            future.await
        }
    };
    if let Err(error) =
        result.unwrap_or_else(|_| Err(ApplicationError::resource(name, "cleanup panicked")))
    {
        errors.push(error.clone());
        life.fail(error);
    }
}

struct HistoryResource(Arc<dyn crate::HistoryProvider>);
impl ApplicationResource for HistoryResource {
    fn name(&self) -> &str {
        "history"
    }
    fn initialize(&mut self, _: ResourceContext) -> ResourceFuture<'_, ReadyInfo> {
        Box::pin(async move {
            self.0.initialize().await?;
            Ok(ReadyInfo::default())
        })
    }
    fn rollback_start(&mut self) -> ResourceFuture<'_> {
        self.0.rollback_initialize()
    }
    fn close(&mut self) -> ResourceFuture<'_> {
        self.0.close()
    }
}
