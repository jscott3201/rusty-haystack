use chrono::{DateTime, FixedOffset};
use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, Number},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
fn time(text: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(text).unwrap()
}
fn graph(kind: &str, tz: &str) -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    let mut point = HDict::new();
    point.set("id", Kind::Ref(HRef::from_val("p")));
    point.set("his", Kind::Marker);
    point.set("kind", Kind::Str(kind.into()));
    point.set("tz", Kind::Str(tz.into()));
    if kind == "Number" {
        point.set("unit", Kind::Str("°C".into()));
    }
    graph.add(point).unwrap();
    graph
}
fn context(subject: &str) -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: subject.into(),
        },
        Duration::from_secs(5),
    )
}
fn request() -> HistoryReadRequest {
    HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01".into(),
    }
}
fn item(second: i64, value: Kind) -> HisItem {
    HisItem {
        ts: time("2024-06-01T00:00:00Z") + chrono::Duration::seconds(second),
        val: value,
    }
}
fn number(value: f64) -> Kind {
    Kind::Number(Number::unitless(value))
}
fn service(
    store: Arc<dyn HistoryProvider>,
    graph: SharedGraph,
    limits: HistoryLimits,
) -> HistoryService {
    HistoryService::new(
        ReadService::new(graph, Arc::new(AllowAll), ReadLimits::default()).unwrap(),
        store,
        limits,
    )
    .unwrap()
}

