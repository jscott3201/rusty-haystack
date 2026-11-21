//! Explicit ignored runtime tests. The selected-profile runner must request
//! these tests and provide its isolated fixture; missing configuration fails.
use super::*;
use crate::*;
use haystack_core::{
    codecs::entity::*,
    data::HDict,
    graph::EntityOperation,
    kinds::{HRef, Kind},
};
use sqlx::Connection;
use std::time::Duration;

fn fixture(label: &str) -> PostgresConfig {
    let path = std::env::var_os("RUSTY_HAYSTACK_POSTGRES_FIXTURE")
        .expect("PostgreSQL qualification requires RUSTY_HAYSTACK_POSTGRES_FIXTURE");
    let bytes = std::fs::read(path).expect("read isolated PostgreSQL fixture configuration");
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("valid fixture configuration");
    let field = |name: &str| {
        value[name]
            .as_str()
            .expect("missing fixture field")
            .to_owned()
    };
    let ca = std::fs::read(field("root_certificate")).expect("read fixture CA");
    PostgresConfig {
        host: field("tls_server_name"),
        port: value["port"].as_u64().expect("fixture port") as u16,
        database: field("database"),
        username: field("user"),
        password: Some(field("password")),
        schema: format!("rh_{label}_{:016x}", rand::random::<u64>()),
        transport: PostgresTransport::VerifyFull {
            additional_ca_pem: Some(ca),
        },
        limits: PostgresLimits::default(),
    }
}
fn policies() -> PostgresPolicies {
    PostgresPolicies {
        reads: Arc::new(AllowAll),
        entities: Arc::new(AllowAllMutations),
        history: Arc::new(AllowAllHistoryMutations),
    }
}
fn context() -> ReadContext {
    ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "qualified-owner".into(),
        },
        Duration::from_secs(10),
    )
}
async fn open(config: PostgresConfig) -> ApplicationOwner {
    open_with_limits(config, PostgresServiceLimits::default()).await
}
async fn open_with_limits(
    config: PostgresConfig,
    limits: PostgresServiceLimits,
) -> ApplicationOwner {
    let builder = application(
        config,
        Catalog::load_http_pinned().unwrap(),
        policies(),
        limits,
    )
    .await
    .expect("durable builder");
    assert!(builder.handle().read_service().native_graph().is_none());
    let owner = builder
        .start(&tokio::runtime::Handle::current())
        .expect("start durable owner");
    owner.ready().await.expect("durable readiness");
    owner
}
async fn close(owner: ApplicationOwner) {
    owner.close().await.expect("close durable owner");
    let termination = owner.terminated().await;
    assert!(termination.cleanup_errors.is_empty());
}
async fn cleanup(config: &PostgresConfig) {
    let mut connection = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .expect("fixture cleanup connection");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA \"{}\" CASCADE",
        config.schema
    )))
    .execute(&mut connection)
    .await
    .expect("remove task-owned test schema");
    connection.close().await.expect("close cleanup connection");
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn entity_commit_restart_keeps_receipt_and_feed() {
    let config = fixture("entities");
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let original = service.store().graph().state();
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
    let request = EntityBatchRequest {
        identity: OperationIdentity {
            dataset: service.store().dataset(),
            incarnation: original.incarnation,
            operation_id: "first-batch".into(),
        },
        expected_revision: original.revision,
        operations: vec![
            EntityOperation::Add(row("a")),
            EntityOperation::Add(row("b")),
        ],
    };
    let outcome = service.submit(context(), request.clone()).await.unwrap();
    let MutationOutcome::Committed(receipt) = &outcome else {
        panic!("expected durable commit: {outcome:?}")
    };
    assert_eq!(receipt.qualification, ReceiptQualification::PostgresDurable);
    assert_eq!(receipt.after_revision, original.revision + 2);
    assert!(service.store().native_graph().is_none());
    let state = service.store().graph().state();
    close(owner).await;
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    assert_eq!(service.store().graph().state(), state);
    let recovered = service.store().graph().get("a").unwrap();
    assert!(matches!(recovered.get("explicitNull"), Some(Kind::Null)));
    assert!(recovered.get("absent").is_none());
    assert_eq!(
        service.submit(context(), request.clone()).await.unwrap(),
        outcome
    );
    assert_eq!(
        service
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        outcome
    );
    let mut conflict = request.clone();
    conflict.operations = vec![EntityOperation::Add(row("different"))];
    assert!(matches!(
        service.submit(context(), conflict).await.unwrap(),
        MutationOutcome::Rejected {
            reason: RejectionReason::Conflict,
            ..
        }
    ));
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
    assert_eq!(changes.position, state.revision);
    assert_eq!(service.store().graph().len(), 2);
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn durable_application_selects_same_history_authority() {
    let config = fixture("history_selection");
    let owner = open(config.clone()).await;
    let handle = owner.handle();
    let history = handle.history_service();
    let mutations = handle.history_mutation_service();
    close(owner).await;
    cleanup(&config).await;
    assert!(
        history.is_some(),
        "durable application must select point history"
    );
    assert!(
        mutations.is_some(),
        "durable application must select authorized history writes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn history_commit_restart_preserves_authority_samples_and_original_intent() {
    use haystack_core::kinds::{HDateTime, Number};
    let config = fixture("history");
    let owner = open(config.clone()).await;
    let entities = owner.handle().mutation_service().unwrap();
    let mut point = HDict::new();
    point.set("id", Kind::Ref(HRef::from_val("p")));
    point.set("his", Kind::Marker);
    point.set("kind", Kind::Str("Number".into()));
    point.set("tz", Kind::Str("New_York".into()));
    point.set("unit", Kind::Str("°F".into()));
    let graph = entities.store().graph().state();
    assert!(matches!(
        entities
            .submit(
                context(),
                EntityBatchRequest {
                    identity: OperationIdentity {
                        dataset: entities.store().dataset(),
                        incarnation: graph.incarnation,
                        operation_id: "point".into()
                    },
                    expected_revision: graph.revision,
                    operations: vec![EntityOperation::Add(point)]
                }
            )
            .await
            .unwrap(),
        MutationOutcome::Committed(_)
    ));
    let history = owner.handle().history_mutation_service().unwrap();
    let initial = history.store().state("p").unwrap();
    assert_eq!(
        history.store().write("p", vec![]),
        Err(HistoryProviderError::Unsupported)
    );
    assert_eq!(
        history.store().reset_point("p"),
        Err(HistoryProviderError::Unsupported)
    );
    let ts = chrono::DateTime::parse_from_rfc3339("2024-06-01T08:30:00.123456789-04:00").unwrap();
    let request = HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: initial.authority,
            incarnation: initial.incarnation,
            point: "p".into(),
            operation_id: "samples".into(),
        },
        expected_generation: initial.generation,
        samples: vec![
            HistorySample {
                ts: HDateTime::new(ts, "New_York"),
                val: Kind::Number(Number::unitless(1.25)),
            },
            HistorySample {
                ts: HDateTime::new(ts, "New_York"),
                val: Kind::Number(Number::unitless(2.5)),
            },
            HistorySample {
                ts: HDateTime::new(ts + chrono::Duration::nanoseconds(1), "New_York"),
                val: Kind::NA,
            },
        ],
    };
    *owner
        .handle()
        .read_service()
        .postgres_backend()
        .fault
        .lock() = Some(super::storage::CommitFault::LoseAcknowledgment);
    let outcome = history.submit(context(), request.clone()).await.unwrap();
    let HistoryWriteOutcome::Committed(receipt) = &outcome else {
        panic!("expected durable history commit: {outcome:?}")
    };
    assert_eq!(
        receipt.qualification,
        HistoryReceiptQualification::PostgresDurable
    );
    assert_eq!(receipt.submitted_samples, 3);
    assert_eq!(receipt.unique_samples, 2);
    let state = history.store().state("p").unwrap();
    let read_request = HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01".into(),
    };
    let first = history
        .history_service()
        .collect(context(), read_request.clone())
        .await
        .unwrap();
    assert_eq!(first.samples.len(), 2);
    assert_eq!(first.samples[0].ts.dt.timestamp_subsec_nanos(), 123456789);
    assert_eq!(first.samples[0].ts.tz_name, "New_York");
    let changes = history.store().retained_changes();
    let mut failed = request.clone();
    failed.identity.operation_id = "rolled-back-samples".into();
    failed.expected_generation = state.generation;
    *owner
        .handle()
        .read_service()
        .postgres_backend()
        .fault
        .lock() = Some(super::storage::CommitFault::TerminateBeforeCommit);
    assert!(matches!(
        history.submit(context(), failed).await.unwrap(),
        HistoryWriteOutcome::Rejected {
            reason: HistoryWriteRejection::Provider,
            ..
        }
    ));
    assert_eq!(history.store().state("p").unwrap(), state);
    assert_eq!(history.store().retained_changes(), changes);
    close(owner).await;
    let owner = open(config.clone()).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(history.store().state("p").unwrap(), state);
    assert_eq!(history.store().retained_changes(), changes);
    assert_eq!(
        history.submit(context(), request.clone()).await.unwrap(),
        outcome
    );
    assert_eq!(
        history
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        outcome
    );
    let mut different = request.clone();
    different.samples[0].val = Kind::Number(Number::unitless(99.0));
    assert!(matches!(
        history.submit(context(), different).await.unwrap(),
        HistoryWriteOutcome::Rejected {
            reason: HistoryWriteRejection::Conflict,
            ..
        }
    ));
    let mut offset_only = request.clone();
    offset_only.samples[0].ts.dt = offset_only.samples[0]
        .ts
        .dt
        .with_timezone(&chrono::FixedOffset::east_opt(0).unwrap());
    assert!(matches!(
        history.submit(context(), offset_only).await.unwrap(),
        HistoryWriteOutcome::Rejected {
            reason: HistoryWriteRejection::Conflict,
            ..
        }
    ));
    let recovered = history
        .history_service()
        .collect(context(), read_request)
        .await
        .unwrap();
    assert_eq!(recovered.samples, first.samples);
    assert_eq!(recovered.metadata.history, first.metadata.history);
    assert_eq!(
        recovered.metadata.capabilities,
        HistoryCapabilities::BOUNDED_LIVE
    );
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn history_retention_watermark_and_sequence_survive_restart() {
    use haystack_core::kinds::{HDateTime, Number};
    let config = fixture("retention");
    let mut limits = PostgresServiceLimits::default();
    limits.history_store.max_items_per_point = 2;
    let owner = open_with_limits(config.clone(), limits.clone()).await;
    let mut point = HDict::new();
    point.set("id", Kind::Ref(HRef::from_val("p")));
    point.set("his", Kind::Marker);
    point.set("kind", Kind::Str("Number".into()));
    point.set("tz", Kind::Str("UTC".into()));
    point.set("unit", Kind::Str("°F".into()));
    assert!(matches!(
        entity_batch(
            &owner.handle().mutation_service().unwrap(),
            "point",
            vec![EntityOperation::Add(point)]
        )
        .await,
        MutationOutcome::Committed(_)
    ));
    let history = owner.handle().history_mutation_service().unwrap();
    let initial = history.store().state("p").unwrap();
    let ts = chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00+00:00").unwrap();
    let request = HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: initial.authority,
            incarnation: initial.incarnation,
            point: "p".into(),
            operation_id: "retained-samples".into(),
        },
        expected_generation: initial.generation,
        samples: (0..3)
            .map(|i| HistorySample {
                ts: HDateTime::new(ts + chrono::Duration::seconds(i), "UTC"),
                val: Kind::Number(Number::unitless(i as f64)),
            })
            .collect(),
    };
    let committed = history.submit(context(), request.clone()).await.unwrap();
    assert!(
        matches!(committed, HistoryWriteOutcome::Committed(_)),
        "retention write: {committed:?}"
    );
    let state = history.store().state("p").unwrap();
    let changes = history.store().retained_changes();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].sequence, 1);
    assert_eq!(changes[0].retained_samples, 2);
    assert_eq!(changes[0].evicted_samples, 1);
    assert_eq!(changes[0].evicted_through, Some(ts));
    close(owner).await;
    let owner = open_with_limits(config.clone(), limits).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(history.store().state("p").unwrap(), state);
    assert_eq!(history.store().retained_changes(), changes);
    assert_eq!(history.submit(context(), request).await.unwrap(), committed);
    let read = history
        .history_service()
        .collect(
            context(),
            HistoryReadRequest {
                id: "p".into(),
                range: "2024-06-01".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(read.samples.len(), 2);
    assert_eq!(read.samples[0].ts.dt, ts + chrono::Duration::seconds(1));
    assert_eq!(
        read.metadata.coverage.evicted_through.map(|value| value.dt),
        Some(ts)
    );
    close(owner).await;
    cleanup(&config).await;
}

async fn entity_batch(
    service: &MutationService,
    id: &str,
    operations: Vec<EntityOperation>,
) -> MutationOutcome {
    let state = service.store().graph().state();
    service
        .submit(
            context(),
            EntityBatchRequest {
                identity: OperationIdentity {
                    dataset: service.store().dataset(),
                    incarnation: state.incarnation,
                    operation_id: id.into(),
                },
                expected_revision: state.revision,
                operations,
            },
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn reservation_hit_reauthorizes_retained_intent_before_outcome_or_conflict() {
    use haystack_core::graph::PreparedChange;
    use parking_lot::Mutex;
    struct DenyReconcile(Mutex<Vec<Vec<u8>>>);
    impl MutationPolicy for DenyReconcile {
        fn authorize_intent(&self, _: &Principal, _: &EntityBatchRequest) -> bool {
            true
        }
        fn authorize_change(&self, _: &Principal, _: &PreparedChange) -> bool {
            true
        }
        fn authorize_reconcile(
            &self,
            _: &Principal,
            _: &OperationIdentity,
            original: Option<&EntityBatchRequest>,
        ) -> bool {
            self.0.lock().push(
                haystack_core::codecs::entity::encode(original.expect("retained original intent"))
                    .unwrap(),
            );
            false
        }
    }
    let config = fixture("reservation_policy");
    let owner = open(config.clone()).await;
    let reads = owner.handle().read_service();
    let backend = reads.postgres_backend();
    let service = owner.handle().mutation_service().unwrap();
    let policy = Arc::new(DenyReconcile(Mutex::new(vec![])));
    service.replace_policy(policy.clone());
    let state = reads.graph().state();
    for changed in [false, true] {
        let original = EntityBatchRequest {
            identity: OperationIdentity {
                dataset: service.store().dataset(),
                incarnation: state.incarnation,
                operation_id: format!("reservation-{changed}"),
            },
            expected_revision: state.revision,
            operations: vec![EntityOperation::Add(row("original"))],
        };
        let canonical = haystack_core::codecs::entity::encode(&original).unwrap();
        let pending = MutationOutcome::Unknown {
            identity: original.identity.clone(),
            cause: UnknownCause::Pending,
        };
        let pending_bytes = haystack_core::codecs::entity::encode(&pending).unwrap();
        *backend.reservation_seed.lock() = Some(super::storage::StoredReceipt {
            request: canonical.clone(),
            outcome: pending_bytes.clone(),
        });
        let mut submitted = original.clone();
        if changed {
            submitted.operations = vec![EntityOperation::Add(row("different"))];
        }
        let outcome = service.submit(context(), submitted).await.unwrap();
        assert!(
            backend.reservation_seed.lock().is_none(),
            "must reach reservation after initial absence and memory miss"
        );
        let retained = backend.receipt_rows(1).await.unwrap();
        use sqlx::Row;
        assert!(
            retained
                .iter()
                .any(|record| record.get::<Vec<u8>, _>("request") == canonical
                    && record.get::<Vec<u8>, _>("outcome") == pending_bytes)
        );
        assert_eq!(reads.graph().state(), state);
        assert!(reads.graph().read(|graph| graph.is_empty()));
        assert!(
            matches!(
                outcome,
                MutationOutcome::Rejected {
                    reason: RejectionReason::Forbidden,
                    ..
                }
            ),
            "retained outcome or intent conflict must remain unauthorized: {outcome:?}"
        );
        assert_eq!(policy.0.lock().last(), Some(&canonical));
    }
    assert_eq!(policy.0.lock().len(), 2);
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn snapshot_pages_and_reference_traversal_share_sql_view_and_release_capacity() {
    let mut config = fixture("snapshot");
    // This case accounts for snapshot-owned resources exactly. The separate
    // periodic ownership case below exercises overlapping refresh connections.
    config.limits.refresh_interval = Duration::from_secs(60);
    let owner = open(config.clone()).await;
    let handle = owner.handle();
    let entities = handle.mutation_service().unwrap();
    let reads = handle.read_service();
    let backend = reads.postgres_backend();
    let mut target = row("target");
    target.set("dis", Kind::Str("before".into()));
    let mut a = row("a");
    a.set("siteRef", Kind::Ref(HRef::from_val("target")));
    let mut b = row("b");
    b.set("siteRef", Kind::Ref(HRef::from_val("target")));
    assert!(matches!(
        entity_batch(
            &entities,
            "seed",
            vec![
                EntityOperation::Add(a),
                EntityOperation::Add(b),
                EntityOperation::Add(target)
            ]
        )
        .await,
        MutationOutcome::Committed(_)
    ));
    let mut snapshot = reads.postgres_snapshot(context()).await.unwrap();
    let snapshot_state = snapshot.state();
    assert_eq!(reads.load().admitted, 1);
    assert_eq!(backend.pool.size() as usize - backend.pool.num_idle(), 1);
    let mut request = ReadRequest::new(
        ReadQuery::Filter("siteRef->dis == \"before\"".into()),
        OutputProfile::Typed,
    );
    request.page_size = 1;
    let first = snapshot.read(request.clone()).await.unwrap();
    assert_eq!(first.row_count, 1);
    assert!(!first.complete);
    let mut change = HDict::new();
    change.set("dis", Kind::Str("after".into()));
    assert!(matches!(
        entity_batch(
            &entities,
            "update",
            vec![EntityOperation::Patch {
                id: "target".into(),
                changes: change
            }]
        )
        .await,
        MutationOutcome::Committed(_)
    ));
    assert!(reads.graph().version() > snapshot_state.revision);
    request.cursor = first.cursor;
    let second = snapshot.read(request).await.unwrap();
    assert_eq!(second.row_count, 1);
    assert!(second.complete);
    let live = reads
        .read(
            context(),
            ReadRequest::new(
                ReadQuery::Filter("siteRef->dis == \"before\"".into()),
                OutputProfile::Typed,
            ),
        )
        .await
        .unwrap();
    assert_eq!(live.row_count, 0);
    snapshot.close().await.unwrap();
    assert_eq!(
        server_sessions(&config).await.1,
        0,
        "closed snapshot left no server transaction"
    );
    assert_eq!(reads.load().admitted, 0);
    assert_eq!(backend.pool.size() as usize, backend.pool.num_idle());
    let snapshot = reads.postgres_snapshot(context()).await.unwrap();
    assert_eq!(reads.load().admitted, 1);
    drop(snapshot);
    tokio::time::timeout(Duration::from_secs(3), async {
        while reads.load().admitted != 0 || backend.pool.size() as usize != backend.pool.num_idle()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("dropped snapshot physically closed and released admission");
    assert_eq!(
        server_sessions(&config).await.1,
        0,
        "dropped snapshot left no server transaction"
    );
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn periodic_refresh_owns_a_separate_connection_and_joins_before_owner_close() {
    let mut config = fixture("snapshot_periodic_owner");
    config.limits.refresh_interval = Duration::from_millis(100);
    let owner = open(config.clone()).await;
    let reads = owner.handle().read_service();
    let backend = reads.postgres_backend();
    let snapshot = reads.postgres_snapshot(context()).await.unwrap();
    let (entered, periodic_started) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    *backend.refresh_fault.lock() = Some(super::storage::RefreshFault {
        stage: super::storage::RefreshStage::AfterEntities,
        entered,
        release: released,
    });
    // No admission or explicit health call triggers this capture. The actual
    // application-owned timer starts it and the barrier retains its SQL cut.
    tokio::time::timeout(Duration::from_secs(3), periodic_started)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        reads.load().admitted,
        1,
        "only the snapshot owns read admission"
    );
    assert_eq!(backend.pool.size() as usize - backend.pool.num_idle(), 2);
    assert_eq!(server_sessions(&config).await, (2, 2));
    snapshot.close().await.unwrap();
    assert_eq!(reads.load().admitted, 0);
    assert_eq!(backend.pool.size() as usize - backend.pool.num_idle(), 1);
    assert_eq!(server_sessions(&config).await, (1, 1));
    let mut closing = tokio::spawn(close(owner));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut closing)
            .await
            .is_err(),
        "owner close must join the actual periodic capture before closing its pool"
    );
    assert_eq!(backend.pool.size() as usize - backend.pool.num_idle(), 1);
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), closing)
        .await
        .unwrap()
        .unwrap();
    assert!(backend.pool.is_closed());
    assert_eq!(backend.pool.size(), 0);
    assert_eq!(reads.load().admitted, 0);
    assert_eq!(server_sessions(&config).await, (0, 0));
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn exhausted_pool_cancelled_and_expired_waiters_release_actual_ownership() {
    use std::sync::atomic::Ordering;
    let mut config = fixture("pool_waiters");
    config.limits.refresh_interval = Duration::from_secs(60);
    config.limits.pool_connections = 1;
    let owner = open(config.clone()).await;
    let reads = owner.handle().read_service();
    let backend = reads.postgres_backend();
    assert!(matches!(
        entity_batch(
            &owner.handle().mutation_service().unwrap(),
            "seed",
            vec![EntityOperation::Add(row("a"))]
        )
        .await,
        MutationOutcome::Committed(_)
    ));
    for expire in [false, true] {
        let mut holder = reads.postgres_snapshot(context()).await.unwrap();
        assert_eq!(backend.pool.size(), 1);
        assert_eq!(backend.pool.num_idle(), 0);
        let mut waiter_context = context();
        if expire {
            waiter_context.deadline = std::time::Instant::now() + Duration::from_millis(500);
        }
        let cancel = waiter_context.cancellation.clone();
        let waiter = tokio::spawn({
            let reads = reads.clone();
            async move { reads.postgres_snapshot(waiter_context).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while backend.snapshot_acquiring.load(Ordering::SeqCst) != 1 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("second snapshot reached actual pool acquisition");
        assert!(!waiter.is_finished());
        assert_eq!(reads.load().admitted, 2);
        assert_eq!(
            server_sessions(&config).await.1,
            1,
            "only the holder has a SQL transaction"
        );
        if !expire {
            cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        if expire {
            assert!(matches!(result, Err(ReadError::Deadline)));
            tokio::time::timeout(Duration::from_secs(2), async {
                while backend.snapshot_acquiring.load(Ordering::SeqCst) != 0
                    || reads.load().admitted != 1
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("expired acquisition releases its original work lease");
        } else {
            assert!(matches!(result, Err(ReadError::Cancelled)));
            // Caller cancellation does not fake worker completion. The blocked
            // acquisition retains its admission until it can actually finish.
            assert_eq!(backend.snapshot_acquiring.load(Ordering::SeqCst), 1);
            assert_eq!(reads.load().admitted, 2);
        }
        assert_eq!(server_sessions(&config).await.1, 1);
        let page = holder
            .read(ReadRequest::new(
                ReadQuery::Ids(vec!["a".into()]),
                OutputProfile::Typed,
            ))
            .await
            .unwrap();
        assert_eq!(
            page.row_count, 1,
            "waiter cancellation did not damage the holder"
        );
        holder.close().await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while backend.snapshot_acquiring.load(Ordering::SeqCst) != 0
                || reads.load().admitted != 0
                || backend.pool.size() as usize != backend.pool.num_idle()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("all workers released pool and application ownership");
        assert_eq!(
            server_sessions(&config).await.1,
            0,
            "no abandoned transaction remains"
        );
        let mut fresh = reads.postgres_snapshot(context()).await.unwrap();
        assert_eq!(
            fresh
                .read(ReadRequest::new(
                    ReadQuery::Ids(vec!["a".into()]),
                    OutputProfile::Typed
                ))
                .await
                .unwrap()
                .row_count,
            1
        );
        fresh.close().await.unwrap();
        assert_eq!(reads.load().admitted, 0);
    }
    let holder = reads.postgres_snapshot(context()).await.unwrap();
    let waiter = tokio::spawn({
        let reads = reads.clone();
        async move { reads.postgres_snapshot(context()).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while backend.snapshot_acquiring.load(Ordering::SeqCst) != 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owner-close waiter reached actual pool acquisition");
    assert_eq!(reads.load().admitted, 2);
    tokio::time::timeout(Duration::from_secs(5), close(owner))
        .await
        .expect("owner close cancels holder and joins blocked refresh before pool close");
    assert!(matches!(
        waiter.await.unwrap(),
        Err(ReadError::Unavailable | ReadError::Closed | ReadError::Cancelled)
    ));
    let _ = holder.close().await;
    assert_eq!(backend.snapshot_acquiring.load(Ordering::SeqCst), 0);
    assert_eq!(reads.load().admitted, 0);
    assert!(backend.pool.is_closed());
    assert_eq!(server_sessions(&config).await.1, 0);
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn failed_snapshot_schema_gate_retains_work_until_physical_setup_cleanup() {
    let mut config = fixture("snapshot_gate_close");
    config.limits.refresh_interval = Duration::from_secs(60);
    config.limits.pool_connections = 1;
    config.limits.lock_timeout = Duration::from_millis(200);
    let mut owner = Some(open(config.clone()).await);
    let reads = owner.as_ref().unwrap().handle().read_service();
    let backend = reads.postgres_backend();
    let options = config
        .options()
        .unwrap()
        .application_name("rusty-haystack-qualification");
    let mut admin = sqlx::PgConnection::connect_with(&options).await.unwrap();
    // Cover original gate failure, caller cancellation, and caller deadline.
    // The final phase also closes the owner while actual cleanup is held.
    for abandonment in [None, Some(false), Some(true)] {
        let mut request = context();
        if abandonment == Some(true) {
            request.deadline = std::time::Instant::now() + Duration::from_secs(1);
        }
        let cancellation = request.cancellation.clone();
        let admission = reads.begin(request).await.unwrap();
        sqlx::query("SELECT pg_advisory_lock(hashtextextended(current_database() || ':' || $1,0))")
            .bind(&config.schema)
            .execute(&mut admin)
            .await
            .unwrap();
        let (entered, mut cleanup_entered) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        *backend.snapshot_setup_close.lock() = Some(super::storage::SetupCloseFault {
            entered,
            release: released,
        });
        let mut opening = tokio::spawn(async move { admission.postgres_snapshot().await });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock' AND query LIKE 'SELECT pg_advisory_lock_shared%'")
                    .bind(format!("rusty-haystack:{}", config.schema)).fetch_one(&mut admin).await.unwrap();
                if waiting == 1 { break; }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.expect("snapshot waits on the actual exclusive schema gate");
        tokio::select! {
            entered = &mut cleanup_entered => entered.expect("owned setup cleanup boundary"),
            result = &mut opening => {
                assert!(matches!(result.unwrap(), Err(ReadError::Unavailable)));
                panic!("snapshot worker finished before owned physical setup cleanup");
            },
            _ = tokio::time::sleep(Duration::from_secs(2)) => panic!("gate timeout must enter bounded setup cleanup"),
        }
        // The real lock timeout has happened, but physical connection retirement
        // is paused: the original admission and sole pool permit must stay owned.
        assert!(!opening.is_finished());
        assert_eq!(reads.load().admitted, 1);
        assert_eq!(backend.pool.size(), 1);
        assert_eq!(backend.pool.num_idle(), 0);
        assert_eq!(server_sessions(&config).await, (1, 0));
        if let Some(expire) = abandonment {
            if !expire {
                cancellation.cancel();
            }
            let result = tokio::time::timeout(Duration::from_secs(2), &mut opening)
                .await
                .unwrap()
                .unwrap();
            if expire {
                assert!(matches!(result, Err(ReadError::Deadline)));
            } else {
                assert!(matches!(result, Err(ReadError::Cancelled)));
            }
            assert_eq!(
                reads.load().admitted,
                1,
                "caller departure does not finish physical cleanup"
            );
            assert_eq!(backend.pool.num_idle(), 0);
            assert_eq!(server_sessions(&config).await, (1, 0));
        }
        sqlx::query(
            "SELECT pg_advisory_unlock(hashtextextended(current_database() || ':' || $1,0))",
        )
        .bind(&config.schema)
        .execute(&mut admin)
        .await
        .unwrap();
        let closing = if abandonment == Some(true) {
            let mut closing = tokio::spawn(close(owner.take().unwrap()));
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut closing)
                    .await
                    .is_err(),
                "owner must await the held setup cleanup"
            );
            assert_eq!(reads.load().admitted, 1);
            Some(closing)
        } else {
            None
        };
        release.send(()).unwrap();
        if abandonment.is_none() {
            assert!(
                matches!(opening.await.unwrap(), Err(ReadError::Unavailable)),
                "preserve the original gate failure"
            );
        }
        if let Some(closing) = closing {
            tokio::time::timeout(Duration::from_secs(3), closing)
                .await
                .unwrap()
                .unwrap();
            assert!(backend.pool.is_closed());
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while reads.load().admitted != 0
                || backend.pool.size() as usize != backend.pool.num_idle()
                || server_sessions(&config).await != (0, 0)
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("actual setup connection and work are fully retired");
        if abandonment != Some(true) {
            let snapshot = reads.postgres_snapshot(context()).await.unwrap();
            snapshot.close().await.unwrap();
            assert_eq!(reads.load().admitted, 0);
        }
    }
    admin.close().await.unwrap();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn commit_faults_distinguish_proven_effect_rollback_and_restart_reconciliation() {
    use super::storage::CommitFault;
    let config = fixture("commit_faults");
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let reads = owner.handle().read_service();
    let backend = reads.postgres_backend();
    *backend.fault.lock() = Some(CommitFault::LoseAcknowledgment);
    let recovered = entity_batch(
        &service,
        "lost_ack",
        vec![EntityOperation::Add(row("committed"))],
    )
    .await;
    assert!(
        matches!(recovered, MutationOutcome::Committed(_)),
        "fresh serialized receipt proves lost-ack COMMIT"
    );
    *backend.fault.lock() = Some(CommitFault::TerminateBeforeCommit);
    let rolled_back = entity_batch(
        &service,
        "terminated",
        vec![EntityOperation::Add(row("rolled_back"))],
    )
    .await;
    assert!(matches!(
        rolled_back,
        MutationOutcome::Rejected {
            reason: RejectionReason::Provider,
            ..
        }
    ));
    assert_eq!(reads.graph().version(), 1);
    assert!(reads.graph().get("rolled_back").is_none());
    assert!(matches!(
        entity_batch(&service, "next", vec![EntityOperation::Add(row("next"))]).await,
        MutationOutcome::Committed(_)
    ));
    assert_eq!(
        reads.graph().version(),
        2,
        "rolled-back SQL transaction consumed no public revision"
    );
    *backend.fault.lock() = Some(CommitFault::SealBeforePublication);
    let before = reads.graph().state();
    let request = EntityBatchRequest {
        identity: OperationIdentity {
            dataset: service.store().dataset(),
            incarnation: before.incarnation,
            operation_id: "before_publish".into(),
        },
        expected_revision: before.revision,
        operations: vec![EntityOperation::Add(row("restart_proves"))],
    };
    let unknown = service.submit(context(), request.clone()).await.unwrap();
    assert!(matches!(unknown, MutationOutcome::Unknown { .. }));
    assert_eq!(
        reads.graph().state(),
        before,
        "committed SQL cannot publish a partially prepared view"
    );
    assert!(matches!(
        reads
            .read(
                context(),
                ReadRequest::new(
                    ReadQuery::Ids(vec!["committed".into()]),
                    OutputProfile::Typed
                )
            )
            .await,
        Err(ReadError::Unavailable)
    ));
    close(owner).await;
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let proven = service
        .reconcile(context(), request.identity.clone())
        .await
        .unwrap();
    assert!(matches!(proven, MutationOutcome::Committed(_)));
    assert_eq!(service.submit(context(), request).await.unwrap(), proven);
    assert_eq!(service.store().graph().version(), 3);
    assert!(service.store().graph().get("restart_proves").is_some());
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn unresolved_commit_barrier_keeps_unknown_and_owns_cleanup_until_actual_completion() {
    use super::storage::CommitFault;
    let mut config = fixture("uncertain");
    config.limits.refresh_interval = Duration::from_secs(60);
    config.limits.lock_timeout = Duration::from_millis(150);
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let reads = owner.handle().read_service();
    let backend = reads.postgres_backend();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    let (completed, done) = tokio::sync::oneshot::channel();
    *backend.fault.lock() = Some(CommitFault::HoldUncertain {
        entered,
        release: held,
        completed,
    });
    let before = reads.graph().state();
    let request = EntityBatchRequest {
        identity: OperationIdentity {
            dataset: service.store().dataset(),
            incarnation: before.incarnation,
            operation_id: "held_commit".into(),
        },
        expected_revision: before.revision,
        operations: vec![EntityOperation::Add(row("late_commit"))],
    };
    let operation = tokio::spawn({
        let service = service.clone();
        let request = request.clone();
        async move { service.submit(context(), request).await }
    });
    ready.await.unwrap();
    let outcome = operation.await.unwrap().unwrap();
    assert!(
        matches!(outcome, MutationOutcome::Unknown { .. }),
        "blocked fresh barrier cannot prove absence/no effect"
    );
    assert_eq!(reads.graph().state(), before);
    assert_eq!(
        reads.load().admitted,
        1,
        "original lease remains with unresolved SQL transaction"
    );
    assert!(backend.pool.size() as usize > backend.pool.num_idle());
    release.send(()).unwrap();
    done.await.unwrap().unwrap();
    assert_eq!(reads.load().admitted, 0);
    assert_eq!(backend.pool.size() as usize, backend.pool.num_idle());
    close(owner).await;
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    assert!(matches!(
        service
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        MutationOutcome::Committed(_)
    ));
    assert!(matches!(
        service.submit(context(), request).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    assert_eq!(service.store().graph().version(), 1);
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn pooled_connection_loss_reconnects_between_shared_instances() {
    let config = fixture("reconnect");
    let owner = open(config.clone()).await;
    let peer = open(config.clone()).await;
    let reads = owner.handle().read_service();
    let backend = reads.postgres_backend();
    let mut live = backend.pool.acquire().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *live)
        .await
        .unwrap();
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&mut admin)
        .await
        .unwrap();
    assert!(terminated);
    drop(live);
    assert!(matches!(
        entity_batch(
            &peer.handle().mutation_service().unwrap(),
            "peer",
            vec![EntityOperation::Add(row("reconnected"))]
        )
        .await,
        MutationOutcome::Committed(_)
    ));
    let page = reads
        .read(
            context(),
            ReadRequest::new(
                ReadQuery::Ids(vec!["reconnected".into()]),
                OutputProfile::Typed,
            ),
        )
        .await
        .unwrap();
    assert_eq!(page.row_count, 1);
    assert!(backend.check().is_ok());
    close(owner).await;
    close(peer).await;
    admin.close().await.unwrap();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn transaction_generation_mismatch_fences_a_still_connected_owner() {
    let mut config = fixture("generation");
    config.limits.refresh_interval = Duration::from_secs(60);
    let owner = open(config.clone()).await;
    let reads = owner.handle().read_service();
    let backend = reads.postgres_backend();
    let mut live = backend.pool.acquire().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *live)
        .await
        .unwrap();
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE \"{}\".metadata SET owner_generation = owner_generation + 1 WHERE singleton = 1",
        config.schema
    )))
    .execute(&mut admin)
    .await
    .unwrap();
    let connected: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)")
            .bind(pid)
            .fetch_one(&mut admin)
            .await
            .unwrap();
    assert!(connected);
    // Exercise the SQL generation check itself before any health probe can seal
    // this owner. A cached local seal or socket error cannot satisfy this case.
    assert!(backend.check().is_ok());
    assert!(
        backend
            .transaction(std::time::Instant::now() + Duration::from_secs(2))
            .await
            .is_err()
    );
    assert!(matches!(
        reads
            .read(
                context(),
                ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed)
            )
            .await,
        Err(ReadError::Unavailable)
    ));
    live.close().await.unwrap();
    close(owner).await;
    admin.close().await.unwrap();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn verified_tls_rejects_untrusted_ca_wrong_name_and_plain_only_endpoint() {
    let config = fixture("tls");
    let mut untrusted = config.clone();
    untrusted.transport = PostgresTransport::VerifyFull {
        additional_ca_pem: None,
    };
    assert!(matches!(
        application(
            untrusted,
            Catalog::load_http_pinned().unwrap(),
            policies(),
            PostgresServiceLimits::default()
        )
        .await,
        Err(PostgresError::Connection)
    ));
    // getaddrinfo recognizes the legacy numeric loopback spelling, while the
    // certificate does not name it. Prove reachability before testing TLS so a
    // connection-refused error cannot masquerade as name-verification evidence.
    let mut wrong_name = config.clone();
    wrong_name.host = "127.1".into();
    let reachable = wrong_name
        .options()
        .unwrap()
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);
    sqlx::PgConnection::connect_with(&reachable)
        .await
        .expect("wrong-name endpoint is reachable")
        .close()
        .await
        .unwrap();
    assert!(matches!(
        application(
            wrong_name,
            Catalog::load_http_pinned().unwrap(),
            policies(),
            PostgresServiceLimits::default()
        )
        .await,
        Err(PostgresError::Connection)
    ));
    let path = std::env::var_os("RUSTY_HAYSTACK_POSTGRES_PLAIN_FIXTURE")
        .expect("TLS qualification requires plain-only fixture");
    let plain: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut plain_config = config.clone();
    plain_config.port = plain["port"].as_u64().unwrap() as u16;
    plain_config.database = plain["database"].as_str().unwrap().to_owned();
    plain_config.username = plain["user"].as_str().unwrap().to_owned();
    plain_config.password = Some(plain["password"].as_str().unwrap().to_owned());
    assert!(matches!(
        application(
            plain_config.clone(),
            Catalog::load_http_pinned().unwrap(),
            policies(),
            PostgresServiceLimits::default()
        )
        .await,
        Err(PostgresError::Connection)
    ));
    plain_config.transport = PostgresTransport::LoopbackPlaintext;
    let owner = open(plain_config.clone()).await;
    close(owner).await;
    cleanup(&plain_config).await;
    // The verified connection succeeds with the exact trusted fixture CA/name.
    let owner = open(config.clone()).await;
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn incompatible_catalog_oversized_projection_and_unlogged_schema_never_become_ready() {
    let config = fixture("bad_recovery");
    let owner = open(config.clone()).await;
    close(owner).await;
    let mismatch = application(
        config.clone(),
        Catalog::load_pinned().unwrap(),
        policies(),
        PostgresServiceLimits::default(),
    )
    .await;
    assert!(matches!(mismatch, Err(PostgresError::Incompatible)));
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    let payload = crate::postgres::value::encode_dict(&row("a")).unwrap();
    sqlx::query("INSERT INTO entities VALUES($1,$2)")
        .bind("x".repeat(8192))
        .bind(&payload)
        .execute(&mut admin)
        .await
        .unwrap();
    let mut bounded = config.clone();
    bounded.limits.recovery.max_bytes = 4096;
    assert!(matches!(
        application(
            bounded,
            Catalog::load_http_pinned().unwrap(),
            policies(),
            PostgresServiceLimits::default()
        )
        .await,
        Err(PostgresError::Limit)
    ));
    sqlx::query("DELETE FROM entities")
        .execute(&mut admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO entities VALUES('a',$1)")
        .bind(b"malformed".as_slice())
        .execute(&mut admin)
        .await
        .unwrap();
    assert!(matches!(
        application(
            config.clone(),
            Catalog::load_http_pinned().unwrap(),
            policies(),
            PostgresServiceLimits::default()
        )
        .await,
        Err(PostgresError::Incompatible)
    ));
    sqlx::query("DELETE FROM entities")
        .execute(&mut admin)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE entities SET UNLOGGED")
        .execute(&mut admin)
        .await
        .unwrap();
    assert!(matches!(
        application(
            config.clone(),
            Catalog::load_http_pinned().unwrap(),
            policies(),
            PostgresServiceLimits::default()
        )
        .await,
        Err(PostgresError::Durability)
    ));
    admin.close().await.unwrap();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn typed_payload_keeps_integer_float_bits_units_nominal_provenance_and_nested_absence() {
    use haystack_core::kinds::{Coord, Float, HDateTime, NominalScalar, Number};
    let config = fixture("typed_values");
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
        Kind::List(vec![Kind::Dict(Box::new(nested)), Kind::NA, Kind::Marker]),
    );
    let ts = chrono::DateTime::parse_from_rfc3339("2024-06-01T08:30:00.123456789-04:00").unwrap();
    value.set("when", Kind::DateTime(HDateTime::new(ts, "New_York")));
    let original = crate::postgres::value::encode_dict(&value).unwrap();
    assert!(matches!(
        entity_batch(&service, "rich", vec![EntityOperation::Add(value)]).await,
        MutationOutcome::Committed(_)
    ));
    close(owner).await;
    let owner = open(config.clone()).await;
    let recovered = owner.handle().read_service().graph().get("rich").unwrap();
    assert_eq!(
        crate::postgres::value::encode_dict(&recovered).unwrap(),
        original
    );
    close(owner).await;
    cleanup(&config).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn explicit_passfile_is_never_loaded_when_password_is_absent() {
    const CHILD: &str = "RUSTY_HAYSTACK_PG_PASSFILE_CHILD";
    let mut config = fixture("passfile");
    if std::env::var_os(CHILD).is_some() {
        config.password = None;
        assert!(matches!(
            application(
                config,
                Catalog::load_http_pinned().unwrap(),
                policies(),
                PostgresServiceLimits::default()
            )
            .await,
            Err(PostgresError::Connection)
        ));
        return;
    }
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let path = std::env::temp_dir().join(format!(
        "rusty-haystack-passfile-{:016x}",
        rand::random::<u64>()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let escaped = |s: &str| s.replace('\\', "\\\\").replace(':', "\\:");
    writeln!(
        file,
        "{}:{}:{}:{}:{}",
        escaped(&config.host),
        config.port,
        escaped(&config.database),
        escaped(&config.username),
        escaped(config.password.as_ref().unwrap())
    )
    .unwrap();
    drop(file);
    let output=tokio::task::spawn_blocking({let path=path.clone();move||{
        std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("postgres::qualification::explicit_passfile_is_never_loaded_when_password_is_absent")
            .arg("--ignored").env(CHILD,"1").env("PGPASSFILE",path).output().unwrap()
    }}).await.unwrap();
    std::fs::remove_file(path).unwrap();
    assert!(
        output.status.success(),
        "explicit passfile must not supply a missing password"
    );
}

async fn server_sessions(config: &PostgresConfig) -> (i64, i64) {
    use sqlx::Row;
    let options = config
        .options()
        .unwrap()
        .application_name("rusty-haystack-qualification");
    let mut admin = sqlx::PgConnection::connect_with(&options).await.unwrap();
    let counts=sqlx::query("SELECT count(*) AS sessions,count(*) FILTER(WHERE xact_start IS NOT NULL) AS transactions FROM pg_stat_activity WHERE application_name=$1 AND datname=current_database()")
        .bind(format!("rusty-haystack:{}",config.schema)).fetch_one(&mut admin).await.unwrap();
    let result = (counts.get("sessions"), counts.get("transactions"));
    admin.close().await.unwrap();
    result
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn abandoned_constructor_and_unstarted_builder_release_real_server_sessions() {
    let config = fixture("abandon");
    let owner = open(config.clone()).await;
    close(owner).await;
    let options = config
        .options()
        .unwrap()
        .application_name("rusty-haystack-qualification");
    let mut admin = sqlx::PgConnection::connect_with(&options).await.unwrap();
    sqlx::query("BEGIN").execute(&mut admin).await.unwrap();
    sqlx::query("SELECT owner_generation FROM metadata WHERE singleton=1 FOR UPDATE")
        .fetch_one(&mut admin)
        .await
        .unwrap();
    let pending = tokio::spawn({
        let config = config.clone();
        async move {
            application(
                config,
                Catalog::load_http_pinned().unwrap(),
                policies(),
                PostgresServiceLimits::default(),
            )
            .await
        }
    });
    // Observe the independent construction session holding the advisory lock.
    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // Observe without acquiring the lock ourselves: try-lock polling
            // can race the constructor and spuriously make it AlreadyOwned.
            let owned: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND granted AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) AND classid = ((hashtextextended(current_database() || ':' || $1,0) >> 32) & 4294967295)::oid AND objid = (hashtextextended(current_database() || ':' || $1,0) & 4294967295)::oid AND objsubid = 1)",
            )
            .bind(&config.schema)
            .fetch_one(&mut admin)
            .await
            .unwrap();
            if owned {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if observed.is_err() && pending.is_finished() {
        match pending.await {
            Ok(Err(error)) => panic!("construction ended before lock observation: {error:?}"),
            Ok(Ok(_)) => panic!("construction completed despite the held metadata lock"),
            Err(error) => panic!("construction task failed: {error:?}"),
        }
    }
    observed.expect("observe construction advisory ownership");
    pending.abort();
    assert!(matches!(pending.await,Err(error) if error.is_cancelled()));
    sqlx::query("ROLLBACK").execute(&mut admin).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while server_sessions(&config).await.0 != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled construction worker awaited real database release");
    let builder = application(
        config.clone(),
        Catalog::load_http_pinned().unwrap(),
        policies(),
        PostgresServiceLimits::default(),
    )
    .await
    .unwrap();
    let early = builder.handle();
    drop(builder);
    assert!(
        early
            .read_service()
            .read(
                context(),
                ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed)
            )
            .await
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while server_sessions(&config).await.0 != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("unstarted builder dropped actual database owner and pool sessions");
    admin.close().await.unwrap();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn committed_retry_waits_for_projection_publication_and_notification() {
    let config = fixture("publication");
    let owner = open(config.clone()).await;
    let service = owner.handle().mutation_service().unwrap();
    let reads = owner.handle().read_service();
    let backend = reads.postgres_backend();
    let before = reads.graph().state();
    let mut wakes = reads.graph().subscribe();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel();
    *backend.fault.lock() = Some(super::storage::CommitFault::PauseAfterCommit {
        entered,
        release: held,
    });
    let request = EntityBatchRequest {
        identity: OperationIdentity {
            dataset: service.store().dataset(),
            incarnation: before.incarnation,
            operation_id: "publication".into(),
        },
        expected_revision: before.revision,
        operations: vec![EntityOperation::Add(row("committed"))],
    };
    let first = tokio::spawn({
        let service = service.clone();
        let request = request.clone();
        async move { service.submit(context(), request).await }
    });
    ready.await.unwrap();
    let second = tokio::spawn({
        let service = service.clone();
        let request = request.clone();
        async move { service.submit(context(), request).await }
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !second.is_finished(),
        "a SQL receipt cannot acknowledge an unpublished local projection"
    );
    assert_eq!(reads.graph().state(), before);
    assert!(matches!(
        wakes.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    release.send(()).unwrap();
    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    assert_eq!(first, second);
    assert!(matches!(first, MutationOutcome::Committed(_)));
    assert_eq!(wakes.recv().await.unwrap(), before.revision + 1);
    assert_eq!(reads.graph().version(), before.revision + 1);
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn reversed_distinct_writes_preserve_durable_feed_order_without_gaps() {
    use haystack_core::graph::{CommitSpan, DiffOp, GraphState};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct HoldFirst {
        calls: AtomicUsize,
        entered: tokio::sync::Semaphore,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl MutationProvider for HoldFirst {
        fn qualification(&self) -> ReceiptQualification {
            ReceiptQualification::PostgresDurable
        }
        fn commit(&self, prepared: PreparedMutation) -> MutationOutcome {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.entered.add_permits(1);
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .expect("bounded release of first prepared PostgreSQL write");
            }
            // Preserve the selected durable store's actual publication path.
            prepared.publish()
        }
    }

    async fn check_feed_and_sql(
        service: &MutationService,
        config: &PostgresConfig,
        cursor: &str,
        before: GraphState,
    ) {
        let graph = service.store().graph();
        assert_eq!(
            graph.state(),
            GraphState {
                revision: before.revision + 2,
                ..before
            }
        );
        assert_eq!(graph.len(), 2);
        assert!(graph.get("first").is_none());
        let page = service
            .changes(
                context(),
                ChangesRequest {
                    cursor: Some(cursor.into()),
                    max_diffs: 128,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.dataset, service.store().dataset());
        assert_eq!(page.incarnation, before.incarnation);
        assert_eq!(page.floor, before.revision);
        assert_eq!(page.head, before.revision + 2);
        assert_eq!(page.position, page.head);
        assert!(page.complete);
        assert_eq!(page.changes.len(), 2);

        let mut sql = sqlx::PgConnection::connect_with(&config.options().unwrap())
            .await
            .unwrap();
        let (head, floor): (String, String) =
            sqlx::query_as("SELECT revision::text, floor::text FROM metadata WHERE singleton=1")
                .fetch_one(&mut sql)
                .await
                .unwrap();
        assert_eq!(head, page.head.to_string());
        assert_eq!(floor, page.floor.to_string());
        let stored: Vec<(String, Vec<u8>)> =
            sqlx::query_as("SELECT revision::text, payload FROM entity_changes ORDER BY revision")
                .fetch_all(&mut sql)
                .await
                .unwrap();
        assert_eq!(stored.len(), 2);
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM entities ORDER BY id")
            .fetch_all(&mut sql)
            .await
            .unwrap();
        assert_eq!(ids, vec!["second", "third"]);
        for (index, id) in ["second", "third"].into_iter().enumerate() {
            let revision = before.revision + index as u64 + 1;
            let span = CommitSpan {
                first: revision,
                last: revision,
            };
            let diff = &page.changes[index];
            assert_eq!(diff.id, id);
            assert_eq!(diff.revision, revision);
            assert_eq!(diff.span, span);
            assert_eq!(diff.operation, DiffOp::Add);
            assert_eq!(
                value::encode_dict(&diff.changed).unwrap(),
                value::encode_dict(&row(id)).unwrap()
            );
            let change = value::decode_change(&stored[index].1).unwrap();
            assert_eq!(stored[index].0, revision.to_string());
            assert_eq!(change.version, revision);
            assert_eq!(change.id, id);
            assert_eq!(change.span, span);
            assert_eq!(change.op, DiffOp::Add);
            assert_eq!(
                value::encode_dict(change.new.as_ref().unwrap()).unwrap(),
                value::encode_dict(&diff.changed).unwrap()
            );
        }
        sql.close().await.unwrap();
    }

    let config = fixture("ordering");
    let owner = open(config.clone()).await;
    // Feed/cursor assertions always use the application's canonical service;
    // the provider wrapper is used only to schedule its same durable store.
    let primary = owner.handle().mutation_service().unwrap();
    let before = primary.store().graph().state();
    let cursor = primary
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
    let request = |operation_id: &str, id: &str, revision| EntityBatchRequest {
        identity: OperationIdentity {
            dataset: primary.store().dataset(),
            incarnation: before.incarnation,
            operation_id: operation_id.into(),
        },
        expected_revision: revision,
        operations: vec![EntityOperation::Add(row(id))],
    };
    let first_request = request("prepared-first", "first", before.revision);
    let second_request = request("committed-first", "second", before.revision);
    let (release, held) = std::sync::mpsc::channel();
    let provider = Arc::new(HoldFirst {
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Semaphore::new(0),
        release: std::sync::Mutex::new(held),
    });
    let scheduled = MutationService::with_provider(
        primary.read_service(),
        primary.store(),
        Arc::new(AllowAllMutations),
        primary.limits().clone(),
        provider.clone(),
    )
    .unwrap();
    let first = tokio::spawn({
        let service = scheduled.clone();
        let request = first_request.clone();
        async move { service.submit(context(), request).await.unwrap() }
    });
    tokio::time::timeout(Duration::from_secs(2), provider.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    let rows = primary
        .read_service()
        .postgres_backend()
        .receipt_rows(1)
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "A reserved its durable identity before B began"
    );
    use sqlx::Row;
    let pending: MutationOutcome =
        haystack_core::codecs::entity::decode(&rows[0].get::<Vec<u8>, _>("outcome")).unwrap();
    assert_eq!(
        pending,
        MutationOutcome::Unknown {
            identity: first_request.identity.clone(),
            cause: UnknownCause::Pending
        }
    );
    assert!(!first.is_finished());
    let second_outcome = scheduled
        .submit(context(), second_request.clone())
        .await
        .unwrap();
    let MutationOutcome::Committed(second_receipt) = &second_outcome else {
        panic!("B must commit while earlier A remains held: {second_outcome:?}");
    };
    assert_eq!(second_receipt.before_revision, before.revision);
    assert_eq!(second_receipt.after_revision, before.revision + 1);
    assert_eq!(
        second_receipt.qualification,
        ReceiptQualification::PostgresDurable
    );
    assert!(!first.is_finished());
    release.send(()).unwrap();
    let first_outcome = first.await.unwrap();
    assert_eq!(
        first_outcome,
        MutationOutcome::Rejected {
            identity: first_request.identity.clone(),
            reason: RejectionReason::Conflict
        }
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let third_request = request("after-conflict", "third", before.revision + 1);
    let third_outcome = primary
        .submit(context(), third_request.clone())
        .await
        .unwrap();
    let MutationOutcome::Committed(third_receipt) = &third_outcome else {
        panic!("C must commit without a rejected-A feed gap: {third_outcome:?}");
    };
    assert_eq!(third_receipt.before_revision, before.revision + 1);
    assert_eq!(third_receipt.after_revision, before.revision + 2);
    assert_eq!(
        third_receipt.qualification,
        ReceiptQualification::PostgresDurable
    );
    check_feed_and_sql(&primary, &config, &cursor, before).await;
    let expected = [
        (first_request, first_outcome),
        (second_request, second_outcome),
        (third_request, third_outcome),
    ];
    for (request, outcome) in &expected {
        assert_eq!(
            primary
                .reconcile(context(), request.identity.clone())
                .await
                .unwrap(),
            *outcome
        );
    }
    drop(scheduled);
    drop(primary);
    close(owner).await;
    let owner = open(config.clone()).await;
    let primary = owner.handle().mutation_service().unwrap();
    for (request, outcome) in &expected {
        assert_eq!(
            primary
                .reconcile(context(), request.identity.clone())
                .await
                .unwrap(),
            *outcome
        );
        assert_eq!(
            primary.submit(context(), request.clone()).await.unwrap(),
            *outcome
        );
    }
    check_feed_and_sql(&primary, &config, &cursor, before).await;
    close(owner).await;
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn independent_application_processes_share_one_authority() {
    const CHILD: &str = "RUSTY_HAYSTACK_PG_SHARED_CHILD_SCHEMA";
    if let Some(schema) = std::env::var_os(CHILD) {
        let mut config = fixture("shared_child");
        config.schema = schema.into_string().unwrap();
        let owner = open(config).await;
        let service = owner.handle().mutation_service().unwrap();
        let state = service.store().graph().state();
        let request = EntityBatchRequest {
            identity: OperationIdentity {
                dataset: service.store().dataset(),
                incarnation: state.incarnation,
                operation_id: "child-write".into(),
            },
            expected_revision: state.revision,
            operations: vec![EntityOperation::Add(row("from-child"))],
        };
        assert!(matches!(
            service.submit(context(), request).await.unwrap(),
            MutationOutcome::Committed(_)
        ));
        close(owner).await;
        return;
    }
    let config = fixture("shared_processes");
    let owner = open(config.clone()).await;
    let status = tokio::task::spawn_blocking({
        let schema = config.schema.clone();
        move || std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg("postgres::qualification::independent_application_processes_share_one_authority")
            .arg("--ignored").arg("--nocapture").env(CHILD, schema).output().unwrap()
    }).await.unwrap();
    assert!(
        status.status.success(),
        "second real application process must start and commit: {}",
        String::from_utf8_lossy(&status.stdout)
    );
    owner
        .handle()
        .read_service()
        .read(
            context(),
            ReadRequest::new(
                ReadQuery::Ids(vec!["from-child".into()]),
                OutputProfile::Typed,
            ),
        )
        .await
        .unwrap();
    assert!(
        owner
            .handle()
            .mutation_service()
            .unwrap()
            .store()
            .graph()
            .get("from-child")
            .is_some()
    );
    close(owner).await;
    cleanup(&config).await;
}

mod process;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn schema_validation_does_not_deparse_unrelated_removed_relations() {
    removed_schema_validation_probe(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn schema_validation_does_not_deparse_unrelated_removed_foreign_keys() {
    removed_schema_validation_probe(true).await;
}

async fn removed_schema_validation_probe(history: bool) {
    let config = fixture("catalog_scope");
    let unrelated = fixture("catalog_other");
    let mut admin = sqlx::PgConnection::connect_with(&unrelated.options().unwrap())
        .await
        .unwrap();
    let tables = if history {
        "CREATE TABLE history_points(id text PRIMARY KEY); CREATE TABLE history_samples(point text REFERENCES history_points(id));"
    } else {
        "CREATE TABLE metadata(format integer CHECK(format=2));"
    };
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA \"{}\"; SET search_path=\"{}\"; {tables}",
        unrelated.schema, unrelated.schema
    )))
    .execute(&mut admin)
    .await
    .unwrap();
    let owner = open(config.clone()).await;
    close(owner).await;
    let mut connection = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_lock(hashtextextended(current_database() || ':' || $1,0))")
        .bind(&config.schema)
        .execute(&mut connection)
        .await
        .unwrap();
    if history {
        // Keep EXISTS semantics: an earlier, differently defined owned FK does
        // not hide a later matching one, and is not itself sufficient.
        sqlx::raw_sql("ALTER TABLE history_samples DROP CONSTRAINT history_samples_point_fkey; ALTER TABLE history_samples ADD CONSTRAINT cascade_reference FOREIGN KEY(point) REFERENCES history_points(id) ON DELETE CASCADE; ALTER TABLE history_samples ADD CONSTRAINT history_samples_point_fkey FOREIGN KEY(point) REFERENCES history_points(id);")
            .execute(&mut connection).await.unwrap();
    }
    sqlx::raw_sql("BEGIN ISOLATION LEVEL REPEATABLE READ; SET LOCAL enable_indexscan=off; SET LOCAL enable_bitmapscan=off; SET LOCAL enable_hashjoin=off; SET LOCAL enable_mergejoin=off;")
        .execute(&mut connection).await.unwrap();
    let _: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_constraint")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    // Preserve the catalog observation that a concurrent statement may already
    // hold when another schema is dropped. Name lookup during deparse consults
    // current catalogs, independently of that observation.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA \"{}\" CASCADE",
        unrelated.schema
    )))
    .execute(&mut admin)
    .await
    .unwrap();
    let result = super::storage::validate_schema(&mut connection, &config.schema).await;
    sqlx::query("ROLLBACK")
        .execute(&mut connection)
        .await
        .unwrap();
    if history {
        sqlx::query("ALTER TABLE history_samples DROP CONSTRAINT history_samples_point_fkey")
            .execute(&mut connection)
            .await
            .unwrap();
        assert!(
            matches!(
                super::storage::validate_schema(&mut connection, &config.schema).await,
                Err(PostgresError::Incompatible)
            ),
            "a different FK definition cannot satisfy the required schema contract"
        );
    }
    connection.close().await.unwrap();
    admin.close().await.unwrap();
    cleanup(&config).await;
    assert!(
        result.is_ok(),
        "validation of the intact owned schema must not deparse a removed unrelated relation: {result:?}"
    );
}

mod administration;
