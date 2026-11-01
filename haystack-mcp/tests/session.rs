#![cfg(feature = "stdio")]
mod history_support;
mod resource_support;
use haystack_app::*;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, NominalScalar},
};
use haystack_mcp::{Limits, McpError, Report};
use rmcp::{
    model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
    service::{ClientLifecycleMode, serve_client_with_lifecycle},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
};

#[derive(Clone)]
struct Policy(Arc<AtomicUsize>);
struct Scope(usize, String);
impl ReadPolicy for Policy {
    fn snapshot(&self, principal: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        assert_eq!(*principal, principal_for_test());
        let epoch = self.0.load(Ordering::SeqCst);
        if epoch == 99 {
            return Err(ReadError::Forbidden);
        }
        Ok(Arc::new(Scope(epoch, format!("scope-{epoch}"))))
    }
}
impl PolicySnapshot for Scope {
    fn scope_key(&self) -> &str {
        &self.1
    }
    fn function(&self, _: &FunctionIdentity) -> bool {
        false
    }
    fn operation(&self, operation: ReadOperation) -> bool {
        operation == ReadOperation::Read && self.0 != 99
    }
    fn entity(&self, id: &str) -> bool {
        id != "denied"
    }
    fn tag(&self, _: &str, tag: &str) -> bool {
        tag != "secret"
    }
    fn reference(&self, target: &str) -> bool {
        target != "denied"
    }
    fn reference_display(&self, _: &str) -> bool {
        false
    }
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        false
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        false
    }
}
fn principal_for_test() -> Principal {
    Principal::TrustedEmbedding {
        subject: "fixture".into(),
    }
}

#[tokio::test]
async fn selected_history_is_discoverable_and_preserves_a_limited_prefix() {
    use haystack_core::codecs::{Codec, zinc::ZincCodec};
    let graph = SharedGraph::new(EntityGraph::new());
    let mut point = HDict::new();
    point.set("id", Kind::Ref(HRef::from_val("p")));
    point.set("his", Kind::Marker);
    point.set("kind", Kind::Str("Bool".into()));
    point.set("tz", Kind::Str("UTC".into()));
    graph.add(point).unwrap();
    let builder =
        ApplicationBuilder::new(graph, Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let store = Arc::new(HisStore::new());
    let values = ["2024-06-01T00:00:00Z UTC", "2024-06-01T00:00:01Z UTC"]
        .into_iter()
        .enumerate()
        .map(|(i, ts)| {
            let Kind::DateTime(ts) = ZincCodec.decode_scalar(ts).unwrap() else {
                panic!()
            };
            HisItem {
                ts: ts.dt,
                val: if i == 0 { Kind::Bool(true) } else { Kind::NA },
            }
        })
        .collect();
    store.write("p", values).unwrap();
    let history = HistoryService::new(
        builder.handle().read_service(),
        store,
        HistoryLimits::default(),
    )
    .unwrap();
    let owner = builder
        .owned_history(history)
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    let (mut wire, task) = wire(
        &owner,
        Limits {
            max_response_bytes: 2 * 1024 * 1024,
            ..Limits::default()
        },
        2 * 1024 * 1024,
    );
    initialize(&mut wire).await;
    wire.send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .await;
    let listed = wire.receive().await;
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "haystack_history")
    );
    let result = wire.call(3, "haystack_history", json!({"id":"p","start":"2024-06-01T00:00:00Z UTC","end":"2024-06-01T00:00:02Z UTC","max_rows":1,"max_bytes":2 * 1024 * 1024,"timeout_ms":1000})).await;
    assert_eq!(result["result"]["isError"], true);
    assert_eq!(result["result"]["structuredContent"]["row_count"], 1);
    assert_eq!(result["result"]["structuredContent"]["complete"], false);
    drop(wire);
    assert_eq!(task.await.unwrap().unwrap().admitted, 3);
    close(owner).await;
}
async fn history_app(probe: Arc<history_support::Probe>) -> ApplicationOwner {
    let builder = ApplicationBuilder::new(
        probe.graph.clone(),
        Arc::new(AllowAll),
        ReadLimits::default(),
    )
    .unwrap()
    .shutdown_policy(ShutdownPolicy {
        drain_timeout: Duration::ZERO,
        stop_timeout: Duration::from_secs(2),
    });
    let history = HistoryService::new(
        builder.handle().read_service(),
        probe,
        HistoryLimits {
            batch_rows: 1,
            ..HistoryLimits::default()
        },
    )
    .unwrap();
    let owner = builder
        .owned_history(history)
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    owner
}

#[tokio::test]
async fn history_stdio_slow_consumer_holds_delivery_until_deadline_or_eof() {
    for profile in [
        haystack_mcp::OutputProfile::H4,
        haystack_mcp::OutputProfile::Jeto,
    ] {
        for event in ["deadline", "eof"] {
            let probe = history_support::Probe::new(None);
            let owner = history_app(probe.clone()).await;
            let (mut wire, task) = wire_profile(
                &owner,
                Limits {
                    max_response_bytes: haystack_mcp::HISTORY_RESPONSE_BYTES,
                    request_timeout: Duration::from_secs(3),
                    ..Limits::default()
                },
                128,
                profile,
            );
            initialize(&mut wire).await;
            let mut arguments = history_support::args(profile);
            arguments["timeout_ms"] = json!(250);
            wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_history","arguments":arguments}})).await;
            let mut prefix = [0; 32];
            tokio::time::timeout(Duration::from_secs(1), wire.input.read_exact(&mut prefix))
                .await
                .unwrap()
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(probe.closed(), 1);
            assert_eq!(probe.closes(), 1);
            assert_eq!(probe.pulls(), 3, "the three one-row batches have finished");
            assert_eq!(owner.handle().read_service().load().admitted, 0);
            assert!(
                owner.handle().outstanding_tasks() > 0,
                "physical response must still own transport work"
            );
            assert!(!task.is_finished());
            if event == "eof" {
                wire.output.shutdown().await.unwrap();
            }
            let result = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            if event == "deadline" {
                assert_eq!(result.unwrap_err(), McpError::Deadline);
            } else {
                let report = result.unwrap();
                assert_eq!(report.admitted, report.released);
            }
            assert_eq!(
                probe.pulls(),
                3,
                "slow output cannot trigger more provider pulls"
            );
            assert_eq!(owner.handle().outstanding_tasks(), 0);
            close(owner).await;
        }
    }
}

