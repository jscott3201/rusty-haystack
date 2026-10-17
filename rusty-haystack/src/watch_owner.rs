//! Demand-driven watch actors on the existing HTTP owner runtime. No Python
//! objects, producer queue, implicit ACK/renew or replacement session live here.
use crate::http_owner::{Owner, Ticket};
use haystack_client::{
    ClientError, HaystackClient,
    transport::{http::HttpTransport, subscription_ws::SubscriptionWsTransport},
};
use haystack_core::codecs::subscription::{
    SubscriptionCreate, SubscriptionDelivery, SubscriptionId, SubscriptionOutcome as Outcome,
    SubscriptionRejection as Rejection, SubscriptionRequest as Request,
    SubscriptionResync as Resync,
};
use parking_lot::Mutex;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

type Http = HaystackClient<HttpTransport>;
type Ws = HaystackClient<SubscriptionWsTransport>;
pub(crate) const MAX_WATCH_RESERVATIONS: usize = 32;
const CLEANUP_ATTEMPTS: usize = 6;

#[derive(Default)]
struct Slot(AtomicBool);
impl Slot {
    fn reserve(self: &Arc<Self>) -> Result<Reservation, ClientError> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| invalid("watch operation overlaps an unfinished operation"))?;
        Ok(Reservation(self.clone()))
    }
}
struct Reservation(Arc<Slot>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.0.store(false, Ordering::Release);
    }
}
#[derive(Clone, Debug)]
pub(crate) enum Event {
    Ready,
    Wire(Outcome),
    End,
    NotCreated,
}
impl Event {
    /// Only explicit remote terminal proof or no dispatch releases a session
    /// key. Local completion, Idle and unknown authority/identity do not.
    pub(crate) fn confirms_release(&self) -> bool {
        matches!(
            self,
            Self::NotCreated
                | Self::Wire(
                    Outcome::Closed { .. }
                        | Outcome::Resync(
                            Resync::SessionExpired
                                | Resync::Revoked
                                | Resync::LeaseExpired
                                | Resync::Gap
                                | Resync::Incarnation
                                | Resync::Catalog
                                | Resync::Policy
                                | Resync::ScopeChanged
                                | Resync::Shutdown
                                | Resync::Overflow
                        )
                )
        )
    }
}
#[derive(Clone)]
pub(crate) enum Action {
    Ready,
    Next,
    Resume,
    Ack(Arc<SubscriptionDelivery>),
    Renew(u64),
}
struct State {
    phase: &'static str,
    create: Option<SubscriptionCreate>,
    dispatched: bool,
    identity: Option<SubscriptionId>,
    scope: u64,
    acknowledged: u64,
    pending: Option<Arc<SubscriptionDelivery>>,
    delivered: bool,
    uncertain_ack: Option<Request>,
    cleanup_deadline: Option<Instant>,
    report: Option<Event>,
    native_done: bool,
}
pub(crate) struct Inner {
    state: Mutex<State>,
    slot: Arc<Slot>,
    stop: CancellationToken,
    changed: watch::Sender<()>,
    ids: Vec<String>,
    key: String,
    lease_ms: u64,
    cleanup_budget: Duration,
    commands: mpsc::Sender<Command>,
}
#[derive(Clone)]
pub(crate) struct Handle {
    pub(crate) owner: Owner,
    pub(crate) inner: Arc<Inner>,
}
pub(crate) struct Registration {
    pub(crate) inner: Arc<Inner>,
    receiver: mpsc::Receiver<Command>,
}
struct Command {
    action: Action,
    deadline: Instant,
    ticket: Arc<Ticket>,
    guard: Arc<WatchTicket>,
    response: oneshot::Sender<Result<Event, ClientError>>,
}
struct WatchTicket {
    inner: Arc<Inner>,
    reservation: Option<Reservation>,
}
impl Drop for WatchTicket {
    fn drop(&mut self) {
        drop(self.reservation.take());
        self.inner.changed.send_replace(());
    }
}
pub(crate) struct Operation {
    response: oneshot::Receiver<Result<Event, ClientError>>,
    ticket: Arc<Ticket>,
    _guard: Arc<WatchTicket>,
    creation: bool,
    inner: Arc<Inner>,
    completed: bool,
}
impl Operation {
    pub(crate) async fn result(&mut self) -> Result<Event, ClientError> {
        let result = (&mut self.response)
            .await
            .unwrap_or(Err(ClientError::ConnectionClosed));
        self.completed = true;
        result
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.ticket.cancel.cancel();
        if self.creation && !self.completed {
            self.inner.begin_close();
        }
    }
}
fn invalid(message: &str) -> ClientError {
    ClientError::Connection(message.into())
}
impl Inner {
    pub(crate) fn begin_close(&self) {
        let mut state = self.state.lock();
        if state.cleanup_deadline.is_none() {
            state.cleanup_deadline = Some(Instant::now() + self.cleanup_budget);
            state.phase = "closing";
        }
        drop(state);
        self.stop.cancel();
        self.changed.send_replace(());
    }
    pub(crate) fn finished(&self) -> bool {
        self.state.lock().native_done
    }
    pub(crate) fn key(&self) -> &str {
        &self.key
    }
    pub(crate) fn release_confirmed(&self) -> bool {
        let state = self.state.lock();
        state.native_done
            && state.report.as_ref().is_some_and(Event::confirms_release)
            && !self.slot.0.load(Ordering::Acquire)
    }
    fn cleanup_request(&self, remaining: usize) -> Result<Request, Event> {
        if remaining == 0 {
            return Err(Event::Wire(Outcome::Unknown));
        }
        let state = self.state.lock();
        if let Some(watch) = &state.identity {
            Ok(Request::Unsubscribe {
                watch: watch.clone(),
            })
        } else if !state.dispatched {
            Err(Event::NotCreated)
        } else if remaining > 1 {
            // A possibly allocating Create is authorized only while another
            // request remains for immediate Unsubscribe of a recovered identity.
            Ok(Request::Create(
                state.create.clone().expect("dispatched intent"),
            ))
        } else {
            Err(Event::Wire(Outcome::Unknown))
        }
    }
    fn closing(&self) -> bool {
        self.stop.is_cancelled()
    }
    fn observe(&self, outcome: &Outcome) {
        let mut state = self.state.lock();
        // The transport has correlated this outcome with the exact request.
        // A legal Create Idle owns the same remote identity as Delivery.
        if let Some(watch) = outcome.watch() {
            state.identity = Some(watch.clone());
        }
        match outcome {
            Outcome::Delivery(delivery) => {
                state.scope = delivery.scope_generation;
                if state.pending.is_none() {
                    state.acknowledged = delivery.from;
                }
                state.pending = Some(delivery.clone());
                state.delivered = false;
                state.phase = "open";
            }
            Outcome::Acknowledged { through, .. } => {
                state.pending = None;
                state.acknowledged = *through;
                state.uncertain_ack = None;
                state.phase = "open";
            }
            Outcome::Idle {
                scope_generation,
                acknowledged,
                ..
            } => {
                state.scope = *scope_generation;
                state.acknowledged = *acknowledged;
                state.phase = "open";
            }
            Outcome::Unknown | Outcome::Rejected(Rejection::Pending) => state.phase = "uncertain",
            Outcome::Resync(_) | Outcome::Closed { .. } => {
                state.phase = "terminal";
                state.pending = None;
            }
            _ => {}
        }
        if state.cleanup_deadline.is_some() {
            state.phase = "closing";
        }
    }
    async fn cleanup_expired(&self) {
        self.stop.cancelled().await;
        let deadline = self
            .state
            .lock()
            .cleanup_deadline
            .expect("close owns deadline");
        tokio::time::sleep_until(deadline).await;
    }
    fn finish(&self, report: Event) {
        let mut state = self.state.lock();
        state.report = Some(report);
        state.native_done = true;
        state.phase = "closed";
        drop(state);
        self.changed.send_replace(());
    }
}
impl Handle {
    pub(crate) fn new(
        owner: Owner,
        ids: Vec<String>,
        key: String,
        lease_ms: u64,
        cleanup_budget: Duration,
    ) -> Result<(Self, Registration), ClientError> {
        Request::Create(SubscriptionCreate {
            authority: [0; 16],
            key: key.clone(),
            ids: ids.clone(),
            lease_ms,
        })
        .source_bytes()
        .map_err(|_| invalid("invalid watch key, IDs, lease or intent byte bound"))?;
        let (commands, receiver) = mpsc::channel(1);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                phase: "new",
                create: None,
                dispatched: false,
                identity: None,
                scope: 0,
                acknowledged: 0,
                pending: None,
                delivered: false,
                uncertain_ack: None,
                cleanup_deadline: None,
                report: None,
                native_done: false,
            }),
            slot: Arc::default(),
            stop: CancellationToken::new(),
            changed: watch::channel(()).0,
            ids,
            key,
            lease_ms,
            cleanup_budget,
            commands,
        });
        Ok((
            Self {
                owner,
                inner: inner.clone(),
            },
            Registration { inner, receiver },
        ))
    }
    pub(crate) fn phase(&self) -> &'static str {
        self.inner.state.lock().phase
    }
    pub(crate) fn pending(&self) -> Option<Arc<SubscriptionDelivery>> {
        self.inner.state.lock().pending.clone()
    }
    pub(crate) fn key(&self) -> &str {
        &self.inner.key
    }
    pub(crate) fn begin_close(&self) {
        self.inner.begin_close();
    }
    pub(crate) fn closed(&self) -> bool {
        self.inner.finished() && !self.inner.slot.0.load(Ordering::Acquire)
    }
    pub(crate) fn operation(
        &self,
        action: Action,
        deadline: Instant,
    ) -> Result<Operation, ClientError> {
        let reservation = self.inner.slot.reserve()?;
        let creation;
        {
            let state = self.inner.state.lock();
            if state.cleanup_deadline.is_some() {
                return Err(ClientError::ConnectionClosed);
            }
            creation = state.phase == "new";
            match &action {
                Action::Ready if state.phase == "uncertain" => {
                    return Err(invalid("watch outcome is uncertain; explicitly resume"));
                }
                Action::Renew(_) if state.phase == "terminal" => {
                    return Err(invalid("terminal watch requires a new observation"));
                }
                Action::Next if state.pending.is_some() && state.delivered => {
                    return Err(invalid(
                        "acknowledge the pending delivery before requesting another",
                    ));
                }
                Action::Next if state.phase == "uncertain" => {
                    return Err(invalid(
                        "watch outcome is uncertain; explicitly resume or reconcile the exact acknowledgement",
                    ));
                }
                Action::Ack(delivery) => {
                    let Some(pending) = &state.pending else {
                        return Err(invalid("watch has no pending delivery"));
                    };
                    if pending.watch != delivery.watch
                        || pending.scope_generation != delivery.scope_generation
                        || pending.token != delivery.token
                        || pending.through != delivery.through
                    {
                        return Err(invalid(
                            "acknowledgement does not match this watch's pending delivery",
                        ));
                    }
                }
                Action::Renew(ms) => {
                    Request::Renew {
                        watch: state
                            .identity
                            .clone()
                            .ok_or_else(|| invalid("watch is not ready"))?,
                        lease_ms: *ms,
                    }
                    .source_bytes()
                    .map_err(|_| invalid("invalid lease"))?;
                }
                _ => {}
            }
        }
        let ticket = self.owner.admit()?;
        let guard = Arc::new(WatchTicket {
            inner: self.inner.clone(),
            reservation: Some(reservation),
        });
        let (response, receiver) = oneshot::channel();
        self.inner
            .commands
            .try_send(Command {
                action,
                deadline,
                ticket: ticket.clone(),
                guard: guard.clone(),
                response,
            })
            .map_err(|_| ClientError::ConnectionClosed)?;
        Ok(Operation {
            response: receiver,
            ticket,
            _guard: guard,
            creation,
            inner: self.inner.clone(),
            completed: false,
        })
    }
    pub(crate) async fn wait_closed(&self, deadline: Instant) -> Result<Event, ClientError> {
        let mut changed = self.inner.changed.subscribe();
        loop {
            if self.closed() {
                return Ok(self
                    .inner
                    .state
                    .lock()
                    .report
                    .clone()
                    .expect("closed report"));
            }
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => return Err(ClientError::Timeout(Duration::ZERO)),
                _ = changed.changed() => {},
            }
        }
    }
}
async fn finite(
    inner: &Inner,
    deadline: Instant,
    work: impl std::future::Future<Output = Result<Outcome, ClientError>>,
) -> Outcome {
    if Instant::now() >= deadline {
        return Outcome::Unknown;
    }
    tokio::select! { biased;
        _ = inner.cleanup_expired() => Outcome::Unknown,
        _ = tokio::time::sleep_until(deadline) => Outcome::Unknown,
        result = work => result.unwrap_or(Outcome::Unknown),
    }
}
impl Registration {
    pub(crate) async fn run(mut self, client: Arc<Http>, ws_url: String) {
        let mut ws: Option<Ws> = None;
        loop {
            let command = tokio::select! { biased; _ = self.inner.stop.cancelled() => break, command = self.receiver.recv() => match command { Some(command) => command, None => { self.inner.begin_close(); break; } } };
            let result = if Instant::now() >= command.deadline {
                Err(ClientError::Timeout(Duration::ZERO))
            } else if command.ticket.cancel.is_cancelled() {
                Err(ClientError::ConnectionClosed)
            } else {
                self.execute(&client, &ws_url, &mut ws, &command).await
            };
            let _ = command.response.send(result);
            drop(command.ticket);
            drop(command.guard);
        }
        // Queued observations cannot keep close waiting on unstarted I/O.
        self.receiver.close();
        while let Ok(command) = self.receiver.try_recv() {
            let _ = command.response.send(Err(ClientError::ConnectionClosed));
        }
        let report = self.cleanup(&client).await;
        if let Some(socket) = ws.take() {
            let deadline = self.inner.state.lock().cleanup_deadline.unwrap();
            let _ = tokio::time::timeout_at(deadline, socket.close()).await;
            drop(socket);
        }
        self.inner.finish(report);
    }
    async fn create(
        &self,
        client: &Http,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Outcome {
        let existing = self.inner.state.lock().create.clone();
        let create = if let Some(create) = existing {
            create
        } else {
            let authority = tokio::select! { biased; _ = cancel.cancelled() => return Outcome::Unknown, _ = self.inner.stop.cancelled() => return Outcome::Unknown, outcome = finite(&self.inner, deadline, client.state_subscription(&Request::Describe)) => outcome };
            let Outcome::Authority { authority, .. } = authority else {
                return authority;
            };
            let create = SubscriptionCreate {
                authority,
                key: self.inner.key.clone(),
                ids: self.inner.ids.clone(),
                lease_ms: self.inner.lease_ms,
            };
            self.inner.state.lock().create = Some(create.clone());
            create
        };
        if cancel.is_cancelled() || self.inner.closing() || Instant::now() >= deadline {
            return Outcome::Unknown;
        }
        self.inner.state.lock().dispatched = true;
        // Once dispatched this finite original Create is supervised even when
        // its Python observer vanishes. Close may shorten it only at its single
        // cleanup deadline; exact intent then remains for reconciliation.
        finite(
            &self.inner,
            deadline,
            client.state_subscription(&Request::Create(create)),
        )
        .await
    }
    async fn execute(
        &self,
        client: &Http,
        ws_url: &str,
        ws: &mut Option<Ws>,
        command: &Command,
    ) -> Result<Event, ClientError> {
        let phase = self.inner.state.lock().phase;
        if phase == "terminal" {
            return Ok(Event::End);
        }
        if phase == "new"
            || (matches!(command.action, Action::Resume)
                && self.inner.state.lock().identity.is_none())
        {
            let outcome = self
                .create(client, command.deadline, &command.ticket.cancel)
                .await;
            self.inner.observe(&outcome);
            if !matches!(outcome, Outcome::Delivery(_) | Outcome::Idle { .. }) {
                return Ok(Event::Wire(outcome));
            }
        }
        if self.inner.closing() {
            return Err(ClientError::ConnectionClosed);
        }
        // Explicit recovery replaces the old socket once, under the same
        // retained HTTP session. No ordinary operation reconnects.
        if matches!(command.action, Action::Resume)
            && phase == "uncertain"
            && let Some(socket) = ws.take()
        {
            let _ = tokio::time::timeout_at(command.deadline, socket.close()).await;
        }
        let attach = ws.is_none() && (matches!(command.action, Action::Resume) || phase == "new");
        if attach {
            let attached = tokio::select! { biased;
                _ = self.inner.stop.cancelled() => Err(ClientError::ConnectionClosed),
                _ = command.ticket.cancel.cancelled() => Err(ClientError::ConnectionClosed),
                result = tokio::time::timeout_at(command.deadline, client.attach_subscription_ws(ws_url)) => result.unwrap_or(Err(ClientError::Timeout(Duration::ZERO))),
            };
            match attached {
                Ok(socket) => *ws = Some(socket),
                Err(_) => {
                    self.inner.begin_close();
                    return Ok(Event::Wire(Outcome::Unknown));
                }
            }
        }
        match &command.action {
            Action::Ready => return Ok(Event::Ready),
            Action::Next => {
                let mut state = self.inner.state.lock();
                if let Some(delivery) = state.pending.clone() {
                    state.delivered = true;
                    return Ok(Event::Wire(Outcome::Delivery(delivery)));
                }
            }
            _ => {}
        }
        let request = {
            let mut state = self.inner.state.lock();
            let identity = state
                .identity
                .clone()
                .ok_or_else(|| invalid("watch has no verified identity"))?;
            match &command.action {
                Action::Ready => unreachable!(),
                Action::Next => Request::Poll { watch: identity },
                Action::Resume => {
                    if let Some(ack) = &state.uncertain_ack {
                        ack.clone()
                    } else {
                        Request::Resume {
                            watch: identity,
                            scope_generation: state.scope,
                            acknowledged: state.acknowledged,
                        }
                    }
                }
                Action::Ack(delivery) => {
                    let request = Request::Acknowledge {
                        watch: identity,
                        scope_generation: delivery.scope_generation,
                        token: delivery.token,
                        through: delivery.through,
                    };
                    state.uncertain_ack = Some(request.clone());
                    request
                }
                Action::Renew(ms) => Request::Renew {
                    watch: identity,
                    lease_ms: *ms,
                },
            }
        };
        let Some(socket) = ws.as_ref() else {
            return Ok(Event::Wire(Outcome::Unknown));
        };
        loop {
            let outcome = tokio::select! { biased;
                _ = self.inner.stop.cancelled() => Outcome::Unknown,
                _ = command.ticket.cancel.cancelled() => Outcome::Unknown,
                result = finite(&self.inner, command.deadline, socket.state_subscription(&request)) => result,
            };
            self.inner.observe(&outcome);
            if matches!(command.action, Action::Next) && matches!(outcome, Outcome::Idle { .. }) {
                tokio::select! { biased;
                    _ = self.inner.stop.cancelled() => return Ok(Event::Wire(Outcome::Unknown)),
                    _ = command.ticket.cancel.cancelled() => return Ok(Event::Wire(Outcome::Unknown)),
                    _ = tokio::time::sleep_until(command.deadline) => return Err(ClientError::Timeout(Duration::ZERO)),
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {},
                }
                continue;
            }
            if matches!(outcome, Outcome::Delivery(_)) {
                self.inner.state.lock().delivered = true;
            }
            return Ok(Event::Wire(outcome));
        }
    }
    async fn cleanup(&self, client: &Http) -> Event {
        let deadline = self
            .inner
            .state
            .lock()
            .cleanup_deadline
            .expect("close deadline");
        for remaining in (1..=CLEANUP_ATTEMPTS).rev() {
            if Instant::now() >= deadline {
                return Event::Wire(Outcome::Unknown);
            }
            let request = match self.inner.cleanup_request(remaining) {
                Ok(request) => request,
                Err(report) => return report,
            };
            let outcome = finite(&self.inner, deadline, client.state_subscription(&request)).await;
            self.inner.observe(&outcome);
            match &outcome {
                Outcome::Delivery(_) | Outcome::Idle { .. } => {}
                Outcome::Closed { .. } | Outcome::Resync(_) => return Event::Wire(outcome),
                Outcome::Unknown | Outcome::Rejected(Rejection::Pending) => {
                    let _ = tokio::time::timeout_at(
                        deadline,
                        tokio::time::sleep(Duration::from_millis(20)),
                    )
                    .await;
                }
                _ => return Event::Wire(outcome),
            }
        }
        Event::Wire(Outcome::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_completion_does_not_release_the_python_conversion_slot() {
        let slot = Arc::new(Slot::default());
        let native = Arc::new(slot.reserve().unwrap());
        let conversion = native.clone();
        drop(native);
        assert!(
            slot.reserve().is_err(),
            "conversion still owns the single operation slot"
        );
        drop(conversion);
        assert!(slot.reserve().is_ok());
    }
    #[test]
    fn close_waits_for_inflight_python_conversion() {
        use pyo3::prelude::*;
        Python::initialize();
        let owner = Owner::new(crate::http_owner::Settings {
            url: "http://localhost:1".into(),
            username: "fixture".into(),
            password: zeroize::Zeroizing::new("fixture".into()),
            config: haystack_client::ClientConfig::default(),
        });
        let (handle, _registration) = Handle::new(
            owner,
            vec!["a".into()],
            "conversion".into(),
            1000,
            Duration::from_secs(1),
        )
        .unwrap();
        let native = Arc::new(WatchTicket {
            reservation: Some(handle.inner.slot.reserve().unwrap()),
            inner: handle.inner.clone(),
        });
        let conversion = native.clone();
        drop(native);
        handle.begin_close();
        handle.inner.finish(Event::NotCreated);
        let converted = Arc::new(AtomicBool::new(false));
        let (waiting, wait_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::sync_channel(1);
        let done = converted.clone();
        let worker = std::thread::spawn(move || {
            crate::async_bridge::with_python(|py| {
                waiting.send(()).unwrap();
                py.detach(move || release_rx.recv_timeout(Duration::from_secs(2)).unwrap());
                let value = pyo3::types::PyString::new(py, "owned watch result");
                assert_eq!(value.to_str().unwrap(), "owned watch result");
                done.store(true, Ordering::Release);
                Ok(())
            })
            .unwrap();
            drop(conversion);
        });
        wait_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // The conversion callback retains its ticket while explicitly held.
        crate::async_bridge::runtime().block_on(async {
            assert!(matches!(
                handle
                    .wait_closed(Instant::now() + Duration::from_millis(20))
                    .await,
                Err(ClientError::Timeout(_))
            ));
        });
        assert!(!converted.load(Ordering::Acquire));
        assert!(!handle.closed());
        assert!(handle.inner.slot.reserve().is_err());
        release.send(()).unwrap();
        worker.join().unwrap();
        crate::async_bridge::runtime().block_on(async {
            handle
                .wait_closed(Instant::now() + Duration::from_secs(2))
                .await
                .unwrap();
        });
        assert!(converted.load(Ordering::Acquire) && handle.closed());
    }
    #[test]
    fn correlated_create_idle_retains_cleanup_identity_and_position() {
        let owner = Owner::new(crate::http_owner::Settings {
            url: "http://localhost:1".into(),
            username: "fixture".into(),
            password: zeroize::Zeroizing::new("fixture".into()),
            config: haystack_client::ClientConfig::default(),
        });
        let (handle, _registration) = Handle::new(
            owner,
            vec!["a".into()],
            "idle".into(),
            1000,
            Duration::from_secs(1),
        )
        .unwrap();
        let watch = SubscriptionId {
            authority: [1; 16],
            watch: [2; 16],
            key: "idle".into(),
        };
        handle.inner.observe(&Outcome::Idle {
            watch: watch.clone(),
            scope_generation: 7,
            acknowledged: 19,
        });
        let state = handle.inner.state.lock();
        assert_eq!(
            state.identity,
            Some(watch.clone()),
            "legal Create Idle still owns a remote watch"
        );
        assert_eq!((state.scope, state.acknowledged), (7, 19));
        assert!(state.pending.is_none());
        drop(state);
        assert!(
            matches!(handle.inner.cleanup_request(1), Ok(Request::Unsubscribe { watch: found }) if found == watch)
        );
        let idle = Event::Wire(Outcome::Idle {
            watch,
            scope_generation: 7,
            acknowledged: 19,
        });
        assert!(!idle.confirms_release());
        handle.inner.finish(idle);
        assert!(!handle.inner.release_confirmed());
    }
}