#[tokio::test]
async fn requested_batches_are_bounded_and_generation_is_per_point() {
    let store = Arc::new(HisStore::new());
    store
        .write("p", (0..1000).map(|i| item(i, number(i as f64))).collect())
        .unwrap();
    let service = service(
        store.clone(),
        graph("Number", "UTC"),
        HistoryLimits {
            batch_rows: 3,
            ..HistoryLimits::default()
        },
    );
    let mut session = service.open(context("a"), request()).await.unwrap();
    assert_eq!(session.metadata.coverage.retained_count, 1000);
    let first = session.next().await;
    assert_eq!(first.samples.len(), 3);
    assert_eq!(first.terminal, None);
    store.write("other", vec![item(0, number(3.0))]).unwrap();
    assert_eq!(session.next().await.samples.len(), 3);
    store.write("p", vec![item(999, number(4.0))]).unwrap();
    let next = session.next().await;
    assert!(next.samples.is_empty());
    assert_eq!(
        next.terminal,
        Some(HistoryTerminal::Interrupted(
            HistoryReason::GenerationChanged
        ))
    );
    session.close().await;
    assert_eq!(service.active_sessions(), 0);
    assert_eq!(service.read_service().load().admitted, 0);
}
#[tokio::test]
async fn empty_retained_history_and_evicted_coverage_are_explicit() {
    let store = Arc::new(HisStore::with_retention(2).unwrap());
    let service = service(
        store.clone(),
        graph("Number", "UTC"),
        HistoryLimits::default(),
    );
    let result = service.collect(context("a"), request()).await.unwrap();
    assert!(result.samples.is_empty());
    assert_eq!(result.terminal, HistoryTerminal::Complete);
    assert_eq!(result.metadata.coverage.retained_count, 0);
    store
        .write("p", (0..4).map(|i| item(i, number(i as f64))).collect())
        .unwrap();
    let result = service.collect(context("a"), request()).await.unwrap();
    assert_eq!(result.samples.len(), 2);
    assert_eq!(result.terminal, HistoryTerminal::Complete);
    assert_eq!(
        result.metadata.coverage.evicted_through.unwrap().dt,
        item(1, number(0.0)).ts
    );
}
#[tokio::test]
async fn original_schema_units_na_and_oversized_sources_never_coerce_or_drop() {
    for bad in [
        Kind::Int(3),
        Kind::Null,
        Kind::None,
        Kind::Remove,
        Kind::Ref(HRef::from_val("hidden")),
        Kind::Number(Number::new(1.0, Some("°F".into()))),
        Kind::Number(Number::new(f64::NAN, Some("°C".into()))),
    ] {
        let store = Arc::new(HisStore::new());
        store
            .write("p", vec![item(0, Kind::NA), item(1, bad)])
            .unwrap();
        let service = service(store, graph("Number", "UTC"), HistoryLimits::default());
        let result = service.collect(context("a"), request()).await.unwrap();
        assert_eq!(result.samples.len(), 1);
        assert_eq!(result.samples[0].val, Kind::NA);
        assert_eq!(
            result.terminal,
            HistoryTerminal::Failed(HistoryReason::UnsupportedValue)
        );
    }
    let store = Arc::new(HisStore::new());
    store
        .write("p", vec![item(0, Kind::Str("x".repeat(1_000_000)))])
        .unwrap();
    let service = service(
        store,
        graph("Str", "UTC"),
        HistoryLimits {
            batch_bytes: 1024,
            ..HistoryLimits::default()
        },
    );
    let result = service.collect(context("a"), request()).await.unwrap();
    assert!(result.samples.is_empty());
    assert_eq!(
        result.terminal,
        HistoryTerminal::Limited(HistoryReason::Bytes)
    );
}
#[tokio::test]
async fn row_limit_preserves_partial_data_and_exclusive_midnight() {
    let store = Arc::new(HisStore::new());
    store
        .write(
            "p",
            vec![
                item(0, number(1.0)),
                item(1, number(2.0)),
                item(2, number(3.0)),
                item(86400, number(4.0)),
            ],
        )
        .unwrap();
    let service = service(
        store.clone(),
        graph("Number", "UTC"),
        HistoryLimits {
            total_rows: 2,
            batch_rows: 1,
            ..HistoryLimits::default()
        },
    );
    let result = service.collect(context("a"), request()).await.unwrap();
    assert_eq!(result.samples.len(), 2);
    assert_eq!(
        result.terminal,
        HistoryTerminal::Limited(HistoryReason::Rows)
    );
    let service = service_for_full(store);
    let result = service.collect(context("a"), request()).await.unwrap();
    assert_eq!(result.samples.len(), 3);
    assert_eq!(result.terminal, HistoryTerminal::Complete);
}
fn service_for_full(store: Arc<HisStore>) -> HistoryService {
    service(store, graph("Number", "UTC"), HistoryLimits::default())
}
#[tokio::test]
async fn session_limits_and_idle_cancellation_release_without_another_pull() {
    let service = service(
        Arc::new(HisStore::new()),
        graph("Number", "UTC"),
        HistoryLimits {
            max_sessions: 2,
            max_sessions_per_principal: 1,
            ..HistoryLimits::default()
        },
    );
    let context = context("a");
    let cancel = context.cancellation.clone();
    let mut first = service.open(context, request()).await.unwrap();
    assert!(matches!(
        service.open(crate::context("a"), request()).await,
        Err(ReadError::Capacity)
    ));
    let mut second = service.open(crate::context("b"), request()).await.unwrap();
    assert!(matches!(
        service.open(crate::context("c"), request()).await,
        Err(ReadError::Capacity)
    ));
    cancel.cancel();
    first.close().await;
    second.close().await;
    assert_eq!(
        first.next().await.terminal,
        Some(HistoryTerminal::Interrupted(HistoryReason::Cancelled))
    );
    assert_eq!(service.active_sessions(), 0);
}
#[tokio::test]
async fn idle_deadline_and_metadata_changes_are_interrupted() {
    let graph = graph("Number", "UTC");
    let service = service(
        Arc::new(HisStore::new()),
        graph.clone(),
        HistoryLimits::default(),
    );
    let mut session = service
        .open(
            ReadContext::with_timeout(Principal::Anonymous, Duration::from_millis(30)),
            request(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        session.next().await.terminal,
        Some(HistoryTerminal::Interrupted(HistoryReason::Deadline))
    );
    session.close().await;
    let mut session = service.open(context("a"), request()).await.unwrap();
    let mut changes = HDict::new();
    changes.set("dis", Kind::Str("changed".into()));
    graph.update("p", changes).unwrap();
    assert_eq!(
        session.next().await.terminal,
        Some(HistoryTerminal::Interrupted(HistoryReason::MetadataChanged))
    );
    session.close().await;
}

struct Fixture {
    store: HisStore,
    pulls: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    gate: Option<Arc<tokio::sync::Semaphore>>,
    failure: bool,
}
struct FixtureSession {
    inner: Box<dyn HistorySession>,
    pulls: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    gate: Option<Arc<tokio::sync::Semaphore>>,
    failure: bool,
}
impl HistoryProvider for Fixture {
    fn open(
        &self,
        id: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        Box::pin(async move {
            Ok(Box::new(FixtureSession {
                inner: self.store.open(id, start, end, budget).await?,
                pulls: self.pulls.clone(),
                closes: self.closes.clone(),
                gate: self.gate.clone(),
                failure: self.failure,
            }) as Box<dyn HistorySession>)
        })
    }
    fn his_write(&self, id: &str, items: Vec<HisItem>) -> HistoryFuture<'_, ()> {
        self.store.his_write(id, items)
    }
}
impl HistorySession for FixtureSession {
    fn metadata(&self) -> &ProviderHistoryMetadata {
        self.inner.metadata()
    }
    fn pull(&mut self, budget: HistoryPullBudget) -> HistoryFuture<'_, ProviderHistoryBatch> {
        let n = self.pulls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if let Some(gate) = &self.gate {
                let _ = gate.acquire().await.unwrap();
            }
            if self.failure && n > 0 {
                return Err(HistoryProviderError::Failed);
            }
            self.inner.pull(budget).await
        })
    }
    fn close(&mut self) -> HistoryFuture<'_, ()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        self.inner.close()
    }
}
fn fixture(
    gate: Option<Arc<tokio::sync::Semaphore>>,
    failure: bool,
) -> (Arc<Fixture>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let pulls = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let store = HisStore::new();
    store
        .write("p", (0..6).map(|i| item(i, number(i as f64))).collect())
        .unwrap();
    (
        Arc::new(Fixture {
            store,
            pulls: pulls.clone(),
            closes: closes.clone(),
            gate,
            failure,
        }),
        pulls,
        closes,
    )
}
#[tokio::test]
async fn no_prefetch_and_failure_keeps_nonempty_partial_result() {
    let (provider, pulls, closes) = fixture(None, true);
    let service = service(
        provider,
        graph("Number", "UTC"),
        HistoryLimits {
            batch_rows: 2,
            ..HistoryLimits::default()
        },
    );
    let session = service.open(context("a"), request()).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(pulls.load(Ordering::SeqCst), 0);
    let result = session.collect().await.unwrap();
    assert_eq!(result.samples.len(), 2);
    assert_eq!(
        result.terminal,
        HistoryTerminal::Failed(HistoryReason::Provider)
    );
    tokio::task::yield_now().await;
    assert_eq!(pulls.load(Ordering::SeqCst), 2);
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancellation_retains_work_until_provider_future_really_finishes() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (provider, pulls, closes) = fixture(Some(gate.clone()), false);
    let builder = ApplicationBuilder::new(
        graph("Number", "UTC"),
        Arc::new(AllowAll),
        ReadLimits::default(),
    )
    .unwrap()
    .shutdown_policy(ShutdownPolicy {
        drain_timeout: Duration::from_millis(10),
        stop_timeout: Duration::from_millis(20),
    });
    let service = HistoryService::new(
        builder.handle().read_service(),
        provider,
        HistoryLimits::default(),
    )
    .unwrap();
    let owner = builder
        .borrowed_history(service.clone())
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    let mut session = service.open(context("a"), request()).await.unwrap();
    let task = tokio::spawn(async move {
        let batch = session.next().await;
        (session, batch)
    });
    while pulls.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        owner.close().await,
        Err(ApplicationError::ShutdownTimeout { .. })
    ));
    let (mut session, batch) = task.await.unwrap();
    assert_eq!(
        batch.terminal,
        Some(HistoryTerminal::Interrupted(HistoryReason::OwnerStopped))
    );
    assert!(owner.handle().outstanding_tasks() > 0);
    assert_eq!(service.read_service().load().admitted, 1);
    assert_eq!(closes.load(Ordering::SeqCst), 0);
    gate.add_permits(1);
    session.close().await;
    owner.terminated().await;
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert_eq!(service.read_service().load().admitted, 0);
}

