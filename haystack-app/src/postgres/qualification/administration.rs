//! Real disposable PostgreSQL administration qualification. Faults below kill
//! the SQL backend; post-COMMIT injection loses the consumer result only.
use super::*;
use crate::postgres::admin::{self, Backup, Command, Interruption, Outcome, Status};
use haystack_core::kinds::{Float, HDateTime, Number};

fn sibling(config: &PostgresConfig, label: &str) -> PostgresConfig {
    let mut c = config.clone();
    c.schema = format!("rh_{label}_{:016x}", rand::random::<u64>());
    c
}
async fn execute(config: &PostgresConfig, command: Command) -> Result<Outcome, PostgresError> {
    admin::execute(
        config.clone(),
        Catalog::load_http_pinned().unwrap(),
        command,
    )
    .await
}
async fn export(
    control: &PostgresConfig,
    source: &PostgresConfig,
    target: &PostgresConfig,
    id: &str,
) -> Backup {
    match execute(
        control,
        Command::Export {
            operation: id.into(),
            source: source.schema.clone(),
            target: target.schema.clone(),
        },
    )
    .await
    .unwrap()
    {
        Outcome::Backup(b) => b,
        _ => panic!("expected archive"),
    }
}
async fn status(control: &PostgresConfig, id: &str) -> Status {
    match execute(
        control,
        Command::Status {
            operation: Some(id.into()),
        },
    )
    .await
    .unwrap()
    {
        Outcome::Status(s) => s,
        _ => panic!("expected journal"),
    }
}
async fn submit(owner: &ApplicationOwner, id: &str, rows: Vec<HDict>) -> EntityBatchRequest {
    let service = owner.handle().mutation_service().unwrap();
    let state = service.store().graph().state();
    let request = EntityBatchRequest {
        identity: OperationIdentity {
            dataset: service.store().dataset(),
            incarnation: state.incarnation,
            operation_id: id.into(),
        },
        expected_revision: state.revision,
        operations: rows.into_iter().map(EntityOperation::Add).collect(),
    };
    assert!(matches!(
        service.submit(context(), request.clone()).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    request
}
async fn history(owner: &ApplicationOwner, id: &str, offset: i64) -> HistoryWriteRequest {
    let service = owner.handle().history_mutation_service().unwrap();
    let state = service.store().state("point").unwrap();
    let ts = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00.123456789+00:00").unwrap()
        + chrono::Duration::seconds(offset);
    let request = HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: state.authority,
            incarnation: state.incarnation,
            point: "point".into(),
            operation_id: id.into(),
        },
        expected_generation: state.generation,
        samples: vec![HistorySample {
            ts: HDateTime::new(ts, "UTC"),
            val: Kind::Number(Number::unitless(offset as f64)),
        }],
    };
    assert!(matches!(
        service.submit(context(), request.clone()).await.unwrap(),
        HistoryWriteOutcome::Committed(_)
    ));
    request
}
fn point() -> HDict {
    let mut p = row("point");
    p.set("his", Kind::Marker);
    p.set("kind", Kind::Str("Number".into()));
    p.set("tz", Kind::Str("UTC".into()));
    p.set("unit", Kind::Str("°F".into()));
    p.set(
        "signedZero",
        Kind::Float(Float::from_bits(0x8000_0000_0000_0000)),
    );
    p.set(
        "nanPayload",
        Kind::Float(Float::from_bits(0x7ff8_0000_0000_0042)),
    );
    p.set("integer", Kind::Int(i64::MAX));
    p.set(
        "ref",
        Kind::Ref(HRef {
            val: "site".into(),
            dis: Some("Archive display".into()),
        }),
    );
    p
}
type Stored = (
    Vec<(String, Vec<u8>)>,
    Vec<(String, i64, i32, Vec<u8>)>,
    Vec<(String, String, Option<Vec<u8>>)>,
);
async fn stored(config: &PostgresConfig) -> Stored {
    let mut c = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    let rows = sqlx::query_as("SELECT id,payload FROM entities ORDER BY id")
        .fetch_all(&mut c)
        .await
        .unwrap();
    let samples = sqlx::query_as(
        "SELECT point,seconds,nanos,payload FROM history_samples ORDER BY point,seconds,nanos",
    )
    .fetch_all(&mut c)
    .await
    .unwrap();
    let points =
        sqlx::query_as("SELECT id,generation::text,evicted FROM history_points ORDER BY id")
            .fetch_all(&mut c)
            .await
            .unwrap();
    c.close().await.unwrap();
    (rows, samples, points)
}
async fn refuses(config: &PostgresConfig) {
    assert!(
        application(
            config.clone(),
            Catalog::load_http_pinned().unwrap(),
            policies(),
            Default::default()
        )
        .await
        .is_err()
    );
}

