//! Opt-in local command simulation. Acceptance never claims equipment effect.
use crate::{
    ApplicationError, ApplicationResource, Principal, ReadError, ReadService, ReadyInfo,
    ResourceContext, ResourceFuture,
};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAction {
    Inspect,
    Set,
    Relinquish,
}
/// Immutable decisions. The scope key changes whenever an effective decision changes.
pub trait CommandPolicySnapshot: Send + Sync + 'static {
    fn scope_key(&self) -> &str;
    fn action(&self, action: CommandAction) -> bool;
    fn point(&self, id: &str, action: CommandAction) -> bool;
}
pub trait CommandPolicy: Send + Sync + 'static {
    fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn CommandPolicySnapshot>, ReadError>;
}
/// Explicit simulator policy, independent of transport and read/disclosure authority.
pub struct AllowAllSimulatedCommands;
impl CommandPolicy for AllowAllSimulatedCommands {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn CommandPolicySnapshot>, ReadError> {
        Ok(Arc::new(Self))
    }
}
impl CommandPolicySnapshot for AllowAllSimulatedCommands {
    fn scope_key(&self) -> &str {
        "allow-local-simulation-v1"
    }
    fn action(&self, _: CommandAction) -> bool {
        true
    }
    fn point(&self, _: &str, _: CommandAction) -> bool {
        true
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulatedCommandMode {
    InspectOnly,
    Writable,
}
#[derive(Debug, Clone)]
pub struct CommandLimits {
    pub max_points: usize,
    pub max_pending: usize,
    pub max_retained_bytes: usize,
    pub max_value_bytes: usize,
    pub max_who_bytes: usize,
    pub max_duration: Duration,
    pub maintenance_interval: Duration,
}
impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            max_points: 128,
            max_pending: 32,
            max_retained_bytes: 16 * 1024 * 1024,
            max_value_bytes: 4096,
            max_who_bytes: 256,
            max_duration: Duration::from_secs(3600),
            maintenance_interval: Duration::from_millis(100),
        }
    }
}
impl CommandLimits {
    fn validate(&self) -> Result<(), ReadError> {
        if self.max_points == 0
            || self.max_points > 4096
            || self.max_pending == 0
            || self.max_pending > 1024
            || self.max_retained_bytes == 0
            || self.max_retained_bytes > 512 * 1024 * 1024
            || self.max_value_bytes == 0
            || self.max_value_bytes > 65_536
            || self.max_who_bytes == 0
            || self.max_who_bytes > 1024
            || self.max_duration.is_zero()
            || self.max_duration > Duration::from_secs(86400)
            || self.maintenance_interval.is_zero()
            || self.maintenance_interval > Duration::from_secs(1)
        {
            return Err(ReadError::InvalidLimits);
        }
        Ok(())
    }
}
/// One monotonic clock for simulator deadlines and expiry work. Manual clocks
/// advance only through this trusted local configuration handle, never HTTP.
#[derive(Clone)]
pub struct SimulatorClock(Arc<Clock>);
struct Clock {
    source: ClockSource,
    changed: Notify,
}
enum ClockSource {
    Realtime(Instant),
    Manual(AtomicU64),
}
impl Default for SimulatorClock {
    fn default() -> Self {
        Self::realtime()
    }
}
impl SimulatorClock {
    pub fn realtime() -> Self {
        Self(Arc::new(Clock {
            source: ClockSource::Realtime(Instant::now()),
            changed: Notify::new(),
        }))
    }
    pub fn manual() -> Self {
        Self(Arc::new(Clock {
            source: ClockSource::Manual(AtomicU64::new(0)),
            changed: Notify::new(),
        }))
    }
    pub fn advance(&self, duration: Duration) -> Result<(), ReadError> {
        let ClockSource::Manual(ticks) = &self.0.source else {
            return Err(ReadError::InvalidQuery("clock is not manual"));
        };
        let nanos = u64::try_from(duration.as_nanos())
            .map_err(|_| ReadError::InvalidQuery("clock overflow"))?;
        ticks
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(nanos)
            })
            .map_err(|_| ReadError::InvalidQuery("clock overflow"))?;
        self.0.changed.notify_one();
        Ok(())
    }
    fn now(&self) -> Duration {
        match &self.0.source {
            ClockSource::Realtime(start) => start.elapsed(),
            ClockSource::Manual(ticks) => Duration::from_nanos(ticks.load(Ordering::Acquire)),
        }
    }
}
#[derive(Clone)]
pub struct SimulatedCommandService {
    inner: Arc<CommandInner>,
}
struct CommandInner {
    reads: ReadService,
    policy: Arc<dyn CommandPolicy>,
    mode: SimulatedCommandMode,
    limits: CommandLimits,
    clock: SimulatorClock,
    started: AtomicBool,
    stopped: AtomicBool,
    state: Mutex<state::Store>,
    pending: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    before_dispatch: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_dispatch: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    publication_retry: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    store_retry: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl SimulatedCommandService {
    pub fn new(
        reads: ReadService,
        policy: Arc<dyn CommandPolicy>,
        mode: SimulatedCommandMode,
        limits: CommandLimits,
    ) -> Result<Self, ReadError> {
        Self::with_clock(reads, policy, mode, limits, SimulatorClock::realtime())
    }
    pub fn with_clock(
        reads: ReadService,
        policy: Arc<dyn CommandPolicy>,
        mode: SimulatedCommandMode,
        limits: CommandLimits,
        clock: SimulatorClock,
    ) -> Result<Self, ReadError> {
        limits.validate()?;
        Ok(Self {
            inner: Arc::new(CommandInner {
                reads,
                policy,
                mode,
                limits,
                clock,
                started: AtomicBool::new(false),
                stopped: AtomicBool::new(false),
                state: Mutex::new(state::Store::default()),
                pending: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(test)]
                before_dispatch: Mutex::new(None),
                #[cfg(test)]
                after_dispatch: Mutex::new(None),
                #[cfg(test)]
                publication_retry: Mutex::new(None),
                #[cfg(test)]
                store_retry: Mutex::new(None),
            }),
        })
    }
    pub fn read_service(&self) -> &ReadService {
        &self.inner.reads
    }
    pub(crate) fn resource(&self) -> CommandResource {
        CommandResource(self.clone())
    }
    fn maintain(&self) {
        // The owned async task never blocks a runtime thread behind a command
        // worker. A busy store is revisited on the next bounded tick; requests
        // also apply due expiry under their own admitted publication guard.
        if let Some(mut state) = self.inner.state.try_lock() {
            state.expire(self.inner.clock.now());
        }
    }

