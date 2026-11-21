//! Real HTTP consumer for the installed standalone process owned by the runner.
//! No in-process application or mutable graph shortcut participates in this flow.
#![cfg(feature = "postgres")]
use haystack_client::{ClientConfig, HaystackClient};
use haystack_core::{
    codecs::{
        Codec,
        entity::{
            self, ChangesRequest, EntityBatchRequest, MutationOutcome, OperationIdentity,
            ReceiptQualification,
        },
        history::{HistoryReadRequest, HistorySample, HistoryTerminal},
        history_mutation::{
            self, HistoryOperationIdentity, HistoryReceiptQualification, HistoryWriteOutcome,
            HistoryWriteRequest,
        },
        zinc::ZincCodec,
    },
    data::{HDict, HGrid},
    graph::EntityOperation,
    kinds::{HRef, Kind, Number},
};
use std::path::PathBuf;

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "missing standalone fixture input {name}; run scripts/postgres/standalone.py --docker"
        )
    })
}
fn point() -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("standalone_point")));
    row.set("dis", Kind::Str("Standalone temperature".into()));
    row.set("point", Kind::Marker);
    row.set("his", Kind::Marker);
    row.set("kind", Kind::Str("Number".into()));
    row.set("tz", Kind::Str("UTC".into()));
    row.set("unit", Kind::Str("°C".into()));
    row
}
fn samples() -> Vec<HistorySample> {
    ["2026-01-01T00:00:00Z UTC", "2026-01-01T00:01:00Z UTC"]
        .into_iter()
        .enumerate()
        .map(|(i, value)| {
            let Kind::DateTime(ts) = ZincCodec.decode_scalar(value).unwrap() else {
                panic!("timestamp")
            };
            HistorySample {
                ts,
                val: Kind::Number(Number::new(20.5 + i as f64, Some("°C".into()))),
            }
        })
        .collect()
}
async fn forbidden(http: &reqwest::Client, url: &str, token: &str, op: &str, grid: HGrid) {
    let response = http
        .post(format!("{url}/{op}"))
        .header("Authorization", format!("BEARER authToken={token}"))
        .header("Content-Type", "text/zinc")
        .body(ZincCodec.encode_grid(&grid).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let grid = ZincCodec
        .decode_grid(&response.text().await.unwrap())
        .unwrap();
    assert!(grid.meta.has("err"));
}

#[tokio::test]
#[ignore = "requires runner-owned installed server and PostgreSQL; run scripts/postgres/standalone.py --docker"]
async fn installed_standalone_authorized_workflow() {
    let url = required("HAYSTACK_STANDALONE_URL");
    let password = required("HAYSTACK_STANDALONE_PASSWORD");
    let phase = required("HAYSTACK_STANDALONE_PHASE");
    assert!(phase == "populate" || phase == "recover");
    let state = PathBuf::from(required("HAYSTACK_STANDALONE_STATE"));
    let http = ClientConfig::default().build_reqwest_client().unwrap();
    let unauthenticated = http.get(format!("{url}/about")).send().await.unwrap();
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        HaystackClient::connect(&url, "operator", "wrong-fixture-password")
            .await
            .is_err()
    );
    let client = HaystackClient::connect(&url, "operator", &password)
        .await
        .unwrap();
    let viewer = HaystackClient::connect(&url, "viewer", &password)
        .await
        .unwrap();
    let token = haystack_client::auth::authenticate(&http, &url, "viewer", &password)
        .await
        .unwrap();
    let request = ChangesRequest {
        cursor: None,
        max_diffs: 16,
    };
    let before = client.entity_changes(&request).await.unwrap();
    let batch = if phase == "populate" {
        assert_eq!(before.head, 0);
        assert!(
            client
                .read("point", Some(10))
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        EntityBatchRequest {
            identity: OperationIdentity {
                operation_id: "standalone-create".into(),
                dataset: before.dataset,
                incarnation: before.incarnation,
            },
            expected_revision: before.head,
            operations: vec![EntityOperation::Add(point())],
        }
    } else {
        entity::from_grid(
            &ZincCodec
                .decode_grid(&std::fs::read_to_string(state.join("entity.zinc")).unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    // Observe explicit HTTP denial; the typed submission helper correctly treats
    // transport failures as Unknown, which alone cannot prove no effect.
    forbidden(
        &http,
        &url,
        &token,
        "entityBatch",
        entity::to_grid(&batch).unwrap(),
    )
    .await;
    assert_eq!(
        client.entity_changes(&request).await.unwrap().head,
        before.head
    );
    let outcome = if phase == "populate" {
        client.submit_entities(&batch).await.unwrap()
    } else {
        client.reconcile_entity(&batch.identity).await.unwrap()
    };
    let MutationOutcome::Committed(receipt) = outcome else {
        panic!("entity commit/receipt was not confirmed")
    };
    assert_eq!(receipt.identity, batch.identity);
    assert_eq!(receipt.before_revision, batch.expected_revision);
    assert_eq!(receipt.after_revision, batch.expected_revision + 1);
    assert_eq!(receipt.qualification, ReceiptQualification::PostgresDurable);
    if phase == "populate" {
        std::fs::write(
            state.join("entity-receipt.zinc"),
            ZincCodec
                .encode_grid(
                    &entity::to_grid(&MutationOutcome::Committed(receipt.clone())).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
    } else {
        let retained: MutationOutcome = entity::from_grid(
            &ZincCodec
                .decode_grid(&std::fs::read_to_string(state.join("entity-receipt.zinc")).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(MutationOutcome::Committed(receipt), retained);
    }
    let rows = viewer.read("point", Some(10)).await.unwrap().rows;
    assert_eq!(rows, vec![point()]);
    let history_query = HistoryReadRequest {
        id: "standalone_point".into(),
        range: "2026-01-01,2026-01-02".into(),
    };
    let before_history = viewer.his_read_scoped(&history_query).await.unwrap();
    assert_eq!(before_history.terminal, HistoryTerminal::Complete);
    let write = if phase == "populate" {
        assert!(before_history.samples.is_empty());
        let history = before_history.metadata.history;
        HistoryWriteRequest {
            identity: HistoryOperationIdentity {
                authority: history.authority,
                point: "standalone_point".into(),
                incarnation: history.incarnation,
                operation_id: "standalone-history".into(),
            },
            expected_generation: history.generation,
            samples: samples(),
        }
    } else {
        history_mutation::request_from_grid(
            &ZincCodec
                .decode_grid(&std::fs::read_to_string(state.join("history.zinc")).unwrap())
                .unwrap(),
        )
        .unwrap()
    };
    forbidden(
        &http,
        &url,
        &token,
        "hisWrite",
        history_mutation::request_grid(&write).unwrap(),
    )
    .await;
    let after_denial = client.his_read_scoped(&history_query).await.unwrap();
    assert_eq!(
        after_denial.metadata.history,
        before_history.metadata.history
    );
    assert_eq!(after_denial.samples, before_history.samples);
    let outcome = if phase == "populate" {
        client.his_write_scoped(&write).await.unwrap()
    } else {
        client.reconcile_history(&write.identity).await.unwrap()
    };
    let HistoryWriteOutcome::Committed(receipt) = outcome else {
        panic!("history commit/receipt was not confirmed")
    };
    assert_eq!(receipt.identity, write.identity);
    assert_eq!(receipt.before_generation, write.expected_generation);
    assert_eq!(receipt.after_generation, write.expected_generation + 1);
    assert_eq!(
        receipt.qualification,
        HistoryReceiptQualification::PostgresDurable
    );
    if phase == "populate" {
        std::fs::write(
            state.join("history-receipt.zinc"),
            ZincCodec
                .encode_grid(
                    &history_mutation::outcome_grid(&HistoryWriteOutcome::Committed(
                        receipt.clone(),
                    ))
                    .unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
    } else {
        let retained = history_mutation::outcome_from_grid(
            &ZincCodec
                .decode_grid(&std::fs::read_to_string(state.join("history-receipt.zinc")).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(HistoryWriteOutcome::Committed(receipt), retained);
    }
    let observed = viewer.his_read_scoped(&history_query).await.unwrap();
    assert_eq!(observed.terminal, HistoryTerminal::Complete);
    assert_eq!(
        observed.metadata.history.authority,
        write.identity.authority
    );
    assert_eq!(
        observed.metadata.history.incarnation,
        write.identity.incarnation
    );
    assert_eq!(
        observed.metadata.history.generation,
        write.expected_generation + 1
    );
    assert_eq!(observed.samples, samples());
    assert_eq!(observed.metadata.coverage.retained_count, 2);
    assert_eq!(
        client.entity_changes(&request).await.unwrap().dataset,
        batch.identity.dataset
    );
    assert_eq!(
        observed.metadata.graph.incarnation,
        batch.identity.incarnation
    );
    assert_eq!(
        observed.metadata.graph.revision,
        batch.expected_revision + 1
    );
    if phase == "populate" {
        std::fs::write(
            state.join("entity.zinc"),
            ZincCodec
                .encode_grid(&entity::to_grid(&batch).unwrap())
                .unwrap(),
        )
        .unwrap();
        std::fs::write(
            state.join("history.zinc"),
            ZincCodec
                .encode_grid(&history_mutation::request_grid(&write).unwrap())
                .unwrap(),
        )
        .unwrap();
    }
    client.close().await.unwrap();
    viewer.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires runner-owned installed selector server and restored PostgreSQL authority"]
async fn installed_standalone_restore_selected_authority() {
    let url = required("HAYSTACK_STANDALONE_URL");
    let password = required("HAYSTACK_STANDALONE_PASSWORD");
    let state = PathBuf::from(required("HAYSTACK_STANDALONE_STATE"));
    let client = HaystackClient::connect(&url, "operator", &password)
        .await
        .unwrap();
    let original: EntityBatchRequest = entity::from_grid(
        &ZincCodec
            .decode_grid(&std::fs::read_to_string(state.join("entity.zinc")).unwrap())
            .unwrap(),
    )
    .unwrap();
    let old_history = history_mutation::request_from_grid(
        &ZincCodec
            .decode_grid(&std::fs::read_to_string(state.join("history.zinc")).unwrap())
            .unwrap(),
    )
    .unwrap();
    let feed = client
        .entity_changes(&ChangesRequest {
            cursor: None,
            max_diffs: 16,
        })
        .await
        .unwrap();
    assert_eq!(feed.dataset, original.identity.dataset);
    assert_ne!(feed.incarnation, original.identity.incarnation);
    assert_eq!(feed.head, original.expected_revision + 1);
    assert_eq!(feed.floor, feed.head);
    assert!(feed.changes.is_empty());
    assert_eq!(
        client.read("point", Some(10)).await.unwrap().rows,
        vec![point()]
    );
    let history = client
        .his_read_scoped(&HistoryReadRequest {
            id: "standalone_point".into(),
            range: "2026-01-01,2026-01-02".into(),
        })
        .await
        .unwrap();
    assert_eq!(history.terminal, HistoryTerminal::Complete);
    assert_eq!(history.samples, samples());
    assert_ne!(
        history.metadata.history.authority,
        old_history.identity.authority
    );
    assert_ne!(
        history.metadata.history.incarnation,
        old_history.identity.incarnation
    );
    assert_eq!(
        history.metadata.history.generation,
        old_history.expected_generation + 1
    );
    assert!(!matches!(
        client.reconcile_entity(&original.identity).await.unwrap(),
        MutationOutcome::Committed(_)
    ));
    assert!(!matches!(
        client
            .reconcile_history(&old_history.identity)
            .await
            .unwrap(),
        HistoryWriteOutcome::Committed(_)
    ));
    client.close().await.unwrap();
}