#[tokio::test]
async fn history_stdio_profiles_floor_empty_limits_failure_and_invalid_arguments() {
    for profile in [
        haystack_mcp::OutputProfile::H4,
        haystack_mcp::OutputProfile::Jeto,
    ] {
        let probe = history_support::Probe::new(None);
        let owner = history_app(probe.clone()).await;
        let (mut wire, task) = wire_profile(
            &owner,
            Limits {
                max_response_bytes: haystack_mcp::HISTORY_RESPONSE_BYTES,
                ..Limits::default()
            },
            haystack_mcp::HISTORY_RESPONSE_BYTES,
            profile,
        );
        initialize(&mut wire).await;
        let id = "\u{1}".repeat(128);
        wire.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/list"}))
            .await;
        let discovery = wire.receive().await;
        assert_eq!(discovery["result"]["tools"].as_array().unwrap().len(), 3);
        assert!(
            serde_json::to_vec(&discovery).unwrap().len() <= haystack_mcp::HISTORY_RESPONSE_BYTES
        );
        let args = history_support::args(profile);
        wire.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"haystack_history","arguments":args}})).await;
        let full = wire.receive().await;
        assert_eq!(full["id"], id);
        assert!(serde_json::to_vec(&full).unwrap().len() <= haystack_mcp::HISTORY_RESPONSE_BYTES);
        let full = history_support::decoded(&full, profile);
        assert_eq!(full.terminal, HistoryTerminal::Complete);
        assert_eq!(
            full.samples
                .iter()
                .map(|s| s.val.clone())
                .collect::<Vec<_>>(),
            [Kind::Bool(true), Kind::NA, Kind::Bool(false)]
        );
        let mut limited = args.clone();
        limited["max_rows"] = json!(1);
        let partial =
            history_support::decoded(&wire.call(5, "haystack_history", limited).await, profile);
        assert_eq!(partial.samples.len(), 1);
        assert_eq!(
            partial.terminal,
            HistoryTerminal::Limited(HistoryReason::Rows)
        );
        let mut empty = args.clone();
        empty["end"] = empty["start"].clone();
        let empty =
            history_support::decoded(&wire.call(6, "haystack_history", empty).await, profile);
        assert!(empty.samples.is_empty());
        assert_eq!(empty.terminal, HistoryTerminal::Complete);
        let opens = probe.opens();
        let mut tiny = args.clone();
        tiny["max_bytes"] = json!(1);
        assert_eq!(
            wire.call(7, "haystack_history", tiny).await["result"]["structuredContent"]["error"]["code"],
            "budget"
        );
        for (field, value) in [
            ("max_rows", json!(0)),
            ("max_bytes", json!(0)),
            ("timeout_ms", json!(0)),
            ("max_rows", Value::Null),
            ("start", json!("today")),
            ("end", json!("2023-01-01T00:00:00Z UTC")),
            ("cursor", json!("opaque")),
        ] {
            let mut invalid = args.clone();
            invalid[field] = value;
            assert_eq!(
                wire.call(8, "haystack_history", invalid).await["error"]["code"],
                -32602
            );
        }
        if profile == haystack_mcp::OutputProfile::Jeto {
            let mut missing = args.clone();
            missing.as_object_mut().unwrap().remove("profile");
            assert_eq!(
                wire.call(8, "haystack_history", missing).await["error"]["code"],
                -32602
            );
        }
        assert_eq!(probe.opens(), opens);
        probe.fail_after(probe.pulls() + 1);
        let failed = history_support::decoded(
            &wire.call(9, "haystack_history", args.clone()).await,
            profile,
        );
        assert_eq!(failed.samples.len(), 1);
        assert_eq!(
            failed.terminal,
            HistoryTerminal::Failed(HistoryReason::Provider)
        );
        probe.fail_after(usize::MAX);
        let graph = probe.graph.clone();
        let mut point = graph.get("a").unwrap();
        point.set("kind", Kind::Str("Str".into()));
        graph.update("a", point).unwrap();
        probe
            .store
            .write(
                "a",
                vec![HisItem {
                    ts: chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:00Z").unwrap(),
                    val: Kind::Str("x".repeat(300_000)),
                }],
            )
            .unwrap();
        // Replace the series so the oversized first row cannot masquerade as
        // empty complete, and no value-sized response is ever produced.
        let oversized =
            history_support::decoded(&wire.call(10, "haystack_history", args).await, profile);
        assert!(oversized.samples.is_empty());
        assert_eq!(
            oversized.terminal,
            HistoryTerminal::Limited(HistoryReason::Bytes)
        );
        finished(wire, task).await;
        assert_eq!(probe.opens(), probe.closes());
        close(owner).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_stdio_cancel_and_teardown_retain_gated_provider_and_request_id() {
    use history_support::Stage;
    for profile in [
        haystack_mcp::OutputProfile::H4,
        haystack_mcp::OutputProfile::Jeto,
    ] {
        for stage in [Stage::Open, Stage::Pull, Stage::Close] {
            let probe = history_support::Probe::new(Some(stage));
            let owner = history_app(probe.clone()).await;
            let (mut wire, mut task) = wire_profile(
                &owner,
                Limits {
                    max_response_bytes: haystack_mcp::HISTORY_RESPONSE_BYTES,
                    max_in_flight: 1,
                    ..Limits::default()
                },
                64 * 1024,
                profile,
            );
            initialize(&mut wire).await;
            wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_history","arguments":history_support::args(profile)}})).await;
            probe.wait().await;
            wire.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}})).await;
            let duplicate = wire
                .call(7, "haystack_history", history_support::args(profile))
                .await;
            assert_eq!(duplicate["error"]["data"]["reason"], "duplicate_request_id");
            let pressure = wire
                .call(8, "haystack_history", history_support::args(profile))
                .await;
            assert_eq!(pressure["error"]["data"]["reason"], "capacity");
            drop(wire);
            assert!(
                tokio::time::timeout(Duration::from_millis(40), &mut task)
                    .await
                    .is_err()
            );
            assert_eq!(owner.handle().read_service().load().admitted, 1);
            assert_eq!(
                owner.handle().history_service().unwrap().active_sessions(),
                1
            );
            assert_eq!(probe.closed(), 0);
            probe.release();
            let report = tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(report.admitted, report.released);
            assert_eq!(probe.closes(), 1);
            assert_eq!(probe.closed(), 1);
            assert_eq!(owner.handle().read_service().load().admitted, 0);
            assert_eq!(
                owner.handle().history_service().unwrap().active_sessions(),
                0
            );
            close(owner).await;
        }
    }
}