    fn shutdown(&self) {
        self.inner.stopped.store(true, Ordering::Release);
    }
}
pub(crate) struct CommandResource(SimulatedCommandService);
impl ApplicationResource for CommandResource {
    fn name(&self) -> &str {
        "local command simulator"
    }
    fn initialize(&mut self, context: ResourceContext) -> ResourceFuture<'_, ReadyInfo> {
        let service = self.0.clone();
        Box::pin(async move {
            if service.inner.started.swap(true, Ordering::AcqRel)
                || service.inner.stopped.load(Ordering::Acquire)
            {
                return Err(ApplicationError::Configuration(
                    "simulator owner already started or stopped",
                ));
            }
            let closing = context.cancellation();
            context.spawn("simulator expiry", async move {
                let mut tick = tokio::time::interval(service.inner.limits.maintenance_interval);
                loop {
                    tokio::select! { biased;
                        _ = closing.cancelled() => { service.shutdown(); return Ok(()); },
                        _ = service.inner.clock.0.changed.notified() => service.maintain(),
                        _ = tick.tick() => service.maintain(),
                    }
                }
            })?;
            Ok(ReadyInfo::default())
        })
    }
    fn rollback_start(&mut self) -> ResourceFuture<'_> {
        self.0.shutdown();
        Box::pin(async { Ok(()) })
    }
    fn close(&mut self) -> ResourceFuture<'_> {
        self.0.shutdown();
        Box::pin(async { Ok(()) })
    }
}

/// Request-local dispatch evidence, never a receipt or a replay identity.
/// Abandonment and dispatch compete through one atomic transition.
#[derive(Clone, Default)]
pub struct CommandAttempt(Arc<std::sync::atomic::AtomicU8>);
impl CommandAttempt {
    pub fn classify(&self, error: crate::ApiError) -> crate::ApiError {
        match self
            .0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(2) => error,
            Err(_) => crate::ApiError::CommandUnknown,
        }
    }
    fn dispatch(&self) -> Result<(), crate::ApiError> {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| crate::ApiError::Unavailable)
    }
}

