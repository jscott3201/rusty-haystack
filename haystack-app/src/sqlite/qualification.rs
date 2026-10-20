//! Native local-file tests use the bundled SQLite runtime, with no provider mocks.
#[path = "history_qualification.rs"]
mod history;

use super::*;
use crate::*;
use haystack_core::{
    codecs::entity::*,
    data::HDict,
    graph::EntityOperation,
    kinds::{HRef, Kind},
};
use std::time::Duration;

pub(crate) enum Fault {
    Constraint,
    PanicBeforeCommit,
    LostAck,
    UnreadableProof,
    BeforeCommit(Arc<Gate>),
    AfterCommit(Arc<Gate>),
    ProcessCrash {
        signal: std::path::PathBuf,
        before: bool,
    },
}
#[derive(Debug, Default)]
pub(crate) struct Gate {
    entered: std::sync::atomic::AtomicBool,
    released: parking_lot::Mutex<bool>,
    condition: parking_lot::Condvar,
    deadline: parking_lot::Mutex<Option<Instant>>,
}
struct ReleaseGate(Arc<Gate>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}
impl Gate {
    pub(crate) fn hold_at(&self, deadline: Instant) {
        *self.deadline.lock() = Some(deadline);
        self.hold();
    }
    fn hold(&self) {
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        let mut released = self.released.lock();
        while !*released {
            self.condition.wait(&mut released);
        }
    }
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.entered.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("native fault entered");
    }
    fn release(&self) {
        *self.released.lock() = true;
        self.condition.notify_all();
    }
}
pub(crate) fn before_commit(
    fault: Option<&Fault>,
    connection: &rusqlite::Connection,
) -> Result<(), SqliteError> {
    match fault {
        Some(Fault::Constraint) => {
            connection
                .execute("INSERT INTO metadata SELECT * FROM metadata", [])
                .map_err(storage::sql_error)?;
        }
        Some(Fault::PanicBeforeCommit) => panic!("intentional native worker panic before COMMIT"),
        Some(Fault::BeforeCommit(gate)) => gate.hold(),
        Some(Fault::ProcessCrash {
            signal,
            before: true,
        }) => crash_boundary(signal),
        _ => {}
    }
    Ok(())
}
pub(crate) fn after_commit(
    fault: Option<&Fault>,
    backend: &storage::Backend,
) -> Result<(), SqliteError> {
    match fault {
        Some(Fault::LostAck) => return Err(SqliteError::Unavailable),
        Some(Fault::UnreadableProof) => {
            backend.seal();
            return Err(SqliteError::Unavailable);
        }
        Some(Fault::AfterCommit(gate)) => gate.hold(),
        Some(Fault::ProcessCrash {
            signal,
            before: false,
        }) => crash_boundary(signal),
        _ => {}
    }
    Ok(())
}
fn crash_boundary(signal: &std::path::Path) {
    std::fs::write(signal, b"native transaction boundary").unwrap();
    loop {
        std::thread::park();
    }
}
async fn wait_for_observation(counter: &std::sync::atomic::AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while counter.load(std::sync::atomic::Ordering::Acquire) < expected {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("native wait boundary was entered");
}
fn policies() -> SqlitePolicies {
    SqlitePolicies {
        reads: Arc::new(AllowAll),
        entities: Arc::new(AllowAllMutations),
        history: Arc::new(AllowAllHistoryMutations),
    }
}
fn context() -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "sqlite-owner".into(),
        },
        Duration::from_secs(10),
    )
}
async fn open(config: SqliteConfig) -> ApplicationOwner {
    let builder = application(
        config,
        Catalog::load_http_pinned().unwrap(),
        policies(),
        SqliteServiceLimits::default(),
    )
    .await
    .expect("SQLite builder");
    assert!(builder.handle().read_service().native_graph().is_none());
    let owner = builder
        .start(&tokio::runtime::Handle::current())
        .expect("start SQLite owner");
    owner.ready().await.expect("SQLite readiness");
    owner
}
async fn close(owner: ApplicationOwner) {
    owner.close().await.expect("close SQLite owner");
    assert!(owner.terminated().await.cleanup_errors.is_empty());
}
fn row(id: &str) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row.set("site", Kind::Marker);
    row.set("explicitNull", Kind::Null);
    row.set("notAvailable", Kind::NA);
    row.set("nothing", Kind::None);
    row
}
fn request(
    service: &MutationService,
    id: &str,
    operations: Vec<EntityOperation>,
) -> EntityBatchRequest {
    let state = service.store().graph().state();
    EntityBatchRequest {
        identity: OperationIdentity {
            dataset: service.store().dataset(),
            incarnation: state.incarnation,
            operation_id: id.into(),
        },
        expected_revision: state.revision,
        operations,
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entity_commit_restart_keeps_receipt_and_feed() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    assert!(owner.handle().history_service().is_some());
    assert!(service.store().native_graph().is_none());
    let cursor = service
        .changes(
            context(),
            ChangesRequest {
                cursor: None,
                max_diffs: 128,
            },
        )
        .await
        .unwrap()
        .cursor;
    let request = request(
        &service,
        "first",
        vec![
            EntityOperation::Add(row("a")),
            EntityOperation::Add(row("b")),
        ],
    );
    let outcome = service.submit(context(), request.clone()).await.unwrap();
    let MutationOutcome::Committed(receipt) = &outcome else {
        panic!("expected SQLite durable commit: {outcome:?}")
    };
    assert_eq!(receipt.qualification, ReceiptQualification::SqliteDurable);
    assert_eq!(receipt.after_revision, 2);
    let state = service.store().graph().state();
    close(owner).await;
    let owner = open(config).await;
    let service = owner.handle().mutation_service().unwrap();
    assert_eq!(service.store().graph().state(), state);
    assert_eq!(service.store().graph().get("a").unwrap(), row("a"));
    assert_eq!(
        service.submit(context(), request.clone()).await.unwrap(),
        outcome
    );
    assert_eq!(
        service
            .reconcile(context(), request.identity)
            .await
            .unwrap(),
        outcome
    );
    let changes = service
        .changes(
            context(),
            ChangesRequest {
                cursor: Some(cursor),
                max_diffs: 128,
            },
        )
        .await
        .unwrap();
    assert_eq!(changes.changes.len(), 2);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crud_expected_revision_principal_binding_and_empty_patch_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let first = request(
        &service,
        "add",
        vec![
            EntityOperation::Add(row("a")),
            EntityOperation::Add(row("b")),
        ],
    );
    let original = service.submit(context(), first.clone()).await.unwrap();
    assert!(matches!(original, MutationOutcome::Committed(_)));
    let mut different = first.clone();
    different.operations = vec![EntityOperation::Add(row("different"))];
    assert!(matches!(
        service.submit(context(), different).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Conflict,
            ..
        }
    ));
    let mut other = context();
    other.principal = Principal::TrustedEmbedding {
        subject: "other-owner".into(),
    };
    assert!(matches!(
        service
            .reconcile(other, first.identity.clone())
            .await
            .unwrap(),
        MutationOutcome::Unknown {
            cause: UnknownCause::Missing,
            ..
        }
    ));
    let mut stale = request(
        &service,
        "stale",
        vec![EntityOperation::Remove { id: "a".into() }],
    );
    stale.expected_revision = 0;
    assert!(matches!(
        service.submit(context(), stale).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Conflict,
            ..
        }
    ));
    let no_op = request(
        &service,
        "noop",
        vec![EntityOperation::Patch {
            id: "a".into(),
            changes: HDict::new(),
        }],
    );
    assert!(matches!(
        service.submit(context(), no_op).await.unwrap(),
        MutationOutcome::Committed(EntityReceipt {
            before_revision: 2,
            after_revision: 2,
            ..
        })
    ));
    let mut changes = HDict::new();
    changes.set("display", Kind::Str("updated".into()));
    changes.set("explicitNull", Kind::Remove);
    let change = request(
        &service,
        "update-remove",
        vec![
            EntityOperation::Patch {
                id: "a".into(),
                changes,
            },
            EntityOperation::Remove { id: "b".into() },
        ],
    );
    assert!(matches!(
        service.submit(context(), change).await.unwrap(),
        MutationOutcome::Committed(EntityReceipt {
            after_revision: 4,
            ..
        })
    ));
    close(owner).await;
    let owner = open(config).await;
    let reads = owner.handle().read_service();
    let row = reads.graph().get("a").unwrap();
    assert_eq!(row.get("display"), Some(&Kind::Str("updated".into())));
    assert!(row.get("explicitNull").is_none());
    assert!(reads.graph().get("b").is_none());
    let service = owner.handle().mutation_service().unwrap();
    assert_eq!(service.submit(context(), first).await.unwrap(), original);
    assert_eq!(reads.graph().version(), 4);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_constraint_rollback_lost_ack_and_unreadable_proof_are_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let backend = service.store().sqlite_backend();
    *backend.fault.lock() = Some(Fault::Constraint);
    let failed = request(
        &service,
        "rollback",
        vec![EntityOperation::Add(row("rolled-back"))],
    );
    let rejected = service.submit(context(), failed.clone()).await.unwrap();
    assert!(matches!(
        rejected,
        MutationOutcome::Rejected {
            reason: RejectionReason::Provider,
            ..
        }
    ));
    assert_eq!(service.store().graph().version(), 0);
    assert!(service.store().graph().is_empty());
    *backend.fault.lock() = Some(Fault::LostAck);
    let lost = request(
        &service,
        "lost",
        vec![EntityOperation::Add(row("committed"))],
    );
    let committed = service.submit(context(), lost.clone()).await.unwrap();
    assert!(matches!(committed, MutationOutcome::Committed(_)));
    assert_eq!(service.store().graph().version(), 1);
    *backend.fault.lock() = Some(Fault::UnreadableProof);
    let uncertain = request(
        &service,
        "unknown",
        vec![EntityOperation::Add(row("restart-proves"))],
    );
    assert!(matches!(
        service.submit(context(), uncertain.clone()).await.unwrap(),
        MutationOutcome::Unknown { .. }
    ));
    assert_eq!(service.store().graph().version(), 1);
    assert!(matches!(
        owner.handle().read_service().begin(context()).await,
        Err(ReadError::Unavailable)
    ));
    close(owner).await;
    let owner = open(config).await;
    let service = owner.handle().mutation_service().unwrap();
    assert_eq!(
        service.reconcile(context(), failed.identity).await.unwrap(),
        rejected
    );
    assert_eq!(service.submit(context(), lost).await.unwrap(), committed);
    let recovered = service
        .reconcile(context(), uncertain.identity.clone())
        .await
        .unwrap();
    assert!(matches!(recovered, MutationOutcome::Committed(_)));
    assert_eq!(
        service.submit(context(), uncertain).await.unwrap(),
        recovered
    );
    assert_eq!(service.store().graph().version(), 2);
    assert!(service.store().graph().get("rolled-back").is_none());
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_receipt_waits_for_projection_and_one_notification() {
    let directory = tempfile::tempdir().unwrap();
    let owner = open(SqliteConfig::new(directory.path().join("entities.db"))).await;
    let service = owner.handle().mutation_service().unwrap();
    let gate = Arc::new(Gate::default());
    let backend = service.store().sqlite_backend();
    let _release = ReleaseGate(gate.clone());
    *backend.fault.lock() = Some(Fault::AfterCommit(gate.clone()));
    let request = request(
        &service,
        "publication",
        vec![EntityOperation::Add(row("a"))],
    );
    let mut notifications = owner.handle().read_service().graph().subscribe();
    // Complete native read verification before the first publisher holds its
    // gate, so these two operations must wait in receipt lookup itself.
    let retry_admission = owner
        .handle()
        .read_service()
        .begin(context())
        .await
        .unwrap();
    let receipt_admission = owner
        .handle()
        .read_service()
        .begin(context())
        .await
        .unwrap();
    let first = tokio::spawn({
        let service = service.clone();
        let request = request.clone();
        async move { service.submit(context(), request).await }
    });
    gate.entered().await;
    let waits = backend
        .publication_waits
        .load(std::sync::atomic::Ordering::Acquire);
    let retry = tokio::spawn({
        let service = service.clone();
        let request = request.clone();
        async move { service.submit_admitted(retry_admission, request).await }
    });
    let receipt = tokio::spawn({
        let service = service.clone();
        let identity = request.identity;
        async move {
            service
                .reconcile_admitted(receipt_admission, identity)
                .await
        }
    });
    wait_for_observation(&backend.publication_waits, waits + 2).await;
    assert!(!retry.is_finished());
    assert!(!receipt.is_finished());
    assert_eq!(service.store().graph().version(), 0);
    assert!(notifications.try_recv().is_err());
    gate.release();
    let result = first.await.unwrap().unwrap();
    assert!(matches!(result, MutationOutcome::Committed(_)));
    assert_eq!(retry.await.unwrap().unwrap(), result);
    assert_eq!(receipt.await.unwrap().unwrap(), result);
    assert_eq!(notifications.recv().await.unwrap(), 1);
    assert!(notifications.try_recv().is_err());
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_native_work_keeps_admission_and_file_owner_until_real_rollback() {
    for deadline in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let config = SqliteConfig::new(directory.path().join("entities.db"));
        let owner = open(config.clone()).await;
        let service = owner.handle().mutation_service().unwrap();
        let reads = owner.handle().read_service();
        let gate = Arc::new(Gate::default());
        let _release = ReleaseGate(gate.clone());
        *service.store().sqlite_backend().fault.lock() = Some(Fault::BeforeCommit(gate.clone()));
        let request = request(
            &service,
            "cancel",
            vec![EntityOperation::Add(row("cancelled"))],
        );
        let mut context = context();
        if deadline {
            context.deadline = Instant::now() + Duration::from_millis(300);
        }
        let cancel = context.cancellation.clone();
        let task = tokio::spawn({
            let service = service.clone();
            async move { service.submit(context, request).await }
        });
        gate.entered().await;
        if !deadline {
            cancel.cancel();
        }
        assert!(matches!(
            task.await.unwrap().unwrap(),
            MutationOutcome::Unknown { .. }
        ));
        assert_eq!(
            reads.load().admitted,
            1,
            "native transaction retains admission after observer leaves"
        );
        assert!(matches!(
            application(
                config.clone(),
                Catalog::load_http_pinned().unwrap(),
                policies(),
                SqliteServiceLimits::default()
            )
            .await,
            Err(SqliteError::AlreadyOwned)
        ));
        let close = owner.close();
        tokio::pin!(close);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut close)
                .await
                .is_err()
        );
        gate.release();
        close.await.unwrap();
        assert!(owner.terminated().await.cleanup_errors.is_empty());
        assert_eq!(reads.load().admitted, 0);
        let owner = open(config).await;
        assert_eq!(owner.handle().read_service().graph().version(), 0);
        assert!(owner.handle().read_service().graph().is_empty());
        close_owner(owner).await;
    }
}
async fn close_owner(owner: ApplicationOwner) {
    close(owner).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_native_writer_contention_is_bounded_and_does_not_consume_revision() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let backend = service.store().sqlite_backend();
    let external = rusqlite::Connection::open(&config.path).unwrap();
    external.execute_batch("BEGIN IMMEDIATE").unwrap();
    for cancelled in [false, true] {
        let request = request(
            &service,
            if cancelled {
                "cancel-lock"
            } else {
                "deadline-lock"
            },
            vec![EntityOperation::Add(row("locked"))],
        );
        let mut context = context();
        context.deadline = Instant::now() + Duration::from_millis(300);
        let cancel = context.cancellation.clone();
        let waits = backend
            .native_busy_waits
            .load(std::sync::atomic::Ordering::Acquire);
        let started = Instant::now();
        let task = tokio::spawn({
            let service = service.clone();
            async move { service.submit(context, request).await }
        });
        wait_for_observation(&backend.native_busy_waits, waits + 1).await;
        if cancelled {
            cancel.cancel();
        }
        assert!(matches!(
            task.await.unwrap().unwrap(),
            MutationOutcome::Unknown { .. }
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        // No previous observer's worker can produce the next iteration's native
        // busy receipt: wait for the actual retained work lease to be released.
        tokio::time::timeout(Duration::from_secs(2), async {
            while owner.handle().read_service().load().admitted != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    external.execute_batch("ROLLBACK").unwrap();
    external.close().unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while owner.handle().read_service().load().admitted != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(service.store().graph().version(), 0);
    let request = request(
        &service,
        "after-lock",
        vec![EntityOperation::Add(row("free"))],
    );
    assert!(matches!(
        service.submit(context(), request).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_lock_aliases_unstarted_builder_and_incompatible_files_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let builder = application(
        config.clone(),
        Catalog::load_http_pinned().unwrap(),
        policies(),
        SqliteServiceLimits::default(),
    )
    .await
    .unwrap();
    assert!(matches!(
        application(
            config.clone(),
            Catalog::load_http_pinned().unwrap(),
            policies(),
            SqliteServiceLimits::default()
        )
        .await,
        Err(SqliteError::AlreadyOwned)
    ));
    #[cfg(unix)]
    {
        let alias = directory.path().join("alias.db");
        std::os::unix::fs::symlink(&config.path, &alias).unwrap();
        assert!(matches!(
            application(
                SqliteConfig::new(alias),
                Catalog::load_http_pinned().unwrap(),
                policies(),
                SqliteServiceLimits::default()
            )
            .await,
            Err(SqliteError::AlreadyOwned)
        ));
    }
    drop(builder);
    let owner = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match application(
                config.clone(),
                Catalog::load_http_pinned().unwrap(),
                policies(),
                SqliteServiceLimits::default(),
            )
            .await
            {
                Ok(builder) => {
                    let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
                    owner.ready().await.unwrap();
                    break owner;
                }
                Err(SqliteError::AlreadyOwned) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(error) => panic!("unexpected reopen: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
    close(owner).await;
    #[cfg(unix)]
    {
        let hardlink = directory.path().join("hard.db");
        std::fs::hard_link(&config.path, &hardlink).unwrap();
        assert!(matches!(
            application(
                SqliteConfig::new(hardlink.clone()),
                Catalog::load_http_pinned().unwrap(),
                policies(),
                SqliteServiceLimits::default()
            )
            .await,
            Err(SqliteError::Configuration)
        ));
        std::fs::remove_file(hardlink).unwrap();
    }
    let connection = rusqlite::Connection::open(&config.path).unwrap();
    connection.execute("DELETE FROM metadata", []).unwrap();
    connection.close().unwrap();
    for _ in 0..2 {
        assert!(matches!(
            application(
                config.clone(),
                Catalog::load_http_pinned().unwrap(),
                policies(),
                SqliteServiceLimits::default()
            )
            .await,
            Err(SqliteError::Incompatible)
        ));
    }
    let connection = rusqlite::Connection::open(&config.path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM metadata", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    connection.close().unwrap();
    let unknown = SqliteConfig::new(directory.path().join("unknown.db"));
    let connection = rusqlite::Connection::open(&unknown.path).unwrap();
    connection
        .execute("CREATE TABLE user_owned(value TEXT)", [])
        .unwrap();
    connection.close().unwrap();
    assert!(matches!(
        application(
            unknown.clone(),
            Catalog::load_http_pinned().unwrap(),
            policies(),
            SqliteServiceLimits::default()
        )
        .await,
        Err(SqliteError::Incompatible)
    ));
    let connection = rusqlite::Connection::open(&unknown.path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    connection.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_payload_and_recovery_limits_never_become_ready() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let request = request(
        &service,
        "rows",
        vec![
            EntityOperation::Add(row("a")),
            EntityOperation::Add(row("b")),
        ],
    );
    assert!(matches!(
        service.submit(context(), request).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    close(owner).await;
    let mut limited = config.clone();
    limited.limits.recovery.max_rows = 1;
    assert!(matches!(
        application(
            limited,
            Catalog::load_http_pinned().unwrap(),
            policies(),
            SqliteServiceLimits::default()
        )
        .await,
        Err(SqliteError::Limit)
    ));
    let connection = rusqlite::Connection::open(&config.path).unwrap();
    connection
        .execute("UPDATE entities SET payload=x'00' WHERE id='a'", [])
        .unwrap();
    connection.close().unwrap();
    assert!(matches!(
        application(
            config,
            Catalog::load_http_pinned().unwrap(),
            policies(),
            SqliteServiceLimits::default()
        )
        .await,
        Err(SqliteError::Incompatible)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_values_keep_bits_units_reference_display_and_nested_distinctions() {
    use haystack_core::{
        data::HGrid,
        kinds::{Coord, Float, HDateTime, NominalScalar, Number, Symbol, Uri, XStr},
    };
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let mut value = row("rich");
    value.set("large", Kind::Int(9_007_199_254_740_993));
    value.set(
        "minusZero",
        Kind::Float(Float::from_bits(0x8000_0000_0000_0000)),
    );
    value.set(
        "nanPayload",
        Kind::Float(Float::from_bits(0x7ff8_0000_0000_0042)),
    );
    value.set("number", Kind::Number(Number::new(-0.0, Some("kW".into()))));
    value.set("location", Kind::Coord(Coord::new(-0.0, 42.0)));
    value.set(
        "ref",
        Kind::Ref(HRef::new("other", Some("Display retained".into()))),
    );
    value.set("buffer", Kind::Buf(vec![0, 255, 128]));
    value.set("flag", Kind::Bool(true));
    value.set("uri", Kind::Uri(Uri::new("https://example.test/value")));
    value.set("symbol", Kind::Symbol(Symbol::new("ph::site")));
    value.set("external", Kind::XStr(XStr::new("Custom", "raw")));
    value.set(
        "date",
        Kind::Date(chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()),
    );
    value.set(
        "time",
        Kind::Time(chrono::NaiveTime::from_hms_nano_opt(12, 34, 56, 123456789).unwrap()),
    );
    let timestamp =
        chrono::DateTime::parse_from_rfc3339("2024-06-01T08:30:00.123456789-04:00").unwrap();
    value.set(
        "when",
        Kind::DateTime(HDateTime::new(timestamp, "New_York")),
    );
    let mut nested = HDict::new();
    nested.set("null", Kind::Null);
    nested.set("none", Kind::None);
    nested.set(
        "serial",
        Kind::Nominal(
            NominalScalar::new("example::Serial", "catalog-A", "sha256:revision", "000042")
                .unwrap(),
        ),
    );
    value.set(
        "nested",
        Kind::List(vec![
            Kind::Dict(Box::new(nested.clone())),
            Kind::NA,
            Kind::Marker,
        ]),
    );
    value.set(
        "grid",
        Kind::Grid(Box::new(HGrid::from_parts(
            HDict::new(),
            vec![],
            vec![nested],
        ))),
    );
    let canonical = crate::storage_value::encode_dict(&value).unwrap();
    let request = request(&service, "rich", vec![EntityOperation::Add(value)]);
    assert!(matches!(
        service.submit(context(), request).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    close(owner).await;
    let owner = open(config).await;
    let value = owner.handle().read_service().graph().get("rich").unwrap();
    assert_eq!(
        crate::storage_value::encode_dict(&value).unwrap(),
        canonical
    );
    assert!(value.get("absent").is_none());
    close(owner).await;
}

#[test]
fn process_crash_worker() {
    let Some(path) = std::env::var_os("RUSTY_HAYSTACK_SQLITE_CRASH_FILE") else {
        return;
    };
    let signal = std::env::var_os("RUSTY_HAYSTACK_SQLITE_CRASH_SIGNAL").unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let owner = open(SqliteConfig::new(path)).await;
            let service = owner.handle().mutation_service().unwrap();
            *service.store().sqlite_backend().fault.lock() = Some(Fault::ProcessCrash {
                signal: signal.into(),
                before: std::env::var_os("RUSTY_HAYSTACK_SQLITE_CRASH_BEFORE").is_some(),
            });
            let request = request(
                &service,
                "process-crash",
                vec![EntityOperation::Add(row("survives"))],
            );
            let _ = service.submit(context(), request).await;
            panic!("parent should terminate the child at the native commit boundary");
        });
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_kill_after_native_commit_recovers_receipt_without_replay() {
    process_kill(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_kill_before_native_commit_keeps_pending_identity_without_effect() {
    process_kill(true).await;
}
async fn process_kill(before: bool) {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let signal = directory.path().join("boundary");
    let owner = open(config.clone()).await;
    let cursor = owner
        .handle()
        .mutation_service()
        .unwrap()
        .changes(
            context(),
            ChangesRequest {
                cursor: None,
                max_diffs: 128,
            },
        )
        .await
        .unwrap()
        .cursor;
    close(owner).await;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "sqlite::qualification::process_crash_worker",
            "--nocapture",
        ])
        .env("RUSTY_HAYSTACK_SQLITE_CRASH_FILE", &config.path)
        .env("RUSTY_HAYSTACK_SQLITE_CRASH_SIGNAL", &signal)
        .env_remove("RUSTY_HAYSTACK_SQLITE_CRASH_BEFORE")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if before {
        command.env("RUSTY_HAYSTACK_SQLITE_CRASH_BEFORE", "1");
    }
    let mut child = command.spawn().unwrap();
    let reached = tokio::time::timeout(Duration::from_secs(15), async {
        while !signal.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before transaction boundary"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    let _ = child.kill();
    let status = child.wait().unwrap();
    reached.expect("child reached transaction boundary before process kill");
    assert!(!status.success());
    let owner = open(config).await;
    let service = owner.handle().mutation_service().unwrap();
    let mut request = request(
        &service,
        "process-crash",
        vec![EntityOperation::Add(row("survives"))],
    );
    request.expected_revision = 0;
    let proof = service
        .reconcile(context(), request.identity.clone())
        .await
        .unwrap();
    if before {
        assert!(matches!(
            proof,
            MutationOutcome::Unknown {
                cause: UnknownCause::Pending,
                ..
            }
        ));
    } else {
        assert!(matches!(
            proof,
            MutationOutcome::Committed(EntityReceipt {
                after_revision: 1,
                ..
            })
        ));
    }
    assert_eq!(service.submit(context(), request).await.unwrap(), proof);
    assert_eq!(service.store().graph().version(), u64::from(!before));
    assert_eq!(service.store().graph().len(), usize::from(!before));
    let changes = service
        .changes(
            context(),
            ChangesRequest {
                cursor: Some(cursor),
                max_diffs: 128,
            },
        )
        .await
        .unwrap();
    assert_eq!(changes.changes.len(), usize::from(!before));
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_engine_identity_and_durability_pragmas_are_verified() {
    let directory = tempfile::tempdir().unwrap();
    let owner = open(SqliteConfig::new(directory.path().join("entities.db"))).await;
    let backend = owner
        .handle()
        .mutation_service()
        .unwrap()
        .store()
        .sqlite_backend();
    let stop = storage::Stop::new(
        Instant::now() + Duration::from_secs(2),
        CancellationToken::new(),
    );
    backend.with_connection(&stop,|connection| {
        storage::verify_durability(connection)?;
        let (version,source):(String,String)=connection.query_row("SELECT sqlite_version(),sqlite_source_id()",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(version,"3.53.2");assert_eq!(source,"2026-06-03 19:12:13 d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24");
        println!("Bundled native SQLite {version}; source_id={source}");
        for name in ["journal_mode","synchronous","foreign_keys","busy_timeout","fullfsync","checkpoint_fullfsync"] {
            let value=connection.pragma_query_value(None,name,|row| match row.get_ref(0)? {rusqlite::types::ValueRef::Integer(value)=>Ok(value.to_string()),rusqlite::types::ValueRef::Text(value)=>Ok(String::from_utf8_lossy(value).into_owned()),_=>panic!("unexpected pragma value")}).unwrap();println!("{name}={value}");
        }
        Ok(())
    }).unwrap();
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abandoned_constructor_and_failed_start_release_physical_owner() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = SqliteConfig::new(directory.path().join("entities.db"));
    close(open(config.clone()).await).await;
    config.limits.operation_timeout = Duration::from_secs(1);
    let external = rusqlite::Connection::open(&config.path).unwrap();
    external.execute_batch("BEGIN IMMEDIATE").unwrap();
    let task = tokio::spawn({
        let config = config.clone();
        async move {
            application(
                config,
                Catalog::load_http_pinned().unwrap(),
                policies(),
                SqliteServiceLimits::default(),
            )
            .await
        }
    });
    // Observe the native owner lock rather than assuming constructor scheduling.
    let mut lock_path = config.path.as_os_str().to_os_string();
    lock_path.push(".owner.lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match lock.try_lock() {
                Ok(()) => {
                    lock.unlock().unwrap();
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Err(std::fs::TryLockError::WouldBlock) => break,
                Err(error) => panic!("owner lock probe: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
    drop(lock);
    task.abort();
    let _ = task.await;
    assert!(matches!(
        application(
            config.clone(),
            Catalog::load_http_pinned().unwrap(),
            policies(),
            SqliteServiceLimits::default()
        )
        .await,
        Err(SqliteError::AlreadyOwned)
    ));
    external.execute_batch("ROLLBACK").unwrap();
    external.close().unwrap();
    let builder = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match application(
                config.clone(),
                Catalog::load_http_pinned().unwrap(),
                policies(),
                SqliteServiceLimits::default(),
            )
            .await
            {
                Ok(builder) => break builder,
                Err(SqliteError::AlreadyOwned) => {
                    tokio::time::sleep(Duration::from_millis(5)).await
                }
                Err(error) => panic!("constructor cleanup: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
    struct FailStart;
    impl ApplicationResource for FailStart {
        fn name(&self) -> &str {
            "sqlite-test-failure"
        }
        fn initialize(&mut self, _: ResourceContext) -> ResourceFuture<'_, ReadyInfo> {
            Box::pin(async {
                Err(ApplicationError::resource(
                    "sqlite-test-failure",
                    "intentional startup failure",
                ))
            })
        }
        fn rollback_start(&mut self) -> ResourceFuture<'_> {
            Box::pin(async { Ok(()) })
        }
        fn close(&mut self) -> ResourceFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }
    let owner = builder
        .owned_resource(FailStart)
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    assert!(owner.ready().await.is_err());
    let termination = owner.terminated().await;
    assert!(termination.cleanup_errors.is_empty());
    close(open(config).await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_worker_panic_rolls_back_and_seals_before_releasing_work() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    *service.store().sqlite_backend().fault.lock() = Some(Fault::PanicBeforeCommit);
    let request = request(
        &service,
        "panic",
        vec![EntityOperation::Add(row("never-committed"))],
    );
    assert!(matches!(
        service.submit(context(), request.clone()).await.unwrap(),
        MutationOutcome::Unknown { .. }
    ));
    assert!(matches!(
        owner.handle().read_service().begin(context()).await,
        Err(ReadError::Unavailable)
    ));
    let external = rusqlite::Connection::open(&config.path).unwrap();
    assert_eq!(
        external
            .query_row("SELECT count(*) FROM entities", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    external.close().unwrap();
    close(owner).await;
    let owner = open(config).await;
    let service = owner.handle().mutation_service().unwrap();
    assert_eq!(service.store().graph().version(), 0);
    assert!(matches!(
        service.submit(context(), request).await.unwrap(),
        MutationOutcome::Unknown {
            cause: UnknownCause::Pending,
            ..
        }
    ));
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_progress_interrupt_is_scoped_and_next_command_remains_usable() {
    let directory = tempfile::tempdir().unwrap();
    let owner = open(SqliteConfig::new(directory.path().join("entities.db"))).await;
    let backend = owner
        .handle()
        .mutation_service()
        .unwrap()
        .store()
        .sqlite_backend();
    for cancellation in [false, true] {
        let token = CancellationToken::new();
        let stop = storage::Stop::new(Instant::now() + Duration::from_millis(100), token.clone());
        let (entered, ready) = tokio::sync::oneshot::channel();
        let started = Instant::now();
        let native = tokio::task::spawn_blocking({
            let backend = backend.clone();
            move || {
                backend.read(&stop,|connection| {let _=entered.send(());connection.query_row("WITH RECURSIVE sequence(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM sequence WHERE n<100000000) SELECT sum(n) FROM sequence",[],|row|row.get::<_,i64>(0)).map_err(storage::sql_error)})
            }
        });
        ready.await.unwrap();
        if cancellation {
            token.cancel();
        }
        assert!(native.await.unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        let stop = storage::Stop::new(
            Instant::now() + Duration::from_secs(2),
            CancellationToken::new(),
        );
        assert_eq!(
            backend
                .read(&stop, |connection| connection
                    .query_row("SELECT 42", [], |row| row.get::<_, i64>(0))
                    .map_err(storage::sql_error))
                .unwrap(),
            42
        );
        backend.verify(&stop).unwrap();
    }
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsigned_revision_above_signed_range_commits_exactly_and_overflow_rejects() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    close(open(config.clone()).await).await;
    // Build a valid retained-history-floor fixture while no application owns it.
    let connection = rusqlite::Connection::open(&config.path).unwrap();
    let before = u64::MAX - 1;
    connection
        .execute(
            "UPDATE metadata SET revision=?1,floor=?1",
            [before.to_be_bytes().as_slice()],
        )
        .unwrap();
    connection.close().unwrap();
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    assert_eq!(service.store().graph().version(), before);
    let request = request(&service, "last", vec![EntityOperation::Add(row("last"))]);
    let outcome = service.submit(context(), request.clone()).await.unwrap();
    assert!(
        matches!(outcome,MutationOutcome::Committed(EntityReceipt {before_revision,after_revision:u64::MAX,..}) if before_revision==before)
    );
    close(owner).await;
    let owner = open(config).await;
    let service = owner.handle().mutation_service().unwrap();
    assert_eq!(service.store().graph().version(), u64::MAX);
    assert_eq!(service.submit(context(), request).await.unwrap(), outcome);
    let overflow = super::qualification::request(
        &service,
        "overflow",
        vec![EntityOperation::Add(row("too-far"))],
    );
    assert!(matches!(
        service.submit(context(), overflow).await.unwrap(),
        MutationOutcome::Rejected { .. }
    ));
    assert!(service.store().graph().get("too-far").is_none());
    assert_eq!(service.store().graph().version(), u64::MAX);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_r1_unknown_sqlitex_table_is_rejected_without_adoption_or_journal_change() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("unrelated.db"));
    let connection = rusqlite::Connection::open(&config.path).unwrap();
    connection
        .execute_batch("CREATE TABLE sqlitex(value TEXT); INSERT INTO sqlitex VALUES('user-owned')")
        .unwrap();
    connection.close().unwrap();
    let result = application(
        config.clone(),
        Catalog::load_http_pinned().unwrap(),
        policies(),
        SqliteServiceLimits::default(),
    )
    .await;
    let rejected = matches!(result, Err(SqliteError::Incompatible));
    drop(result);
    assert!(
        rejected,
        "legal user object sqlitex must not be mistaken for SQLite's sqlite_ prefix"
    );
    let connection = rusqlite::Connection::open(&config.path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT value FROM sqlitex", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "user-owned"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='table'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    connection.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_r1_extra_sqlitex_trigger_is_rejected_without_changing_authority() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let identity = owner.handle().mutation_service().unwrap().store().dataset();
    close(owner).await;
    let connection = rusqlite::Connection::open(&config.path).unwrap();
    connection
        .execute_batch("CREATE TRIGGER sqlitex AFTER INSERT ON entities BEGIN SELECT 1; END;")
        .unwrap();
    connection.close().unwrap();
    let result = application(
        config.clone(),
        Catalog::load_http_pinned().unwrap(),
        policies(),
        SqliteServiceLimits::default(),
    )
    .await;
    let rejected = matches!(result, Err(SqliteError::Incompatible));
    drop(result);
    assert!(
        rejected,
        "a trigger with a legal sqlitex name must be included in schema validation"
    );
    let connection = rusqlite::Connection::open(&config.path).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT dataset FROM metadata", [], |row| row
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        identity
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name='sqlitex'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM entities", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    connection.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_r1_tight_receipt_limit_keeps_pending_rejection_and_reopen_bounded() {
    use haystack_core::codecs::typed;
    let directory = tempfile::tempdir().unwrap();
    let mut config = SqliteConfig::new(directory.path().join("entities.db"));
    let owner = open(config.clone()).await;
    let request = request(
        &owner.handle().mutation_service().unwrap(),
        "tight-cap",
        vec![EntityOperation::Add(row("unwritten"))],
    );
    close(owner).await;
    let key = typed::encode(&Kind::List(vec![
        Kind::Str("embedding".into()),
        Kind::Str("sqlite-owner".into()),
        request.identity.to_kind().unwrap(),
    ]))
    .unwrap();
    let pending = MutationOutcome::Unknown {
        identity: request.identity.clone(),
        cause: UnknownCause::Pending,
    };
    let encoded_pending = haystack_core::codecs::entity::encode(&pending).unwrap();
    let rejected = MutationOutcome::Rejected {
        identity: request.identity.clone(),
        reason: RejectionReason::Cancelled,
    };
    assert_eq!(
        haystack_core::codecs::entity::encode(&rejected)
            .unwrap()
            .len(),
        encoded_pending.len() + 4,
        "the independently encoded rejected envelope grows beyond Pending"
    );
    config.limits.max_receipt_bytes = key.len()
        + haystack_core::codecs::entity::encode(&request)
            .unwrap()
            .len()
        + encoded_pending.len();
    struct RejectAfterReservation {
        path: std::path::PathBuf,
        bound: usize,
        entered: Arc<std::sync::atomic::AtomicBool>,
    }
    impl MutationProvider for RejectAfterReservation {
        fn qualification(&self) -> ReceiptQualification {
            ReceiptQualification::SqliteDurable
        }
        fn commit(&self, prepared: PreparedMutation) -> MutationOutcome {
            let native = rusqlite::Connection::open(&self.path).unwrap();
            let (bytes, outcome): (i64, Vec<u8>) = native
                .query_row(
                    "SELECT length(binding)+length(request)+length(outcome),outcome FROM receipts",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(bytes as usize, self.bound);
            assert!(matches!(
                haystack_core::codecs::entity::decode::<MutationOutcome>(&outcome).unwrap(),
                MutationOutcome::Unknown {
                    cause: UnknownCause::Pending,
                    ..
                }
            ));
            native.close().unwrap();
            self.entered
                .store(true, std::sync::atomic::Ordering::Release);
            prepared.reject(RejectionReason::Cancelled)
        }
    }
    let owner = open(config.clone()).await;
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let selected = owner.handle().mutation_service().unwrap();
    let service = MutationService::with_provider(
        owner.handle().read_service(),
        selected.store(),
        Arc::new(AllowAllMutations),
        MutationLimits::default(),
        Arc::new(RejectAfterReservation {
            path: config.path.clone(),
            bound: config.limits.max_receipt_bytes,
            entered: entered.clone(),
        }),
    )
    .unwrap();
    let result = service.submit(context(), request.clone()).await.unwrap();
    assert!(
        entered.load(std::sync::atomic::Ordering::Acquire),
        "test reached durable Pending before any commit-capacity check"
    );
    let native = rusqlite::Connection::open(&config.path).unwrap();
    let bytes: i64 = native
        .query_row(
            "SELECT coalesce(sum(length(binding)+length(request)+length(outcome)),0) FROM receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    native.close().unwrap();
    assert!(
        bytes as usize <= config.limits.max_receipt_bytes,
        "settlement persisted {bytes} bytes above cap {}",
        config.limits.max_receipt_bytes
    );
    assert_eq!(result, pending);
    assert_eq!(selected.store().graph().version(), 0);
    close(owner).await;
    let owner = open(config).await;
    let service = owner.handle().mutation_service().unwrap();
    assert_eq!(
        service
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        pending
    );
    assert_eq!(service.submit(context(), request).await.unwrap(), pending);
    assert_eq!(service.store().graph().version(), 0);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_r1_admitted_read_continues_during_graceful_drain_with_mutations_fenced() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("entities.db"));
    let builder = application(
        config,
        Catalog::load_http_pinned().unwrap(),
        policies(),
        SqliteServiceLimits::default(),
    )
    .await
    .unwrap()
    .shutdown_policy(ShutdownPolicy {
        drain_timeout: Duration::from_secs(10),
        stop_timeout: Duration::from_secs(2),
    });
    let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    let handle = owner.handle();
    let reads = handle.read_service();
    let read_guard = handle.admit().unwrap();
    let mutation_guard = handle.admit().unwrap();
    owner.request_close();
    assert!(handle.closing().is_cancelled());
    assert!(!handle.cancellation().is_cancelled());
    assert!(handle.admit().is_err());
    assert!(reads.begin(context()).await.is_err());
    let admission = reads
        .begin_admitted(context(), read_guard)
        .await
        .expect("pre-admitted read retains its native verification during drain");
    let page = admission
        .read(ReadRequest::new(
            ReadQuery::Ids(vec!["absent".into()]),
            OutputProfile::Typed,
        ))
        .await
        .unwrap();
    assert_eq!(page.row_count, 0);
    let admission = reads
        .begin_admitted(context(), mutation_guard)
        .await
        .unwrap();
    let service = handle.mutation_service().unwrap();
    let request = request(
        &service,
        "sealed-mutation",
        vec![EntityOperation::Add(row("must-not-write"))],
    );
    assert!(!matches!(
        service.submit_admitted(admission, request).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    assert_eq!(reads.graph().version(), 0);
    assert!(!owner.close().await.unwrap().drain_expired);
    assert!(owner.terminated().await.cleanup_errors.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_r1_final_recovery_cpu_phase_cannot_deliver_success_after_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = SqliteConfig::new(directory.path().join("entities.db"));
    let gate = Arc::new(Gate::default());
    let _release = ReleaseGate(gate.clone());
    config.recovery_end = Some(gate.clone());
    let construction = tokio::spawn({
        let config = config.clone();
        async move {
            application(
                config,
                Catalog::load_http_pinned().unwrap(),
                policies(),
                SqliteServiceLimits::default(),
            )
            .await
        }
    });
    gate.entered().await;
    let deadline = gate.deadline.lock().unwrap();
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    assert!(Instant::now() >= deadline);
    assert!(!construction.is_finished());
    gate.release();
    let result = construction.await.unwrap();
    let expired = matches!(result, Err(SqliteError::Deadline));
    drop(result);
    assert!(
        expired,
        "native recovery deadline must also cover final Rust construction and delivery"
    );
    config.recovery_end = None;
    close(open(config).await).await;
}