#[tokio::test]
async fn history_stdio_generation_and_graph_changes_preserve_an_explicit_prefix() {
    for graph_change in [false, true] {
        let profile = haystack_mcp::OutputProfile::Jeto;
        let probe = history_support::Probe::new(Some(history_support::Stage::Pull));
        probe.gate_after(1);
        let owner = history_app(probe.clone()).await;
        let (mut wire, task) = wire_profile(
            &owner,
            Limits {
                max_response_bytes: haystack_mcp::HISTORY_RESPONSE_BYTES,
                ..Limits::default()
            },
            64 * 1024,
            profile,
        );
        initialize(&mut wire).await;
        wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_history","arguments":history_support::args(profile)}})).await;
        probe.wait().await;
        if graph_change {
            let graph = probe.graph.clone();
            let mut row = graph.get("a").unwrap();
            row.set("dis", Kind::Str("changed".into()));
            graph.update("a", row).unwrap();
        } else {
            probe
                .store
                .write(
                    "a",
                    vec![HisItem {
                        ts: chrono::DateTime::parse_from_rfc3339("2024-06-01T00:00:02Z").unwrap(),
                        val: Kind::Bool(true),
                    }],
                )
                .unwrap();
        }
        probe.release();
        let result = history_support::decoded(&wire.receive().await, profile);
        assert_eq!(result.samples.len(), 1);
        assert_eq!(
            result.terminal,
            HistoryTerminal::Interrupted(if graph_change {
                HistoryReason::MetadataChanged
            } else {
                HistoryReason::GenerationChanged
            })
        );
        finished(wire, task).await;
        close(owner).await;
    }
}
fn graph() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    for id in ["a", "b", "denied"] {
        let mut row = HDict::new();
        row.set(
            "id",
            Kind::Ref(HRef::new(id, Some("hidden display".into()))),
        );
        row.set("site", Kind::Marker);
        row.set("dis", Kind::Str(format!("entity {id}")));
        row.set("secret", Kind::Str("NEVER_DISCLOSE".into()));
        row.set("target", Kind::Ref(HRef::from_val("denied")));
        graph.add(row).unwrap();
    }
    graph
}
async fn app(graph: SharedGraph, limits: ReadLimits) -> (ApplicationOwner, Arc<AtomicUsize>) {
    let epoch = Arc::new(AtomicUsize::new(0));
    let owner = ApplicationBuilder::new(graph, Arc::new(Policy(epoch.clone())), limits)
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    owner.ready().await.unwrap();
    (owner, epoch)
}
async fn close(owner: ApplicationOwner) {
    owner.close().await.unwrap();
    assert!(owner.terminated().await.cleanup_errors.is_empty());
    assert_eq!(owner.handle().outstanding_tasks(), 0);
}
struct Wire {
    modern: bool,
    input: BufReader<ReadHalf<DuplexStream>>,
    output: WriteHalf<DuplexStream>,
}
impl Wire {
    async fn send(&mut self, mut value: Value) {
        if self.modern && value.get("id").is_some() {
            value["params"]["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}});
        }
        self.output
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn receive(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), self.input.read_line(&mut line))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        serde_json::from_str(&line).unwrap()
    }
    async fn call(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).await;
        self.receive().await
    }
}
fn wire(
    owner: &ApplicationOwner,
    limits: Limits,
    bytes: usize,
) -> (Wire, tokio::task::JoinHandle<Result<Report, McpError>>) {
    wire_profile(owner, limits, bytes, haystack_mcp::OutputProfile::H4)
}
fn wire_profile(
    owner: &ApplicationOwner,
    limits: Limits,
    bytes: usize,
    profile: haystack_mcp::OutputProfile,
) -> (Wire, tokio::task::JoinHandle<Result<Report, McpError>>) {
    wire_selection(owner, limits, bytes, profile, false)
}
fn wire_selection(
    owner: &ApplicationOwner,
    limits: Limits,
    bytes: usize,
    profile: haystack_mcp::OutputProfile,
    semantic_resources: bool,
) -> (Wire, tokio::task::JoinHandle<Result<Report, McpError>>) {
    let (client, server) = tokio::io::duplex(bytes);
    let (input, output) = tokio::io::split(server);
    let application = owner.handle();
    let task = tokio::spawn(haystack_mcp::serve(
        application,
        principal_for_test(),
        input,
        output,
        haystack_mcp::StdioConfig {
            limits,
            output_profile: profile,
            semantic_resources,
        },
        CancellationToken::new(),
    ));
    let (input, output) = tokio::io::split(client);
    (
        Wire {
            modern: false,
            input: BufReader::new(input),
            output,
        },
        task,
    )
}

