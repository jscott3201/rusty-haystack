//! History qualification runs against a real local file and the bundled engine.
use super::*;
use haystack_core::{
    codecs::{entity, typed},
    graph::{CommitSpan, DiffOp, RecoveryChange},
    kinds::{HDateTime, Number},
};
use rusqlite::{Connection, params, types::Value};

async fn open_with_limits(config: SqliteConfig, limits: SqliteServiceLimits) -> ApplicationOwner {
    let owner = application(
        config,
        Catalog::load_http_pinned().unwrap(),
        policies(),
        limits,
    )
    .await
    .unwrap()
    .start(&tokio::runtime::Handle::current())
    .unwrap();
    owner.ready().await.unwrap();
    owner
}
async fn add_point(
    owner: &ApplicationOwner,
    id: &str,
    kind: &str,
    timezone: &str,
    unit: Option<&str>,
) {
    let service = owner.handle().mutation_service().unwrap();
    let mut point = HDict::new();
    point.set("id", Kind::Ref(HRef::from_val(id)));
    point.set("his", Kind::Marker);
    point.set("kind", Kind::Str(kind.into()));
    point.set("tz", Kind::Str(timezone.into()));
    if let Some(unit) = unit {
        point.set("unit", Kind::Str(unit.into()));
    }
    let request = request(
        &service,
        &format!("point-{id}"),
        vec![EntityOperation::Add(point)],
    );
    assert!(matches!(
        service.submit(context(), request).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
}
fn sample(timestamp: &str, timezone: &str, value: Kind) -> HistorySample {
    HistorySample {
        ts: HDateTime::new(
            chrono::DateTime::parse_from_rfc3339(timestamp).unwrap(),
            timezone,
        ),
        val: value,
    }
}
fn history_request(
    service: &HistoryMutationService,
    point: &str,
    operation: &str,
    samples: Vec<HistorySample>,
) -> HistoryWriteRequest {
    let state = service.store().state(point).unwrap();
    HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: state.authority,
            incarnation: state.incarnation,
            point: point.into(),
            operation_id: operation.into(),
        },
        expected_generation: state.generation,
        samples,
    }
}
fn committed(outcome: &HistoryWriteOutcome) -> &HistoryWriteReceipt {
    let HistoryWriteOutcome::Committed(receipt) = outcome else {
        panic!("durable history commit: {outcome:?}")
    };
    assert_eq!(
        receipt.qualification,
        HistoryReceiptQualification::SqliteDurable
    );
    receipt
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_upserts_reopen_keeps_exact_samples_intent_and_entity_cursor() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let mut limits = SqliteServiceLimits::default();
    limits.history.batch_rows = 1;
    let owner = open_with_limits(config.clone(), limits.clone()).await;
    let entities = owner.handle().mutation_service().unwrap();
    let cursor = entities
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
    add_point(&owner, "p", "Number", "New_York", Some("°F")).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let empty = history.store().state("p").unwrap();
    assert_eq!(
        history.store().write("p", vec![]),
        Err(HistoryProviderError::Unsupported)
    );
    assert_eq!(
        history.store().reset_point("p"),
        Err(HistoryProviderError::Unsupported)
    );
    close(owner).await;
    let owner = open_with_limits(config.clone(), limits.clone()).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(
        history.store().state("p").unwrap(),
        empty,
        "observed empty point identity survives without a write"
    );
    let entity_state = owner.handle().read_service().graph().state();
    let samples = vec![
        sample(
            "2024-06-01T08:30:00.123456789-04:00",
            "New_York",
            Kind::Number(Number::unitless(1.0)),
        ),
        sample("2024-06-01T08:30:00.123456790-04:00", "New_York", Kind::NA),
        sample(
            "2024-06-01T08:30:00.123456789-04:00",
            "New_York",
            Kind::Number(Number::new(-0.0, Some("fahrenheit".into()))),
        ),
    ];
    // The same operation-ID text already names an entity receipt; namespaces differ.
    let request = history_request(&history, "p", "point-p", samples);
    let outcome = history.submit(context(), request.clone()).await.unwrap();
    let receipt = committed(&outcome);
    assert_eq!(
        (
            receipt.before_generation,
            receipt.after_generation,
            receipt.change_sequence
        ),
        (0, 1, 1)
    );
    assert_eq!(
        (
            receipt.submitted_samples,
            receipt.unique_samples,
            receipt.retained_samples
        ),
        (3, 2, 2)
    );
    assert_eq!(owner.handle().read_service().graph().state(), entity_state);
    let read = HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01".into(),
    };
    let first = history
        .history_service()
        .collect(context(), read.clone())
        .await
        .unwrap();
    assert_eq!(first.terminal, HistoryTerminal::Complete);
    assert_eq!(first.samples.len(), 2);
    let Kind::Number(value) = &first.samples[0].val else {
        panic!()
    };
    assert_eq!(value.val.to_bits(), (-0.0f64).to_bits());
    assert_eq!(value.unit.as_deref(), Some("fahrenheit"));
    assert_eq!(first.samples[0].ts.dt.timestamp_subsec_nanos(), 123456789);
    assert_eq!(first.samples[0].ts.dt.offset().local_minus_utc(), -4 * 3600);
    assert_eq!(first.samples[0].ts.tz_name, "New_York");
    assert_eq!(first.samples[1].val, Kind::NA);
    let state = history.store().state("p").unwrap();
    let ledger = history.store().retained_changes();
    close(owner).await;
    let owner = open_with_limits(config, limits).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(history.store().state("p").unwrap(), state);
    assert_eq!(history.store().retained_changes(), ledger);
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
    for edit in [0, 1, 2] {
        let mut different = request.clone();
        match edit {
            0 => different.samples.reverse(),
            1 => different.expected_generation += 1,
            _ => different.samples[0].ts.tz_name = "America/New_York".into(),
        }
        assert!(matches!(
            history.submit(context(), different).await.unwrap(),
            HistoryWriteOutcome::Rejected {
                reason: HistoryWriteRejection::Conflict,
                ..
            }
        ));
    }
    let other = ReadContext::with_timeout(
        Principal::TrustedEmbedding {
            subject: "other".into(),
        },
        Duration::from_secs(5),
    );
    assert!(matches!(
        history.reconcile(other, request.identity).await.unwrap(),
        HistoryWriteOutcome::Unknown {
            cause: HistoryWriteUnknown::Missing,
            ..
        }
    ));
    let reopened = history
        .history_service()
        .collect(context(), read)
        .await
        .unwrap();
    assert_eq!(reopened.samples, first.samples);
    assert_eq!(reopened.metadata.history, first.metadata.history);
    assert_eq!(
        reopened.metadata.capabilities,
        HistoryCapabilities::BOUNDED_LIVE
    );
    let changes = owner
        .handle()
        .mutation_service()
        .unwrap()
        .changes(
            context(),
            ChangesRequest {
                cursor: Some(cursor),
                max_diffs: 128,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        changes.changes.len(),
        1,
        "history never advances the entity feed"
    );
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_history_selection_is_the_application_selection() {
    let directory = tempfile::tempdir().unwrap();
    let owner = open(SqliteConfig::new(directory.path().join("history.db"))).await;
    let handle = owner.handle();
    let history = handle.history_service();
    let writes = handle.history_mutation_service();
    close(owner).await;
    let history = history.expect("SQLite must select shared bounded history");
    let writes = writes.expect("SQLite must select authorized history writes");
    assert!(history.same_selection(writes.history_service()));
}

// This fixture describes the exact PR119 format independently of the current
// SQLite schema/constructor. Shared canonical payload codecs construct valid
// domain records; no current SQLite writer creates or upgrades this file.
fn legacy_v1(path: &std::path::Path) {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(
        "CREATE TABLE metadata(singleton INTEGER PRIMARY KEY CHECK(singleton=1), format INTEGER NOT NULL CHECK(format=1), dataset BLOB NOT NULL CHECK(length(dataset)=16), incarnation BLOB NOT NULL CHECK(length(incarnation)=16), revision BLOB NOT NULL CHECK(length(revision)=8), catalog_generation BLOB NOT NULL CHECK(length(catalog_generation)=8), floor BLOB NOT NULL CHECK(length(floor)=8), catalog TEXT NOT NULL CHECK(length(CAST(catalog AS BLOB))<=1048576), cursor_key BLOB NOT NULL CHECK(length(cursor_key)=32)) STRICT;
         CREATE TABLE entities(id TEXT PRIMARY KEY, payload BLOB NOT NULL CHECK(length(payload)<=1048576)) STRICT, WITHOUT ROWID;
         CREATE TABLE entity_changes(revision BLOB PRIMARY KEY CHECK(length(revision)=8), payload BLOB NOT NULL CHECK(length(payload)<=8388608)) STRICT, WITHOUT ROWID;
         CREATE TABLE receipts(binding BLOB PRIMARY KEY CHECK(length(binding)<=1048576), request BLOB NOT NULL CHECK(length(request)<=1048576), outcome BLOB NOT NULL CHECK(length(outcome)<=1048576)) STRICT, WITHOUT ROWID;
         PRAGMA user_version=1;
         PRAGMA application_id=1213813573;"
    ).unwrap();
    let dataset = [1u8; 16];
    let incarnation = [2u8; 16];
    let cursor = [3u8; 32];
    let catalog = ActivatedCatalog::new(Catalog::load_http_pinned().unwrap(), None).unwrap();
    connection
        .execute(
            "INSERT INTO metadata VALUES(1,1,?1,?2,?3,?3,?4,?5,?6)",
            params![
                dataset.as_slice(),
                incarnation.as_slice(),
                1u64.to_be_bytes().as_slice(),
                0u64.to_be_bytes().as_slice(),
                catalog.selection_identity(),
                cursor.as_slice()
            ],
        )
        .unwrap();
    let row = row("legacy");
    let payload = crate::storage_value::encode_dict(&row).unwrap();
    let span = CommitSpan { first: 1, last: 1 };
    let change = crate::storage_value::encode_change(&RecoveryChange {
        version: 1,
        span,
        timestamp: 1_700_000_000,
        op: DiffOp::Add,
        id: "legacy".into(),
        old: None,
        new: Some(row.clone()),
        changed: None,
        previous: None,
    })
    .unwrap();
    connection
        .execute("INSERT INTO entities VALUES('legacy',?1)", [&payload])
        .unwrap();
    connection
        .execute(
            "INSERT INTO entity_changes VALUES(?1,?2)",
            params![1u64.to_be_bytes().as_slice(), change],
        )
        .unwrap();
    let identity = OperationIdentity {
        dataset,
        incarnation,
        operation_id: "legacy-operation".into(),
    };
    let request = EntityBatchRequest {
        identity: identity.clone(),
        expected_revision: 0,
        operations: vec![EntityOperation::Add(row)],
    };
    let key = typed::encode(&Kind::List(vec![
        Kind::Str("embedding".into()),
        Kind::Str("sqlite-owner".into()),
        identity.to_kind().unwrap(),
    ]))
    .unwrap();
    let outcome = MutationOutcome::Committed(EntityReceipt {
        identity,
        before_revision: 0,
        after_revision: 1,
        span: Some(span),
        qualification: ReceiptQualification::SqliteDurable,
    });
    connection
        .execute(
            "INSERT INTO receipts VALUES(?1,?2,?3)",
            params![
                key,
                entity::encode(&request).unwrap(),
                entity::encode(&outcome).unwrap()
            ],
        )
        .unwrap();
    connection.close().unwrap();
}

fn legacy_image(path: &std::path::Path) -> Vec<Vec<Vec<Value>>> {
    let connection = Connection::open(path).unwrap();
    [
        "SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name",
        "SELECT * FROM metadata",
        "SELECT * FROM entities ORDER BY id",
        "SELECT * FROM entity_changes ORDER BY revision",
        "SELECT * FROM receipts ORDER BY binding",
        "PRAGMA journal_mode",
        "PRAGMA user_version",
        "PRAGMA application_id",
    ]
    .into_iter()
    .map(|sql| {
        let mut statement = connection.prepare(sql).unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get(index))
                    .collect::<Result<Vec<Value>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    })
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_entity_v1_is_rejected_without_changing_authority_or_journal() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("legacy.db"));
    legacy_v1(&config.path);
    let before = legacy_image(&config.path);
    let result = application(
        config.clone(),
        Catalog::load_http_pinned().unwrap(),
        policies(),
        SqliteServiceLimits::default(),
    )
    .await;
    let rejected = matches!(result, Err(SqliteError::Incompatible));
    if let Ok(builder) = result {
        let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
        owner.ready().await.unwrap();
        close(owner).await;
    }
    assert!(rejected, "entity-only format 1 must be explicitly rejected");
    assert!(
        legacy_image(&config.path) == before,
        "legacy rows, schema, metadata, receipts, feed and journal must be unchanged"
    );
}

