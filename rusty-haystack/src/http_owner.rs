//! Per-client native I/O ownership. No Python objects or callbacks live here.
//!
//! Dropping a request does not join Hyper's connection tasks or an already
//! running DNS/SCRAM blocking job. The owner thread destroys its runtime before
//! it returns; the independent reaper then joins that thread and drains tickets.
use crate::watch_owner;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use haystack_client::{ClientConfig, ClientError, HaystackClient};
use haystack_core::data::HGrid;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub(crate) const MAX_IN_FLIGHT: usize = 16;
pub(crate) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    New,
    Connecting,
    Open,
    Closing,
    Closed,
}
impl Phase {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Connecting => "connecting",
            Self::Open => "open",
            Self::Closing => "closing",
            Self::Closed => "closed",
        }
    }
}

pub(crate) struct Settings {
    pub(crate) url: String,
    pub(crate) username: String,
    pub(crate) password: Zeroizing<String>,
    pub(crate) config: ClientConfig,
}

struct Control {
    phase: Phase,
    settings: Option<Settings>,
    commands: Option<mpsc::Sender<Command>>,
    // Retain bounded logical key ownership even after locally joined Unknown
    // cleanup or after Python drops its handle. Inner has no back-reference to
    // Owner; only an admitted operation's temporary ticket holds the parent.
    watches: Vec<Arc<watch_owner::Inner>>,
    thread: Option<JoinHandle<()>>,
    starting: bool,
    active: usize,
    cleanup_failed: bool,
}
struct Inner {
    control: Mutex<Control>,
    stop: CancellationToken,
    changed: watch::Sender<()>,
}
#[derive(Clone)]
pub(crate) struct Owner(Arc<Inner>);

/// Shared by the native task and Python-result conversion. Admission is released
/// only when BOTH legs have finished, including cancellation/late conversion.
pub(crate) struct Ticket {
    owner: Owner,
    pub(crate) cancel: CancellationToken,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.owner.0.control.lock().active -= 1;
        self.owner.0.changed.send_replace(());
    }
}

pub(crate) struct Operation<T = Option<HGrid>> {
    response: oneshot::Receiver<Result<T, ClientError>>,
    ticket: Arc<Ticket>,
    connecting: bool,
    completed: bool,
}
impl<T> Operation<T> {
    pub(crate) async fn result(&mut self) -> Result<T, ClientError> {
        let result = (&mut self.response)
            .await
            .unwrap_or(Err(ClientError::ConnectionClosed));
        self.completed = true;
        result
    }
    pub(crate) fn cancellation(&self) -> Cancellation {
        Cancellation {
            owner: self.ticket.owner.clone(),
            token: self.ticket.cancel.clone(),
            connecting: self.connecting,
        }
    }
}
impl<T> Drop for Operation<T> {
    fn drop(&mut self) {
        self.ticket.cancel.cancel();
        if self.connecting && !self.completed {
            self.ticket.owner.begin_close();
        }
    }
}

#[derive(Clone)]
pub(crate) struct Cancellation {
    owner: Owner,
    token: CancellationToken,
    connecting: bool,
}
impl Cancellation {
    pub(crate) fn read_control(&self, deadline: Instant) -> haystack_client::h5::ReadControl {
        haystack_client::h5::ReadControl::new(
            deadline,
            self.token.clone(),
            self.owner.0.stop.clone(),
        )
    }
    pub(crate) fn cancel(&self) {
        self.token.cancel();
        if self.connecting {
            self.owner.begin_close();
        }
    }
}
enum Command {
    Read(Read),
    H5(H5Read),
    Watch(watch_owner::Registration),
}
struct H5Read {
    request: haystack_client::h5::ReadRequest,
    deadline: Instant,
    ticket: Arc<Ticket>,
    response: oneshot::Sender<Result<haystack_core::kinds::Kind, ClientError>>,
}
struct Read {
    filter: String,
    limit: usize,
    deadline: Instant,
    ticket: Arc<Ticket>,
    response: oneshot::Sender<Result<Option<HGrid>, ClientError>>,
}