#[derive(Clone, Copy)]
enum HostileMode {
    TooMany,
    TooLarge,
    Duplicate,
    WrongKind,
    Empty,
    ShortComplete,
    ChangedMetadata,
    CloseFailure,
}
struct Hostile {
    store: HisStore,
    mode: HostileMode,
}
struct HostileSession {
    inner: Box<dyn HistorySession>,
    mode: HostileMode,
    calls: usize,
    metadata: ProviderHistoryMetadata,
}
impl HistoryProvider for Hostile {
    fn open(
        &self,
        id: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        Box::pin(async move {
            let inner = self.store.open(id, start, end, budget).await?;
            let metadata = inner.metadata().clone();
            Ok(Box::new(HostileSession {
                inner,
                mode: self.mode,
                calls: 0,
                metadata,
            }) as Box<dyn HistorySession>)
        })
    }
    fn his_write(&self, id: &str, items: Vec<HisItem>) -> HistoryFuture<'_, ()> {
        self.store.his_write(id, items)
    }
}
impl HistorySession for HostileSession {
    fn metadata(&self) -> &ProviderHistoryMetadata {
        &self.metadata
    }
    fn pull(&mut self, budget: HistoryPullBudget) -> HistoryFuture<'_, ProviderHistoryBatch> {
        self.calls += 1;
        Box::pin(async move {
            let mut batch = self.inner.pull(budget.clone()).await?;
            if self.calls == 2 {
                match self.mode {
                    HostileMode::TooMany => {
                        while batch.items.len() <= budget.max_rows {
                            batch.items.push(item(4, number(4.0)));
                        }
                    }
                    HostileMode::TooLarge => {
                        batch.items[0].val = Kind::Str("x".repeat(budget.max_bytes + 1))
                    }
                    HostileMode::Duplicate => batch.items[0].ts = item(1, number(1.0)).ts,
                    HostileMode::WrongKind => batch.items[0].val = Kind::Int(3),
                    HostileMode::Empty => {
                        batch.items.clear();
                        batch.terminal = None;
                    }
                    HostileMode::ShortComplete => {
                        batch.items.clear();
                        batch.terminal = Some(HistoryTerminal::Complete);
                    }
                    HostileMode::ChangedMetadata => self.metadata.coverage.retained_count += 1,
                    HostileMode::CloseFailure => {}
                }
            }
            Ok(batch)
        })
    }
    fn close(&mut self) -> HistoryFuture<'_, ()> {
        Box::pin(async move {
            if matches!(self.mode, HostileMode::CloseFailure) {
                Err(HistoryProviderError::Failed)
            } else {
                self.inner.close().await
            }
        })
    }
}
#[tokio::test]
async fn hostile_provider_cannot_relabel_overflow_duplicates_schema_or_missing_rows_as_complete() {
    for mode in [
        HostileMode::TooMany,
        HostileMode::TooLarge,
        HostileMode::Duplicate,
        HostileMode::WrongKind,
        HostileMode::Empty,
        HostileMode::ShortComplete,
        HostileMode::ChangedMetadata,
    ] {
        let store = HisStore::new();
        store
            .write("p", (0..6).map(|i| item(i, number(i as f64))).collect())
            .unwrap();
        let service = service(
            Arc::new(Hostile { store, mode }),
            graph("Number", "UTC"),
            HistoryLimits {
                batch_rows: 2,
                ..HistoryLimits::default()
            },
        );
        let result = service.collect(context("a"), request()).await.unwrap();
        assert_eq!(result.samples.len(), 2);
        assert_eq!(
            result.terminal,
            HistoryTerminal::Failed(if matches!(mode, HostileMode::WrongKind) {
                HistoryReason::UnsupportedValue
            } else {
                HistoryReason::InvalidProvider
            })
        );
    }
    let store = HisStore::new();
    store.write("p", vec![item(0, number(1.0))]).unwrap();
    let service = service(
        Arc::new(Hostile {
            store,
            mode: HostileMode::CloseFailure,
        }),
        graph("Number", "UTC"),
        HistoryLimits::default(),
    );
    let result = service.collect(context("a"), request()).await.unwrap();
    assert_eq!(result.samples.len(), 1);
    assert_eq!(
        result.terminal,
        HistoryTerminal::Failed(HistoryReason::Provider)
    );
}
#[tokio::test]
async fn dropped_pull_wait_resumes_the_same_batch_without_prefetch_or_row_loss() {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (provider, pulls, _) = fixture(Some(gate.clone()), false);
    let service = service(
        provider,
        graph("Number", "UTC"),
        HistoryLimits {
            batch_rows: 2,
            ..HistoryLimits::default()
        },
    );
    let mut session = service.open(context("a"), request()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), session.next())
            .await
            .is_err()
    );
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    gate.add_permits(1);
    let batch = session.next().await;
    assert_eq!(batch.samples.len(), 2);
    assert_eq!(batch.samples[0].val, number(0.0));
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    session.close().await;
}
#[tokio::test]
async fn explicit_zero_width_range_is_complete_and_empty() {
    let store = Arc::new(HisStore::new());
    store.write("p", vec![item(0, number(1.0))]).unwrap();
    let service = service(store, graph("Number", "UTC"), HistoryLimits::default());
    let result = service
        .collect(
            context("a"),
            HistoryReadRequest {
                id: "p".into(),
                range: "2024-06-01T00:00:00Z UTC,2024-06-01T00:00:00Z UTC".into(),
            },
        )
        .await
        .unwrap();
    assert!(result.samples.is_empty());
    assert_eq!(result.terminal, HistoryTerminal::Complete);
}
#[derive(Clone)]
struct MutableRules(Arc<AtomicUsize>);
struct MutableSnapshot(usize);
impl ReadPolicy for MutableRules {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(MutableSnapshot(self.0.load(Ordering::SeqCst))))
    }
}
impl PolicySnapshot for MutableSnapshot {
    fn scope_key(&self) -> &str {
        if self.0 == 0 { "allowed" } else { "revoked" }
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, _: &str) -> bool {
        true
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        self.0 == 0 || tag != "val"
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
async fn current_sample_field_policy_is_checked_before_every_disclosure() {
    let generation = Arc::new(AtomicUsize::new(0));
    let (provider, pulls, _) = fixture(None, false);
    let reads = ReadService::new(
        graph("Number", "UTC"),
        Arc::new(MutableRules(generation.clone())),
        ReadLimits::default(),
    )
    .unwrap();
    let service = HistoryService::new(
        reads,
        provider,
        HistoryLimits {
            batch_rows: 2,
            ..HistoryLimits::default()
        },
    )
    .unwrap();
    let mut session = service.open(context("a"), request()).await.unwrap();
    assert_eq!(session.next().await.samples.len(), 2);
    generation.store(1, Ordering::SeqCst);
    let batch = session.next().await;
    assert!(batch.samples.is_empty());
    assert_eq!(
        batch.terminal,
        Some(HistoryTerminal::Interrupted(HistoryReason::PolicyChanged))
    );
    assert_eq!(pulls.load(Ordering::SeqCst), 1);
    session.close().await;
    assert!(matches!(
        service.open(context("a"), request()).await,
        Err(ReadError::Unavailable)
    ));
}

#[tokio::test]
async fn metadata_lock_wait_allows_runtime_cancellation_before_deadline() {
    let graph = graph("Number", "UTC");
    let service = service(
        Arc::new(HisStore::new()),
        graph.clone(),
        HistoryLimits::default(),
    );
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        graph.write(|_| {
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
    });
    held_rx.recv().unwrap();
    let context = ReadContext::with_timeout(Principal::Anonymous, Duration::from_millis(250));
    let token = context.cancellation.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        token.cancel();
    });
    let outcome = service.open(context, request()).await;
    release_tx.send(()).unwrap();
    thread.join().unwrap();
    timer.await.unwrap();
    assert!(matches!(outcome, Err(ReadError::Cancelled)));
}

