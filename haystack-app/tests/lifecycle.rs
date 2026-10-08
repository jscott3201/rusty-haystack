use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind},
};
use std::{
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

fn graph() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("a")));
    graph.add(row).unwrap();
    graph
}
fn builder() -> ApplicationBuilder {
    ApplicationBuilder::new(graph(), Arc::new(AllowAll), ReadLimits::default()).unwrap()
}
fn context() -> ReadContext {
    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(2))
}
fn request() -> ReadRequest {
    ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed)
}
fn short_close() -> ShutdownPolicy {
    ShutdownPolicy {
        drain_timeout: Duration::ZERO,
        stop_timeout: Duration::from_millis(40),
    }
}
async fn state(handle: &ApplicationHandle, expected: ApplicationState) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while handle.state() != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("state transition");
}
async fn load(service: &ReadService, expected: ReadLoad) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while service.load() != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("read load transition");
}

#[derive(Clone, Copy)]
enum Init {
    Ready,
    Wait,
    Fail,
    Panic,
}
struct ProbeState {
    held: AtomicUsize,
    initialized: AtomicUsize,
    closed: AtomicUsize,
    rolled_back: AtomicUsize,
    entered: Semaphore,
    release: Semaphore,
    events: Mutex<Vec<String>>,
}
impl ProbeState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            held: AtomicUsize::new(0),
            initialized: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            rolled_back: AtomicUsize::new(0),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            events: Mutex::new(vec![]),
        })
    }
}
struct Probe {
    name: &'static str,
    state: Arc<ProbeState>,
    init: Init,
    acquired: bool,
}
impl Probe {
    fn new(name: &'static str, state: &Arc<ProbeState>, init: Init) -> Self {
        Self {
            name,
            state: state.clone(),
            init,
            acquired: false,
        }
    }
    fn release(&mut self, event: &str) {
        if self.acquired {
            self.state.held.fetch_sub(1, Ordering::SeqCst);
            self.acquired = false;
        }
        self.state
            .events
            .lock()
            .unwrap()
            .push(format!("{}:{event}", self.name));
    }
}
impl ApplicationResource for Probe {
    fn name(&self) -> &str {
        self.name
    }
    fn initialize(&mut self, _: ResourceContext) -> ResourceFuture<'_, ReadyInfo> {
        Box::pin(async move {
            self.acquired = true;
            self.state.held.fetch_add(1, Ordering::SeqCst);
            self.state
                .events
                .lock()
                .unwrap()
                .push(format!("{}:acquire", self.name));
            self.state.entered.add_permits(1);
            match self.init {
                Init::Ready => {}
                Init::Wait => {
                    self.state.release.acquire().await.unwrap().forget();
                }
                Init::Fail => {
                    return Err(ApplicationError::resource(
                        self.name,
                        "injected failure after acquisition",
                    ));
                }
                Init::Panic => panic!("injected panic after acquisition"),
            }
            self.state.initialized.fetch_add(1, Ordering::SeqCst);
            Ok(ReadyInfo::default())
        })
    }
    fn rollback_start(&mut self) -> ResourceFuture<'_> {
        Box::pin(async move {
            self.state.rolled_back.fetch_add(1, Ordering::SeqCst);
            self.release("rollback");
            Ok(())
        })
    }
    fn close(&mut self) -> ResourceFuture<'_> {
        Box::pin(async move {
            self.state.closed.fetch_add(1, Ordering::SeqCst);
            self.release("close");
            Ok(())
        })
    }
}