impl Owner {
    pub(crate) fn read_h5(
        &self,
        request: haystack_client::h5::ReadRequest,
        deadline: Instant,
    ) -> Result<Operation<haystack_core::kinds::Kind>, ClientError> {
        let ticket = self.admit()?;
        let (response, receiver) = oneshot::channel();
        let commands = self
            .0
            .control
            .lock()
            .commands
            .clone()
            .ok_or(ClientError::ConnectionClosed)?;
        commands
            .try_send(Command::H5(H5Read {
                request,
                deadline,
                ticket: ticket.clone(),
                response,
            }))
            .map_err(|_| ClientError::ConnectionClosed)?;
        Ok(Operation {
            response: receiver,
            ticket,
            connecting: false,
            completed: false,
        })
    }
    pub(crate) fn new(settings: Settings) -> Self {
        Self(Arc::new(Inner {
            control: Mutex::new(Control {
                phase: Phase::New,
                settings: Some(settings),
                commands: None,
                watches: Vec::new(),
                thread: None,
                starting: false,
                active: 0,
                cleanup_failed: false,
            }),
            stop: CancellationToken::new(),
            changed: watch::channel(()).0,
        }))
    }
    pub(crate) fn phase(&self) -> Phase {
        self.0.control.lock().phase
    }

    pub(crate) fn connect(&self, deadline: Instant) -> Result<Operation, ClientError> {
        let (tx, rx) = mpsc::channel(MAX_IN_FLIGHT);
        let (response, receiver) = oneshot::channel();
        let settings = {
            let mut control = self.0.control.lock();
            if control.phase != Phase::New {
                return Err(ClientError::Connection(
                    "connect requires a new client".into(),
                ));
            }
            control.phase = Phase::Connecting;
            control.starting = true;
            control.active += 1;
            control.commands = Some(tx);
            control.settings.take().expect("new owner has settings")
        };
        let ticket = Arc::new(Ticket {
            owner: self.clone(),
            cancel: CancellationToken::new(),
        });
        let native_ticket = ticket.clone();
        let owner = self.clone();
        // Never spawn/drop tickets under the control lock: their destructor also
        // acquires it. The reaper waits until handle publication is complete.
        let spawn = std::thread::Builder::new()
            .name("haystack-http-owner".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .max_blocking_threads(2)
                        .build();
                    match runtime {
                        Ok(runtime) => {
                            runtime.block_on(owner.run(
                                settings,
                                deadline,
                                native_ticket,
                                response,
                                rx,
                            ));
                            owner.begin_close();
                            // Real runtime destruction, on its own non-async thread,
                            // waits for started blocking work as well as async tasks.
                            drop(runtime);
                        }
                        Err(_) => {
                            let _ = response.send(Err(ClientError::Connection(
                                "native runtime initialization failed".into(),
                            )));
                        }
                    }
                }));
                if outcome.is_err() {
                    owner.0.control.lock().cleanup_failed = true;
                }
                owner.begin_close();
            });
        {
            let mut control = self.0.control.lock();
            control.starting = false;
            if let Ok(thread) = spawn {
                control.thread = Some(thread);
            } else {
                control.cleanup_failed = true;
            }
        }
        self.0.changed.send_replace(());
        if self.0.control.lock().cleanup_failed {
            self.begin_close();
        }
        Ok(Operation {
            response: receiver,
            ticket,
            connecting: true,
            completed: false,
        })
    }

    pub(crate) fn admit(&self) -> Result<Arc<Ticket>, ClientError> {
        {
            let mut control = self.0.control.lock();
            if control.phase != Phase::Open {
                return Err(ClientError::ConnectionClosed);
            }
            if control.active >= MAX_IN_FLIGHT {
                return Err(ClientError::TooManyRequests);
            }
            control.active += 1;
        }
        Ok(Arc::new(Ticket {
            owner: self.clone(),
            cancel: CancellationToken::new(),
        }))
    }

    pub(crate) fn watch(
        &self,
        ids: Vec<String>,
        key: String,
        lease_ms: u64,
        cleanup: Duration,
    ) -> Result<watch_owner::Handle, ClientError> {
        let (handle, registration) =
            watch_owner::Handle::new(self.clone(), ids, key, lease_ms, cleanup)?;
        let mut control = self.0.control.lock();
        if control.phase != Phase::Open {
            return Err(ClientError::ConnectionClosed);
        }
        control.watches.retain(|watch| !watch.release_confirmed());
        if control
            .watches
            .iter()
            .any(|watch| watch.key() == handle.key())
        {
            return Err(ClientError::Connection(
                "watch creation key is already reserved by this session".into(),
            ));
        }
        if control.watches.len() >= watch_owner::MAX_WATCH_RESERVATIONS {
            return Err(ClientError::TooManyRequests);
        }
        // Reserve before registration while the same parent lock serializes
        // every constructor. Roll back only if no native registration occurred.
        control.watches.push(handle.inner.clone());
        if control
            .commands
            .as_ref()
            .expect("open commands")
            .try_send(Command::Watch(registration))
            .is_err()
        {
            control.watches.pop();
            return Err(ClientError::TooManyRequests);
        }
        Ok(handle)
    }

    pub(crate) fn read(
        &self,
        filter: String,
        limit: usize,
        deadline: Instant,
    ) -> Result<Operation, ClientError> {
        let (response, receiver) = oneshot::channel();
        let commands = {
            let mut control = self.0.control.lock();
            if control.phase != Phase::Open {
                return Err(ClientError::ConnectionClosed);
            }
            if control.active >= MAX_IN_FLIGHT {
                return Err(ClientError::TooManyRequests);
            }
            control.active += 1;
            control
                .commands
                .as_ref()
                .expect("open owner has sender")
                .clone()
        };
        let ticket = Arc::new(Ticket {
            owner: self.clone(),
            cancel: CancellationToken::new(),
        });
        let command = Read {
            filter,
            limit,
            deadline,
            ticket: ticket.clone(),
            response,
        };
        // Admission above linearizes before close. If close wins after it, this
        // accepted operation is cancelled; the ticket still joins both legs.
        commands
            .try_send(Command::Read(command))
            .map_err(|_| ClientError::ConnectionClosed)?;
        Ok(Operation {
            response: receiver,
            ticket,
            connecting: false,
            completed: false,
        })
    }

    async fn run(
        &self,
        settings: Settings,
        deadline: Instant,
        ticket: Arc<Ticket>,
        response: oneshot::Sender<Result<Option<HGrid>, ClientError>>,
        mut commands: mpsc::Receiver<Command>,
    ) {
        // A newly registered Tokio timer may be pending until its driver ticks,
        // even when its deadline is already past. Check before polling any I/O.
        let connected = if Instant::now() >= deadline {
            Err(timeout())
        } else {
            tokio::select! {
                biased;
                _ = self.0.stop.cancelled() => Err(ClientError::ConnectionClosed),
                _ = ticket.cancel.cancelled() => Err(ClientError::ConnectionClosed),
                _ = tokio::time::sleep_until(deadline) => Err(timeout()),
                result = HaystackClient::connect_with_config(&settings.url, &settings.username, &settings.password, &settings.config) => result,
            }
        };
        let ws_url = url::Url::parse(&settings.url)
            .ok()
            .map(|mut url| {
                let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
                let _ = url.set_scheme(scheme);
                url.set_path(&format!("{}/ws", url.path().trim_end_matches('/')));
                url.to_string()
            })
            .unwrap_or_default();
        drop(settings);
        let client = match connected {
            Ok(client) => {
                let mut control = self.0.control.lock();
                if control.phase == Phase::Connecting && !ticket.cancel.is_cancelled() {
                    control.phase = Phase::Open;
                    client
                } else {
                    let _ = response.send(Err(ClientError::ConnectionClosed));
                    return;
                }
            }
            Err(error) => {
                let _ = response.send(Err(error));
                return;
            }
        };
        let client = Arc::new(client);
        let _ = response.send(Ok(None));
        drop(ticket);
        let mut tasks = tokio::task::JoinSet::new();
        let mut watches = tokio::task::JoinSet::new();
        let mut had_watches = false;
        loop {
            tokio::select! {
                biased;
                _ = self.0.stop.cancelled() => break,
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                Some(_) = watches.join_next(), if !watches.is_empty() => {},
                command = commands.recv() => {
                    let Some(command) = command else { break };
                    let command = match command {
                        Command::H5(command) => {
                            let client = client.clone();
                            let stop = self.0.stop.clone();
                            tasks.spawn(async move {
                                let control = haystack_client::h5::ReadControl::new(
                                    command.deadline, command.ticket.cancel.clone(), stop.clone(),
                                );
                                let result = if Instant::now() >= command.deadline { Err(timeout()) } else { tokio::select! {
                                    biased;
                                    _ = stop.cancelled() => Err(ClientError::ConnectionClosed),
                                    _ = command.ticket.cancel.cancelled() => Err(ClientError::ConnectionClosed),
                                    _ = tokio::time::sleep_until(command.deadline) => Err(timeout()),
                                    result = client.read_h5(&command.request, &control) => result,
                                }};
                                let result = control.check().and(result);
                                let _ = command.response.send(result);
                                drop(command.ticket);
                            });
                            continue;
                        }
                        Command::Watch(watch) => {
                            had_watches = true;
                            watches.spawn(watch.run(client.clone(), ws_url.clone()));
                            continue;
                        }
                        Command::Read(read) => read,
                    };
                    let client = client.clone();
                    let stop = self.0.stop.clone();
                    tasks.spawn(async move {
                        let result = if Instant::now() >= command.deadline { Err(timeout()) } else { tokio::select! {
                            biased;
                            _ = stop.cancelled() => Err(ClientError::ConnectionClosed),
                            _ = command.ticket.cancel.cancelled() => Err(ClientError::ConnectionClosed),
                            _ = tokio::time::sleep_until(command.deadline) => Err(timeout()),
                            result = client.read_bounded(&command.filter, command.limit, MAX_RESPONSE_BYTES) => result.map(Some),
                        }};
                        let _ = command.response.send(result);
                        drop(command.ticket);
                    });
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        // Every admitted child, including one still in the registration queue,
        // cleans up under this original session before its runtime is destroyed.
        commands.close();
        while let Some(command) = commands.recv().await {
            if let Command::Watch(watch) = command {
                had_watches = true;
                watch.inner.begin_close();
                watches.spawn(watch.run(client.clone(), ws_url.clone()));
            }
        }
        while watches.join_next().await.is_some() {}
        // Owned Python sessions dispose their auth authority after their children;
        // generic HttpTransport::close remains unchanged. A lost logout response
        // does not change an individual child's remote cleanup receipt.
        if had_watches {
            let _ = tokio::time::timeout(Duration::from_secs(2), client.close_session()).await;
        }
        drop(client);
    }

    pub(crate) fn begin_close(&self) {
        let reap = {
            let mut control = self.0.control.lock();
            match control.phase {
                Phase::Closing | Phase::Closed => return,
                Phase::New => {
                    control.settings.take();
                    control.phase = Phase::Closed;
                    false
                }
                Phase::Connecting | Phase::Open => {
                    control.phase = Phase::Closing;
                    control.commands.take();
                    true
                }
            }
        };
        let watches = self.0.control.lock().watches.clone();
        for watch in watches {
            watch.begin_close();
        }
        self.0.stop.cancel();
        self.0.changed.send_replace(());
        if reap {
            let owner = self.clone();
            crate::async_bridge::runtime().spawn(async move {
                owner.reap().await;
            });
        }
    }

    async fn reap(self) {
        let mut changed = self.0.changed.subscribe();
        let thread = loop {
            let ready = {
                let mut control = self.0.control.lock();
                if control.starting {
                    None
                } else {
                    Some(control.thread.take())
                }
            };
            if let Some(thread) = ready {
                break thread;
            }
            let _ = changed.changed().await;
        };
        if let Some(thread) = thread {
            let joined = tokio::task::spawn_blocking(move || thread.join()).await;
            if !matches!(joined, Ok(Ok(()))) {
                self.0.control.lock().cleanup_failed = true;
            }
        }
        loop {
            {
                let mut control = self.0.control.lock();
                if control.active == 0 {
                    // The owned session/runtime is disposed, and no conversion
                    // leg can still refer to an admitted remote operation.
                    control.watches.clear();
                    control.phase = Phase::Closed;
                    break;
                }
            }
            let _ = changed.changed().await;
        }
        self.0.changed.send_replace(());
    }

    pub(crate) async fn wait_closed(&self, deadline: Instant) -> Result<(), ClientError> {
        let mut changed = self.0.changed.subscribe();
        loop {
            {
                let control = self.0.control.lock();
                if control.phase == Phase::Closed {
                    return if control.cleanup_failed {
                        Err(ClientError::Connection(
                            "native owner failed during cleanup".into(),
                        ))
                    } else {
                        Ok(())
                    };
                }
            }
            tokio::time::timeout_at(deadline, changed.changed())
                .await
                .map_err(|_| timeout())?
                .map_err(|_| ClientError::ConnectionClosed)?;
        }
    }
}
fn timeout() -> ClientError {
    ClientError::Timeout(Duration::ZERO)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct HeldOwner {
        pub(crate) owner: Owner,
        pub(crate) release: std::sync::mpsc::Sender<()>,
        pub(crate) thread_exited: std::sync::mpsc::Receiver<()>,
        pub(crate) conversion: Arc<Ticket>,
    }

    pub(crate) fn held_owner() -> HeldOwner {
        let owner = Owner::new(Settings {
            url: "http://localhost:1".into(),
            username: "fixture".into(),
            password: Zeroizing::new("fixture".into()),
            config: ClientConfig::default(),
        });
        let (started, started_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let (thread_exited, thread_exited_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                // Like an already-started DNS/PBKDF job, dropping this JoinHandle
                // cannot stop the closure. Only actual runtime Drop waits for it.
                let job = tokio::task::spawn_blocking(move || {
                    started.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                });
                drop(job);
            });
            drop(runtime);
            thread_exited.send(()).unwrap();
        });
        // Confirm that this is real running work, not a queued-only fixture.
        if started_rx.recv_timeout(Duration::from_secs(2)).is_err() {
            let _ = thread.join();
            panic!("blocking fixture did not start");
        }
        {
            let mut control = owner.0.control.lock();
            control.phase = Phase::Open;
            control.settings.take();
            control.thread = Some(thread);
            control.active = 1;
        }
        let conversion = Arc::new(Ticket {
            owner: owner.clone(),
            cancel: CancellationToken::new(),
        });
        HeldOwner {
            owner,
            release,
            thread_exited: thread_exited_rx,
            conversion,
        }
    }

    pub(crate) fn ready_operation(owner: &Owner, connecting: bool) -> Operation<()> {
        let ticket = owner.admit().unwrap();
        let (response, receiver) = oneshot::channel();
        response.send(Ok(())).unwrap();
        Operation {
            response: receiver,
            ticket,
            connecting,
            completed: false,
        }
    }

    #[test]
    fn close_joins_blocking_tail_then_result_conversion_guard() {
        let HeldOwner {
            owner,
            release,
            thread_exited: thread_exited_rx,
            conversion,
        } = held_owner();
        owner.begin_close();
        let runtime = crate::async_bridge::runtime();
        runtime.block_on(async {
            assert!(matches!(
                owner
                    .wait_closed(Instant::now() + Duration::from_millis(10))
                    .await,
                Err(ClientError::Timeout(_))
            ));
        });
        assert_eq!(owner.phase(), Phase::Closing);
        assert!(thread_exited_rx.try_recv().is_err());
        release.send(()).unwrap();
        thread_exited_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        // Even a returned owner thread is insufficient while the conversion leg
        // is retained. The repeated observer does not restart native cleanup.
        runtime.block_on(async {
            assert!(matches!(
                owner
                    .wait_closed(Instant::now() + Duration::from_millis(10))
                    .await,
                Err(ClientError::Timeout(_))
            ));
        });
        assert_eq!(owner.phase(), Phase::Closing);
        drop(conversion);
        runtime.block_on(async {
            owner
                .wait_closed(Instant::now() + Duration::from_secs(2))
                .await
                .unwrap();
        });
        assert_eq!(owner.phase(), Phase::Closed);
    }
}