#[tokio::test]
async fn rich_profile_requires_acknowledgment_and_preserves_scope_and_exact_errors() {
    let graph = graph();
    let mut row = graph.get("a").unwrap();
    row.set("large", Kind::Int(9_007_199_254_740_993));
    row.set("buf", Kind::Buf(vec![0, 255, 251]));
    row.set(
        "nominal",
        Kind::Nominal(
            NominalScalar::new(
                "sys::Version",
                "https://github.com/Project-Haystack/xeto",
                "873b922451d3ef4c0c9c08ef3daa542f352d69f3",
                "1.2.3",
            )
            .unwrap(),
        ),
    );
    graph.update("a", row).unwrap();
    let (owner, _) = app(graph.clone(), ReadLimits::default()).await;
    let (mut wire, task) = wire_profile(
        &owner,
        Limits::default(),
        1024 * 1024,
        haystack_mcp::OutputProfile::Jeto,
    );
    initialize(&mut wire).await;
    wire.send(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await;
    let inventory = wire.receive().await;
    for tool in inventory["result"]["tools"].as_array().unwrap() {
        assert_eq!(
            tool["inputSchema"]["properties"]["profile"]["const"],
            haystack_mcp::JETO_PROFILE
        );
        assert!(
            tool["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("profile"))
        );
    }
    for arguments in [
        json!({"ids":["a"]}),
        json!({"ids":["a"],"profile":"haystack-h4-json-v1"}),
        json!({"ids":["a"],"profile":7}),
    ] {
        assert_eq!(
            wire.call(2, "haystack_read", arguments).await["error"]["code"],
            -32602
        );
    }
    let response = wire
        .call(
            3,
            "haystack_read",
            json!({"ids":["a","denied","missing"],"profile":haystack_mcp::JETO_PROFILE}),
        )
        .await;
    let result = &response["result"];
    let page = &result["structuredContent"];
    assert_eq!(
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap(),
        *page
    );
    assert_eq!(page["profile"], haystack_mcp::JETO_PROFILE);
    assert_eq!(page["row_count"], 1);
    assert_eq!(
        page["grid"]["rows"][0]["large"],
        json!({"spec":"sys::Int","val":"9007199254740993"})
    );
    assert_eq!(
        page["grid"]["rows"][0]["buf"],
        json!({"spec":"sys::Buf","val":"AP_7"})
    );
    for hidden in ["secret", "target", "nominal"] {
        assert!(page["grid"]["rows"][0].get(hidden).is_none());
    }
    assert_eq!(
        page["catalog"]["revision"],
        "873b922451d3ef4c0c9c08ef3daa542f352d69f3"
    );
    let mut row = graph.get("a").unwrap();
    row.set(
        "unsupported",
        Kind::Symbol(haystack_core::kinds::Symbol::new("demo")),
    );
    graph.update("a", row).unwrap();
    let response = wire
        .call(
            4,
            "haystack_query",
            json!({"filter":"site","profile":haystack_mcp::JETO_PROFILE}),
        )
        .await;
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"],
        json!({"profile":haystack_mcp::JETO_PROFILE,"ok":false,"error":{"code":"projection"}})
    );
    finished(wire, task).await;
    close(owner).await;
}

#[tokio::test]
async fn released_sdk_client_reads_exact_rich_profile() {
    let graph = graph();
    let mut row = graph.get("a").unwrap();
    row.set("large", Kind::Int(i64::MAX));
    graph.update("a", row).unwrap();
    let (owner, _) = app(graph, ReadLimits::default()).await;
    let (client, server) = tokio::io::duplex(1024 * 1024);
    let (input, output) = tokio::io::split(server);
    let task = tokio::spawn(haystack_mcp::serve(
        owner.handle(),
        principal_for_test(),
        input,
        output,
        haystack_mcp::StdioConfig {
            output_profile: haystack_mcp::OutputProfile::Jeto,
            ..Default::default()
        },
        CancellationToken::new(),
    ));
    let client = serve_client_with_lifecycle(
        ClientConfig::default(),
        client,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .unwrap();
    let tools = client.list_tools(None).await.unwrap();
    assert_eq!(tools.tools.len(), 2);
    let result = client
        .call_tool(
            CallToolRequestParams::new("haystack_read").with_arguments(
                json!({"ids":["a"],"profile":haystack_mcp::JETO_PROFILE})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    let page = result.structured_content.unwrap();
    assert_eq!(page["ok"], true);
    assert_eq!(
        page["grid"]["rows"][0]["large"]["val"],
        "9223372036854775807"
    );
    client.cancel().await.unwrap();
    let report = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.admitted, report.released);
    close(owner).await;
}
async fn initialize(wire: &mut Wire) {
    wire.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wire","version":"1"}}})).await;
    let result = wire.receive().await;
    assert_eq!(result["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(result["result"]["capabilities"], json!({"tools":{}}));
    wire.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .await;
}
fn envelope(response: &Value) -> Value {
    let result = &response["result"];
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);
    assert_eq!(text["profile"], "haystack-h4-json-v1");
    text
}
async fn finished(wire: Wire, task: tokio::task::JoinHandle<Result<Report, McpError>>) -> Report {
    drop(wire);
    let report = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.admitted, report.released);
    report
}

#[tokio::test]
async fn discovery_response_floor_rejects_rich_limits_before_serving() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    for max_response_bytes in [8192, 16 * 1024 - 1] {
        let result = haystack_mcp::serve(
            owner.handle(),
            principal_for_test(),
            tokio::io::empty(),
            tokio::io::sink(),
            haystack_mcp::StdioConfig {
                limits: Limits {
                    max_response_bytes,
                    ..Default::default()
                },
                output_profile: haystack_mcp::OutputProfile::Jeto,
                semantic_resources: false,
            },
            CancellationToken::new(),
        )
        .await;
        assert!(
            matches!(result, Err(McpError::Configuration)),
            "rich limit {max_response_bytes} was accepted: {result:?}"
        );
    }
    close(owner).await;
}

#[tokio::test]
async fn discovery_response_floor_fits_both_tools_and_maximally_escaped_id() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    for (profile, max_response_bytes) in [
        (haystack_mcp::OutputProfile::H4, 8192),
        (haystack_mcp::OutputProfile::Jeto, 16 * 1024),
    ] {
        let (mut wire, task) = wire_profile(
            &owner,
            Limits {
                max_response_bytes,
                ..Default::default()
            },
            64 * 1024,
            profile,
        );
        initialize(&mut wire).await;
        let id = "\u{1}".repeat(128);
        wire.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/list"}))
            .await;
        let mut line = String::new();
        assert!(
            tokio::time::timeout(Duration::from_secs(3), wire.input.read_line(&mut line))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        assert!(
            line.len() <= max_response_bytes,
            "discovery exceeded the selected frame cap"
        );
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 2);
        finished(wire, task).await;
    }
    close(owner).await;
}

#[tokio::test]
async fn released_sdk_client_discovers_and_reads_modern_profile() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (client, server) = tokio::io::duplex(1024 * 1024);
    let (input, output) = tokio::io::split(server);
    let task = tokio::spawn(haystack_mcp::serve(
        owner.handle(),
        principal_for_test(),
        input,
        output,
        haystack_mcp::StdioConfig::default(),
        CancellationToken::new(),
    ));
    let client = serve_client_with_lifecycle(
        ClientConfig::default(),
        client,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .unwrap();
    let tools = client.list_tools(None).await.unwrap();
    assert_eq!(tools.tools.len(), 2);
    assert_eq!(tools.tools[0].name, "haystack_query");
    assert_eq!(tools.tools[1].name, "haystack_read");
    for tool in tools.tools {
        assert_eq!(tool.annotations.unwrap().read_only_hint, Some(true));
        assert!(tool.output_schema.is_some());
    }
    let result = client
        .call_tool(
            CallToolRequestParams::new("haystack_read")
                .with_arguments(json!({"ids":["a"]}).as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let value = result.structured_content.unwrap();
    assert_eq!(value["row_count"], 1);
    assert_eq!(value["ok"], true);
    assert!(
        client
            .peer_info()
            .is_some_and(|info| info.protocol_version == ProtocolVersion::V_2026_07_28)
    );
    client.cancel().await.unwrap();
    let report = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.admitted, report.released);
    owner.ready().await.unwrap();
    assert_eq!(
        owner
            .handle()
            .read_service()
            .read(
                ReadContext::with_timeout(principal_for_test(), Duration::from_secs(1)),
                ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed)
            )
            .await
            .unwrap()
            .row_count,
        1
    );
    close(owner).await;
}

#[tokio::test]
async fn raw_legacy_scope_paging_reauthorization_and_strict_errors() {
    let graph = graph();
    let (owner, epoch) = app(graph.clone(), ReadLimits::default()).await;
    let (mut wire, task) = wire(&owner, Limits::default(), 1024 * 1024);
    initialize(&mut wire).await;
    let first = envelope(
        &wire
            .call(1, "haystack_query", json!({"filter":"site","page_size":1}))
            .await,
    );
    assert_eq!(first["row_count"], 1);
    assert_eq!(first["complete"], false);
    let cursor = first["cursor"].clone();
    assert!(!first.to_string().contains("NEVER_DISCLOSE"));
    assert!(!first.to_string().contains("hidden display"));
    assert!(!first.to_string().contains("denied"));
    let second = envelope(
        &wire
            .call(
                2,
                "haystack_query",
                json!({"filter":"site","page_size":1,"cursor":cursor}),
            )
            .await,
    );
    assert_eq!(second["complete"], true);
    assert_eq!(second["row_count"], 1);
    let missing = envelope(
        &wire
            .call(3, "haystack_read", json!({"ids":["denied","missing"]}))
            .await,
    );
    assert_eq!(missing["row_count"], 0);
    assert_eq!(missing["ok"], true);
    for args in [
        json!({"ids":["a"],"principal":"admin"}),
        json!({"ids":["a"],"path":"/private"}),
        json!({"ids":"a"}),
    ] {
        assert_eq!(
            wire.call(4, "haystack_read", args).await["error"]["code"],
            -32602
        );
    }
    assert_eq!(
        wire.call(4, "write", json!({})).await["error"]["code"],
        -32602
    );
    wire.send(json!({"jsonrpc":"2.0","id":4,"method":"resources/list"}))
        .await;
    assert_eq!(wire.receive().await["error"]["code"], -32601);
    let page = envelope(
        &wire
            .call(5, "haystack_query", json!({"filter":"site","page_size":1}))
            .await,
    );
    epoch.store(1, Ordering::SeqCst);
    assert_eq!(
        envelope(
            &wire
                .call(
                    6,
                    "haystack_query",
                    json!({"filter":"site","page_size":1,"cursor":page["cursor"]})
                )
                .await
        )["error"]["code"],
        "stale_cursor"
    );
    let page = envelope(
        &wire
            .call(7, "haystack_query", json!({"filter":"site","page_size":1}))
            .await,
    );
    let mut row = graph.get("a").unwrap();
    row.set("dis", Kind::Str("changed".into()));
    graph.update("a", row).unwrap();
    assert_eq!(
        envelope(
            &wire
                .call(
                    8,
                    "haystack_query",
                    json!({"filter":"site","page_size":1,"cursor":page["cursor"]})
                )
                .await
        )["error"]["code"],
        "stale_cursor"
    );
    epoch.store(99, Ordering::SeqCst);
    assert_eq!(
        envelope(&wire.call(9, "haystack_read", json!({"ids":["a"]})).await)["error"]["code"],
        "forbidden"
    );
    finished(wire, task).await;
    close(owner).await;
}

#[tokio::test]
async fn rich_nested_projection_and_application_input_output_limits() {
    let graph = graph();
    let mut row = graph.get("a").unwrap();
    row.set("rich", Kind::List(vec![Kind::List(vec![Kind::Int(42)])]));
    graph.update("a", row).unwrap();
    let (owner, _) = app(
        graph,
        ReadLimits {
            max_ids: 2,
            max_rows: 2,
            ..ReadLimits::default()
        },
    )
    .await;
    let (mut wire, task) = wire(
        &owner,
        Limits {
            max_response_bytes: 8192,
            ..Limits::default()
        },
        64 * 1024,
    );
    initialize(&mut wire).await;
    assert_eq!(
        envelope(&wire.call(1, "haystack_read", json!({"ids":["a"]})).await)["error"]["code"],
        "projection"
    );
    assert_eq!(
        envelope(
            &wire
                .call(2, "haystack_read", json!({"ids":["a"],"projection":["id"]}))
                .await
        )["ok"],
        true
    );
    assert_eq!(
        envelope(
            &wire
                .call(3, "haystack_read", json!({"ids":["a","b","c"]}))
                .await
        )["error"]["code"],
        "budget"
    );
    assert_eq!(
        envelope(
            &wire
                .call(4, "haystack_read", json!({"ids":["a"],"page_size":3}))
                .await
        )["error"]["code"],
        "budget"
    );
    finished(wire, task).await;
    close(owner).await;
}

#[tokio::test]
async fn framing_depth_ids_and_slow_output_fail_closed_and_release_work() {
    for attack in [
        vec![b'x'; 1025],
        format!("{}0{}\n", "[".repeat(10), "]".repeat(10)).into_bytes(),
    ] {
        let (owner, _) = app(graph(), ReadLimits::default()).await;
        let (mut wire, task) = wire(
            &owner,
            Limits {
                max_frame_bytes: 1024,
                max_json_depth: 4,
                ..Limits::default()
            },
            8192,
        );
        wire.output.write_all(&attack).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err(),
            McpError::Framing
        );
        assert_eq!(owner.handle().outstanding_tasks(), 0);
        close(owner).await;
    }
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(
        &owner,
        Limits {
            request_timeout: Duration::from_millis(100),
            ..Limits::default()
        },
        64,
    );
    wire.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wire","version":"1"}}})).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(owner.handle().outstanding_tasks() > 0);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        McpError::Deadline
    );
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    close(owner).await;
}

