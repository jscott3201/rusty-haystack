//! Actual OS application instances controlled by bounded, private pipe IPC.
//! Commands exercise application services; SQL is used only for independent
//! assertions and explicit fault/migration/restore boundaries.
use super::*;
use haystack_core::{
    codecs::{entity, subscription as watch_wire},
    kinds::{HDateTime, Number},
};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};

const CHILD: &str = "RUSTY_HAYSTACK_PG_PROCESS_SCHEMA";
const PREFIX: &str = "RH_PROCESS ";
const TEST: &str = "postgres::qualification::process::two_processes_recover_shared_entities_history_receipts_and_subscriptions";

struct Process {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    replies: std::sync::mpsc::Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}
impl Process {
    fn start(config: &PostgresConfig, interval: u64) -> Self {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(TEST)
            .arg("--ignored")
            .arg("--nocapture")
            .env(CHILD, &config.schema)
            .env("RUSTY_HAYSTACK_PG_PROCESS_HOST", &config.host)
            .env("RUSTY_HAYSTACK_PG_PROCESS_PORT", config.port.to_string())
            .env("RUSTY_HAYSTACK_PG_PROCESS_INTERVAL", interval.to_string())
            .env(
                "RUSTY_HAYSTACK_PG_PROCESS_RETENTION",
                config.limits.recovery.max_changes.to_string(),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take().unwrap();
        let (send, replies) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut output = std::io::BufReader::new(output);
            loop {
                let mut line = String::new();
                if output
                    .by_ref()
                    .take(1_048_577)
                    .read_line(&mut line)
                    .unwrap_or(0)
                    == 0
                {
                    break;
                }
                if line.len() > 1_048_576 {
                    break;
                }
                if let Some(payload) = line.strip_prefix(PREFIX) {
                    let Ok(value) = serde_json::from_str(payload) else {
                        break;
                    };
                    if send.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            child,
            input,
            replies,
            reader: Some(reader),
        }
    }
    fn ready(&self) -> Value {
        self.receive()
    }
    fn receive(&self) -> Value {
        self.replies
            .recv_timeout(Duration::from_secs(20))
            .expect("bounded child response")
    }
    fn call(&mut self, command: Value) -> Value {
        let bytes = serde_json::to_vec(&command).unwrap();
        assert!(bytes.len() <= 1_048_576);
        let input = self.input.as_mut().unwrap();
        input.write_all(&bytes).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
        self.receive()
    }
    fn stop(&mut self) {
        assert_eq!(self.call(json!({"op":"stop"}))["stopped"], true);
        self.input.take();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "bounded child shutdown"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.input.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn emit(value: Value) {
    let bytes = serde_json::to_string(&value).unwrap();
    assert!(bytes.len() <= 1_048_576);
    println!("{PREFIX}{bytes}");
    std::io::stdout().flush().unwrap();
}
#[derive(Default)]
struct HoldEntity(parking_lot::Mutex<Option<PreparedMutation>>);
impl MutationProvider for HoldEntity {
    fn qualification(&self) -> ReceiptQualification {
        ReceiptQualification::PostgresDurable
    }
    fn commit(&self, prepared: PreparedMutation) -> MutationOutcome {
        let outcome = MutationOutcome::Unknown {
            identity: prepared.identity().clone(),
            cause: UnknownCause::Pending,
        };
        assert!(self.0.lock().replace(prepared).is_none());
        outcome
    }
}
struct HoldHistory {
    store: HisStore,
    held: parking_lot::Mutex<Option<PreparedHistoryMutation>>,
}
impl HistoryProvider for HoldHistory {
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
        let outcome = HistoryWriteOutcome::Unknown {
            identity: prepared.identity().clone(),
            cause: HistoryWriteUnknown::Pending,
        };
        assert!(self.held.lock().replace(prepared).is_none());
        outcome
    }
}
fn history_result(outcome: HistoryWriteOutcome) -> Value {
    match outcome {
        HistoryWriteOutcome::Committed(receipt) => {
            json!({"kind":"committed","sequence":receipt.change_sequence,"generation":receipt.after_generation})
        }
        HistoryWriteOutcome::Rejected { reason, .. } => {
            json!({"kind":"rejected","reason":format!("{reason:?}")})
        }
        HistoryWriteOutcome::Unknown { cause, .. } => {
            json!({"kind":"unknown","cause":format!("{cause:?}")})
        }
    }
}
async fn submit_history(
    owner: &ApplicationOwner,
    service: HistoryMutationService,
    command: &Value,
) -> HistoryWriteOutcome {
    owner
        .handle()
        .read_service()
        .read(
            context(),
            ReadRequest::new(ReadQuery::Ids(vec!["p".into()]), OutputProfile::Typed),
        )
        .await
        .unwrap();
    let state = service.store().state("p").unwrap();
    let index = command["index"].as_i64().unwrap();
    let ts = chrono::DateTime::parse_from_rfc3339("2024-06-01T08:30:00-04:00").unwrap()
        + chrono::Duration::seconds(index);
    service
        .submit(
            context(),
            HistoryWriteRequest {
                identity: HistoryOperationIdentity {
                    authority: state.authority,
                    incarnation: state.incarnation,
                    point: "p".into(),
                    operation_id: command["key"].as_str().unwrap().into(),
                },
                expected_generation: state.generation,
                samples: vec![HistorySample {
                    ts: HDateTime::new(ts, "New_York"),
                    val: Kind::Number(Number::unitless(index as f64)),
                }],
            },
        )
        .await
        .unwrap()
}
fn cached(owner: &ApplicationOwner) -> Value {
    let entity = owner.handle().mutation_service().unwrap();
    let history = owner.handle().history_mutation_service().unwrap();
    let state = entity.store().graph().state();
    let store = history.store();
    let history_state = store.inner.state.lock();
    json!({"pid":std::process::id(),"dataset":entity.store().dataset(),"incarnation":state.incarnation,
        "revision":state.revision,"catalog":state.catalog_generation,"history_authority":store.authority(),
        "sequence":history_state.sequence,"rows":entity.store().graph().len()})
}
fn entity_request(owner: &ApplicationOwner, command: &Value) -> EntityBatchRequest {
    let service = owner.handle().mutation_service().unwrap();
    let state = service.store().graph().state();
    let id = command["id"].as_str().unwrap();
    let mut value = row(id);
    if command["point"].as_bool().unwrap_or(false) {
        value.remove_tag("site");
        value.set("his", Kind::Marker);
        value.set("kind", Kind::Str("Number".into()));
        value.set("tz", Kind::Str("New_York".into()));
        value.set("unit", Kind::Str("°F".into()));
        value.set("siteRef", Kind::Ref(HRef::from_val("site")));
    }
    if let Some(label) = command["label"].as_str() {
        value.set("dis", Kind::Str(label.into()));
    }
    let operation = if command["update"].as_bool().unwrap_or(false) {
        {
            value.remove_tag("id");
            EntityOperation::Patch {
                id: id.into(),
                changes: value,
            }
        }
    } else {
        EntityOperation::Add(value)
    };
    EntityBatchRequest {
        identity: OperationIdentity {
            dataset: service.store().dataset(),
            incarnation: state.incarnation,
            operation_id: command["key"].as_str().unwrap().into(),
        },
        expected_revision: command["expected"].as_u64().unwrap_or(state.revision),
        operations: vec![operation],
    }
}
fn entity_result(outcome: Result<MutationOutcome, ReadError>) -> Value {
    match outcome {
        Ok(outcome) => json!({"wire":entity::encode(&outcome).unwrap()}),
        Err(error) => json!({"error":error.to_string()}),
    }
}
fn decoded(value: &Value) -> MutationOutcome {
    entity::decode(&serde_json::from_value::<Vec<u8>>(value["wire"].clone()).unwrap()).unwrap()
}
fn committed(value: &Value) -> EntityReceipt {
    let outcome = decoded(value);
    let MutationOutcome::Committed(receipt) = outcome else {
        panic!("expected committed: {outcome:?}")
    };
    receipt
}
fn watch_result(value: &Value) -> SubscriptionOutcome {
    watch_wire::decode(&serde_json::from_value::<Vec<u8>>(value["wire"].clone()).unwrap()).unwrap()
}
fn delivery(value: &Value) -> Arc<SubscriptionDelivery> {
    let outcome = watch_result(value);
    let SubscriptionOutcome::Delivery(delivery) = outcome else {
        panic!("expected delivery: {outcome:?}")
    };
    delivery
}
fn watch_command(request: SubscriptionRequest) -> Value {
    json!({"op":"watch", "wire":watch_wire::encode(&request).unwrap()})
}
fn ack(delivery: &SubscriptionDelivery) -> SubscriptionRequest {
    SubscriptionRequest::Acknowledge {
        watch: delivery.watch.clone(),
        scope_generation: delivery.scope_generation,
        token: delivery.token,
        through: delivery.through,
    }
}
async fn child() {
    let mut config = fixture("child");
    config.schema = std::env::var(CHILD).unwrap();
    config.host = std::env::var("RUSTY_HAYSTACK_PG_PROCESS_HOST").unwrap();
    config.port = std::env::var("RUSTY_HAYSTACK_PG_PROCESS_PORT")
        .unwrap()
        .parse()
        .unwrap();
    config.limits.refresh_interval = Duration::from_millis(
        std::env::var("RUSTY_HAYSTACK_PG_PROCESS_INTERVAL")
            .unwrap()
            .parse()
            .unwrap(),
    );
    config.limits.recovery.max_changes = std::env::var("RUSTY_HAYSTACK_PG_PROCESS_RETENTION")
        .unwrap()
        .parse()
        .unwrap();
    config.limits.acquire_timeout = Duration::from_millis(800);
    config.limits.statement_timeout = Duration::from_secs(2);
    config.limits.lock_timeout = Duration::from_millis(1500);
    let builder = application(
        config,
        Catalog::load_http_pinned().unwrap(),
        policies(),
        PostgresServiceLimits::default(),
    )
    .await
    .unwrap();
    let entity = builder.handle().mutation_service().unwrap();
    let watches = StateSubscriptionService::new(
        builder.handle().read_service(),
        entity.store(),
        SubscriptionLimits::default(),
    )
    .unwrap();
    let owner = builder
        .state_subscriptions(watches.clone())
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    let held = Arc::new(HoldEntity::default());
    let scheduled = MutationService::with_provider(
        entity.read_service(),
        entity.store(),
        Arc::new(AllowAllMutations),
        entity.limits().clone(),
        held.clone(),
    )
    .unwrap();
    let session =
        SubscriptionSession::trusted("qualified-owner", Duration::from_secs(120)).unwrap();
    let held_history = Arc::new(HoldHistory {
        store: owner.handle().history_mutation_service().unwrap().store(),
        held: parking_lot::Mutex::new(None),
    });
    let scheduled_history = HistoryMutationService::new(
        HistoryService::new(
            owner.handle().read_service(),
            held_history.clone(),
            HistoryLimits::default(),
        )
        .unwrap(),
        Arc::new(AllowAllHistoryMutations),
        HistoryMutationLimits::default(),
    )
    .unwrap();
    let backend = owner.handle().read_service().postgres_backend();
    let mut refresh_release = None;
    let mut refreshing = None;
    let mut transaction_release = None;
    let mut transaction_entered = None;
    let mut transaction_done = None;
    let mut pending = None;
    let mut snapshot_admission = None;
    let mut snapshot_opening = None;
    let mut history_session = None;
    emit(cached(&owner));
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        assert!(line.len() <= 1_048_576);
        let command: Value = serde_json::from_str(&line).unwrap();
        let response = match command["op"].as_str().unwrap() {
            "snapshot_prepare" => {
                snapshot_admission = Some(
                    owner
                        .handle()
                        .read_service()
                        .begin(context())
                        .await
                        .unwrap(),
                );
                json!({"prepared":true})
            }
            "snapshot_start" => {
                let admission = snapshot_admission.take().unwrap();
                snapshot_opening =
                    Some(tokio::spawn(
                        async move { admission.postgres_snapshot().await },
                    ));
                json!({"started":true})
            }
            "snapshot_finish" => match snapshot_opening.take().unwrap().await.unwrap() {
                Ok(snapshot) => {
                    let revision = snapshot.state().revision;
                    snapshot.close().await.unwrap();
                    json!({"revision":revision})
                }
                Err(error) => json!({"error":error.to_string()}),
            },
            "cached" => cached(&owner),
            "read" => {
                let query = if let Some(filter) = command["filter"].as_str() {
                    ReadQuery::Filter(filter.into())
                } else {
                    ReadQuery::Ids(vec![command["id"].as_str().unwrap_or("site").into()])
                };
                match owner
                    .handle()
                    .read_service()
                    .read(context(), ReadRequest::new(query, OutputProfile::Typed))
                    .await
                {
                    Ok(page) => {
                        let mut value = cached(&owner);
                        value["read_rows"] = json!(page.row_count);
                        value
                    }
                    Err(error) => json!({"error":error.to_string()}),
                }
            }
            "entity" | "prepare" | "start_entity" => {
                let request = entity_request(&owner, &command);
                if command["op"] == "start_entity" {
                    let entity = entity.clone();
                    assert!(pending.is_none());
                    pending = Some(tokio::spawn(async move {
                        entity.submit(context(), request).await
                    }));
                    json!({"started":true})
                } else {
                    entity_result(if command["op"] == "prepare" {
                        scheduled.submit(context(), request).await
                    } else {
                        entity.submit(context(), request).await
                    })
                }
            }
            "operation_pending" => {
                json!({"pending":pending.as_ref().is_some_and(|task|!task.is_finished())})
            }
            "finish_entity" => entity_result(pending.take().unwrap().await.unwrap()),
            "publish" => {
                let prepared = held.0.lock().take().unwrap();
                entity_result(Ok(tokio::task::spawn_blocking(move || prepared.publish())
                    .await
                    .unwrap()))
            }
            "reconcile" => {
                let identity: OperationIdentity = entity::decode(
                    &serde_json::from_value::<Vec<u8>>(command["identity"].clone()).unwrap(),
                )
                .unwrap();
                entity_result(entity.reconcile(context(), identity).await)
            }
            "feed" => match entity
                .changes(
                    context(),
                    ChangesRequest {
                        cursor: command["cursor"].as_str().map(str::to_owned),
                        max_diffs: 128,
                    },
                )
                .await
            {
                Ok(page) => {
                    json!({"cursor":page.cursor,"position":page.position,"versions":page.changes.iter().map(|change|change.revision).collect::<Vec<_>>() })
                }
                Err(error) => json!({"error":error.to_string()}),
            },
            "history" | "prepare_history" => {
                let service = if command["op"] == "prepare_history" {
                    scheduled_history.clone()
                } else {
                    owner.handle().history_mutation_service().unwrap()
                };
                history_result(submit_history(&owner, service, &command).await)
            }
            "publish_history" => {
                let prepared = held_history.held.lock().take().unwrap();
                history_result(
                    tokio::task::spawn_blocking(move || prepared.publish())
                        .await
                        .unwrap(),
                )
            }
            "refresh_start" => {
                use super::super::storage::{RefreshFault, RefreshStage};
                let stage = if command["stage"] == "entities" {
                    RefreshStage::AfterEntities
                } else {
                    RefreshStage::BeforePublication
                };
                let (entered, ready) = tokio::sync::oneshot::channel();
                let (release, held) = tokio::sync::oneshot::channel();
                *backend.refresh_fault.lock() = Some(RefreshFault {
                    stage,
                    entered,
                    release: held,
                });
                refresh_release = Some(release);
                let backend = backend.clone();
                refreshing = Some(tokio::spawn(async move { backend.health().await }));
                tokio::time::timeout(Duration::from_secs(5), ready)
                    .await
                    .unwrap()
                    .unwrap();
                json!({"paused":true})
            }
            "refresh_release" => {
                refresh_release.take().unwrap().send(()).unwrap();
                refreshing.take().unwrap().await.unwrap().unwrap();
                cached(&owner)
            }
            "start_publish" => {
                let prepared = held.0.lock().take().unwrap();
                assert!(pending.is_none());
                pending = Some(tokio::spawn(async move {
                    Ok(tokio::task::spawn_blocking(move || prepared.publish())
                        .await
                        .unwrap())
                }));
                json!({"started":true})
            }
            "hold_transaction" => {
                let (entered, ready) = tokio::sync::oneshot::channel();
                let (release, held) = tokio::sync::oneshot::channel();
                let (completed, done) = tokio::sync::oneshot::channel();
                *backend.fault.lock() = Some(super::super::storage::CommitFault::HoldUncertain {
                    entered,
                    release: held,
                    completed,
                });
                transaction_release = Some(release);
                transaction_entered = Some(ready);
                transaction_done = Some(done);
                json!({"armed":true})
            }
            "transaction_held" => {
                transaction_entered.take().unwrap().await.unwrap();
                json!({"held":true,"admitted":owner.handle().read_service().load().admitted,"allocated":backend.provisional_revision.load(std::sync::atomic::Ordering::SeqCst)})
            }
            "abort_transaction" => {
                drop(transaction_release.take());
                transaction_done.take().unwrap().await.unwrap().unwrap();
                json!({"aborted":true,"admitted":owner.handle().read_service().load().admitted})
            }
            "history_read" => match owner
                .handle()
                .history_service()
                .unwrap()
                .collect(
                    context(),
                    HistoryReadRequest {
                        id: "p".into(),
                        range: "2024-06-01".into(),
                    },
                )
                .await
            {
                Ok(result) => json!({"samples":result.samples.len()}),
                Err(error) => json!({"error":error.to_string()}),
            },
            "history_open" => {
                history_session = Some(
                    owner
                        .handle()
                        .history_service()
                        .unwrap()
                        .open(
                            context(),
                            HistoryReadRequest {
                                id: "p".into(),
                                range: "2024-06-01".into(),
                            },
                        )
                        .await
                        .unwrap(),
                );
                json!({"opened":true})
            }
            "history_pull" => {
                let batch = history_session.as_mut().unwrap().next().await;
                json!({"result":format!("{batch:?}")})
            }
            "watch_create" => {
                let request = SubscriptionRequest::Create(SubscriptionCreate {
                    authority: watches.authority(),
                    key: command["key"].as_str().unwrap().into(),
                    ids: vec!["site".into(), "p".into()],
                    lease_ms: 60_000,
                });
                let outcome = watches
                    .execute(context(), session.clone(), request)
                    .await
                    .unwrap();
                json!({"wire":watch_wire::encode(&outcome).unwrap()})
            }
            "watch" => {
                let request: SubscriptionRequest = watch_wire::decode(
                    &serde_json::from_value::<Vec<u8>>(command["wire"].clone()).unwrap(),
                )
                .unwrap();
                match watches.execute(context(), session.clone(), request).await {
                    Ok(outcome) => json!({"wire":watch_wire::encode(&outcome).unwrap()}),
                    Err(error) => json!({"error":error.to_string()}),
                }
            }
            "stop" => break,
            op => panic!("unknown bounded child command {op}"),
        };
        emit(response);
    }
    if let Some(mut history) = history_session {
        let _ = history.close().await;
    }
    drop(held.0.lock().take());
    drop(held_history.held.lock().take());
    drop(transaction_release);
    drop(refresh_release);
    close(owner).await;
    emit(json!({"stopped":true}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn two_processes_recover_shared_entities_history_receipts_and_subscriptions() {
    if std::env::var_os(CHILD).is_some() {
        child().await;
        return;
    }
    let config = fixture("two_process");
    let mut a = Process::start(&config, 250);
    let mut b = Process::start(&config, 250);
    let a_initial = a.ready();
    let b_initial = b.ready();
    assert_ne!(a_initial["pid"], b_initial["pid"]);
    assert_ne!(a_initial["pid"], std::process::id());
    for field in ["dataset", "incarnation", "catalog", "history_authority"] {
        assert_eq!(
            a_initial[field], b_initial[field],
            "concurrent startup {field}"
        );
    }
    let origin = a.call(json!({"op":"feed"}))["cursor"].clone();
    let first = committed(&a.call(json!({"op":"entity","id":"site","label":"First","key":"site"})));
    assert_eq!(first.after_revision, 1);
    assert_eq!(b.call(json!({"op":"read"}))["read_rows"], 1);
    committed(&b.call(json!({"op":"entity","id":"p","point":true,"key":"point"})));
    assert_eq!(
        a.call(json!({"op":"read","filter":"his and siteRef->site"}))["read_rows"],
        1
    );
    assert_eq!(
        a.call(json!({"op":"history","key":"h1","index":1}))["sequence"],
        1
    );
    assert_eq!(
        b.call(json!({"op":"history","key":"h2","index":2}))["sequence"],
        2
    );
    assert_eq!(a.call(json!({"op":"history_read"}))["samples"], 2);
    a.call(json!({"op":"history_open"}));
    b.call(json!({"op":"history","key":"h3","index":3}));
    let started = std::time::Instant::now();
    loop {
        if a.call(json!({"op":"cached"}))["sequence"] == 3 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "periodic history-only catch-up without another write"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    eprintln!(
        "shared process history-only idle catch-up: {} ms, interval 250 ms",
        started.elapsed().as_millis()
    );
    let old_history = a.call(json!({"op":"history_pull"}));
    assert!(
        old_history["result"].as_str().unwrap().contains("Changed"),
        "retained point handle must observe changed generation: {old_history}"
    );
    let initial = delivery(&a.call(json!({"op":"watch_create","key":"watch"})));
    assert!(initial.initial);
    a.call(watch_command(ack(&initial)));
    let a_pending = a.call(json!({"op":"prepare","id":"a-held","key":"held"}));
    let MutationOutcome::Unknown {
        identity,
        cause: UnknownCause::Pending,
    } = decoded(&a_pending)
    else {
        panic!("held reservation")
    };
    let foreign = b.call(json!({"op":"reconcile","identity":entity::encode(&identity).unwrap()}));
    assert!(matches!(
        decoded(&foreign),
        MutationOutcome::Unknown {
            cause: UnknownCause::Pending,
            ..
        }
    ));
    let b_commit = committed(
        &b.call(json!({"op":"entity","id":"site","label":"Second","update":true,"key":"site-2"})),
    );
    assert!(matches!(
        decoded(&a.call(json!({"op":"publish"}))),
        MutationOutcome::Rejected {
            reason: RejectionReason::Conflict,
            ..
        }
    ));
    let observed =
        a.call(json!({"op":"reconcile","identity":entity::encode(&b_commit.identity).unwrap()}));
    assert_eq!(committed(&observed), b_commit);
    assert!(
        a.call(json!({"op":"cached"}))["revision"].as_u64().unwrap() >= b_commit.after_revision
    );
    let delta = delivery(&a.call(watch_command(SubscriptionRequest::Resume {
        watch: initial.watch.clone(),
        scope_generation: initial.scope_generation,
        acknowledged: initial.through,
    })));
    assert!(!delta.initial);
    assert_eq!(delta.from, initial.through);
    assert_eq!(delta.through, b_commit.after_revision);
    a.call(watch_command(ack(&delta)));
    assert!(matches!(
        watch_result(&b.call(watch_command(SubscriptionRequest::Resume {
            watch: delta.watch.clone(),
            scope_generation: delta.scope_generation,
            acknowledged: delta.through
        }))),
        SubscriptionOutcome::Resync { .. }
    ));
    let feed = a.call(json!({"op":"feed","cursor":origin}));
    assert_eq!(feed["versions"], json!([1, 2, 3]));
    a.stop();
    let mut restarted = Process::start(&config, 250);
    let after = restarted.ready();
    assert_ne!(after["pid"], a_initial["pid"]);
    assert_eq!(after["incarnation"], a_initial["incarnation"]);
    assert_eq!(after["revision"], 3);
    assert_eq!(after["sequence"], 3);
    assert_eq!(
        committed(&restarted.call(
            json!({"op":"reconcile","identity":entity::encode(&b_commit.identity).unwrap()})
        )),
        b_commit
    );
    assert!(matches!(
        decoded(
            &restarted
                .call(json!({"op":"reconcile","identity":entity::encode(&identity).unwrap()}))
        ),
        MutationOutcome::Rejected {
            reason: RejectionReason::Conflict,
            ..
        }
    ));
    assert_eq!(
        restarted.call(json!({"op":"feed","cursor":origin}))["versions"],
        json!([1, 2, 3])
    );
    assert!(matches!(
        watch_result(&restarted.call(watch_command(SubscriptionRequest::Resume {
            watch: delta.watch.clone(),
            scope_generation: delta.scope_generation,
            acknowledged: delta.through
        }))),
        SubscriptionOutcome::Resync { .. }
    ));
    assert!(delivery(&restarted.call(json!({"op":"watch_create","key":"fresh"}))).initial);
    b.stop();
    restarted.stop();
    cleanup(&config).await;
}

/// Transparent byte forwarding preserves end-to-end verified TLS. Cutting it
/// closes existing sockets and rejects new connections, a real network fault.
struct Relay {
    port: u16,
    online: Arc<std::sync::atomic::AtomicBool>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    sockets: Arc<
        parking_lot::Mutex<
            std::collections::HashMap<u64, (std::net::TcpStream, std::net::TcpStream)>,
        >,
    >,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Relay {
    fn start(upstream: u16) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let listener = std::net::TcpListener::bind(("localhost", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let online = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let sockets = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new()));
        let worker = std::thread::spawn({
            let online = online.clone();
            let stop = stop.clone();
            let sockets = sockets.clone();
            move || {
                let mut workers = Vec::new();
                let mut id = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((client, _)) => {
                            // Darwin accept inherits O_NONBLOCK from its listener.
                            client.set_nonblocking(false).unwrap();
                            if !online.load(Ordering::SeqCst) {
                                let _ = client.shutdown(std::net::Shutdown::Both);
                                continue;
                            }
                            let Ok(server) = std::net::TcpStream::connect_timeout(
                                &std::net::SocketAddr::from(([127, 0, 0, 1], upstream)),
                                Duration::from_millis(200),
                            ) else {
                                continue;
                            };
                            id += 1;
                            let id = id;
                            let sockets = sockets.clone();
                            {
                                let mut active = sockets.lock();
                                if !online.load(Ordering::SeqCst) {
                                    let _ = client.shutdown(std::net::Shutdown::Both);
                                    continue;
                                }
                                assert!(active.len() < 64, "bounded relay connections");
                                active.insert(
                                    id,
                                    (client.try_clone().unwrap(), server.try_clone().unwrap()),
                                );
                            }
                            workers.push(std::thread::spawn(move || {
                                let mut client_read = client.try_clone().unwrap();
                                let mut server_write = server.try_clone().unwrap();
                                let forward = std::thread::spawn(move || {
                                    let _ = std::io::copy(&mut client_read, &mut server_write);
                                    let _ = server_write.shutdown(std::net::Shutdown::Write);
                                });
                                let mut client = client;
                                let mut server = server;
                                let _ = std::io::copy(&mut server, &mut client);
                                let _ = client.shutdown(std::net::Shutdown::Both);
                                let _ = server.shutdown(std::net::Shutdown::Both);
                                forward.join().unwrap();
                                sockets.lock().remove(&id);
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => break,
                    }
                }
                for worker in workers {
                    worker.join().unwrap();
                }
            }
        });
        Self {
            port,
            online,
            stop,
            sockets,
            worker: Some(worker),
        }
    }
    fn cut(&self) {
        self.online
            .store(false, std::sync::atomic::Ordering::SeqCst);
        for (client, server) in self.sockets.lock().values() {
            let _ = client.shutdown(std::net::Shutdown::Both);
            let _ = server.shutdown(std::net::Shutdown::Both);
        }
    }
    fn restore(&self) {
        self.online.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.cut();
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn actual_network_interruption_denies_stale_work_and_periodically_recovers() {
    let config = fixture("network_process");
    let relay = Relay::start(config.port);
    let mut routed = config.clone();
    routed.port = relay.port;
    routed.host = "localhost".into();
    match sqlx::PgConnection::connect_with(&routed.options().unwrap()).await {
        Ok(connection) => connection.close().await.unwrap(),
        Err(sqlx::Error::Io(error)) => panic!("relay IO: {error}"),
        Err(sqlx::Error::Tls(error)) => panic!("relay TLS: {error}"),
        Err(_) => panic!("relay other connection failure"),
    }
    let mut a = Process::start(&routed, 100);
    let mut b = Process::start(&config, 100);
    let initial = a.ready();
    b.ready();
    committed(&b.call(json!({"op":"entity","id":"site","key":"site","label":"Before"})));
    committed(&b.call(json!({"op":"entity","id":"p","key":"point","point":true})));
    b.call(json!({"op":"history","key":"h1","index":1}));
    assert_eq!(a.call(json!({"op":"read"}))["revision"], 2);
    let cursor = a.call(json!({"op":"feed"}))["cursor"].clone();
    let initial_watch = delivery(&a.call(json!({"op":"watch_create","key":"before-cut"})));
    a.call(watch_command(ack(&initial_watch)));
    relay.cut();
    committed(
        &b.call(json!({"op":"entity","id":"site","key":"remote-1","label":"During","update":true})),
    );
    committed(&b.call(json!({"op":"entity","id":"remote","key":"remote-2"})));
    b.call(json!({"op":"history","key":"h2","index":2}));
    assert!(
        a.call(json!({"op":"read"}))["error"].is_string(),
        "isolated read must not succeed from its stale cache"
    );
    assert!(a.call(json!({"op":"feed","cursor":cursor}))["error"].is_string());
    assert!(
        a.call(watch_command(SubscriptionRequest::Resume {
            watch: initial_watch.watch.clone(),
            scope_generation: initial_watch.scope_generation,
            acknowledged: initial_watch.through
        }))["error"]
            .is_string()
    );
    relay.restore();
    let recovery = std::time::Instant::now();
    loop {
        let state = a.call(json!({"op":"cached"}));
        assert_eq!(
            state["pid"], initial["pid"],
            "same application process reconnects"
        );
        if state["revision"] == 4 && state["sequence"] == 2 {
            break;
        }
        assert!(
            recovery.elapsed() < Duration::from_secs(5),
            "bounded periodic reconnect without a fresh managed read or later write"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    eprintln!(
        "shared process actual TCP recovery: {} ms, interval 100 ms",
        recovery.elapsed().as_millis()
    );
    assert_eq!(
        a.call(json!({"op":"read","filter":"his and siteRef->site"}))["read_rows"],
        1
    );
    assert_eq!(a.call(json!({"op":"history_read"}))["samples"], 2);
    assert_eq!(
        a.call(json!({"op":"feed","cursor":cursor}))["versions"],
        json!([3, 4])
    );
    let delta = delivery(&a.call(watch_command(SubscriptionRequest::Resume {
        watch: initial_watch.watch.clone(),
        scope_generation: initial_watch.scope_generation,
        acknowledged: initial_watch.through,
    })));
    assert_eq!(delta.from, 2);
    assert_eq!(delta.through, 4);
    assert_eq!(delta.rows.len(), 1);
    a.stop();
    b.stop();
    drop(relay);
    cleanup(&config).await;
}

async fn wait_for_metadata_waiter(config: &PostgresConfig) {
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1),async {
        loop {
            let waiting:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock' AND query LIKE '%FOR UPDATE%'")
                .bind(format!("rusty-haystack:{}",config.schema)).fetch_one(&mut admin).await.unwrap();
            if waiting>0 {break;}
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("peer reached actual PostgreSQL metadata serialization wait");
    admin.close().await.unwrap();
}
async fn assert_sql_prefix(config: &PostgresConfig, expected: u64) {
    use sqlx::Row;
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    let state=sqlx::query("SELECT revision::text,(SELECT count(*) FROM entity_changes) AS changes FROM metadata WHERE singleton=1").fetch_one(&mut admin).await.unwrap();
    assert_eq!(state.get::<String, _>("revision"), expected.to_string());
    assert_eq!(state.get::<i64, _>("changes"), expected as i64);
    let revisions: Vec<String> =
        sqlx::query_scalar("SELECT revision::text FROM entity_changes ORDER BY revision")
            .fetch_all(&mut admin)
            .await
            .unwrap();
    assert_eq!(
        revisions,
        (1..=expected)
            .map(|revision| revision.to_string())
            .collect::<Vec<_>>()
    );
    admin.close().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn allocated_transaction_abort_preserves_contiguous_committed_prefix_across_processes() {
    let config = fixture("allocated_abort");
    let mut a = Process::start(&config, 60_000);
    let mut b = Process::start(&config, 60_000);
    a.ready();
    b.ready();
    let origin = a.call(json!({"op":"feed"}))["cursor"].clone();
    a.call(json!({"op":"hold_transaction"}));
    let uncertain = a.call(json!({"op":"entity","id":"aborted","key":"aborted"}));
    let MutationOutcome::Unknown { identity, .. } = decoded(&uncertain) else {
        panic!("held SQL outcome must remain uncertain")
    };
    let held = a.call(json!({"op":"transaction_held"}));
    assert_eq!(held["admitted"], 1);
    assert_eq!(
        held["allocated"], 1,
        "SQL effect transaction provisionally allocated revision 1 before rollback"
    );
    assert_eq!(
        b.call(json!({"op":"start_entity","id":"committed","key":"committed"}))["started"],
        true
    );
    // Admission's coherent metadata read can be the first waiter. Observe its
    // actual server lock wait before releasing the transaction, never a sleep.
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1),async {
        loop {
            let waiting:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock'")
                .bind(format!("rusty-haystack:{}",config.schema)).fetch_one(&mut admin).await.unwrap();
            if waiting>0 {break;}tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("peer waits for unresolved allocated transaction");
    assert_eq!(b.call(json!({"op":"operation_pending"}))["pending"], true);
    assert_eq!(a.call(json!({"op":"abort_transaction"}))["admitted"], 0);
    let committed = committed(&b.call(json!({"op":"finish_entity"})));
    assert_eq!(committed.before_revision, 0);
    assert_eq!(committed.after_revision, 1);
    assert_eq!(
        a.call(json!({"op":"feed","cursor":origin}))["versions"],
        json!([1])
    );
    assert_eq!(a.call(json!({"op":"read","id":"aborted"}))["read_rows"], 0);
    assert!(
        matches!(
            decoded(
                &b.call(json!({"op":"reconcile","identity":entity::encode(&identity).unwrap()}))
            ),
            MutationOutcome::Unknown {
                cause: UnknownCause::Pending,
                ..
            }
        ),
        "orphan reservation remains bound without false commit or foreign cancellation"
    );
    assert_sql_prefix(&config, 1).await;
    admin.close().await.unwrap();
    a.stop();
    b.stop();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn refresh_cut_and_publication_barriers_preserve_coherent_monotonic_state() {
    let config = fixture("refresh_barriers");
    let mut a = Process::start(&config, 60_000);
    let mut b = Process::start(&config, 60_000);
    a.ready();
    b.ready();
    committed(&b.call(json!({"op":"entity","id":"site","key":"site"})));
    committed(&b.call(json!({"op":"entity","id":"p","point":true,"key":"point"})));
    b.call(json!({"op":"history","key":"h1","index":1}));
    a.call(json!({"op":"read"}));
    assert_eq!(
        a.call(json!({"op":"refresh_start","stage":"entities"}))["paused"],
        true
    );
    b.call(json!({"op":"start_entity","id":"remote","key":"remote"}));
    wait_for_metadata_waiter(&config).await;
    assert_eq!(b.call(json!({"op":"operation_pending"}))["pending"], true);
    let captured = a.call(json!({"op":"refresh_release"}));
    assert_eq!(captured["revision"], 2);
    assert_eq!(captured["sequence"], 1);
    let remote = committed(&b.call(json!({"op":"finish_entity"})));
    assert_eq!(remote.after_revision, 3);
    assert_eq!(a.call(json!({"op":"read"}))["revision"], 3);
    assert_eq!(
        a.call(json!({"op":"prepare_history","key":"held-history","index":2}))["cause"],
        "Pending"
    );
    committed(&b.call(json!({"op":"entity","id":"p","point":true,"update":true,"label":"Remote schema dependency","key":"point-changed"})));
    assert_eq!(
        a.call(json!({"op":"cached"}))["revision"],
        3,
        "local graph intentionally stale at history publication"
    );
    let rejected = a.call(json!({"op":"publish_history"}));
    assert_eq!(rejected["kind"], "rejected");
    assert_eq!(rejected["reason"], "Conflict");
    assert_eq!(
        b.call(json!({"op":"history_read"}))["samples"],
        1,
        "stale graph preparation made no history effect"
    );
    a.call(json!({"op":"read"}));
    assert!(matches!(
        decoded(&a.call(json!({"op":"prepare","id":"local","key":"local"}))),
        MutationOutcome::Unknown {
            cause: UnknownCause::Pending,
            ..
        }
    ));
    a.call(json!({"op":"refresh_start","stage":"publication"}));
    a.call(json!({"op":"start_publish"}));
    assert_eq!(a.call(json!({"op":"operation_pending"}))["pending"], true);
    let old = a.call(json!({"op":"refresh_release"}));
    assert!(old["revision"].as_u64().unwrap() <= 5);
    assert_eq!(
        committed(&a.call(json!({"op":"finish_entity"}))).after_revision,
        5
    );
    assert_eq!(
        a.call(json!({"op":"cached"}))["revision"],
        5,
        "delayed refresh cannot overwrite newer publication"
    );
    assert_eq!(b.call(json!({"op":"read"}))["revision"], 5);
    assert_sql_prefix(&config, 5).await;
    a.stop();
    b.stop();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn schema_transition_rolls_back_and_snapshot_waits_before_freezing_its_view() {
    let config = fixture("schema_transition");
    let initial = open(config.clone()).await;
    let identity = initial
        .handle()
        .mutation_service()
        .unwrap()
        .store()
        .dataset();
    close(initial).await;
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    sqlx::raw_sql("ALTER TABLE metadata DROP CONSTRAINT metadata_format_check; ALTER TABLE metadata ADD CONSTRAINT metadata_format_check CHECK(format=1) NOT VALID; UPDATE metadata SET format=1; ALTER TABLE metadata VALIDATE CONSTRAINT metadata_format_check; ALTER TABLE entities SET UNLOGGED;").execute(&mut admin).await.unwrap();
    let failed = application(
        config.clone(),
        Catalog::load_http_pinned().unwrap(),
        policies(),
        PostgresServiceLimits::default(),
    )
    .await;
    assert!(matches!(failed, Err(PostgresError::Durability)));
    let format: i32 = sqlx::query_scalar("SELECT format FROM metadata")
        .fetch_one(&mut admin)
        .await
        .unwrap();
    assert_eq!(
        format, 1,
        "failed logged-table validation rolled back the actual format migration"
    );
    sqlx::query("ALTER TABLE entities SET LOGGED")
        .execute(&mut admin)
        .await
        .unwrap();
    let mut a = Process::start(&config, 60_000);
    let mut b = Process::start(&config, 60_000);
    let first = a.ready();
    let second = b.ready();
    assert_eq!(first["dataset"], json!(identity));
    assert_eq!(second["dataset"], first["dataset"]);
    let format: i32 = sqlx::query_scalar("SELECT format FROM metadata")
        .fetch_one(&mut admin)
        .await
        .unwrap();
    assert_eq!(format, 3);
    a.call(json!({"op":"snapshot_prepare"}));
    let mut migration = admin.begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':' || $1,0))",
    )
    .bind(&config.schema)
    .execute(&mut *migration)
    .await
    .unwrap();
    a.call(json!({"op":"snapshot_start"}));
    // Independent monitoring connection: the held migration connection cannot
    // simultaneously observe another session's lock wait through its own tx.
    let mut observer = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1),async {
        loop {
            let waiting:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock' AND query LIKE 'SELECT pg_advisory_lock_shared%'")
                .bind(format!("rusty-haystack:{}",config.schema)).fetch_one(&mut observer).await.unwrap();
            if waiting>0 {break;}tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("snapshot reaches actual shared schema gate wait");
    sqlx::raw_sql("ALTER TABLE metadata DROP CONSTRAINT metadata_format_check; UPDATE metadata SET format=99;").execute(&mut *migration).await.unwrap();
    migration.commit().await.unwrap();
    assert!(
        a.call(json!({"op":"snapshot_finish"}))["error"].is_string(),
        "no pre-gate RR format snapshot escaped"
    );
    assert!(a.call(json!({"op":"read"}))["error"].is_string());
    assert!(b.call(json!({"op":"read"}))["error"].is_string());
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
    a.stop();
    b.stop();
    observer.close().await.unwrap();
    admin.close().await.unwrap();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn subscription_retention_gap_requires_explicit_resync_and_fresh_snapshot() {
    let mut config = fixture("retention_process");
    config.limits.recovery.max_changes = 2;
    let mut a = Process::start(&config, 60_000);
    let mut b = Process::start(&config, 60_000);
    a.ready();
    b.ready();
    committed(&b.call(json!({"op":"entity","id":"site","key":"site"})));
    let first = delivery(&a.call(json!({"op":"watch_create","key":"before-gap"})));
    a.call(watch_command(ack(&first)));
    let cursor = a.call(json!({"op":"feed"}))["cursor"].clone();
    for index in 0..3 {
        committed(&b.call(json!({"op":"entity","id":"site","update":true,"label":format!("change {index}"),"key":format!("change-{index}")})));
    }
    let outcome = watch_result(&a.call(watch_command(SubscriptionRequest::Resume {
        watch: first.watch.clone(),
        scope_generation: first.scope_generation,
        acknowledged: first.through,
    })));
    assert_eq!(
        outcome,
        SubscriptionOutcome::Resync(SubscriptionResync::Gap)
    );
    assert!(
        a.call(json!({"op":"feed","cursor":cursor}))["error"].is_string(),
        "feed gap must not silently skip to current head"
    );
    let fresh = delivery(&a.call(json!({"op":"watch_create","key":"after-gap"})));
    assert!(fresh.initial);
    assert_eq!(fresh.through, 4);
    assert_eq!(fresh.rows.len(), 1);
    a.stop();
    b.stop();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn explicit_restore_epoch_invalidates_live_work_and_old_resume_authority() {
    let config = fixture("restore_process");
    let mut a = Process::start(&config, 60_000);
    let mut b = Process::start(&config, 60_000);
    let original = a.ready();
    b.ready();
    committed(&a.call(json!({"op":"entity","id":"site","key":"site"})));
    committed(&a.call(json!({"op":"entity","id":"p","point":true,"key":"point"})));
    a.call(json!({"op":"history","key":"h1","index":1}));
    a.call(json!({"op":"history_open"}));
    let cursor = a.call(json!({"op":"feed"}))["cursor"].clone();
    let watch = delivery(&a.call(json!({"op":"watch_create","key":"before-restore"})));
    a.call(watch_command(ack(&watch)));
    b.call(json!({"op":"read"}));
    assert!(matches!(
        decoded(&b.call(json!({"op":"prepare","id":"old-prepared","key":"before-restore"}))),
        MutationOutcome::Unknown {
            cause: UnknownCause::Pending,
            ..
        }
    ));
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    let mut restore = admin.begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':' || $1,0))",
    )
    .bind(&config.schema)
    .execute(&mut *restore)
    .await
    .unwrap();
    // Explicit administrator restore boundary. Preserve the restored entity
    // rows, rotate every durable continuation authority and discard old ledgers.
    sqlx::raw_sql("DELETE FROM receipts; DELETE FROM entity_changes; DELETE FROM history_changes; DELETE FROM history_samples; DELETE FROM history_points;").execute(&mut *restore).await.unwrap();
    sqlx::query("UPDATE metadata SET incarnation=$1,cursor_key=$2,history_authority=$3,owner_generation=owner_generation+1,revision=0,floor=0,history_sequence=0 WHERE singleton=1")
        .bind(rand::random::<[u8;16]>().as_slice()).bind(rand::random::<[u8;32]>().as_slice()).bind(rand::random::<[u8;16]>().as_slice()).execute(&mut *restore).await.unwrap();
    restore.commit().await.unwrap();
    assert!(a.call(json!({"op":"read"}))["error"].is_string());
    assert!(
        matches!(
            decoded(&b.call(json!({"op":"publish"}))),
            MutationOutcome::Unknown { .. }
        ),
        "old prepared capability cannot commit in restored authority"
    );
    assert!(b.call(json!({"op":"read"}))["error"].is_string());
    let terminal = a.call(json!({"op":"history_pull"}));
    assert!(
        terminal["result"].as_str().unwrap().contains("Interrupted")
            || terminal["result"].as_str().unwrap().contains("Failed"),
        "old history session must be stopped after epoch fencing: {terminal}"
    );
    a.stop();
    b.stop();
    let mut next = Process::start(&config, 60_000);
    let fresh = next.ready();
    assert_ne!(fresh["incarnation"], original["incarnation"]);
    assert_ne!(fresh["history_authority"], original["history_authority"]);
    assert!(next.call(json!({"op":"feed","cursor":cursor}))["error"].is_string());
    assert!(matches!(
        watch_result(&next.call(watch_command(SubscriptionRequest::Resume {
            watch: watch.watch.clone(),
            scope_generation: watch.scope_generation,
            acknowledged: watch.through
        }))),
        SubscriptionOutcome::Resync(_)
    ));
    assert!(delivery(&next.call(json!({"op":"watch_create","key":"after-restore"}))).initial);
    assert_eq!(
        next.call(json!({"op":"read","id":"old-prepared"}))["read_rows"],
        0
    );
    next.stop();
    admin.close().await.unwrap();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn same_epoch_regression_fences_instead_of_silently_rewinding() {
    let config = fixture("regression_process");
    let mut a = Process::start(&config, 60_000);
    let mut b = Process::start(&config, 60_000);
    a.ready();
    b.ready();
    committed(&a.call(json!({"op":"entity","id":"site","key":"site"})));
    assert_eq!(b.call(json!({"op":"read"}))["revision"], 1);
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    let mut restore = admin.begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':' || $1,0))",
    )
    .bind(&config.schema)
    .execute(&mut *restore)
    .await
    .unwrap();
    sqlx::raw_sql("DELETE FROM receipts; DELETE FROM entity_changes; DELETE FROM entities; UPDATE metadata SET revision=0,floor=0 WHERE singleton=1;").execute(&mut *restore).await.unwrap();
    restore.commit().await.unwrap();
    assert!(a.call(json!({"op":"read"}))["error"].is_string());
    assert!(b.call(json!({"op":"read"}))["error"].is_string());
    assert_eq!(a.call(json!({"op":"cached"}))["revision"], 1);
    assert_eq!(b.call(json!({"op":"cached"}))["revision"], 1);
    a.stop();
    b.stop();
    admin.close().await.unwrap();
    cleanup(&config).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn held_write_cannot_cross_changed_catalog_identity_without_epoch_rotation() {
    let config = fixture("catalog_fence");
    let mut a = Process::start(&config, 60_000);
    let mut b = Process::start(&config, 60_000);
    a.ready();
    b.ready();
    committed(&a.call(json!({"op":"entity","id":"site","key":"site"})));
    assert_eq!(b.call(json!({"op":"read"}))["revision"], 1);
    assert!(matches!(
        decoded(&a.call(json!({"op":"prepare","id":"held","key":"held"}))),
        MutationOutcome::Unknown {
            cause: UnknownCause::Pending,
            ..
        }
    ));
    let mut admin = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    let mut transition = admin.begin().await.unwrap();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended(current_database() || ':' || $1,0))",
    )
    .bind(&config.schema)
    .execute(&mut *transition)
    .await
    .unwrap();
    sqlx::query("UPDATE metadata SET catalog='unsupported-catalog-identity' WHERE singleton=1")
        .execute(&mut *transition)
        .await
        .unwrap();
    transition.commit().await.unwrap();
    assert!(
        matches!(
            decoded(&a.call(json!({"op":"publish"}))),
            MutationOutcome::Unknown { .. }
        ),
        "held preparation must validate durable catalog identity before effect"
    );
    assert!(b.call(json!({"op":"read"}))["error"].is_string());
    assert!(a.call(json!({"op":"read"}))["error"].is_string());
    assert_sql_prefix(&config, 1).await;
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM entities WHERE id='held'")
        .fetch_one(&mut admin)
        .await
        .unwrap();
    assert_eq!(effects, 0);
    a.stop();
    b.stop();
    admin.close().await.unwrap();
    cleanup(&config).await;
}