struct BudgetProbe {
    store: HisStore,
    work: Arc<AtomicUsize>,
}
struct BudgetProbeSession {
    inner: Box<dyn HistorySession>,
    work: Arc<AtomicUsize>,
}
impl HistoryProvider for BudgetProbe {
    fn open(
        &self,
        id: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        Box::pin(async move {
            Ok(Box::new(BudgetProbeSession {
                inner: self.store.open(id, start, end, budget).await?,
                work: self.work.clone(),
            }) as Box<dyn HistorySession>)
        })
    }
    fn his_write(&self, id: &str, items: Vec<HisItem>) -> HistoryFuture<'_, ()> {
        self.store.his_write(id, items)
    }
}
impl HistorySession for BudgetProbeSession {
    fn metadata(&self) -> &ProviderHistoryMetadata {
        self.inner.metadata()
    }
    fn pull(&mut self, budget: HistoryPullBudget) -> HistoryFuture<'_, ProviderHistoryBatch> {
        self.work.store(budget.max_work, Ordering::SeqCst);
        self.inner.pull(budget)
    }
    fn close(&mut self) -> HistoryFuture<'_, ()> {
        self.inner.close()
    }
}
#[tokio::test]
async fn provider_source_work_uses_remaining_lifetime_budget() {
    let store = HisStore::new();
    store.write("p", vec![item(0, number(1.0))]).unwrap();
    let work = Arc::new(AtomicUsize::new(usize::MAX));
    let reads = ReadService::new(
        graph("Number", "UTC"),
        Arc::new(AllowAll),
        ReadLimits {
            max_work: 4096,
            ..ReadLimits::default()
        },
    )
    .unwrap();
    let service = HistoryService::new(
        reads,
        Arc::new(BudgetProbe {
            store,
            work: work.clone(),
        }),
        HistoryLimits {
            total_work: 4096,
            ..HistoryLimits::default()
        },
    )
    .unwrap();
    let result = service.collect(context("a"), request()).await.unwrap();
    assert_eq!(result.terminal, HistoryTerminal::Complete);
    assert!(
        work.load(Ordering::SeqCst) < 4096,
        "metadata already consumed part of the lifetime budget before source copies"
    );
}
struct FixedClock;
impl HistoryClock for FixedClock {
    fn now(&self) -> DateTime<FixedOffset> {
        time("2024-06-02T01:00:00Z")
    }
}
#[tokio::test]
async fn injected_clock_selects_point_local_today_and_preserves_registered_unit_alias() {
    let store = Arc::new(HisStore::new());
    let value = Kind::Number(Number::new(-0.0, Some("celsius".into())));
    store
        .write(
            "p",
            vec![HisItem {
                ts: time("2024-06-01T05:00:00Z"),
                val: value.clone(),
            }],
        )
        .unwrap();
    let service = service(store, graph("Number", "New_York"), HistoryLimits::default())
        .with_clock(Arc::new(FixedClock));
    let result = service
        .collect(
            context("a"),
            HistoryReadRequest {
                id: "p".into(),
                range: "today".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result.metadata.start.dt.to_rfc3339(),
        "2024-06-01T00:00:00-04:00"
    );
    assert_eq!(result.samples[0].val, value);
    let Kind::Number(number) = &result.samples[0].val else {
        panic!()
    };
    assert_eq!(number.val.to_bits(), (-0.0f64).to_bits());
    assert_eq!(number.unit.as_deref(), Some("celsius"));
}
#[tokio::test]
async fn bool_and_string_schemas_preserve_original_values_and_na() {
    for (kind, value) in [
        ("Bool", Kind::Bool(false)),
        ("Str", Kind::Str("working".into())),
    ] {
        let store = Arc::new(HisStore::new());
        store
            .write("p", vec![item(0, value.clone()), item(1, Kind::NA)])
            .unwrap();
        let service = service(store, graph(kind, "UTC"), HistoryLimits::default());
        let result = service.collect(context("a"), request()).await.unwrap();
        assert_eq!(result.terminal, HistoryTerminal::Complete);
        assert_eq!(result.samples[0].val, value);
        assert_eq!(result.samples[1].val, Kind::NA);
    }
}