fn utc_sample(second: u32, value: Kind) -> HistorySample {
    sample(&format!("2024-06-01T00:00:{second:02}Z"), "UTC", value)
}
fn day(point: &str) -> HistoryReadRequest {
    HistoryReadRequest {
        id: point.into(),
        range: "2024-06-01".into(),
    }
}
fn rejected(outcome: &HistoryWriteOutcome, expected: HistoryWriteRejection) {
    assert!(
        matches!(outcome, HistoryWriteOutcome::Rejected { reason, .. } if *reason == expected),
        "{outcome:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn values_dst_ranges_quality_and_nonempty_noop_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let owner = open(config.clone()).await;
    for (id, kind) in [("numbers", "Number"), ("bools", "Bool"), ("strings", "Str")] {
        add_point(
            &owner,
            id,
            kind,
            "New_York",
            (kind == "Number").then_some("°F"),
        )
        .await;
    }
    let history = owner.handle().history_mutation_service().unwrap();
    let values = [
        (
            "numbers",
            vec![
                Kind::Number(Number::unitless(f64::NAN)),
                Kind::Number(Number::unitless(f64::NEG_INFINITY)),
                Kind::NA,
            ],
        ),
        ("bools", vec![Kind::Bool(false), Kind::Bool(true), Kind::NA]),
        (
            "strings",
            vec![
                Kind::Str("a\0雪".into()),
                Kind::Str(String::new()),
                Kind::NA,
            ],
        ),
    ];
    let timestamps = [
        "2024-11-03T01:30:00-04:00",
        "2024-11-03T01:30:00-05:00",
        "2024-11-04T00:00:00-05:00",
    ];
    let mut expected = Vec::new();
    for (id, values) in values {
        let samples: Vec<_> = timestamps
            .iter()
            .zip(values)
            .map(|(ts, value)| sample(ts, "New_York", value))
            .collect();
        let request = history_request(&history, id, "first", samples.clone());
        committed(&history.submit(context(), request).await.unwrap());
        let request = history_request(&history, id, "identical", samples);
        let outcome = history.submit(context(), request).await.unwrap();
        assert_eq!(committed(&outcome).after_generation, 2);
        let read = HistoryReadRequest {
            id: id.into(),
            range: "2024-11-03".into(),
        };
        let result = history
            .history_service()
            .collect(context(), read.clone())
            .await
            .unwrap();
        assert_eq!(result.terminal, HistoryTerminal::Complete);
        assert_eq!(
            result.samples.len(),
            2,
            "fall-back day contains both distinct 01:30 instants and excludes midnight end"
        );
        // Typed canonical bytes preserve the supported canonical NaN and infinities.
        expected.push((
            read,
            result
                .samples
                .iter()
                .map(|s| typed::encode(&s.val).unwrap())
                .collect::<Vec<_>>(),
        ));
    }
    close(owner).await;
    let owner = open(config).await;
    let history = owner.handle().history_mutation_service().unwrap();
    for (read, values) in expected {
        let result = history
            .history_service()
            .collect(context(), read)
            .await
            .unwrap();
        assert_eq!(
            result
                .samples
                .iter()
                .map(|s| typed::encode(&s.val).unwrap())
                .collect::<Vec<_>>(),
            values
        );
        assert_eq!(
            result.samples[0].ts.dt.offset().local_minus_utc(),
            -4 * 3600
        );
        assert_eq!(
            result.samples[1].ts.dt.offset().local_minus_utc(),
            -5 * 3600
        );
        assert_eq!(result.metadata.history.generation, 2);
    }
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_rejection_retention_and_ledger_suffix_are_durable() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let mut limits = SqliteServiceLimits::default();
    limits.history_store.max_items_per_point = 2;
    limits.history_store.change_capacity = 2;
    let owner = open_with_limits(config.clone(), limits.clone()).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let empty = history_request(&history, "p", "empty", vec![]);
    let empty_result = history.submit(context(), empty.clone()).await.unwrap();
    rejected(&empty_result, HistoryWriteRejection::EmptyBatch);
    assert_eq!(history.store().state("p").unwrap().generation, 0);
    assert!(history.store().retained_changes().is_empty());
    for i in 0..4 {
        let request = history_request(
            &history,
            "p",
            &format!("write-{i}"),
            vec![utc_sample(i, Kind::Bool(true))],
        );
        assert_eq!(
            committed(&history.submit(context(), request).await.unwrap()).after_generation,
            u64::from(i + 1)
        );
    }
    let before = history
        .history_service()
        .collect(context(), day("p"))
        .await
        .unwrap();
    assert_eq!(before.samples.len(), 2);
    assert_eq!(
        before
            .metadata
            .coverage
            .evicted_through
            .as_ref()
            .unwrap()
            .dt,
        utc_sample(1, Kind::NA).ts.dt
    );
    let ledger = history.store().retained_changes();
    assert_eq!(
        ledger
            .iter()
            .map(|change| change.sequence)
            .collect::<Vec<_>>(),
        [3, 4]
    );
    close(owner).await;
    let owner = open_with_limits(config, limits).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(
        history.submit(context(), empty).await.unwrap(),
        empty_result
    );
    assert_eq!(history.store().retained_changes(), ledger);
    let after = history
        .history_service()
        .collect(context(), day("p"))
        .await
        .unwrap();
    assert_eq!(after.samples, before.samples);
    assert_eq!(after.metadata.coverage, before.metadata.coverage);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_faults_seal_existing_pulls_and_reopen_proves_committed_outcomes() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let mut limits = SqliteServiceLimits::default();
    limits.history.batch_rows = 1;
    let owner = open_with_limits(config.clone(), limits.clone()).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let backend = owner
        .handle()
        .mutation_service()
        .unwrap()
        .store()
        .sqlite_backend();
    *backend.fault.lock() = Some(Fault::Constraint);
    let failed = history_request(
        &history,
        "p",
        "rollback",
        vec![utc_sample(0, Kind::Bool(false))],
    );
    let rejection = history.submit(context(), failed.clone()).await.unwrap();
    rejected(&rejection, HistoryWriteRejection::Provider);
    assert_eq!(history.store().state("p").unwrap().generation, 0);
    assert!(history.store().retained_changes().is_empty());
    *backend.fault.lock() = Some(Fault::LostAck);
    let lost = history_request(
        &history,
        "p",
        "lost",
        vec![utc_sample(1, Kind::Bool(true)), utc_sample(2, Kind::NA)],
    );
    let proven = history.submit(context(), lost.clone()).await.unwrap();
    committed(&proven);
    let mut pull = history
        .history_service()
        .open(context(), day("p"))
        .await
        .unwrap();
    assert_eq!(pull.next().await.samples.len(), 1);
    *backend.fault.lock() = Some(Fault::UnreadableProof);
    let unknown = history_request(
        &history,
        "p",
        "unreadable",
        vec![utc_sample(3, Kind::Bool(false))],
    );
    assert!(matches!(
        history.submit(context(), unknown.clone()).await.unwrap(),
        HistoryWriteOutcome::Unknown { .. }
    ));
    let terminal = pull.next().await;
    assert!(terminal.samples.is_empty());
    assert!(matches!(
        terminal.terminal,
        Some(HistoryTerminal::Interrupted(_))
    ));
    pull.close().await;
    assert!(history.store().state("p").is_err());
    close(owner).await;
    let owner = open_with_limits(config, limits).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(history.submit(context(), failed).await.unwrap(), rejection);
    assert_eq!(history.submit(context(), lost).await.unwrap(), proven);
    assert_eq!(
        committed(&history.submit(context(), unknown).await.unwrap()).after_generation,
        2
    );
    assert_eq!(history.store().len("p"), 3);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receipt_retry_and_reconcile_wait_for_history_publication() {
    let directory = tempfile::tempdir().unwrap();
    let owner = open(SqliteConfig::new(directory.path().join("history.db"))).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    let service = owner.handle().history_mutation_service().unwrap();
    let backend = owner
        .handle()
        .mutation_service()
        .unwrap()
        .store()
        .sqlite_backend();
    let retry_admission = service.read_service().begin(context()).await.unwrap();
    let proof_admission = service.read_service().begin(context()).await.unwrap();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseGate(gate.clone());
    *backend.fault.lock() = Some(Fault::AfterCommit(gate.clone()));
    let request = history_request(
        &service,
        "p",
        "publication",
        vec![utc_sample(0, Kind::Bool(true))],
    );
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
    let proof = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .reconcile_admitted(proof_admission, request.identity)
                .await
        }
    });
    wait_for_observation(&backend.publication_waits, waits + 2).await;
    assert!(!retry.is_finished());
    assert!(!proof.is_finished());
    assert_eq!(service.store().state("p").unwrap().generation, 0);
    assert!(service.store().retained_changes().is_empty());
    gate.release();
    let outcome = first.await.unwrap().unwrap();
    committed(&outcome);
    assert_eq!(retry.await.unwrap().unwrap(), outcome);
    assert_eq!(proof.await.unwrap().unwrap(), outcome);
    assert_eq!(service.store().retained_changes().len(), 1);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_history_commit_keeps_native_lease_and_owner_until_cleanup() {
    for deadline in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let config = SqliteConfig::new(directory.path().join("history.db"));
        let owner = open(config.clone()).await;
        add_point(&owner, "p", "Bool", "UTC", None).await;
        let history = owner.handle().history_mutation_service().unwrap();
        let reads = owner.handle().read_service();
        let backend = owner
            .handle()
            .mutation_service()
            .unwrap()
            .store()
            .sqlite_backend();
        let gate = Arc::new(Gate::default());
        let _release = ReleaseGate(gate.clone());
        *backend.fault.lock() = Some(Fault::BeforeCommit(gate.clone()));
        let request = history_request(
            &history,
            "p",
            "cancel",
            vec![utc_sample(0, Kind::Bool(true))],
        );
        let mut context = context();
        if deadline {
            context.deadline = Instant::now() + Duration::from_millis(300);
        }
        let cancel = context.cancellation.clone();
        let task = tokio::spawn({
            let history = history.clone();
            async move { history.submit(context, request).await }
        });
        gate.entered().await;
        if !deadline {
            cancel.cancel();
        }
        assert!(matches!(
            task.await.unwrap().unwrap(),
            HistoryWriteOutcome::Unknown { .. }
        ));
        assert_eq!(reads.load().admitted, 1);
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
        let closing = owner.close();
        tokio::pin!(closing);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut closing)
                .await
                .is_err()
        );
        gate.release();
        closing.await.unwrap();
        assert!(owner.terminated().await.cleanup_errors.is_empty());
        assert_eq!(reads.load().admitted, 0);
        assert!(
            history
                .submit(super::context(), history_request_stale(&history))
                .await
                .is_err()
        );
        let owner = open(config).await;
        let history = owner.handle().history_mutation_service().unwrap();
        assert_eq!(history.store().state("p").unwrap().generation, 0);
        assert!(history.store().is_empty("p"));
        close(owner).await;
    }
}
fn history_request_stale(service: &HistoryMutationService) -> HistoryWriteRequest {
    HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: service.store().authority(),
            incarnation: [1; 16],
            point: "p".into(),
            operation_id: "stale".into(),
        },
        expected_generation: 0,
        samples: vec![],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_busy_wait_observes_cancel_and_deadline_without_effect() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let backend = owner
        .handle()
        .mutation_service()
        .unwrap()
        .store()
        .sqlite_backend();
    let external = Connection::open(&config.path).unwrap();
    external.execute_batch("BEGIN IMMEDIATE").unwrap();
    for cancelled in [false, true] {
        let request = history_request(
            &history,
            "p",
            if cancelled { "cancel" } else { "deadline" },
            vec![utc_sample(0, Kind::Bool(true))],
        );
        let mut context = context();
        context.deadline = Instant::now() + Duration::from_millis(300);
        let token = context.cancellation.clone();
        let waits = backend
            .native_busy_waits
            .load(std::sync::atomic::Ordering::Acquire);
        let start = Instant::now();
        let task = tokio::spawn({
            let history = history.clone();
            async move { history.submit(context, request).await }
        });
        wait_for_observation(&backend.native_busy_waits, waits + 1).await;
        if cancelled {
            token.cancel();
        }
        assert!(matches!(
            task.await.unwrap().unwrap(),
            HistoryWriteOutcome::Unknown { .. }
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
        tokio::time::timeout(Duration::from_secs(2), async {
            while history.read_service().load().admitted != 0 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    }
    external.execute_batch("ROLLBACK").unwrap();
    external.close().unwrap();
    assert_eq!(history.store().state("p").unwrap().generation, 0);
    assert_eq!(history.store().receipt_count(), 0);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_history_panic_preserves_pending_and_rolls_back() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let request = history_request(
        &history,
        "p",
        "panic",
        vec![utc_sample(0, Kind::Bool(true))],
    );
    *owner
        .handle()
        .mutation_service()
        .unwrap()
        .store()
        .sqlite_backend()
        .fault
        .lock() = Some(Fault::PanicBeforeCommit);
    assert!(matches!(
        history.submit(context(), request.clone()).await.unwrap(),
        HistoryWriteOutcome::Unknown { .. }
    ));
    assert!(history.read_service().begin(context()).await.is_err());
    close(owner).await;
    let owner = open(config).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert!(matches!(
        history.submit(context(), request).await.unwrap(),
        HistoryWriteOutcome::Unknown {
            cause: HistoryWriteUnknown::Pending,
            ..
        }
    ));
    assert_eq!(history.store().state("p").unwrap().generation, 0);
    close(owner).await;
}

#[test]
fn process_crash_worker_history() {
    let Some(path) = std::env::var_os("RUSTY_HAYSTACK_SQLITE_HISTORY_CRASH_FILE") else {
        return;
    };
    let signal = std::env::var_os("RUSTY_HAYSTACK_SQLITE_HISTORY_CRASH_SIGNAL").unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let owner = open(SqliteConfig::new(path)).await;
            let history = owner.handle().history_mutation_service().unwrap();
            *owner
                .handle()
                .mutation_service()
                .unwrap()
                .store()
                .sqlite_backend()
                .fault
                .lock() = Some(Fault::ProcessCrash {
                signal: signal.into(),
                before: std::env::var_os("RUSTY_HAYSTACK_SQLITE_HISTORY_CRASH_BEFORE").is_some(),
            });
            let request = history_request(
                &history,
                "p",
                "crash",
                vec![utc_sample(0, Kind::Bool(true))],
            );
            let _ = history.submit(context(), request).await;
            panic!("parent must terminate child at native commit boundary");
        });
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_process_kill_before_and_after_native_commit_recovers_one_outcome() {
    for before in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let config = SqliteConfig::new(directory.path().join("history.db"));
        let signal = directory.path().join("boundary");
        let owner = open(config.clone()).await;
        add_point(&owner, "p", "Bool", "UTC", None).await;
        let request = history_request(
            &owner.handle().history_mutation_service().unwrap(),
            "p",
            "crash",
            vec![utc_sample(0, Kind::Bool(true))],
        );
        let entity_state = owner.handle().read_service().graph().state();
        close(owner).await;
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "sqlite::qualification::history::process_crash_worker_history",
                "--nocapture",
            ])
            .env("RUSTY_HAYSTACK_SQLITE_HISTORY_CRASH_FILE", &config.path)
            .env("RUSTY_HAYSTACK_SQLITE_HISTORY_CRASH_SIGNAL", &signal)
            .env_remove("RUSTY_HAYSTACK_SQLITE_HISTORY_CRASH_BEFORE")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if before {
            command.env("RUSTY_HAYSTACK_SQLITE_HISTORY_CRASH_BEFORE", "1");
        }
        let mut child = command.spawn().unwrap();
        let reached = tokio::time::timeout(Duration::from_secs(15), async {
            while !signal.exists() {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "child exited before native boundary"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        let _ = child.kill();
        let status = child.wait().unwrap();
        reached.expect("child reached native boundary");
        assert!(!status.success());
        let owner = open(config).await;
        let history = owner.handle().history_mutation_service().unwrap();
        let proof = history
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap();
        if before {
            assert!(matches!(
                proof,
                HistoryWriteOutcome::Unknown {
                    cause: HistoryWriteUnknown::Pending,
                    ..
                }
            ));
        } else {
            assert_eq!(committed(&proof).after_generation, 1);
        }
        assert_eq!(history.submit(context(), request).await.unwrap(), proof);
        assert_eq!(
            history.store().state("p").unwrap().generation,
            u64::from(!before)
        );
        assert_eq!(history.store().len("p"), usize::from(!before));
        assert_eq!(
            history.store().retained_changes().len(),
            usize::from(!before)
        );
        assert_eq!(owner.handle().read_service().graph().state(), entity_state);
        close(owner).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_recovery_uses_original_deadline_and_cleans_up_on_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = SqliteConfig::new(directory.path().join("history.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let request = history_request(
        &history,
        "p",
        "persist",
        vec![utc_sample(0, Kind::Bool(true))],
    );
    committed(&history.submit(context(), request).await.unwrap());
    close(owner).await;
    let gate = Arc::new(Gate::default());
    let _release = ReleaseGate(gate.clone());
    config.history_recovery_start = Some(gate.clone());
    config.limits.operation_timeout = Duration::from_millis(400);
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
    assert!(!construction.is_finished());
    gate.release();
    assert!(matches!(
        construction.await.unwrap(),
        Err(SqliteError::Deadline)
    ));
    config.history_recovery_start = None;
    config.limits.operation_timeout = Duration::from_secs(5);
    let owner = open(config).await;
    assert_eq!(
        owner
            .handle()
            .history_mutation_service()
            .unwrap()
            .store()
            .len("p"),
        1
    );
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_live_pulls_have_no_native_transaction_and_stop_on_generation_change() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let mut limits = SqliteServiceLimits::default();
    limits.history.batch_rows = 1;
    let owner = open_with_limits(config.clone(), limits).await;
    add_point(&owner, "p", "Str", "UTC", None).await;
    add_point(&owner, "other", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let request = history_request(
        &history,
        "p",
        "first",
        (0..4)
            .map(|i| utc_sample(i, Kind::Str("bounded".repeat(100))))
            .collect(),
    );
    committed(&history.submit(context(), request).await.unwrap());
    let mut session = history
        .history_service()
        .open(context(), day("p"))
        .await
        .unwrap();
    assert_eq!(
        session.metadata.capabilities,
        HistoryCapabilities::BOUNDED_LIVE
    );
    assert_eq!(session.next().await.samples.len(), 1);
    // Calibrate the no-reader oracle: pin the nonempty snapshot produced by
    // the first commit, then put newer frames in the WAL through a real write.
    // BEGIN EXCLUSIVE alone cannot detect this reader in WAL mode.
    let pinned = Connection::open(&config.path).unwrap();
    pinned.execute_batch("BEGIN").unwrap();
    assert_eq!(
        pinned
            .query_row(
                "SELECT count(*) FROM history_samples WHERE point='p'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        4
    );
    let checkpoint = Connection::open(&config.path).unwrap();
    checkpoint.busy_timeout(Duration::from_millis(20)).unwrap();
    let checkpoint_state = || {
        checkpoint
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .unwrap()
    };
    let other = history_request(
        &history,
        "other",
        "new-wal-frames",
        vec![utc_sample(0, Kind::Bool(true))],
    );
    committed(&history.submit(context(), other).await.unwrap());
    let blocked = checkpoint_state();
    pinned.execute_batch("ROLLBACK").unwrap();
    pinned.close().unwrap();
    assert_eq!(
        blocked.0, 1,
        "positive control must detect a pinned WAL reader"
    );
    assert!(blocked.1 > blocked.2 && blocked.2 > 0, "{blocked:?}");
    assert_eq!(
        checkpoint_state(),
        (0, 0, 0),
        "consumer history session must not retain a WAL reader between pulls"
    );
    assert_eq!(session.next().await.samples.len(), 1);
    assert_eq!(checkpoint_state(), (0, 0, 0));
    println!(
        "WAL reader positive control busy/log/checkpointed={blocked:?}; between-pull checkpoints=(0,0,0)"
    );
    checkpoint.close().unwrap();
    let request = history_request(
        &history,
        "p",
        "changed",
        vec![utc_sample(3, Kind::Str("replaced".into()))],
    );
    committed(&history.submit(context(), request).await.unwrap());
    let batch = session.next().await;
    assert!(
        batch.samples.is_empty(),
        "no prefetched old rows escape generation change"
    );
    assert_eq!(
        batch.terminal,
        Some(HistoryTerminal::Interrupted(
            HistoryReason::GenerationChanged
        ))
    );
    session.close().await;
    assert_eq!(history.history_service().active_sessions(), 0);
    // Exercise source-side admission directly, before any sample cloning.
    let budget = HistoryPullBudget {
        max_rows: 1,
        max_bytes: 512,
        max_work: 512,
        deadline: Instant::now() + Duration::from_secs(5),
        cancellation: CancellationToken::new(),
    };
    let store = history.store();
    let mut source = HistoryProvider::open(
        &store,
        "p".into(),
        utc_sample(0, Kind::NA).ts.dt,
        utc_sample(59, Kind::NA).ts.dt,
        budget.clone(),
    )
    .await
    .unwrap();
    let batch = source
        .pull(HistoryPullBudget {
            max_bytes: 1,
            max_work: 1,
            ..budget
        })
        .await
        .unwrap();
    assert!(batch.items.is_empty());
    assert_eq!(
        batch.terminal,
        Some(HistoryTerminal::Limited(HistoryReason::Bytes))
    );
    source.close().await.unwrap();
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_intent_is_reauthorized_after_point_removal_and_stale_generation_rejects() {
    struct Deny;
    impl HistoryMutationPolicy for Deny {
        fn authorize_intent(&self, _: &Principal, _: &HistoryWriteRequest) -> bool {
            false
        }
        fn authorize_schema(
            &self,
            _: &Principal,
            _: &HistoryWriteRequest,
            _: &HistorySchema,
        ) -> bool {
            false
        }
        fn authorize_reconcile(
            &self,
            _: &Principal,
            _: &HistoryOperationIdentity,
            _: Option<&HistoryWriteRequest>,
        ) -> bool {
            false
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "p", "Number", "UTC", Some("°F")).await;
    let history = owner.handle().history_mutation_service().unwrap();
    for (i, value) in [
        Kind::Int(1),
        Kind::Null,
        Kind::Number(Number::unitless(f64::from_bits(0x7ff8_0000_0000_0042))),
    ]
    .into_iter()
    .enumerate()
    {
        let request = history_request(
            &history,
            "p",
            &format!("unsupported-{i}"),
            vec![utc_sample(0, value)],
        );
        rejected(
            &history.submit(context(), request).await.unwrap(),
            HistoryWriteRejection::Unsupported,
        );
    }
    let request = history_request(
        &history,
        "p",
        "original",
        vec![utc_sample(0, Kind::Number(Number::unitless(-0.0)))],
    );
    let original = history.submit(context(), request.clone()).await.unwrap();
    committed(&original);
    let mut stale = request.clone();
    stale.identity.operation_id = "stale".into();
    rejected(
        &history.submit(context(), stale).await.unwrap(),
        HistoryWriteRejection::Conflict,
    );
    let entities = owner.handle().mutation_service().unwrap();
    let delete = super::request(
        &entities,
        "remove-p",
        vec![EntityOperation::Remove { id: "p".into() }],
    );
    assert!(matches!(
        entities.submit(context(), delete).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    assert_eq!(
        history.submit(context(), request.clone()).await.unwrap(),
        original
    );
    history.replace_policy(Arc::new(Deny));
    rejected(
        &history.submit(context(), request.clone()).await.unwrap(),
        HistoryWriteRejection::Forbidden,
    );
    close(owner).await;
    let owner = open(config).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(
        history
            .reconcile(context(), request.identity)
            .await
            .unwrap(),
        original
    );
    close(owner).await;
}

fn encoded_history_outcome(outcome: &HistoryWriteOutcome) -> Vec<u8> {
    typed::encode(&Kind::Grid(Box::new(
        haystack_core::codecs::history_mutation::outcome_grid(outcome).unwrap(),
    )))
    .unwrap()
}

/// Defers only the real prepared plan. Durable binding/publication still run
/// through the selected native SQLite authority.
struct HeldHistory {
    store: HisStore,
    held: parking_lot::Mutex<Vec<PreparedHistoryMutation>>,
}
impl HistoryProvider for HeldHistory {
    fn open(
        &self,
        id: String,
        start: chrono::DateTime<chrono::FixedOffset>,
        end: chrono::DateTime<chrono::FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        self.store.open(id, start, end, budget)
    }
    fn history_write_capability(&self) -> Option<HistoryWriteCapability> {
        self.store.history_write_capability()
    }
    fn commit_history(&self, prepared: PreparedHistoryMutation) -> HistoryWriteOutcome {
        let identity = prepared.identity().clone();
        self.held.lock().push(prepared);
        HistoryWriteOutcome::Unknown {
            identity,
            cause: HistoryWriteUnknown::Pending,
        }
    }
}
fn deferred_history(owner: &ApplicationOwner) -> (HistoryMutationService, Arc<HeldHistory>) {
    let provider = Arc::new(HeldHistory {
        store: owner.handle().history_mutation_service().unwrap().store(),
        held: Default::default(),
    });
    let reads = HistoryService::new(
        owner.handle().read_service(),
        provider.clone(),
        HistoryLimits::default(),
    )
    .unwrap();
    (
        HistoryMutationService::new(
            reads,
            Arc::new(AllowAllHistoryMutations),
            HistoryMutationLimits::default(),
        )
        .unwrap(),
        provider,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_native_sample_byte_work_and_recovery_limits_fail_without_partial_effect() {
    for bound in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let mut config = SqliteConfig::new(directory.path().join("history.db"));
        match bound {
            0 => config.limits.max_history_samples = 1,
            1 => config.limits.max_history_bytes = 1,
            _ => config.limits.max_history_work = 1,
        }
        let owner = open(config.clone()).await;
        add_point(&owner, "p", "Str", "UTC", None).await;
        let history = owner.handle().history_mutation_service().unwrap();
        let request = history_request(
            &history,
            "p",
            "over-cap",
            vec![
                utc_sample(0, Kind::Str("large".repeat(50))),
                utc_sample(1, Kind::NA),
            ],
        );
        let result = history.submit(context(), request.clone()).await.unwrap();
        assert!(
            !matches!(result, HistoryWriteOutcome::Committed(_)),
            "bound {bound}"
        );
        assert_eq!(history.store().state("p").unwrap().generation, 0);
        assert!(history.store().retained_changes().is_empty());
        close(owner).await;
        let owner = open(config).await;
        let history = owner.handle().history_mutation_service().unwrap();
        assert_eq!(history.store().state("p").unwrap().generation, 0);
        assert!(history.store().is_empty("p"));
        close(owner).await;
    }
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("recover.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "p", "Str", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let request = history_request(
        &history,
        "p",
        "seed",
        vec![
            utc_sample(0, Kind::Str("bounded".repeat(200))),
            utc_sample(1, Kind::NA),
        ],
    );
    committed(&history.submit(context(), request).await.unwrap());
    close(owner).await;
    for bound in 0..6 {
        let mut config = config.clone();
        let mut limits = SqliteServiceLimits::default();
        match bound {
            0 => config.limits.max_history_samples = 1,
            1 => config.limits.max_history_bytes = 1,
            2 => config.limits.max_history_work = 1,
            3 => config.limits.max_receipt_bytes = 1,
            4 => limits.history_store.receipt_bytes = 1,
            _ => limits.history_store.max_items_per_point = 1,
        }
        assert!(
            matches!(
                application(
                    config,
                    Catalog::load_http_pinned().unwrap(),
                    policies(),
                    limits
                )
                .await,
                Err(SqliteError::Limit)
            ),
            "recovery bound {bound}"
        );
    }
    close(open(config).await).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn final_history_receipt_growth_keeps_durable_pending_with_same_config_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = SqliteConfig::new(directory.path().join("history.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "p", "Str", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let request = history_request(
        &history,
        "p",
        "tight-cap",
        vec![utc_sample(0, Kind::Str("x".repeat(4096)))],
    );
    close(owner).await;
    let key = typed::encode(&Kind::List(vec![
        Kind::Str("embedding".into()),
        Kind::Str("sqlite-owner".into()),
        Kind::Grid(Box::new(
            haystack_core::codecs::history_mutation::lookup_grid(&request.identity).unwrap(),
        )),
    ]))
    .unwrap();
    let pending = HistoryWriteOutcome::Unknown {
        identity: request.identity.clone(),
        cause: HistoryWriteUnknown::Pending,
    };
    let rejected = HistoryWriteOutcome::Rejected {
        identity: request.identity.clone(),
        reason: HistoryWriteRejection::Cancelled,
    };
    let pending_bytes = encoded_history_outcome(&pending).len();
    assert!(encoded_history_outcome(&rejected).len() > pending_bytes);
    config.limits.max_receipt_bytes = key.len()
        + haystack_core::codecs::history_mutation::canonical_request(&request)
            .unwrap()
            .len()
        + pending_bytes;
    let owner = open(config.clone()).await;
    let (history, deferred) = deferred_history(&owner);
    assert_eq!(
        history.submit(context(), request.clone()).await.unwrap(),
        pending
    );
    let prepared = deferred.held.lock().pop().expect("durably reserved plan");
    assert_eq!(prepared.reject(HistoryWriteRejection::Cancelled), pending);
    assert_eq!(history.store().state("p").unwrap().generation, 0);
    let native = Connection::open(&config.path).unwrap();
    let bytes: i64 = native
        .query_row(
            "SELECT sum(length(binding)+length(request)+length(outcome)) FROM history_receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bytes as usize, config.limits.max_receipt_bytes);
    native.close().unwrap();
    close(owner).await;
    let owner = open(config).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(
        history
            .reconcile(context(), request.identity.clone())
            .await
            .unwrap(),
        pending
    );
    assert_eq!(history.submit(context(), request).await.unwrap(), pending);
    assert_eq!(history.store().state("p").unwrap().generation, 0);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_history_projection_payload_ledger_receipt_and_versions_fail_closed() {
    for damage in 0..12 {
        let directory = tempfile::tempdir().unwrap();
        let config = SqliteConfig::new(directory.path().join("history.db"));
        let owner = open(config.clone()).await;
        add_point(&owner, "p", "Bool", "UTC", None).await;
        let history = owner.handle().history_mutation_service().unwrap();
        let request = history_request(&history, "p", "seed", vec![utc_sample(0, Kind::Bool(true))]);
        let outcome = history.submit(context(), request.clone()).await.unwrap();
        committed(&outcome);
        close(owner).await;
        let native = Connection::open(&config.path).unwrap();
        match damage {
            0 => {
                native
                    .execute("UPDATE history_samples SET seconds=seconds+1", [])
                    .unwrap();
            }
            1 => {
                native
                    .execute("UPDATE history_samples SET payload=x'00'", [])
                    .unwrap();
            }
            2 => {
                native
                    .execute("UPDATE history_points SET incarnation=zeroblob(16)", [])
                    .unwrap();
            }
            3 => {
                native
                    .execute(
                        "UPDATE history_changes SET sequence=?1",
                        [2u64.to_be_bytes().as_slice()],
                    )
                    .unwrap();
            }
            4 => {
                native.execute("DELETE FROM history_changes", []).unwrap();
            }
            5 => {
                let mut changed = outcome.clone();
                let HistoryWriteOutcome::Committed(receipt) = &mut changed else {
                    unreachable!()
                };
                receipt.qualification = HistoryReceiptQualification::EphemeralMemory;
                native
                    .execute(
                        "UPDATE history_receipts SET outcome=?1",
                        [encoded_history_outcome(&changed)],
                    )
                    .unwrap();
            }
            6 => {
                let mut changed = request;
                changed.identity.operation_id = "foreign".into();
                native
                    .execute(
                        "UPDATE history_receipts SET request=?1",
                        [
                            haystack_core::codecs::history_mutation::canonical_request(&changed)
                                .unwrap(),
                        ],
                    )
                    .unwrap();
            }
            7 => {
                native.pragma_update(None, "user_version", 99).unwrap();
            }
            8 => {
                native
                    .execute("UPDATE metadata SET history_authority=zeroblob(16)", [])
                    .unwrap();
            }
            9 => {
                native
                    .pragma_update(None, "ignore_check_constraints", true)
                    .unwrap();
                native
                    .execute("UPDATE history_samples SET payload=zeroblob(1048577)", [])
                    .unwrap();
            }
            10 => {
                let mut changed = outcome;
                let HistoryWriteOutcome::Committed(receipt) = &mut changed else {
                    unreachable!()
                };
                receipt.retained_samples += 1;
                native
                    .execute(
                        "UPDATE history_receipts SET outcome=?1",
                        [encoded_history_outcome(&changed)],
                    )
                    .unwrap();
            }
            _ => {
                let bytes: Vec<u8> = native
                    .query_row("SELECT payload FROM history_changes", [], |row| row.get(0))
                    .unwrap();
                let mut change = crate::history_storage_value::decode_change(&bytes).unwrap();
                change.operation.as_mut().unwrap().operation_id.clear();
                native
                    .execute(
                        "UPDATE history_changes SET payload=?1",
                        [crate::history_storage_value::encode_change(&change).unwrap()],
                    )
                    .unwrap();
            }
        }
        native.close().unwrap();
        let result = application(
            config.clone(),
            Catalog::load_http_pinned().unwrap(),
            policies(),
            SqliteServiceLimits::default(),
        )
        .await;
        assert!(
            matches!(result, Err(SqliteError::Incompatible | SqliteError::Limit)),
            "damage {damage} must reject"
        );
        // Failed recovery releases ownership, preserves rows and never resets authority.
        let native = Connection::open(&config.path).unwrap();
        assert_eq!(
            native
                .query_row("SELECT count(*) FROM history_points", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            native
                .query_row("SELECT count(*) FROM history_samples", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        native.close().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_u64_generation_and_sequence_roundtrip_and_overflow_reject() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    add_point(&owner, "q", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let mut request = history_request(&history, "p", "seed", vec![utc_sample(0, Kind::Bool(true))]);
    let mut outcome = history.submit(context(), request.clone()).await.unwrap();
    committed(&outcome);
    let mut change = history.store().retained_changes().pop().unwrap();
    close(owner).await;
    let before = u64::MAX - 1;
    request.expected_generation = before - 1;
    let HistoryWriteOutcome::Committed(receipt) = &mut outcome else {
        unreachable!()
    };
    receipt.before_generation = before - 1;
    receipt.after_generation = before;
    receipt.change_sequence = before;
    change.before.generation = before - 1;
    change.after.generation = before;
    change.sequence = before;
    let native = Connection::open(&config.path).unwrap();
    native
        .execute(
            "UPDATE metadata SET history_sequence=?1",
            [before.to_be_bytes().as_slice()],
        )
        .unwrap();
    native
        .execute(
            "UPDATE history_points SET generation=?1",
            [before.to_be_bytes().as_slice()],
        )
        .unwrap();
    native
        .execute(
            "UPDATE history_changes SET sequence=?1,payload=?2",
            params![
                before.to_be_bytes().as_slice(),
                crate::history_storage_value::encode_change(&change).unwrap()
            ],
        )
        .unwrap();
    native
        .execute(
            "UPDATE history_receipts SET request=?1,outcome=?2",
            params![
                haystack_core::codecs::history_mutation::canonical_request(&request).unwrap(),
                encoded_history_outcome(&outcome)
            ],
        )
        .unwrap();
    native.close().unwrap();
    let owner = open(config.clone()).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(history.submit(context(), request).await.unwrap(), outcome);
    let request = history_request(
        &history,
        "p",
        "last",
        vec![utc_sample(1, Kind::Bool(false))],
    );
    let final_outcome = history.submit(context(), request.clone()).await.unwrap();
    let receipt = committed(&final_outcome);
    assert_eq!(
        (receipt.after_generation, receipt.change_sequence),
        (u64::MAX, u64::MAX)
    );
    close(owner).await;
    let owner = open(config).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(
        history.submit(context(), request).await.unwrap(),
        final_outcome
    );
    for point in ["p", "q"] {
        let request = history_request(
            &history,
            point,
            "overflow",
            vec![utc_sample(2, Kind::Bool(true))],
        );
        rejected(
            &history.submit(context(), request).await.unwrap(),
            HistoryWriteRejection::Limit,
        );
    }
    assert_eq!(history.store().state("p").unwrap().generation, u64::MAX);
    assert_eq!(history.store().state("q").unwrap().generation, 0);
    assert_eq!(
        history.store().retained_changes().last().unwrap().sequence,
        u64::MAX
    );
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_native_history_rechecks_graph_policy_point_and_owner_fences() {
    for action in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let config = SqliteConfig::new(directory.path().join("history.db"));
        let owner = open(config.clone()).await;
        add_point(&owner, "p", "Bool", "UTC", None).await;
        let (history, deferred) = deferred_history(&owner);
        let request = history_request(
            &history,
            "p",
            "delayed",
            vec![utc_sample(0, Kind::Bool(true))],
        );
        assert!(matches!(
            history.submit(context(), request.clone()).await.unwrap(),
            HistoryWriteOutcome::Unknown {
                cause: HistoryWriteUnknown::Pending,
                ..
            }
        ));
        assert_eq!(
            history.read_service().load().admitted,
            1,
            "prepared plan retains native admission"
        );
        match action {
            0 => {
                let entities = owner.handle().mutation_service().unwrap();
                let mut changes = HDict::new();
                changes.set("tz", Kind::Str("GMT".into()));
                let request = super::request(
                    &entities,
                    "graph-change",
                    vec![EntityOperation::Patch {
                        id: "p".into(),
                        changes,
                    }],
                );
                assert!(matches!(
                    entities.submit(context(), request).await.unwrap(),
                    MutationOutcome::Committed(_)
                ));
            }
            1 => history.replace_policy(Arc::new(AllowAllHistoryMutations)),
            2 => {
                let selected = owner.handle().history_mutation_service().unwrap();
                let request = history_request(
                    &selected,
                    "p",
                    "other-write",
                    vec![utc_sample(1, Kind::Bool(false))],
                );
                committed(&selected.submit(context(), request).await.unwrap());
            }
            _ => owner.request_close(),
        }
        let prepared = deferred.held.lock().pop().unwrap();
        let outcome = prepared.publish();
        rejected(
            &outcome,
            if action == 3 {
                HistoryWriteRejection::Cancelled
            } else {
                HistoryWriteRejection::Conflict
            },
        );
        assert_eq!(history.store().len("p"), usize::from(action == 2));
        assert_eq!(history.read_service().load().admitted, 0);
        close(owner).await;
        let owner = open(config).await;
        let history = owner.handle().history_mutation_service().unwrap();
        assert_eq!(
            history
                .reconcile(context(), request.identity)
                .await
                .unwrap(),
            outcome
        );
        assert_eq!(
            history.store().state("p").unwrap().generation,
            u64::from(action == 2)
        );
        close(owner).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preadmitted_history_reads_drain_but_mutation_publication_is_fenced() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
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
    add_point(&owner, "p", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let request = history_request(
        &history,
        "p",
        "first",
        vec![utc_sample(0, Kind::Bool(true))],
    );
    committed(&history.submit(context(), request).await.unwrap());
    let handle = owner.handle();
    let read_guard = handle.admit().unwrap();
    let mutation_guard = handle.admit().unwrap();
    let request = history_request(
        &history,
        "p",
        "fenced",
        vec![utc_sample(1, Kind::Bool(false))],
    );
    owner.request_close();
    assert!(!handle.cancellation().is_cancelled());
    assert!(
        history
            .history_service()
            .open(context(), day("p"))
            .await
            .is_err()
    );
    let admission = history
        .read_service()
        .begin_admitted(context(), read_guard)
        .await
        .unwrap();
    let mut read = history
        .history_service()
        .open_admitted(admission, day("p"))
        .await
        .unwrap();
    assert_eq!(read.next().await.samples.len(), 1);
    read.close().await;
    let admission = history
        .read_service()
        .begin_admitted(context(), mutation_guard)
        .await
        .unwrap();
    assert!(!matches!(
        history.submit_admitted(admission, request).await.unwrap(),
        HistoryWriteOutcome::Committed(_)
    ));
    assert_eq!(history.store().state("p").unwrap().generation, 1);
    assert!(!owner.close().await.unwrap().drain_expired);
    assert!(owner.terminated().await.cleanup_errors.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_receipt_and_point_capacity_have_no_cross_domain_consumption() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("history.db"));
    let mut limits = SqliteServiceLimits::default();
    limits.history_store.max_points = 1;
    limits.history_store.receipt_capacity = 1;
    let owner = open_with_limits(config.clone(), limits.clone()).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let request = history_request(
        &history,
        "p",
        "first",
        vec![utc_sample(0, Kind::Bool(true))],
    );
    let outcome = history.submit(context(), request.clone()).await.unwrap();
    committed(&outcome);
    assert_eq!(history.store().state("q"), Err(HistoryProviderError::Limit));
    let excess = history_request(
        &history,
        "p",
        "excess",
        vec![utc_sample(1, Kind::Bool(false))],
    );
    rejected(
        &history.submit(context(), excess).await.unwrap(),
        HistoryWriteRejection::Capacity,
    );
    assert_eq!(history.store().state("p").unwrap().generation, 1);
    close(owner).await;
    let owner = open_with_limits(config, limits).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(history.submit(context(), request).await.unwrap(), outcome);
    assert_eq!(history.store().receipt_count(), 1);
    assert_eq!(history.store().point_count(), 1);
    close(owner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_f1_minute_leaps_and_neighbors_upsert_read_and_reopen_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("leap.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "p", "Bool", "UTC", None).await;
    add_point(&owner, "negative", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let leap_sample = |seconds, nanos| HistorySample {
        ts: HDateTime::new(
            chrono::DateTime::from_timestamp(seconds, nanos)
                .unwrap()
                .fixed_offset(),
            "UTC",
        ),
        val: Kind::Bool(false),
    };
    let mut samples = vec![leap_sample(1_483_228_799, 999_999_999)];
    samples.extend(
        [
            1_000_000_000,
            1_000_000_001,
            1_123_456_789,
            1_500_000_000,
            1_999_999_999,
        ]
        .into_iter()
        .map(|nanos| leap_sample(1_483_228_799, nanos)),
    );
    samples.push(leap_sample(1_483_228_800, 0));
    let first = history_request(&history, "p", "leaps", samples.clone());
    let first_outcome = history.submit(context(), first.clone()).await.unwrap();
    let receipt = committed(&first_outcome);
    assert_eq!((receipt.unique_samples, receipt.retained_samples), (7, 7));
    let mut replacement = leap_sample(1_483_228_799, 1_500_000_000);
    replacement.val = Kind::Bool(true);
    let duplicate = history_request(
        &history,
        "p",
        "replace-one-leap",
        vec![
            leap_sample(1_483_228_799, 1_500_000_000),
            replacement.clone(),
        ],
    );
    let updated = history.submit(context(), duplicate.clone()).await.unwrap();
    assert_eq!(
        (
            committed(&updated).unique_samples,
            committed(&updated).retained_samples
        ),
        (1, 7)
    );
    samples[4] = replacement;
    let read = HistoryReadRequest {
        id: "p".into(),
        range: "2016-12-31,2017-01-01".into(),
    };
    let before = history
        .history_service()
        .collect(context(), read.clone())
        .await
        .unwrap();
    assert_eq!(before.terminal, HistoryTerminal::Complete);
    assert_eq!(before.samples, samples);
    assert_eq!(
        before.samples[4].ts.dt.timestamp_subsec_nanos(),
        1_500_000_000
    );
    // Negative Unix seconds use the same minute-end condition even though
    // SQLite's remainder is negative. This is an admitted calendar encoding.
    let negative = vec![
        leap_sample(-1, 999_999_999),
        leap_sample(-1, 1_500_000_000),
        leap_sample(0, 0),
    ];
    let negative_request = history_request(&history, "negative", "negative-leap", negative.clone());
    committed(&history.submit(context(), negative_request).await.unwrap());
    close(owner).await;
    let owner = open(config.clone()).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(
        history.submit(context(), first).await.unwrap(),
        first_outcome
    );
    assert_eq!(history.submit(context(), duplicate).await.unwrap(), updated);
    let after = history
        .history_service()
        .collect(context(), read)
        .await
        .unwrap();
    assert_eq!(after.samples, samples);
    assert_eq!(after.metadata.history.generation, 2);
    let recovered_negative = history
        .history_service()
        .collect(
            context(),
            HistoryReadRequest {
                id: "negative".into(),
                range: "1969-12-31,1970-01-01".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered_negative.samples, negative);
    close(owner).await;
    let native = Connection::open(&config.path).unwrap();
    // Invalid excess nanoseconds away from minute end remain rejected by SQL.
    assert!(
        native
            .execute(
                "UPDATE history_samples SET nanos=1000000000 WHERE seconds=1483228800",
                []
            )
            .is_err()
    );
    native.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_f2_evicted_ledger_point_tip_requires_latest_receipt_proof() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("tip.db"));
    let mut limits = SqliteServiceLimits::default();
    limits.history_store.change_capacity = 1;
    let owner = open_with_limits(config.clone(), limits.clone()).await;
    add_point(&owner, "a", "Bool", "UTC", None).await;
    add_point(&owner, "b", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    for point in ["a", "b"] {
        let request = history_request(
            &history,
            point,
            "first",
            vec![utc_sample(0, Kind::Bool(true))],
        );
        assert_eq!(
            committed(&history.submit(context(), request).await.unwrap()).after_generation,
            1
        );
    }
    let ledger = history.store().retained_changes();
    assert_eq!(ledger.len(), 1);
    assert_eq!((&*ledger[0].point, ledger[0].sequence), ("b", 2));
    close(owner).await;
    // A's valid persisted tip remains recoverable even though its ledger aged out.
    let owner = open_with_limits(config.clone(), limits.clone()).await;
    let history = owner.handle().history_mutation_service().unwrap();
    assert_eq!(history.store().state("a").unwrap().generation, 1);
    assert_eq!(history.store().state("b").unwrap().generation, 1);
    close(owner).await;
    let native = Connection::open(&config.path).unwrap();
    let before: Vec<u8> = native
        .query_row("SELECT history_sequence FROM metadata", [], |row| {
            row.get(0)
        })
        .unwrap();
    native
        .execute(
            "UPDATE history_points SET generation=?1 WHERE id='a'",
            [2u64.to_be_bytes().as_slice()],
        )
        .unwrap();
    native.close().unwrap();
    let recovered = application(
        config.clone(),
        Catalog::load_http_pinned().unwrap(),
        policies(),
        limits,
    )
    .await;
    let rejected = matches!(recovered, Err(SqliteError::Incompatible));
    drop(recovered);
    assert!(
        rejected,
        "a generation without a committed tip receipt must fail after ledger eviction"
    );
    let native = Connection::open(&config.path).unwrap();
    assert_eq!(
        native
            .query_row(
                "SELECT generation FROM history_points WHERE id='a'",
                [],
                |row| row.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
        2u64.to_be_bytes()
    );
    assert_eq!(
        native
            .query_row("SELECT history_sequence FROM metadata", [], |row| row
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        native
            .query_row("SELECT count(*) FROM history_receipts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    native.close().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_f3_inflight_open_rejects_after_other_point_seals_native_authority() {
    r1_f3_inflight(false).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r1_f3_inflight_pull_rejects_after_other_point_seals_native_authority() {
    r1_f3_inflight(true).await;
}
async fn r1_f3_inflight(pull: bool) {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteConfig::new(directory.path().join("seal.db"));
    let owner = open(config.clone()).await;
    add_point(&owner, "a", "Bool", "UTC", None).await;
    add_point(&owner, "b", "Bool", "UTC", None).await;
    let history = owner.handle().history_mutation_service().unwrap();
    let request = history_request(
        &history,
        "a",
        "first",
        vec![utc_sample(0, Kind::Bool(true))],
    );
    committed(&history.submit(context(), request).await.unwrap());
    let session = if pull {
        Some(
            history
                .history_service()
                .open(context(), day("a"))
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let store = history.store();
    let point = store.inner.state.lock().points.get("a").unwrap().clone();
    let backend = owner
        .handle()
        .mutation_service()
        .unwrap()
        .store()
        .sqlite_backend();
    let gate = Arc::new(Gate::default());
    let _release = ReleaseGate(gate.clone());
    let holding = tokio::task::spawn_blocking({
        let gate = gate.clone();
        move || {
            let _point = point.write();
            gate.hold();
        }
    });
    gate.entered().await;
    let counter = if pull {
        &backend.history_pull_waits
    } else {
        &backend.history_open_waits
    };
    let waits = counter.load(std::sync::atomic::Ordering::Acquire);
    let reading = tokio::spawn({
        let service = history.history_service().clone();
        async move {
            if let Some(mut session) = session {
                let batch = session.next().await;
                session.close().await;
                batch.samples.is_empty()
                    && matches!(batch.terminal, Some(HistoryTerminal::Interrupted(_)))
            } else {
                match service.open(context(), day("a")).await {
                    Ok(mut session) => {
                        session.close().await;
                        false
                    }
                    Err(_) => true,
                }
            }
        }
    });
    wait_for_observation(counter, waits + 1).await;
    assert!(
        !reading.is_finished(),
        "source operation is observed inside the point wait"
    );
    *backend.fault.lock() = Some(Fault::UnreadableProof);
    let request = history_request(
        &history,
        "b",
        "uncertain",
        vec![utc_sample(1, Kind::Bool(false))],
    );
    assert!(matches!(
        history.submit(context(), request.clone()).await.unwrap(),
        HistoryWriteOutcome::Unknown { .. }
    ));
    assert!(
        backend.check().is_err(),
        "actual native commit sealed the authority"
    );
    gate.release();
    holding.await.unwrap();
    let rejected = reading.await.unwrap();
    assert!(
        rejected,
        "in-flight source must not return a successful open or samples after a backend seal"
    );
    assert_eq!(history.history_service().active_sessions(), 0);
    close(owner).await;
    let owner = open(config).await;
    committed(
        &owner
            .handle()
            .history_mutation_service()
            .unwrap()
            .reconcile(context(), request.identity)
            .await
            .unwrap(),
    );
    close(owner).await;
}