#[derive(Debug, Clone)]
pub struct CommandRequest {
    id: String,
    level: Option<u8>,
    value: Option<haystack_core::kinds::Kind>,
    who: Option<String>,
    duration: Option<Duration>,
}
impl CommandRequest {
    pub fn inspect(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            level: None,
            value: None,
            who: None,
            duration: None,
        }
    }
    pub fn set(id: impl Into<String>, level: u8, value: haystack_core::kinds::Kind) -> Self {
        Self {
            id: id.into(),
            level: Some(level),
            value: Some(value),
            who: None,
            duration: None,
        }
    }
    pub fn relinquish(id: impl Into<String>, level: u8) -> Self {
        Self {
            id: id.into(),
            level: Some(level),
            value: None,
            who: None,
            duration: None,
        }
    }
    pub fn with_who(mut self, who: impl Into<String>) -> Self {
        self.who = Some(who.into());
        self
    }
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }
    fn action(&self) -> CommandAction {
        if self.level.is_none() {
            CommandAction::Inspect
        } else if self
            .value
            .as_ref()
            .is_none_or(|v| matches!(v, haystack_core::kinds::Kind::Null))
        {
            CommandAction::Relinquish
        } else {
            CommandAction::Set
        }
    }
    pub(crate) fn decode(args: &haystack_core::data::HDict) -> Result<Self, crate::ApiError> {
        use crate::ApiError;
        use haystack_core::kinds::Kind;
        let Some(Kind::Ref(id)) = args.get("id") else {
            return Err(ApiError::InvalidArgs);
        };
        let level = match args.get("level") {
            None | Some(Kind::Null) => None,
            Some(Kind::Number(n))
                if n.unit.is_none()
                    && n.val.is_finite()
                    && n.val.fract() == 0.0
                    && (1.0..=17.0).contains(&n.val) =>
            {
                Some(n.val as u8)
            }
            _ => return Err(ApiError::InvalidArgs),
        };
        let who = match args.get("who") {
            None | Some(Kind::Null) => None,
            Some(Kind::Str(s)) => Some(s.clone()),
            _ => return Err(ApiError::InvalidArgs),
        };
        let duration = match args.get("duration") {
            None | Some(Kind::Null) => None,
            Some(Kind::Number(n)) if n.val.is_finite() && n.val > 0.0 => {
                let unit = n
                    .unit
                    .as_deref()
                    .and_then(haystack_core::kinds::unit_for)
                    .ok_or(ApiError::InvalidArgs)?;
                let seconds = match unit.name.as_str() {
                    "nanosecond" => 1e-9,
                    "microsecond" => 1e-6,
                    "millisecond" => 1e-3,
                    "second" => 1.0,
                    "minute" => 60.0,
                    "hour" => 3600.0,
                    "day" => 86400.0,
                    "week" => 604800.0,
                    "julian_month" => 2592000.0,
                    "year" => 31536000.0,
                    _ => return Err(ApiError::InvalidArgs),
                };
                Some(
                    Duration::try_from_secs_f64(n.val * seconds)
                        .map_err(|_| ApiError::InvalidArgs)?,
                )
            }
            _ => return Err(ApiError::InvalidArgs),
        };
        Ok(Self {
            id: id.val.clone(),
            level,
            value: args
                .get("val")
                .filter(|v| !matches!(v, Kind::Null))
                .cloned(),
            who,
            duration,
        })
    }
}
/// Accepted is only this process's simulator acceptance; it is never equipment
/// confirmation. Unknown must not be retried automatically.
#[derive(Debug)]
pub enum CommandOutcome {
    Inspection(haystack_core::data::HGrid),
    Accepted,
    Rejected(crate::ApiError),
    Unknown,
}
impl SimulatedCommandService {
    pub async fn submit(
        &self,
        context: crate::ReadContext,
        session: crate::SubscriptionSession,
        request: CommandRequest,
    ) -> CommandOutcome {
        if !session.matches(&context.principal) {
            return CommandOutcome::Rejected(crate::ApiError::Permission);
        }
        let result = self.inner.reads.begin(context).await;
        let mut admission = match result {
            Ok(a) => a,
            Err(e) => return CommandOutcome::Rejected(e.into()),
        };
        if let Err(e) = admission.bind_wire_session(session.principal().clone(), session.clone()) {
            return CommandOutcome::Rejected(e.into());
        }
        let attempt = CommandAttempt::default();
        admission.budget_mut().command_attempt = Some(attempt.clone());
        let service = self.clone();
        let inspect = request.action() == CommandAction::Inspect;
        let result = admission
            .run_task(move |principal, budget| {
                Ok(
                    service.execute(&principal, &session, request, None, budget, |grid, _| {
                        Ok(grid)
                    }),
                )
            })
            .await
            .map_err(crate::ApiError::from)
            .and_then(|r| r);
        match result {
            Ok(grid) if inspect => CommandOutcome::Inspection(grid),
            Ok(_) => CommandOutcome::Accepted,
            Err(e) => match attempt.classify(e) {
                crate::ApiError::CommandUnknown => CommandOutcome::Unknown,
                e => CommandOutcome::Rejected(e),
            },
        }
    }
}

mod state;
#[cfg(test)]
mod tests;