#[tokio::test]
async fn listenerless_owner_shares_reads_and_concurrent_close_without_owning_runtime() {
    let probe = ProbeState::new();
    let app = builder().owned_resource(Probe::new("provider", &probe, Init::Ready));
    let handle = app.handle();
    assert_eq!(
        handle
            .read_service()
            .read(context(), request())
            .await
            .unwrap_err(),
        ReadError::NotReady
    );
    let owner = Arc::new(app.start(&tokio::runtime::Handle::current()).unwrap());
    assert!(owner.ready().await.unwrap().listeners.is_empty());
    let service = handle.read_service();
    drop(service.clone());
    drop(handle.clone());
    assert_eq!(
        service.read(context(), request()).await.unwrap().row_count,
        1
    );
    assert_eq!(owner.state(), ApplicationState::Running);
    let mut closers = vec![];
    for _ in 0..12 {
        let owner = owner.clone();
        closers.push(tokio::spawn(async move { owner.close().await }));
    }
    for closer in closers {
        assert_eq!(
            closer.await.unwrap().unwrap(),
            CloseReport {
                drain_expired: false
            }
        );
    }
    assert_eq!(
        owner.close().await.unwrap(),
        CloseReport {
            drain_expired: false
        }
    );
    assert_eq!(owner.state(), ApplicationState::Closed);
    assert_eq!(probe.initialized.load(Ordering::SeqCst), 1);
    assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
    assert_eq!(probe.rolled_back.load(Ordering::SeqCst), 0);
    assert_eq!(probe.held.load(Ordering::SeqCst), 0);
    assert_eq!(
        service.read(context(), request()).await.unwrap_err(),
        ReadError::Closed
    );
    assert!(owner.terminated().await.close.is_ok());
    assert_eq!(tokio::spawn(async { 42 }).await.unwrap(), 42);
}

#[tokio::test]
async fn owner_drop_requests_cleanup_but_handle_drop_does_not() {
    let probe = ProbeState::new();
    let app = builder().owned_resource(Probe::new("provider", &probe, Init::Ready));
    let handle = app.handle();
    let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    drop(handle.clone());
    assert_eq!(owner.state(), ApplicationState::Running);
    drop(owner);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), handle.terminated())
            .await
            .unwrap()
            .close
            .is_ok()
    );
    assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
    assert_eq!(handle.state(), ApplicationState::Closed);
    let abandoned = builder();
    let handle = abandoned.handle();
    drop(abandoned);
    assert_eq!(
        handle.ready().await.unwrap_err(),
        ApplicationError::Abandoned
    );
    assert_eq!(
        handle.terminated().await.close.unwrap_err(),
        ApplicationError::Abandoned
    );
    assert_eq!(
        handle
            .read_service()
            .read(context(), request())
            .await
            .unwrap_err(),
        ReadError::Closed
    );
}

#[tokio::test]
async fn failure_and_panic_rollback_partial_acquisitions_then_initialized_resources() {
    for mode in [Init::Fail, Init::Panic] {
        let probe = ProbeState::new();
        let app = builder()
            .owned_resource(Probe::new("first", &probe, Init::Ready))
            .owned_resource(Probe::new("partial", &probe, mode))
            .owned_resource(Probe::new("never", &probe, Init::Ready));
        let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
        assert!(matches!(
            owner.ready().await,
            Err(ApplicationError::Resource { .. })
        ));
        assert!(owner.close().await.is_err());
        assert!(owner.terminated().await.close.is_err());
        assert_eq!(probe.held.load(Ordering::SeqCst), 0);
        assert_eq!(probe.initialized.load(Ordering::SeqCst), 1);
        assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
        assert_eq!(probe.rolled_back.load(Ordering::SeqCst), 1);
        assert_eq!(
            *probe.events.lock().unwrap(),
            [
                "first:acquire",
                "partial:acquire",
                "partial:rollback",
                "first:close"
            ]
        );
    }
}