#[test]
fn cancelled_work_keeps_quota_and_id_until_real_cleanup_then_allows_reuse() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (owner, _) = runtime.block_on(app(graph(), ReadLimits::default()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = runtime.spawn_blocking(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    runtime.block_on(async {
        let (mut wire,task)=wire(&owner,Limits {max_in_flight:1,..Limits::default()},64*1024); initialize(&mut wire).await;
        wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_read","arguments":{"ids":["a"]}}})).await;
        tokio::time::timeout(Duration::from_secs(1),async { while owner.handle().read_service().load().admitted==0 {tokio::task::yield_now().await;} }).await.unwrap();
        wire.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}})).await;
        let duplicate=wire.call(7,"haystack_read",json!({"ids":["a"]})).await;
        let pressure=wire.call(8,"haystack_read",json!({"ids":["a"]})).await;
        let retained=(owner.handle().read_service().load().admitted,owner.handle().outstanding_tasks());
        release_tx.send(()).unwrap(); blocker.await.unwrap();
        assert_eq!(duplicate["error"]["data"]["reason"],"duplicate_request_id"); assert_eq!(pressure["error"]["data"]["reason"],"capacity"); assert_eq!(retained.0,1); assert!(retained.1>=2);
        tokio::time::timeout(Duration::from_secs(2),async { while owner.handle().outstanding_tasks()>0 {tokio::task::yield_now().await;} }).await.unwrap();
        let reused=envelope(&wire.call(7,"haystack_read",json!({"ids":["a"]})).await); assert_eq!(reused["row_count"],1);
        let report=finished(wire,task).await; assert_eq!(report.peak_in_flight,1); assert_eq!(report.suppressed,1); close(owner).await;
    });
}

#[tokio::test]
async fn oversized_ids_and_full_result_budget_are_explicit_and_bounded() {
    let graph = graph();
    let mut row = graph.get("a").unwrap();
    row.set("large", Kind::Str("x".repeat(4000)));
    graph.update("a", row).unwrap();
    let (owner, _) = app(graph, ReadLimits::default()).await;
    let (mut wire, task) = wire(
        &owner,
        Limits {
            max_response_bytes: 8192,
            ..Limits::default()
        },
        64 * 1024,
    );
    initialize(&mut wire).await;
    wire.send(json!({"jsonrpc":"2.0","id":"x".repeat(129),"method":"tools/list"}))
        .await;
    let rejected = wire.receive().await;
    assert!(rejected.get("id").is_none());
    assert_eq!(rejected["error"]["data"]["reason"], "request_id_too_large");
    assert_eq!(
        envelope(&wire.call(1, "haystack_read", json!({"ids":["a"]})).await)["error"]["code"],
        "output_budget"
    );
    assert_eq!(
        envelope(
            &wire
                .call(2, "haystack_read", json!({"ids":["a"],"projection":["id"]}))
                .await
        )["row_count"],
        1
    );
    finished(wire, task).await;
    close(owner).await;
}

#[tokio::test]
async fn cancellation_interrupts_a_partially_delivered_blocked_response() {
    let graph = graph();
    let mut row = graph.get("a").unwrap();
    row.set("large", Kind::Str("x".repeat(4000)));
    graph.update("a", row).unwrap();
    let (owner, _) = app(graph, ReadLimits::default()).await;
    let (mut wire, mut task) = wire(&owner, Limits::default(), 512);
    initialize(&mut wire).await;
    wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_read","arguments":{"ids":["a"]}}})).await;
    let mut prefix = [0u8; 32];
    tokio::time::timeout(Duration::from_secs(2), wire.input.read_exact(&mut prefix))
        .await
        .unwrap()
        .unwrap();
    // A received prefix proves physical delivery began. Leave the remaining
    // output blocked and cancel over the independent input half.
    wire.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}))
        .await;
    let prompt = tokio::time::timeout(Duration::from_millis(300), &mut task).await;
    drop(wire);
    if prompt.is_err() {
        let _ = task.await;
    }
    assert!(
        prompt.is_ok(),
        "cancellation must interrupt blocked physical delivery"
    );
    assert!(
        prompt.unwrap().unwrap().is_err(),
        "a partially delivered frame cannot leave a reusable connection"
    );
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    close(owner).await;
}