async fn authority_metadata(config: &PostgresConfig) -> String {
    let mut c = sqlx::PgConnection::connect_with(&config.options().unwrap())
        .await
        .unwrap();
    let state = sqlx::query_scalar("SELECT row_to_json(metadata)::text FROM metadata")
        .fetch_one(&mut c)
        .await
        .unwrap();
    c.close().await.unwrap();
    state
}

async fn journal(control: &PostgresConfig) -> Vec<String> {
    let mut c = sqlx::PgConnection::connect_with(&control.options().unwrap())
        .await
        .unwrap();
    let rows =
        sqlx::query_scalar("SELECT row_to_json(operations)::text FROM operations ORDER BY id")
            .fetch_all(&mut c)
            .await
            .unwrap();
    c.close().await.unwrap();
    rows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn committed_replays_preserve_later_cancelled_and_committed_authorities() {
    for restore in [false, true] {
        let source = fixture("replay_source");
        let control = sibling(&source, "replay_control");
        let target = sibling(&source, "replay_target");
        let later = sibling(&source, "replay_later");
        let owner = open(source.clone()).await;
        submit(&owner, "seed", vec![row("one")]).await;
        close(owner).await;
        let archive = if restore {
            let archive = export(&control, &source, &target, "producer").await;
            execute(
                &control,
                Command::Cancel {
                    operation: "producer".into(),
                },
            )
            .await
            .unwrap();
            archive
        } else {
            export(&control, &source, &target, "first").await
        };
        let import = || {
            if restore {
                Command::Restore {
                    operation: "first".into(),
                    target: target.schema.clone(),
                    backup: archive.bytes().to_vec(),
                }
            } else {
                Command::Import {
                    operation: "first".into(),
                    backup: archive.bytes().to_vec(),
                }
            }
        };
        execute(&control, import()).await.unwrap();
        execute(
            &control,
            Command::Activate {
                operation: "first".into(),
            },
        )
        .await
        .unwrap();
        let committed = status(&control, "first").await;
        assert_eq!(committed.phase.as_deref(), Some("committed"));
        for cancel in [true, false] {
            let next = if cancel {
                "later-cancelled"
            } else {
                "later-committed"
            };
            let other_archive = export(&control, &target, &later, next).await;
            assert_ne!(archive.sha256(), other_archive.sha256());
            if cancel {
                execute(
                    &control,
                    Command::Cancel {
                        operation: next.into(),
                    },
                )
                .await
                .unwrap();
            } else {
                execute(
                    &control,
                    Command::Import {
                        operation: next.into(),
                        backup: other_archive.bytes().to_vec(),
                    },
                )
                .await
                .unwrap();
                execute(
                    &control,
                    Command::Activate {
                        operation: next.into(),
                    },
                )
                .await
                .unwrap();
            }
            let current = status(&control, next).await;
            assert_eq!(
                current.selected.schema,
                if cancel {
                    &target.schema
                } else {
                    &later.schema
                }
                .as_str()
            );
            if cancel {
                assert_ne!(current.selected.epoch, committed.selected.epoch);
            }
            let mut expected = serde_json::to_value(&committed).unwrap();
            expected["selected"] = serde_json::to_value(&current.selected).unwrap();
            let schemas = if cancel {
                vec![&source, &target]
            } else {
                vec![&source, &target, &later]
            };
            let mut before = Vec::new();
            for schema in &schemas {
                before.push((authority_metadata(schema).await, stored(schema).await));
            }
            let recorded = journal(&control).await;
            for command in [
                Command::Activate {
                    operation: "first".into(),
                },
                import(),
            ] {
                let Outcome::Status(replayed) = execute(&control, command).await.expect(
                    "committed replay returns its historical outcome after later maintenance",
                ) else {
                    panic!("expected historical operation status")
                };
                assert_eq!(serde_json::to_value(replayed).unwrap(), expected);
            }
            let mut mismatches = vec![
                Command::Import {
                    operation: "first".into(),
                    backup: other_archive.bytes().to_vec(),
                },
                Command::Restore {
                    operation: "first".into(),
                    target: target.schema.clone(),
                    backup: other_archive.bytes().to_vec(),
                },
                Command::Restore {
                    operation: "first".into(),
                    target: later.schema.clone(),
                    backup: archive.bytes().to_vec(),
                },
                Command::Export {
                    operation: "first".into(),
                    source: later.schema.clone(),
                    target: target.schema.clone(),
                },
                Command::Export {
                    operation: "first".into(),
                    source: source.schema.clone(),
                    target: later.schema.clone(),
                },
            ];
            mismatches.push(if restore {
                Command::Export {
                    operation: "first".into(),
                    source: source.schema.clone(),
                    target: target.schema.clone(),
                }
            } else {
                Command::Restore {
                    operation: "first".into(),
                    target: target.schema.clone(),
                    backup: archive.bytes().to_vec(),
                }
            });
            for command in mismatches {
                assert!(matches!(
                    execute(&control, command).await,
                    Err(PostgresError::Conflict)
                ));
            }
            assert_eq!(status(&control, next).await.selected, current.selected);
            assert_eq!(journal(&control).await, recorded);
            for (schema, expected) in schemas.into_iter().zip(before) {
                assert_eq!(
                    (authority_metadata(schema).await, stored(schema).await),
                    expected
                );
            }
            refuses(&source).await;
            if !cancel {
                refuses(&target).await;
            }
        }
        for config in [&source, &target, &later, &control] {
            cleanup(config).await;
        }
    }
}
#[derive(Default)]
struct Held(parking_lot::Mutex<Option<PreparedMutation>>);
impl MutationProvider for Held {
    fn qualification(&self) -> ReceiptQualification {
        ReceiptQualification::PostgresDurable
    }
    fn commit(&self, p: PreparedMutation) -> MutationOutcome {
        let result = MutationOutcome::Unknown {
            identity: p.identity().clone(),
            cause: UnknownCause::Pending,
        };
        *self.0.lock() = Some(p);
        result
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn archived_restore_reuses_cancelled_backup_and_fences_old_authority() {
    let source = fixture("archive_source");
    let control = sibling(&source, "archive_control");
    let unused = sibling(&source, "unused_target");
    let target = sibling(&source, "restore_target");
    let owner = open_with_limits(
        source.clone(),
        PostgresServiceLimits {
            history_store: HistoryStoreLimits {
                max_items_per_point: 2,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let entity_request = submit(&owner, "original", vec![row("site"), point(), row("empty")]).await;
    history(&owner, "h0", 0).await;
    history(&owner, "h1", 1).await;
    let history_request = history(&owner, "h2", 2).await;
    let entities = owner.handle().mutation_service().unwrap();
    let old_cursor = entities
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
    let old_history = owner
        .handle()
        .history_mutation_service()
        .unwrap()
        .store()
        .state("point")
        .unwrap();
    // A valid retained empty series with an eviction watermark is part of the
    // storage recovery contract; seed it explicitly as fixture data.
    let mut sql = sqlx::PgConnection::connect_with(&source.options().unwrap())
        .await
        .unwrap();
    sqlx::query("INSERT INTO history_points VALUES('empty',$1,4,$2)")
        .bind([9u8; 16].as_slice())
        .bind(b"2023-01-01T00:00:00.000000000+00:00".as_slice())
        .execute(&mut sql)
        .await
        .unwrap();
    sql.close().await.unwrap();
    let expected = stored(&source).await;
    let mut retained = owner
        .handle()
        .history_service()
        .unwrap()
        .open(
            context(),
            HistoryReadRequest {
                id: "point".into(),
                range: "2024-01-01".into(),
            },
        )
        .await
        .unwrap();
    let held = Arc::new(Held::default());
    let scheduled = MutationService::with_provider(
        entities.read_service(),
        entities.store(),
        Arc::new(AllowAllMutations),
        entities.limits().clone(),
        held.clone(),
    )
    .unwrap();
    let state = entities.store().graph().state();
    let pending = EntityBatchRequest {
        identity: OperationIdentity {
            dataset: entities.store().dataset(),
            incarnation: state.incarnation,
            operation_id: "held-before-export".into(),
        },
        expected_revision: state.revision,
        operations: vec![EntityOperation::Add(row("held"))],
    };
    assert!(matches!(
        scheduled.submit(context(), pending).await.unwrap(),
        MutationOutcome::Unknown { .. }
    ));
    let archive = export(&control, &source, &unused, "producer").await;
    assert_eq!(archive.manifest().history_samples, 2);
    assert_eq!(archive.manifest().history_points, 2);
    assert_eq!(archive.manifest().history_sequence, 3);
    refuses(&source).await;
    assert!(
        entities
            .changes(
                context(),
                ChangesRequest {
                    cursor: None,
                    max_diffs: 128
                }
            )
            .await
            .is_err()
    );
    let prepared = held.0.lock().take().unwrap();
    let outcome = tokio::task::spawn_blocking(move || prepared.publish())
        .await
        .unwrap();
    assert!(!matches!(outcome, MutationOutcome::Committed(_)));
    let batch = retained.next().await;
    assert!(batch.samples.is_empty());
    assert_ne!(batch.terminal, Some(HistoryTerminal::Complete));
    retained.close().await;
    execute(
        &control,
        Command::Cancel {
            operation: "producer".into(),
        },
    )
    .await
    .unwrap();
    close(owner).await;
    let resumed = open(source.clone()).await;
    submit(&resumed, "newer", vec![row("newer")]).await;
    history(&resumed, "newer-history", 3).await;
    assert_ne!(stored(&source).await, expected);
    let outcome = execute(
        &control,
        Command::Restore {
            operation: "restore-archived".into(),
            target: target.schema.clone(),
            backup: archive.bytes().to_vec(),
        },
    )
    .await
    .unwrap();
    let Outcome::Status(staged) = outcome else {
        panic!("staged status")
    };
    assert_eq!(staged.phase.as_deref(), Some("staged"));
    assert!(staged.source_revision.unwrap() > staged.archive.as_ref().unwrap().revision);
    assert!(
        staged.source_history_sequence.unwrap() > staged.archive.as_ref().unwrap().history_sequence
    );
    refuses(&source).await;
    refuses(&target).await;
    assert_eq!(stored(&target).await, expected);
    execute(
        &control,
        Command::Activate {
            operation: "restore-archived".into(),
        },
    )
    .await
    .unwrap();
    execute(
        &control,
        Command::Activate {
            operation: "restore-archived".into(),
        },
    )
    .await
    .unwrap();
    assert!(
        execute(
            &control,
            Command::Cancel {
                operation: "restore-archived".into()
            }
        )
        .await
        .is_err()
    );
    assert!(
        execute(
            &control,
            Command::Restore {
                operation: "restore-archived".into(),
                target: unused.schema.clone(),
                backup: archive.bytes().to_vec()
            }
        )
        .await
        .is_err()
    );
    let builder = selected_application(
        control.clone(),
        Catalog::load_http_pinned().unwrap(),
        policies(),
        Default::default(),
    )
    .await
    .unwrap();
    let restored = builder.start(&tokio::runtime::Handle::current()).unwrap();
    restored.ready().await.unwrap();
    let service = restored.handle().mutation_service().unwrap();
    let state = service.store().graph().state();
    assert_ne!(state.incarnation, archive.manifest().incarnation);
    assert_eq!(state.revision, archive.manifest().revision);
    assert_eq!(service.store().dataset(), archive.manifest().dataset);
    assert!(service.store().graph().get("newer").is_none());
    let history = restored.handle().history_mutation_service().unwrap();
    let current = history.store().state("point").unwrap();
    assert_ne!(current.authority, old_history.authority);
    assert_ne!(current.incarnation, old_history.incarnation);
    assert_eq!(current.generation, old_history.generation);
    assert!(
        service
            .changes(
                context(),
                ChangesRequest {
                    cursor: Some(old_cursor),
                    max_diffs: 128
                }
            )
            .await
            .is_err()
    );
    assert!(!matches!(
        service
            .reconcile(context(), entity_request.identity)
            .await
            .unwrap(),
        MutationOutcome::Committed(_)
    ));
    assert!(!matches!(
        history
            .reconcile(context(), history_request.identity)
            .await
            .unwrap(),
        HistoryWriteOutcome::Committed(_)
    ));
    let mut sql = sqlx::PgConnection::connect_with(&target.options().unwrap())
        .await
        .unwrap();
    let reset:(String,String,i64,i64,i64)=sqlx::query_as("SELECT floor::text,history_sequence::text,(SELECT count(*) FROM receipts),(SELECT count(*) FROM entity_changes),(SELECT count(*) FROM history_changes) FROM metadata").fetch_one(&mut sql).await.unwrap();
    assert_eq!(
        reset,
        (archive.manifest().revision.to_string(), "0".into(), 0, 0, 0)
    );
    sql.close().await.unwrap();
    // Fresh intents remain writable on the restored identities and reset feeds.
    submit(&restored, "after-restore", vec![row("fresh")]).await;
    self::history(&restored, "after-restore-history", 4).await;
    assert_eq!(
        service.store().graph().state().revision,
        archive.manifest().revision + 1
    );
    assert_eq!(
        history.store().state("point").unwrap().generation,
        old_history.generation + 1
    );
    close(resumed).await;
    close(restored).await;
    refuses(&source).await;
    for c in [&source, &target, &control] {
        cleanup(c).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn backend_disconnects_reconcile_export_import_and_selection() {
    let source = fixture("interrupt_source");
    let control = sibling(&source, "interrupt_control");
    let target = sibling(&source, "interrupt_target");
    let owner = open(source.clone()).await;
    submit(&owner, "seed", vec![row("one")]).await;
    admin::interrupt_once("interrupted", Interruption::AfterQuiesce);
    let command = || Command::Export {
        operation: "interrupted".into(),
        source: source.schema.clone(),
        target: target.schema.clone(),
    };
    assert!(execute(&control, command()).await.is_err());
    assert_eq!(
        status(&control, "interrupted").await.phase.as_deref(),
        Some("quiesced")
    );
    refuses(&source).await;
    let archive = export(&control, &source, &target, "interrupted").await;
    let repeated = export(&control, &source, &target, "interrupted").await;
    assert_eq!(repeated.bytes(), archive.bytes());
    admin::interrupt_once("interrupted", Interruption::DuringImport);
    assert!(
        execute(
            &control,
            Command::Import {
                operation: "interrupted".into(),
                backup: archive.bytes().to_vec()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(
        status(&control, "interrupted").await.phase.as_deref(),
        Some("captured")
    );
    let mut sql = sqlx::PgConnection::connect_with(&source.options().unwrap())
        .await
        .unwrap();
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)")
            .bind(&target.schema)
            .fetch_one(&mut sql)
            .await
            .unwrap();
    assert!(!exists);
    sql.close().await.unwrap();
    execute(
        &control,
        Command::Import {
            operation: "interrupted".into(),
            backup: archive.bytes().to_vec(),
        },
    )
    .await
    .unwrap();
    refuses(&target).await;
    admin::interrupt_once("interrupted", Interruption::BeforeSelection);
    assert!(
        execute(
            &control,
            Command::Activate {
                operation: "interrupted".into()
            }
        )
        .await
        .is_err()
    );
    let before = status(&control, "interrupted").await;
    assert_eq!(before.phase.as_deref(), Some("staged"));
    assert_eq!(before.selected.schema, source.schema);
    refuses(&source).await;
    refuses(&target).await;
    admin::interrupt_once("interrupted", Interruption::AfterSelectionCommit);
    assert!(
        execute(
            &control,
            Command::Activate {
                operation: "interrupted".into()
            }
        )
        .await
        .is_err()
    );
    let committed = status(&control, "interrupted").await;
    assert_eq!(committed.phase.as_deref(), Some("committed"));
    assert_eq!(committed.selected.schema, target.schema);
    execute(
        &control,
        Command::Activate {
            operation: "interrupted".into(),
        },
    )
    .await
    .unwrap();
    execute(
        &control,
        Command::Import {
            operation: "interrupted".into(),
            backup: archive.bytes().to_vec(),
        },
    )
    .await
    .unwrap();
    assert!(
        execute(
            &control,
            Command::Cancel {
                operation: "interrupted".into()
            }
        )
        .await
        .is_err()
    );
    close(owner).await;
    refuses(&source).await;
    let next = open(target.clone()).await;
    assert!(
        next.handle()
            .mutation_service()
            .unwrap()
            .store()
            .graph()
            .get("one")
            .is_some()
    );
    close(next).await;
    for c in [&source, &target, &control] {
        cleanup(c).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated PostgreSQL fixture; run the qualification script"]
async fn corruption_collisions_and_snapshot_gate_leave_unrelated_authority_intact() {
    let source = fixture("reject_source");
    let control = sibling(&source, "reject_control");
    let target = sibling(&source, "reject_target");
    let unrelated = sibling(&source, "unrelated");
    let owner = open(source.clone()).await;
    let other = open(unrelated.clone()).await;
    submit(&owner, "seed", vec![row("source")]).await;
    submit(&other, "seed", vec![row("unrelated")]).await;
    let original = stored(&unrelated).await;
    for (from, to) in [
        (&source, &source),
        (&control, &target),
        (&source, &control),
        (&source, &unrelated),
    ] {
        assert!(
            execute(
                &control,
                Command::Export {
                    operation: "collision".into(),
                    source: from.schema.clone(),
                    target: to.schema.clone()
                }
            )
            .await
            .is_err()
        );
    }
    let snapshot = owner
        .handle()
        .read_service()
        .postgres_snapshot(context())
        .await
        .unwrap();
    let mut short = control.clone();
    short.limits.lock_timeout = Duration::from_millis(50);
    assert!(
        execute(
            &short,
            Command::Export {
                operation: "blocked".into(),
                source: source.schema.clone(),
                target: target.schema.clone()
            }
        )
        .await
        .is_err()
    );
    snapshot.close().await.unwrap();
    let archive = export(&control, &source, &target, "valid").await;
    for bytes in [archive.bytes()[..archive.bytes().len() - 1].to_vec(), {
        let mut b = archive.bytes().to_vec();
        b[10] ^= 1;
        b
    }] {
        assert!(
            execute(
                &control,
                Command::Import {
                    operation: "valid".into(),
                    backup: bytes
                }
            )
            .await
            .is_err()
        );
    }
    // Recompute the integrity digest after changing the schema version, so
    // this specifically exercises semantic compatibility before target DDL.
    use sha2::{Digest, Sha256};
    let text = std::str::from_utf8(archive.bytes()).unwrap();
    let end = text.rfind(",\"sha256\":").unwrap();
    let body =
        text["{\"body\":".len()..end].replacen("\"schema_format\":3", "\"schema_format\":99", 1);
    let digest: String = Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let incompatible = format!("{{\"body\":{body},\"sha256\":\"{digest}\"}}").into_bytes();
    assert!(
        execute(
            &control,
            Command::Import {
                operation: "valid".into(),
                backup: incompatible
            }
        )
        .await
        .is_err()
    );
    assert_eq!(
        status(&control, "valid").await.phase.as_deref(),
        Some("captured")
    );
    assert_eq!(stored(&unrelated).await, original);
    execute(
        &control,
        Command::Import {
            operation: "valid".into(),
            backup: archive.bytes().to_vec(),
        },
    )
    .await
    .unwrap();
    execute(
        &control,
        Command::Cancel {
            operation: "valid".into(),
        },
    )
    .await
    .unwrap();
    refuses(&target).await;
    close(owner).await;
    let fresh = open(source.clone()).await;
    close(fresh).await;
    close(other).await;
    for c in [&source, &target, &control, &unrelated] {
        cleanup(c).await;
    }
}