#[tokio::test]
async fn cancelled_or_expired_start_releases_partial_acquisition() {
    for timeout in [false, true] {
        let probe = ProbeState::new();
        let app = builder().owned_resource(Probe::new("partial", &probe, Init::Wait));
        let app = if timeout {
            app.startup_timeout(Duration::from_millis(40))
        } else {
            app
        };
        let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
        probe.entered.acquire().await.unwrap().forget();
        assert_eq!(probe.held.load(Ordering::SeqCst), 1);
        assert_eq!(owner.state(), ApplicationState::Starting);
        if timeout {
            assert_eq!(
                owner.ready().await.unwrap_err(),
                ApplicationError::StartupTimeout
            );
        }
        let close = owner.close().await;
        assert_eq!(close.is_err(), timeout);
        owner.terminated().await;
        assert_eq!(probe.held.load(Ordering::SeqCst), 0);
        assert_eq!(probe.initialized.load(Ordering::SeqCst), 0);
        assert_eq!(probe.closed.load(Ordering::SeqCst), 0);
        assert_eq!(probe.rolled_back.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn immediate_close_does_not_initialize_selected_resources() {
    let probe = ProbeState::new();
    let owner = builder()
        .owned_resource(Probe::new("provider", &probe, Init::Ready))
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.close().await.unwrap();
    assert_eq!(owner.state(), ApplicationState::Closed);
    assert_eq!(probe.held.load(Ordering::SeqCst), 0);
    assert!(probe.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn admitted_body_can_continue_during_drain_but_new_work_is_rejected() {
    let app = builder();
    let handle = app.handle();
    let owner = Arc::new(app.start(&tokio::runtime::Handle::current()).unwrap());
    owner.ready().await.unwrap();
    let reads = handle.read_service();
    let body = reads.begin(context()).await.unwrap();
    assert_eq!(handle.outstanding_tasks(), 1);
    let closing = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.close().await })
    };
    state(&handle, ApplicationState::Closing).await;
    assert_eq!(
        reads.read(context(), request()).await.unwrap_err(),
        ReadError::Closed
    );
    assert_eq!(body.read(request()).await.unwrap().row_count, 1);
    assert!(!closing.await.unwrap().unwrap().drain_expired);
    assert_eq!(handle.outstanding_tasks(), 0);
}

#[tokio::test]
async fn pre_body_and_queued_work_are_registered_until_actual_release() {
    let app = ApplicationBuilder::new(
        graph(),
        Arc::new(AllowAll),
        ReadLimits {
            max_concurrent: 1,
            max_queued: 1,
            ..ReadLimits::default()
        },
    )
    .unwrap()
    .shutdown_policy(short_close());
    let handle = app.handle();
    let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    let reads = handle.read_service();
    let body = reads.begin(context()).await.unwrap();
    let waiting = {
        let reads = reads.clone();
        tokio::spawn(async move { reads.read(context(), request()).await })
    };
    load(
        &reads,
        ReadLoad {
            admitted: 1,
            waiting: 1,
        },
    )
    .await;
    assert_eq!(handle.outstanding_tasks(), 2);
    let close = owner.close().await.unwrap_err();
    assert!(matches!(
        close,
        ApplicationError::ShutdownTimeout {
            phase: ShutdownPhase::Tasks,
            outstanding: 1
        }
    ));
    assert_eq!(waiting.await.unwrap().unwrap_err(), ReadError::Cancelled);
    body.cancelled().await;
    assert_eq!(
        probe_closed_state(&owner, &handle),
        (ApplicationState::Failed, 1)
    );
    drop(body);
    assert_eq!(owner.terminated().await.close.unwrap_err(), close);
    assert_eq!(handle.outstanding_tasks(), 0);
}
fn probe_closed_state(
    owner: &ApplicationOwner,
    handle: &ApplicationHandle,
) -> (ApplicationState, usize) {
    (owner.state(), handle.outstanding_tasks())
}

#[test]
fn queued_blocking_read_timeout_does_not_join_unrelated_runtime_work() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    runtime.block_on(async {
        let probe = ProbeState::new();
        let app = builder()
            .shutdown_policy(short_close())
            .owned_resource(Probe::new("provider", &probe, Init::Ready));
        let handle = app.handle();
        let owner = app.start(runtime.handle()).unwrap();
        owner.ready().await.unwrap();
        let reads = handle.read_service();
        let read = {
            let reads = reads.clone();
            tokio::spawn(async move { reads.read(context(), request()).await })
        };
        load(
            &reads,
            ReadLoad {
                admitted: 1,
                waiting: 0,
            },
        )
        .await;
        let close = tokio::time::timeout(Duration::from_secs(1), owner.close()).await;
        let before = (
            handle.outstanding_tasks(),
            probe.closed.load(Ordering::SeqCst),
            reads.load().admitted,
        );
        // Release even if an assertion would fail, so the test runtime cannot hang.
        release_tx.send(()).unwrap();
        blocker.await.unwrap();
        let termination = tokio::time::timeout(Duration::from_secs(1), owner.terminated())
            .await
            .unwrap();
        let close = close
            .expect("close deadline is independent of the unrelated blocker")
            .unwrap_err();
        assert!(matches!(
            close,
            ApplicationError::ShutdownTimeout {
                phase: ShutdownPhase::Tasks,
                ..
            }
        ));
        assert_eq!(before, (1, 0, 1));
        assert_eq!(read.await.unwrap().unwrap_err(), ReadError::Cancelled);
        assert_eq!(termination.close.unwrap_err(), close);
        assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
        assert_eq!(handle.outstanding_tasks(), 0);
        assert_eq!(
            reads.read(context(), request()).await.unwrap_err(),
            ReadError::Closed
        );
        assert_eq!(tokio::spawn(async { 7 }).await.unwrap(), 7);
    });
    // The standalone runtime owner is destroyed outside async execution.
    drop(runtime);
}

struct Paused {
    entered: std::sync::mpsc::Sender<()>,
    release: Arc<Barrier>,
}
impl ReadPolicy for Paused {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        self.entered.send(()).unwrap();
        self.release.wait();
        Ok(Arc::new(AllowAll))
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_worker_remains_owned_after_caller_drop_and_close_timeout() {
    let (tx, rx) = std::sync::mpsc::channel();
    let release = Arc::new(Barrier::new(2));
    let probe = ProbeState::new();
    let app = ApplicationBuilder::new(
        graph(),
        Arc::new(Paused {
            entered: tx,
            release: release.clone(),
        }),
        ReadLimits::default(),
    )
    .unwrap()
    .shutdown_policy(short_close())
    .owned_resource(Probe::new("provider", &probe, Init::Ready));
    let handle = app.handle();
    let owner = app.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    let read = {
        let reads = handle.read_service();
        tokio::spawn(async move { reads.read(context(), request()).await })
    };
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    read.abort();
    let _ = read.await;
    let close = tokio::time::timeout(Duration::from_secs(1), owner.close()).await;
    let before = (
        handle.outstanding_tasks(),
        probe.closed.load(Ordering::SeqCst),
    );
    release.wait();
    let termination = tokio::time::timeout(Duration::from_secs(1), owner.terminated())
        .await
        .unwrap();
    let close = close
        .expect("close returned despite noncooperative policy")
        .unwrap_err();
    assert_eq!(before, (1, 0));
    assert!(matches!(
        close,
        ApplicationError::ShutdownTimeout {
            phase: ShutdownPhase::Tasks,
            ..
        }
    ));
    assert_eq!(termination.close.unwrap_err(), close);
    assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
    assert_eq!(probe.held.load(Ordering::SeqCst), 0);
}

struct SlowCleanup(Arc<ProbeState>);
impl ApplicationResource for SlowCleanup {
    fn name(&self) -> &str {
        "slow cleanup"
    }
    fn initialize(&mut self, _: ResourceContext) -> ResourceFuture<'_, ReadyInfo> {
        Box::pin(async { Ok(ReadyInfo::default()) })
    }
    fn rollback_start(&mut self) -> ResourceFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn close(&mut self) -> ResourceFuture<'_> {
        Box::pin(async move {
            self.0.closed.fetch_add(1, Ordering::SeqCst);
            self.0.entered.add_permits(1);
            self.0.release.acquire().await.unwrap().forget();
            self.0
                .events
                .lock()
                .unwrap()
                .push("cleanup completed".into());
            Ok(())
        })
    }
}
#[tokio::test]
async fn cleanup_deadline_reports_failure_without_cancelling_cleanup_or_restarting_it() {
    let probe = ProbeState::new();
    let owner = builder()
        .shutdown_policy(short_close())
        .owned_resource(SlowCleanup(probe.clone()))
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    let outcome = owner.close().await;
    assert_eq!(
        outcome,
        Err(ApplicationError::ShutdownTimeout {
            phase: ShutdownPhase::Resources,
            outstanding: 0
        })
    );
    assert_eq!(owner.close().await, outcome);
    assert_eq!(owner.state(), ApplicationState::Failed);
    assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
    assert!(probe.events.lock().unwrap().is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), owner.terminated())
            .await
            .is_err()
    );
    probe.release.add_permits(1);
    let report = owner.terminated().await;
    assert_eq!(report.close, outcome);
    assert!(report.cleanup_errors.is_empty());
    assert_eq!(*probe.events.lock().unwrap(), vec!["cleanup completed"]);
    assert_eq!(probe.closed.load(Ordering::SeqCst), 1);
}