#[derive(Default)]
struct WriteGate {
    blocked: std::sync::atomic::AtomicBool,
    polled: tokio::sync::Notify,
    waker: std::sync::Mutex<Option<std::task::Waker>>,
}
impl WriteGate {
    fn release(&self) {
        self.blocked.store(false, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}
struct GatedWriter {
    inner: WriteHalf<DuplexStream>,
    gate: Arc<WriteGate>,
}
impl tokio::io::AsyncWrite for GatedWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.gate.blocked.load(Ordering::SeqCst) {
            *self.gate.waker.lock().unwrap() = Some(cx.waker().clone());
            self.gate.polled.notify_one();
            return std::task::Poll::Pending;
        }
        std::pin::Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
fn gated_wire(
    owner: &ApplicationOwner,
    cancelled: CancellationToken,
) -> (
    Wire,
    Arc<WriteGate>,
    tokio::task::JoinHandle<Result<Report, McpError>>,
) {
    gated_wire_selection(owner, cancelled, false, Duration::from_secs(10))
}
fn gated_wire_selection(
    owner: &ApplicationOwner,
    cancelled: CancellationToken,
    semantic_resources: bool,
    timeout: Duration,
) -> (
    Wire,
    Arc<WriteGate>,
    tokio::task::JoinHandle<Result<Report, McpError>>,
) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(server);
    let gate = Arc::new(WriteGate::default());
    let task = tokio::spawn(haystack_mcp::serve(
        owner.handle(),
        principal_for_test(),
        input,
        GatedWriter {
            inner: output,
            gate: gate.clone(),
        },
        haystack_mcp::StdioConfig {
            limits: Limits {
                max_in_flight: 1,
                request_timeout: timeout,
                ..Limits::default()
            },
            semantic_resources,
            ..Default::default()
        },
        cancelled,
    ));
    let (input, output) = tokio::io::split(client);
    (
        Wire {
            modern: false,
            input: BufReader::new(input),
            output,
        },
        gate,
        task,
    )
}
async fn block_response(wire: &mut Wire, gate: &WriteGate) {
    gate.blocked.store(true, Ordering::SeqCst);
    wire.send(json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"haystack_read","arguments":{"ids":["a"]}}})).await;
    tokio::time::timeout(Duration::from_secs(2), gate.polled.notified())
        .await
        .unwrap();
}
#[tokio::test]
async fn cancellation_after_zero_byte_pending_suppresses_and_reuses_id() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, gate, mut task) = gated_wire(&owner, CancellationToken::new());
    initialize(&mut wire).await;
    block_response(&mut wire, &gate).await;
    wire.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}))
        .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while owner.handle().outstanding_tasks() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    if let Ok(result) = tokio::time::timeout(Duration::from_millis(30), &mut task).await {
        close(owner).await;
        panic!("zero-byte cancellation closed the reusable session: {result:?}");
    }
    gate.release();
    assert_eq!(
        envelope(&wire.call(7, "haystack_read", json!({"ids":["a"]})).await)["row_count"],
        1
    );
    let report = finished(wire, task).await;
    assert_eq!(report.suppressed, 1);
    assert_eq!(report.delivered, 2);
    close(owner).await;
}
#[tokio::test]
async fn clean_eof_and_external_stop_join_a_blocked_response_successfully() {
    for external in [false, true] {
        let (owner, _) = app(graph(), ReadLimits::default()).await;
        let cancelled = CancellationToken::new();
        let (mut wire, gate, task) = gated_wire(&owner, cancelled.clone());
        initialize(&mut wire).await;
        block_response(&mut wire, &gate).await;
        if external {
            cancelled.cancel();
        } else {
            wire.output.shutdown().await.unwrap();
        }
        let outcome = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owner.handle().outstanding_tasks(), 0);
        owner.ready().await.unwrap();
        let usable = owner
            .handle()
            .read_service()
            .read(
                ReadContext::with_timeout(principal_for_test(), Duration::from_secs(1)),
                ReadRequest::new(ReadQuery::Ids(vec!["a".into()]), OutputProfile::Typed),
            )
            .await
            .unwrap();
        assert_eq!(usable.row_count, 1);
        close(owner).await;
        let report = outcome.expect("a requested stop is successful after joined cleanup");
        assert_eq!(report.admitted, report.released);
        assert_eq!(
            report.delivered, 1,
            "interrupted response was not delivered"
        );
    }
}
#[tokio::test]
async fn jsonrpc_error_classes_echo_valid_ids_and_leave_session_usable() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(&owner, Limits::default(), 64 * 1024);
    initialize(&mut wire).await;
    let mut mismatches = Vec::new();
    for (frame, id, code) in [
        ("{\n".to_owned(), Value::Null, -32700),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":4}\n".to_owned(),
            json!(4),
            -32600,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":42}}\n"
                .to_owned(),
            json!(5),
            -32602,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/call\",\"params\":{}}\n".to_owned(),
            json!(6),
            -32602,
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"unavailable\",\"params\":{}}\n".to_owned(),
            json!(8),
            -32601,
        ),
    ] {
        wire.output.write_all(frame.as_bytes()).await.unwrap();
        let response = wire.receive().await;
        let expected_id = if id.is_null() { None } else { Some(&id) };
        if response["error"]["code"] != code || response.get("id") != expected_id {
            mismatches.push((id, code, response));
        }
    }
    assert_eq!(
        envelope(&wire.call(9, "haystack_read", json!({"ids":["a"]})).await)["row_count"],
        1
    );
    finished(wire, task).await;
    close(owner).await;
    assert!(
        mismatches.is_empty(),
        "wrong JSON-RPC error classes or IDs: {mismatches:?}"
    );
}
#[tokio::test]
async fn an_unfinished_input_frame_keeps_its_real_deadline() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(
        &owner,
        Limits {
            request_timeout: Duration::from_millis(100),
            ..Limits::default()
        },
        64 * 1024,
    );
    initialize(&mut wire).await;
    wire.output.write_all(b"{\"jsonrpc\":").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        McpError::Deadline
    );
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    close(owner).await;
}

#[tokio::test]
async fn a_real_framing_failure_survives_blocked_writer_cleanup() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, gate, task) = gated_wire(&owner, CancellationToken::new());
    initialize(&mut wire).await;
    block_response(&mut wire, &gate).await;
    wire.output
        .write_all(&vec![b'x'; Limits::default().max_frame_bytes + 1])
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err(), McpError::Framing);
    assert_eq!(owner.handle().outstanding_tasks(), 0);
    close(owner).await;
}

#[tokio::test]
async fn invalid_present_ids_are_rejected_before_notification_fallback() {
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(&owner, Limits::default(), 64 * 1024);
    initialize(&mut wire).await;
    let mut rejected = 0;
    let mut silently_ignored = Vec::new();
    for (index, id) in [
        json!(true),
        json!({}),
        json!([]),
        Value::Null,
        json!(1.5),
        // `1.0` is deliberately absent: since rmcp 3.6.0 (#1346) an integral
        // float ID is read as that integer and answered. See
        // `integral_float_id_is_answered_as_that_integer`.
        json!(u64::MAX),
        json!(-9223372036854775809.0),
    ]
    .into_iter()
    .enumerate()
    {
        wire.send(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"haystack_read","arguments":{"ids":["a"]}}})).await;
        // A later valid request is a processing barrier, so the RED case
        // observes silent consumption without waiting for a guessed timeout.
        let probe = format!("probe-{index}");
        wire.send(json!({"jsonrpc":"2.0","id":probe,"method":"ping"}))
            .await;
        let first = wire.receive().await;
        if first["id"] == probe {
            silently_ignored.push(id);
        } else {
            assert_eq!(first["error"]["code"], -32600, "{first}");
            assert!(
                first.get("id").is_none(),
                "MCP unidentified errors omit the ID: {first}"
            );
            rejected += 1;
            assert_eq!(wire.receive().await["id"], probe);
        }
    }
    assert_eq!(rejected, 7, "every invalid present ID must be rejected");
    // Truly absent IDs remain notifications: initialized, unselected, and
    // cancellation of an unknown request are consumed without a response.
    for notification in [
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","method":"fixture/notification","params":{"value":true}}),
        json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"unknown"}}),
    ] {
        wire.send(notification).await;
    }
    let response = wire.call(99, "haystack_read", json!({"ids":["a"]})).await;
    assert_eq!(response["id"], 99);
    assert_eq!(envelope(&response)["row_count"], 1);
    let report = finished(wire, task).await;
    // initialize, seven probes, and the final call.
    assert_eq!(
        report.admitted, 9,
        "invalid IDs and notifications must not be admitted"
    );
    assert_eq!(
        report.delivered,
        9 + rejected,
        "genuine notifications must not produce responses"
    );
    close(owner).await;
    assert!(
        silently_ignored.is_empty(),
        "invalid present IDs were consumed as notifications: {silently_ignored:?}"
    );
}

