//! Actual HTTP selected typed single-point history and explicit write receipts.
use haystack_app::*;
use haystack_core::{
    codecs::{history, history_mutation as mutation, jeto},
    data::{HDict, HGrid},
    graph::{EntityGraph, SharedGraph},
    kinds::{HDateTime, HRef, Kind, Number},
};
use haystack_server::HaystackServer;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

struct Running {
    url: String,
    owner: ApplicationOwner,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    store: HisStore,
    mutations: HistoryMutationService,
}
fn sample(second: i64, value: Kind) -> HistorySample {
    HistorySample {
        ts: HDateTime::new(
            chrono::DateTime::from_timestamp(1_717_200_000 + second, 123_456_789)
                .unwrap()
                .fixed_offset(),
            "UTC",
        ),
        val: value,
    }
}
fn number(value: f64) -> Kind {
    Kind::Number(Number::unitless(value))
}
impl Running {
    async fn start(selected: bool, rows: usize) -> Self {
        Self::configured(selected, rows, None, false).await
    }
    async fn configured(
        selected: bool,
        rows: usize,
        auth: Option<haystack_server::auth::AuthManager>,
        fail_pull: bool,
    ) -> Self {
        let graph = SharedGraph::new(EntityGraph::new());
        let mut point = HDict::new();
        point.set("id", Kind::Ref(HRef::from_val("p")));
        point.set("his", Kind::Marker);
        point.set("kind", Kind::Str("Number".into()));
        point.set("tz", Kind::Str("UTC".into()));
        point.set("unit", Kind::Str("°F".into()));
        graph.add(point).unwrap();
        let store = HisStore::new();
        let builder =
            ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
                .unwrap();
        let history = HistoryService::new(
            builder.handle().read_service(),
            Arc::new(FaultProvider {
                store: store.clone(),
                fail_pull,
            }),
            HistoryLimits {
                total_rows: rows,
                ..HistoryLimits::default()
            },
        )
        .unwrap();
        let mutations = HistoryMutationService::new(
            history.clone(),
            Arc::new(AllowAllHistoryMutations),
            HistoryMutationLimits::default(),
        )
        .unwrap();
        let builder = builder.owned_history(history).unwrap();
        let builder = if selected {
            builder.history_mutations(mutations.clone()).unwrap()
        } else {
            builder
        };
        let mut server = HaystackServer::new(graph).with_scoped_reads(builder.handle());
        if let Some(auth) = auth {
            server = server.with_auth(auth);
        }
        let router = server.into_external_router().unwrap();
        let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
        owner.ready().await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api", listener.local_addr().unwrap());
        let (stop, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });
        Self {
            url,
            owner,
            stop,
            task,
            store,
            mutations,
        }
    }
    async fn seed(&self) {
        self.store.write("p", vec![]).unwrap();
        let request = self.write(
            "seed",
            vec![
                sample(0, number(1.0)),
                sample(1, Kind::NA),
                sample(2, number(3.0)),
            ],
        );
        assert!(matches!(
            self.mutations.submit(context(), request).await.unwrap(),
            HistoryWriteOutcome::Committed(_)
        ));
    }
    fn write(&self, operation: &str, samples: Vec<HistorySample>) -> HistoryWriteRequest {
        let state = self.store.state("p").unwrap();
        HistoryWriteRequest {
            identity: HistoryOperationIdentity {
                authority: state.authority,
                point: "p".into(),
                incarnation: state.incarnation,
                operation_id: operation.into(),
            },
            expected_generation: state.generation,
            samples,
        }
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
        let _ = self.stop.send(());
        self.task.await.unwrap();
    }
}
fn context() -> ReadContext {
    ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(3))
}
fn client() -> reqwest::Client {
    haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap()
}
fn read_json() -> Value {
    json!({"req":{"spec":"sys::Grid","cols":[{"name":"id","of":"sys::Ref"},{"name":"range","of":"sys::Str"}],"rows":[{"id":"p","range":"2024-06-01"}]}})
}
fn named(grid: &HGrid) -> Value {
    let bytes = jeto::encode(
        &Kind::Grid(Box::new(grid.clone())),
        &jeto::Context::standard(),
        Some("sys::Grid"),
        jeto::Boxing::Auto,
        jeto::Limits::default(),
    )
    .unwrap()
    .into_exact()
    .unwrap();
    json!({"req":serde_json::from_slice::<Value>(&bytes).unwrap()})
}
async fn response_json(response: reqwest::Response) -> Value {
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    serde_json::from_str(&text).unwrap()
}
async fn response_grid(response: reqwest::Response) -> HGrid {
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    match jeto::decode(
        text.as_bytes(),
        &jeto::Context::standard(),
        Some("sys::Grid"),
        jeto::Limits::default(),
    )
    .unwrap()
    {
        Kind::Grid(grid) => *grid,
        value => panic!("{value:?}"),
    }
}
#[tokio::test]
async fn named_whole_grid_and_get_read_preserve_number_na_fraction_and_metadata() {
    let running = Running::start(true, 100).await;
    running.seed().await;
    let http = client();
    let grid = response_grid(
        http.post(format!("{}/ph.api::hisRead", running.url))
            .header("Xeto-Version", "5")
            .json_body(&read_json())
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(grid.rows.len(), 3);
    assert!(!grid.meta.has("history"));
    assert!(matches!(grid.rows[0].get("val"), Some(Kind::Number(n)) if n.val == 1.0));
    assert_eq!(grid.rows[1].get("val"), Some(&Kind::NA));
    assert!(
        matches!(grid.rows[0].get("ts"), Some(Kind::DateTime(ts)) if ts.dt.timestamp_subsec_nanos() == 123_456_789)
    );
    assert!(grid.meta.has("id") && grid.meta.has("hisStart") && grid.meta.has("hisEnd"));
    let get = response_grid(
        http.get(format!(
            "{}/hisRead?req={}",
            running.url,
            percent(&read_json()["req"].to_string())
        ))
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(get, grid);
    let whole = "ver:\"3.0\"\nid,range\n@p,\"2024-06-01\"\n";
    let body = response_grid(
        http.post(format!("{}/ph.api::hisRead", running.url))
            .header("Xeto-Version", "5")
            .header("Content-Type", "text/zinc")
            .header("Accept", "application/json")
            .body(whole)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body, grid);
    running.close().await;
}
#[tokio::test]
async fn writes_preserve_duplicate_order_and_reconcile_without_resubmission() {
    let running = Running::start(true, 100).await;
    running.seed().await;
    let request = running.write(
        "ordered",
        vec![
            sample(3, number(4.0)),
            sample(3, number(5.0)),
            sample(4, Kind::NA),
        ],
    );
    let http = client();
    let send = || {
        http.post(format!("{}/ph.api::hisWrite", running.url))
            .header("Xeto-Version", "5")
            .json_body(&named(&mutation::request_grid(&request).unwrap()))
    };
    let grid = response_grid(send().send().await.unwrap()).await;
    assert!(grid.rows.is_empty());
    let outcome = mutation::outcome_from_grid(&grid).unwrap();
    let HistoryWriteOutcome::Committed(receipt) = &outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(receipt.submitted_samples, 3);
    assert_eq!(receipt.unique_samples, 2);
    assert_eq!(
        mutation::outcome_from_grid(&response_grid(send().send().await.unwrap()).await).unwrap(),
        outcome
    );
    assert_eq!(
        running
            .mutations
            .reconcile(context(), request.identity)
            .await
            .unwrap(),
        outcome
    );
    let read = response_grid(
        http.post(format!("{}/hisRead", running.url))
            .header("Xeto-Version", "5")
            .json_body(&read_json())
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(read.rows.len(), 5);
    assert_eq!(read.rows[3].get("val"), Some(&number(5.0)));
    assert_eq!(read.rows[4].get("val"), Some(&Kind::NA));
    let whole_request = running.write("whole-grid", vec![sample(8, number(12.0))]);
    let whole_body = mutation::encode_request(
        &whole_request,
        haystack_core::codecs::codec_for("text/zinc").unwrap(),
    )
    .unwrap();
    let whole = response_grid(
        http.post(format!("{}/ph.api::hisWrite", running.url))
            .header("Xeto-Version", "5")
            .header("Content-Type", "text/zinc")
            .header("Accept", "application/json")
            .body(whole_body)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(matches!(
        mutation::outcome_from_grid(&whole).unwrap(),
        HistoryWriteOutcome::Committed(_)
    ));
    let method = http
        .get(format!("{}/ph.api::hisWrite", running.url))
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap();
    assert_eq!(method.status(), 405);
    assert_eq!(method.headers()["allow"], "POST");
    running.close().await;
}
#[tokio::test]
async fn surplus_zinc_write_cells_reject_before_publication() {
    malformed_zinc_write_rows_reject_before_publication(",999").await;
}

#[tokio::test]
async fn trailing_zinc_write_scalar_garbage_rejects_before_publication() {
    malformed_zinc_write_rows_reject_before_publication(" garbage").await;
}

async fn malformed_zinc_write_rows_reject_before_publication(tail: &str) {
    let running = Running::start(true, 100).await;
    running.seed().await;
    let http = client();
    for path in ["hisWrite", "ph.api::hisWrite", "ph.api%3A%3AhisWrite"] {
        let request = running.write("strict-zinc", vec![sample(8, number(12.0))]);
        let body = mutation::encode_request(
            &request,
            haystack_core::codecs::codec_for("text/zinc").unwrap(),
        )
        .unwrap();
        // Alter the final sample row, preserving the valid control and
        // declared ts,val columns. Neither extra cells nor scalar tails
        // may disappear before the mutation intent is bound.
        let body = String::from_utf8(body).unwrap();
        let malformed = format!("{}{tail}\n", body.trim_end());
        let before = running.store.state("p").unwrap();
        let samples: Vec<_> = running
            .store
            .read("p", None, None)
            .into_iter()
            .map(|item| (item.ts, item.val))
            .collect();
        let response = http
            .post(format!("{}/{path}", running.url))
            .header("Xeto-Version", "5")
            .header("Content-Type", "text/zinc")
            .body(malformed)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let error: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
        assert_eq!(status, 400, "{path} {tail}: {error}");
        assert_eq!(error["spec"], "sys.api::InvalidArgsErr");
        assert_eq!(running.store.state("p").unwrap(), before);
        assert_eq!(
            running
                .store
                .read("p", None, None)
                .into_iter()
                .map(|item| (item.ts, item.val))
                .collect::<Vec<_>>(),
            samples
        );
    }
    // The same identity is still usable after rejected input; valid whole-grid
    // publication continues to bind and commit the intended sample.
    let request = running.write("strict-zinc", vec![sample(8, number(12.0))]);
    let body = mutation::encode_request(
        &request,
        haystack_core::codecs::codec_for("text/zinc").unwrap(),
    )
    .unwrap();
    let result = response_grid(
        http.post(format!("{}/hisWrite", running.url))
            .header("Xeto-Version", "5")
            .header("Content-Type", "text/zinc")
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(matches!(
        mutation::outcome_from_grid(&result).unwrap(),
        HistoryWriteOutcome::Committed(_)
    ));
    assert_eq!(running.store.len("p"), 4);
    running.close().await;
}

#[tokio::test]
async fn partial_reads_require_control_and_unselected_writes_are_hidden() {
    let running = Running::start(false, 1).await;
    running.seed().await;
    let http = client();
    let response = http
        .post(format!("{}/hisRead", running.url))
        .header("Xeto-Version", "5")
        .json_body(&read_json())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let request = history::request_grid(&HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01".into(),
    })
    .unwrap();
    let grid = response_grid(
        http.post(format!("{}/ph.api::hisRead", running.url))
            .header("Xeto-Version", "5")
            .json_body(&named(&request))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let result = history::result_from_grid(&grid).unwrap();
    assert_eq!(result.samples.len(), 1);
    assert_eq!(
        result.terminal,
        HistoryTerminal::Limited(HistoryReason::Rows)
    );
    let ops = http
        .get(format!("{}/sys.api::ops", running.url))
        .header("Xeto-Version", "5")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(ops.contains("ph.api::hisRead"));
    assert!(!ops.contains("ph.api::hisWrite"));
    let response = http
        .post(format!("{}/ph.api::hisWrite", running.url))
        .header("Xeto-Version", "5")
        .json_body(&json!({"req":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    running.close().await;
}

// Use a distinct name so reqwest/json feature unification cannot shadow this
// standalone-test helper with RequestBuilder::json.
trait JsonRequest {
    fn json_body(self, value: &Value) -> Self;
}
impl JsonRequest for reqwest::RequestBuilder {
    fn json_body(self, value: &Value) -> Self {
        self.header("Content-Type", "application/json")
            .body(value.to_string())
    }
}
fn percent(text: &str) -> String {
    text.bytes().map(|byte| format!("%{byte:02X}")).collect()
}

#[tokio::test]
async fn default_limits_thousand_sample_page_returns_truthful_bounded_control() {
    let running = Running::start(true, 1000).await;
    running
        .store
        .write(
            "p",
            (0..1000)
                .map(|second| {
                    let s = sample(second, number(second as f64));
                    HisItem {
                        ts: s.ts.dt,
                        val: s.val,
                    }
                })
                .collect(),
        )
        .unwrap();
    let request = history::request_grid(&HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01".into(),
    })
    .unwrap();
    let grid = response_grid(
        client()
            .post(format!("{}/ph.api::hisRead", running.url))
            .header("Xeto-Version", "5")
            .json_body(&named(&request))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let result = history::result_from_grid(&grid).unwrap();
    assert_eq!(result.metadata.coverage.retained_count, 1000);
    eprintln!(
        "default 1000-sample typed page: {} returned, {:?}",
        result.samples.len(),
        result.terminal
    );
    assert!(!result.samples.is_empty());
    match result.terminal {
        HistoryTerminal::Complete => assert_eq!(result.samples.len(), 1000),
        HistoryTerminal::Limited(
            HistoryReason::Bytes | HistoryReason::Work | HistoryReason::Rows,
        ) => assert!(result.samples.len() < 1000),
        other => panic!("unexpected terminal {other:?}"),
    }
    running.close().await;
}

#[tokio::test]
async fn write_controls_types_column_context_and_exact_timezone_are_not_coerced() {
    let running = Running::start(true, 100).await;
    running.seed().await;
    let http = client();
    let before = running.store.state("p").unwrap();
    for (index, value) in [
        json!({"spec":"sys::Int","val":"7"}),
        json!({"spec":"sys::Float","val":"7"}),
        Value::Null,
        json!({"spec":"sys::None"}),
        json!({"spec":"sys::Remove"}),
    ]
    .into_iter()
    .enumerate()
    {
        let grid = mutation::request_grid(
            &running.write(&format!("invalid-{index}"), vec![sample(10, number(7.0))]),
        )
        .unwrap();
        let mut body = named(&grid);
        body["req"]["rows"][0]["val"] = value;
        let response = http
            .post(format!("{}/ph.api::hisWrite", running.url))
            .header("Xeto-Version", "5")
            .json_body(&body)
            .send()
            .await
            .unwrap();
        if response.status() == 200 {
            assert!(matches!(
                mutation::outcome_from_grid(&response_grid(response).await).unwrap(),
                HistoryWriteOutcome::Rejected { .. }
            ));
        } else {
            assert_eq!(response.status(), 400);
        }
        assert_eq!(running.store.state("p").unwrap(), before);
    }
    let request = running.write("shape", vec![sample(11, number(8.0))]);
    let base = named(&mutation::request_grid(&request).unwrap());
    let mut invalid_context = base.clone();
    invalid_context["req"]["cols"][1]["of"] = json!("sys::Int");
    invalid_context["req"]["rows"][0]["val"] = json!(8);
    assert_eq!(
        http.post(format!("{}/ph.api::hisWrite", running.url))
            .header("Xeto-Version", "5")
            .json_body(&invalid_context)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut no_control = base.clone();
    no_control["req"]["meta"]
        .as_object_mut()
        .unwrap()
        .remove("historyWrite");
    let mut multipoint = base.clone();
    multipoint["req"]["meta"]
        .as_object_mut()
        .unwrap()
        .remove("id");
    let mut metadata = base.clone();
    metadata["req"]["cols"][0]["meta"] = json!({"hidden": "unrecognized"});
    for body in [no_control, multipoint, metadata] {
        assert_eq!(
            http.post(format!("{}/ph.api::hisWrite", running.url))
                .header("Xeto-Version", "5")
                .json_body(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let mut wrong_zone = request.clone();
    wrong_zone.identity.operation_id = "wrong-zone".into();
    wrong_zone.samples[0].ts.tz_name = "London".into();
    let grid = response_grid(
        http.post(format!("{}/ph.api::hisWrite", running.url))
            .header("Xeto-Version", "5")
            .json_body(&named(&mutation::request_grid(&wrong_zone).unwrap()))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(matches!(
        mutation::outcome_from_grid(&grid).unwrap(),
        HistoryWriteOutcome::Rejected {
            reason: HistoryWriteRejection::Invalid,
            ..
        }
    ));
    assert_eq!(running.store.state("p").unwrap(), before);
    // A supported structural Number context creates Number, not Int/Float.
    let mut valid = base;
    valid["req"]["cols"][1]["of"] = json!("sys::Number");
    valid["req"]["rows"][0]["val"] = json!(8);
    let grid = response_grid(
        http.post(format!("{}/hisWrite", running.url))
            .header("Xeto-Version", "5")
            .json_body(&valid)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(matches!(
        mutation::outcome_from_grid(&grid).unwrap(),
        HistoryWriteOutcome::Committed(_)
    ));
    running.close().await;
}

#[tokio::test]
async fn independent_coarse_write_permission_is_required_even_with_allow_all_mutations() {
    use haystack_server::auth::{
        AuthManager, AuthUser,
        users::{UserRecord, parse_password_hash},
    };
    let users = std::collections::HashMap::from([("fixture".into(), UserRecord {
        credentials: parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap(),
        permissions: vec!["read".into()],
    })]);
    let auth = AuthManager::new(users, Duration::from_secs(60));
    for (permission, permissions) in [
        ("read", vec!["read".into()]),
        ("write", vec!["write".into()]),
        ("read-write", vec!["read".into(), "write".into()]),
        ("admin", vec!["admin".into()]),
    ] {
        auth.inject_token(
            format!("fixture-{permission}"),
            AuthUser {
                username: permission.into(),
                permissions,
            },
        );
    }
    let running = Running::configured(true, 100, Some(auth), false).await;
    running.seed().await;
    let request = running.write("coarse", vec![sample(5, number(9.0))]);
    let body = named(&mutation::request_grid(&request).unwrap());
    let http = client();
    let before = running.store.state("p").unwrap();
    for path in ["ops", "sys.api::ops", "sys.api%3A%3Aops"] {
        for (permission, visible) in [("read", false), ("read-write", true), ("admin", true)] {
            let grid = response_json(
                http.get(format!("{}/{path}", running.url))
                    .header("Xeto-Version", "5")
                    .header(
                        "Authorization",
                        format!("BEARER authToken=fixture-{permission}"),
                    )
                    .send()
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(
                grid["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["qname"] == "ph.api::hisWrite"),
                visible,
                "{path} {permission}: {grid:?}"
            );
        }
    }
    for path in ["hisWrite", "ph.api::hisWrite", "ph.api%3A%3AhisWrite"] {
        assert_eq!(
            http.post(format!("{}/{path}", running.url))
                .header("Xeto-Version", "5")
                .header("Authorization", "BEARER authToken=fixture-read")
                .json_body(&body)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    assert_eq!(running.store.state("p").unwrap(), before);
    let outcome = response_grid(
        http.post(format!("{}/ph.api::hisWrite", running.url))
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=fixture-write")
            .json_body(&body)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(matches!(
        mutation::outcome_from_grid(&outcome).unwrap(),
        HistoryWriteOutcome::Committed(_)
    ));
    assert_eq!(
        http.post(format!("{}/hisRead", running.url))
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=fixture-write")
            .json_body(&read_json())
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    running.close().await;
}

#[tokio::test]
async fn anonymous_selected_mutations_remain_discoverable() {
    let running = Running::start(true, 100).await;
    let http = client();
    for path in ["ops", "sys.api::ops", "sys.api%3A%3Aops"] {
        let grid = response_json(
            http.get(format!("{}/{path}", running.url))
                .header("Xeto-Version", "5")
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert!(
            grid["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["qname"] == "ph.api::hisWrite")
        );
    }
    running.close().await;
}

struct FaultProvider {
    store: HisStore,
    fail_pull: bool,
}
impl HistoryProvider for FaultProvider {
    fn open(
        &self,
        id: String,
        start: chrono::DateTime<chrono::FixedOffset>,
        end: chrono::DateTime<chrono::FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        Box::pin(async move {
            let inner = self.store.open(id, start, end, budget).await?;
            Ok(Box::new(FaultSession {
                inner,
                fail_pull: self.fail_pull,
            }) as Box<dyn HistorySession>)
        })
    }
    fn history_write_capability(&self) -> Option<HistoryWriteCapability> {
        self.store.history_write_capability()
    }
    fn commit_history(&self, prepared: PreparedHistoryMutation) -> HistoryWriteOutcome {
        self.store.commit_history(prepared)
    }
}
struct FaultSession {
    inner: Box<dyn HistorySession>,
    fail_pull: bool,
}
impl HistorySession for FaultSession {
    fn metadata(&self) -> &ProviderHistoryMetadata {
        self.inner.metadata()
    }
    fn pull(&mut self, budget: HistoryPullBudget) -> HistoryFuture<'_, ProviderHistoryBatch> {
        if self.fail_pull {
            Box::pin(async { Err(HistoryProviderError::Failed) })
        } else {
            self.inner.pull(budget)
        }
    }
    fn close(&mut self) -> HistoryFuture<'_, ()> {
        self.inner.close()
    }
}
#[tokio::test]
async fn provider_failure_is_explicit_and_plain_read_never_reports_success() {
    let running = Running::configured(true, 100, None, true).await;
    running.seed().await;
    let http = client();
    assert_eq!(
        http.post(format!("{}/hisRead", running.url))
            .header("Xeto-Version", "5")
            .json_body(&read_json())
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    let grid = history::request_grid(&HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01".into(),
    })
    .unwrap();
    let result = response_grid(
        http.post(format!("{}/hisRead", running.url))
            .header("Xeto-Version", "5")
            .json_body(&named(&grid))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let result = history::result_from_grid(&result).unwrap();
    assert_eq!(result.metadata.coverage.retained_count, 3);
    assert!(result.samples.is_empty());
    assert_eq!(
        result.terminal,
        HistoryTerminal::Failed(HistoryReason::Provider)
    );
    running.close().await;
}
#[tokio::test]
async fn point_zone_conversion_exclusive_boundary_and_lossless_projection_match_shared_history() {
    let running = Running::start(true, 100).await;
    running.seed().await;
    let http = client();
    let mut body = read_json();
    body["req"]["rows"][0]["range"] =
        json!("2024-05-31T20:00:00-04:00 New_York,2024-06-01T20:00:00-04:00 New_York");
    let grid = response_grid(
        http.post(format!("{}/hisRead", running.url))
            .header("Xeto-Version", "5")
            .json_body(&body)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(grid.rows.len(), 3);
    assert!(
        matches!(grid.meta.get("hisStart"), Some(Kind::DateTime(ts)) if ts.tz_name == "UTC" && ts.dt.timestamp() == 1_717_200_000)
    );
    body["req"]["rows"][0]["range"] =
        json!("2024-06-01T00:00:00.123456789Z UTC,2024-06-01T00:00:01.123456789Z UTC");
    let grid = response_grid(
        http.post(format!("{}/hisRead", running.url))
            .header("Xeto-Version", "5")
            .json_body(&body)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(grid.rows.len(), 1);
    let response = http
        .post(format!("{}/hisRead", running.url))
        .header("Xeto-Version", "5")
        .header("Accept", "application/json;box=none")
        .json_body(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 406);
    let response = http
        .post(format!("{}/ph.api::hisRead", running.url))
        .header("Xeto-Version", "4")
        .header("Content-Type", "text/zinc")
        .header("Accept", "text/zinc")
        .body("ver:\"3.0\"\nid,range\n@p,\"2024-06-01\"\n")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let h4 = haystack_core::codecs::codec_for("text/zinc")
        .unwrap()
        .decode_grid(&response.text().await.unwrap())
        .unwrap();
    assert_eq!(h4.rows.len(), 3);
    assert_eq!(h4.rows[1].get("val"), Some(&Kind::NA));
    running.close().await;
}