#[tokio::test]
async fn integral_float_id_is_answered_as_that_integer() {
    // rmcp 3.6.0 (#1346) reads a request ID written in float form as that
    // integer. We accept the SDK behaviour: `"id":1.0` is admitted, served, and
    // answered with the integer ID `1`, not rejected as an invalid ID.
    let (owner, _) = app(graph(), ReadLimits::default()).await;
    let (mut wire, task) = wire(&owner, Limits::default(), 64 * 1024);
    initialize(&mut wire).await;
    // Written raw so the float form reaches the wire exactly as `1.0`.
    wire.output
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1.0,\"method\":\"tools/call\",\"params\":{\"name\":\"haystack_read\",\"arguments\":{\"ids\":[\"a\"]}}}\n")
        .await
        .unwrap();
    let response = wire.receive().await;
    assert!(response["id"].is_i64(), "{response}");
    assert_eq!(response["id"], json!(1), "{response}");
    assert_eq!(envelope(&response)["row_count"], 1);
    let report = finished(wire, task).await;
    assert_eq!(report.admitted, 2, "initialize and the float-form request");
    assert_eq!(report.delivered, 2);
    close(owner).await;
}

#[tokio::test]
async fn unidentified_errors_omit_ids_before_and_after_each_selected_lifecycle() {
    for modern in [false, true] {
        let (owner, _) = app(graph(), ReadLimits::default()).await;
        let (mut wire, task) = wire(&owner, Limits::default(), 64 * 1024);
        wire.output.write_all(b"{\n").await.unwrap();
        let early = wire.receive().await;
        assert_eq!(early["error"]["code"], -32700);
        assert!(early.get("id").is_none());
        let meta = json!({
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientCapabilities":{},
            "io.modelcontextprotocol/clientInfo":{"name":"wire","version":"1"},
        });
        if modern {
            wire.send(json!({"jsonrpc":"2.0","id":"discover","method":"server/discover","params":{"_meta":meta}})).await;
            let discover = wire.receive().await;
            assert_eq!(discover["id"], "discover");
            assert!(
                discover["result"]["supportedVersions"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("2026-07-28"))
            );
        } else {
            initialize(&mut wire).await;
        }
        let mut params = json!({"name":"haystack_read","arguments":{"ids":["a"]}});
        if modern {
            params["_meta"] = meta;
        }
        // A valid call selects the modern lifecycle after its bootstrap probe.
        wire.send(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":params}))
            .await;
        let first = wire.receive().await;
        assert_eq!(first["id"], 1);
        assert_eq!(envelope(&first)["row_count"], 1);
        wire.send(json!({"jsonrpc":"2.0","id":null,"method":"tools/call","params":params}))
            .await;
        let invalid = wire.receive().await;
        assert_eq!(invalid["error"]["code"], -32600);
        assert!(invalid.get("id").is_none());
        wire.send(
            json!({"jsonrpc":"2.0","id":"after-error","method":"tools/call","params":params}),
        )
        .await;
        let recovered = wire.receive().await;
        assert_eq!(recovered["id"], "after-error");
        assert_eq!(envelope(&recovered)["row_count"], 1);
        finished(wire, task).await;
        close(owner).await;
    }
}

async fn semantic_app() -> (ApplicationOwner, Arc<AtomicUsize>, SharedGraph) {
    #[derive(Clone)]
    struct ResourcePolicy(Arc<AtomicUsize>);
    struct ResourceScope(String);
    impl ReadPolicy for ResourcePolicy {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(ResourceScope(format!(
                "resources-{}",
                self.0.load(Ordering::SeqCst)
            ))))
        }
    }
    impl PolicySnapshot for ResourceScope {
        fn scope_key(&self) -> &str {
            &self.0
        }
        fn function(&self, _: &FunctionIdentity) -> bool {
            false
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, _: &str) -> bool {
            true
        }
        fn tag(&self, _: &str, _: &str) -> bool {
            true
        }
        fn reference(&self, _: &str) -> bool {
            true
        }
        fn reference_display(&self, _: &str) -> bool {
            true
        }
        fn catalog(&self, _: CatalogKind, name: &str) -> bool {
            !name.starts_with("hidden")
        }
        fn nominal_provenance(&self, _: &NominalScalar) -> bool {
            false
        }
    }
    let graph = SharedGraph::new(EntityGraph::with_namespace(resource_support::namespace()));
    let epoch = Arc::new(AtomicUsize::new(0));
    let owner = ApplicationBuilder::new(
        graph.clone(),
        Arc::new(ResourcePolicy(epoch.clone())),
        ReadLimits::default(),
    )
    .unwrap()
    .start(&tokio::runtime::Handle::current())
    .unwrap();
    owner.ready().await.unwrap();
    (owner, epoch, graph)
}
async fn initialize_semantics(wire: &mut Wire, modern: bool) {
    wire.modern = modern;
    if modern {
        wire.send(json!({"jsonrpc":"2.0","id":0,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}})).await;
    } else {
        wire.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"wire","version":"1"}}})).await;
    }
    let result = wire.receive().await;
    assert_eq!(
        result["result"]["capabilities"],
        json!({"tools":{},"resources":{}})
    );
    if !modern {
        wire.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
    }
}
#[tokio::test]
async fn semantic_resources_floor_canonical_reads_and_cache_hints_coexist_with_both_tool_profiles()
{
    for profile in [
        haystack_mcp::OutputProfile::H4,
        haystack_mcp::OutputProfile::Jeto,
    ] {
        for modern in [false, true] {
            let (owner, epoch, graph) = semantic_app().await;
            let floor = haystack_mcp::SEMANTIC_RESPONSE_BYTES;
            let (mut wire, task) = wire_selection(
                &owner,
                Limits {
                    max_response_bytes: floor,
                    ..Default::default()
                },
                floor * 2,
                profile,
                true,
            );
            initialize_semantics(&mut wire, modern).await;
            let id = "\u{0001}".repeat(128);
            let mut cursor = None;
            let mut resources = Vec::new();
            loop {
                let params = match cursor.take() {
                    Some(cursor) => json!({"cursor":cursor}),
                    None => json!({}),
                };
                wire.send(
                    json!({"jsonrpc":"2.0","id":id,"method":"resources/list","params":params}),
                )
                .await;
                let response = wire.receive().await;
                assert_eq!(response["id"], id);
                assert!(serde_json::to_vec(&response).unwrap().len() < floor);
                if modern {
                    assert_eq!(response["result"]["ttlMs"], 0);
                    assert_eq!(response["result"]["cacheScope"], "private");
                    assert_eq!(response["result"]["resultType"], "complete");
                } else {
                    assert!(response["result"].get("resultType").is_none());
                }
                let page = response["result"]["resources"].as_array().unwrap();
                assert!(!page.is_empty());
                resources.extend(page.iter().cloned());
                cursor = response["result"]["nextCursor"].as_str().map(str::to_owned);
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(resources.len(), 13);
            let unique = resources
                .iter()
                .map(|r| r["uri"].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(unique.len(), resources.len());
            assert!(
                !serde_json::to_string(&resources)
                    .unwrap()
                    .contains("hiddenDef")
            );
            let mut saved = None;
            for resource in &resources {
                let uri = resource["uri"].as_str().unwrap();
                let label = resource["name"].as_str().unwrap();
                let (kind, name) = label.split_once(':').unwrap();
                let library = if kind == "library" {
                    name.to_owned()
                } else if name == "known" {
                    "phDemo".into()
                } else {
                    "l".repeat(256)
                };
                wire.send(
                    json!({"jsonrpc":"2.0","id":id,"method":"resources/read","params":{"uri":uri}}),
                )
                .await;
                let response = wire.receive().await;
                assert!(serde_json::to_vec(&response).unwrap().len() < floor);
                resource_support::check_content(
                    &response["result"],
                    uri,
                    name,
                    &library,
                    kind == "definition",
                );
                if name == "known" {
                    saved = Some(uri.to_owned());
                }
            }
            let saved = saved.unwrap();
            wire.send(json!({"jsonrpc":"2.0","id":id,"method":"resources/templates/list"}))
                .await;
            assert_eq!(
                wire.receive().await["result"]["resourceTemplates"],
                json!([])
            );
            for method in ["resources/subscribe", "completion/complete", "prompts/list"] {
                wire.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":{"uri":saved}}))
                    .await;
                assert_eq!(wire.receive().await["error"]["code"], -32601);
            }
            epoch.store(1, Ordering::SeqCst);
            wire.send(
                json!({"jsonrpc":"2.0","id":id,"method":"resources/read","params":{"uri":saved}}),
            )
            .await;
            let denied = wire.receive().await;
            assert_eq!(denied["error"]["code"], -32602);
            assert_eq!(
                denied["error"]["message"],
                "Resource unavailable or stale; rediscover"
            );
            graph.set_namespace(resource_support::namespace());
            wire.send(
                json!({"jsonrpc":"2.0","id":id,"method":"resources/read","params":{"uri":saved}}),
            )
            .await;
            assert_eq!(wire.receive().await["error"], denied["error"]);
            finished(wire, task).await;
            close(owner).await;
        }
    }
}
#[tokio::test]
async fn selected_semantic_floor_rejects_before_reading_stdio_and_unselected_inventory_is_absent() {
    let (owner, _, _) = semantic_app().await;
    let result = haystack_mcp::serve(
        owner.handle(),
        principal_for_test(),
        tokio::io::empty(),
        tokio::io::sink(),
        haystack_mcp::StdioConfig {
            limits: Limits {
                max_response_bytes: haystack_mcp::SEMANTIC_RESPONSE_BYTES - 1,
                ..Default::default()
            },
            semantic_resources: true,
            ..Default::default()
        },
        CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(McpError::Configuration)));
    let (mut wire, task) = wire(&owner, Limits::default(), 64 * 1024);
    initialize(&mut wire).await;
    for method in [
        "resources/list",
        "resources/templates/list",
        "resources/read",
    ] {
        wire.send(json!({"jsonrpc":"2.0","id":1,"method":method,"params":{"uri":"haystack://unavailable"}})).await;
        assert_eq!(wire.receive().await["error"]["code"], -32601);
    }
    finished(wire, task).await;
    close(owner).await;
}
#[tokio::test]
async fn released_sdk_discovers_and_reads_resources_and_raw_requests_reject_stale_cache_entries() {
    use rmcp::model::{
        ClientRequest, ReadResourceRequest, ReadResourceRequestParams, ReadResourceResponse,
    };
    let (owner, epoch, _) = semantic_app().await;
    let (client, server) = tokio::io::duplex(256 * 1024);
    let (input, output) = tokio::io::split(server);
    let task = tokio::spawn(haystack_mcp::serve(
        owner.handle(),
        principal_for_test(),
        input,
        output,
        haystack_mcp::StdioConfig {
            semantic_resources: true,
            ..Default::default()
        },
        CancellationToken::new(),
    ));
    let client = serve_client_with_lifecycle(
        ClientConfig::default(),
        client,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await
    .unwrap();
    let resources = client.list_all_resources().await.unwrap();
    let uri = resources
        .iter()
        .find(|r| r.name == "definition:known")
        .unwrap()
        .uri
        .clone();
    let ReadResourceResponse::Complete(result) = client
        .read_resource_once(ReadResourceRequestParams::new(&uri))
        .await
        .unwrap()
    else {
        panic!("unexpected multi-round response")
    };
    resource_support::check_content(
        &serde_json::to_value(result).unwrap(),
        &uri,
        "known",
        "phDemo",
        true,
    );
    assert!(
        client
            .list_resource_templates(None)
            .await
            .unwrap()
            .resource_templates
            .is_empty()
    );
    epoch.store(1, Ordering::SeqCst);
    // send_request bypasses read_resource_once's deliberate stale-on-error cache.
    let error = client
        .send_request(ClientRequest::ReadResourceRequest(
            ReadResourceRequest::new(ReadResourceRequestParams::new(&uri)),
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Resource unavailable or stale"));
    client.cancel().await.unwrap();
    let report = task.await.unwrap().unwrap();
    assert_eq!(report.admitted, report.released);
    close(owner).await;
}

#[tokio::test]
async fn semantic_slow_output_obeys_cancel_deadline_and_stop_without_leaking_capacity() {
    for event in ["cancel", "deadline", "stop"] {
        let (owner, _, _) = semantic_app().await;
        let stop = CancellationToken::new();
        let (mut wire, gate, task) = gated_wire_selection(
            &owner,
            stop.clone(),
            true,
            if event == "deadline" {
                Duration::from_millis(200)
            } else {
                Duration::from_secs(5)
            },
        );
        initialize_semantics(&mut wire, false).await;
        gate.blocked.store(true, Ordering::SeqCst);
        wire.send(json!({"jsonrpc":"2.0","id":7,"method":"resources/list"}))
            .await;
        tokio::time::timeout(Duration::from_secs(2), gate.polled.notified())
            .await
            .unwrap();
        assert!(owner.handle().outstanding_tasks() > 0);
        if event == "cancel" {
            wire.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}})).await;
            tokio::time::timeout(Duration::from_secs(2), async {
                while owner.handle().outstanding_tasks() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            gate.release();
            wire.send(json!({"jsonrpc":"2.0","id":7,"method":"resources/list"}))
                .await;
            assert_eq!(
                wire.receive().await["result"]["resources"]
                    .as_array()
                    .unwrap()
                    .len(),
                8
            );
            let report = finished(wire, task).await;
            assert_eq!(report.suppressed, 1);
        } else {
            if event == "stop" {
                stop.cancel();
            }
            let outcome = tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap();
            if event == "deadline" {
                assert_eq!(outcome.unwrap_err(), McpError::Deadline);
            } else {
                let report = outcome.unwrap();
                assert_eq!(report.admitted, report.released);
            }
            gate.release();
            drop(wire);
        }
        assert_eq!(owner.handle().read_service().load().admitted, 0);
        close(owner).await;
    }
}
