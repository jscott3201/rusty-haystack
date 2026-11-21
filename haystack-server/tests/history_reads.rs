//! HTTP and embedding consume the same authorized bounded-history contract.
use chrono::{DateTime, FixedOffset};
use haystack_app::*;
use haystack_client::{HaystackClient, transport::http::HttpTransport};
use haystack_core::{
    codecs::{codec_for, history},
    data::{HDict, HGrid},
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, NominalScalar, Number},
};
use haystack_server::HaystackServer;
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
struct Provider {
    store: HisStore,
    opens: Arc<AtomicUsize>,
}
impl HistoryProvider for Provider {
    fn open(
        &self,
        id: String,
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        budget: HistoryPullBudget,
    ) -> HistoryFuture<'_, Box<dyn HistorySession>> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.store.open(id, start, end, budget)
    }
    fn his_write(&self, id: &str, items: Vec<HisItem>) -> HistoryFuture<'_, ()> {
        self.store.his_write(id, items)
    }
}
#[derive(Clone)]
struct Rules;
impl ReadPolicy for Rules {
    fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
        Ok(Arc::new(Self))
    }
}
impl PolicySnapshot for Rules {
    fn function(&self, _: &haystack_app::FunctionIdentity) -> bool {
        true
    }
    fn scope_key(&self) -> &str {
        "history-fixture"
    }
    fn operation(&self, _: ReadOperation) -> bool {
        true
    }
    fn entity(&self, id: &str) -> bool {
        id != "denied"
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
    fn catalog(&self, _: CatalogKind, _: &str) -> bool {
        true
    }
    fn nominal_provenance(&self, _: &NominalScalar) -> bool {
        true
    }
}
async fn setup(total_rows: usize) -> (ApplicationOwner, String, HistoryService, Arc<AtomicUsize>) {
    let graph = SharedGraph::new(EntityGraph::new());
    for id in ["p", "denied"] {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val(id)));
        row.set("his", Kind::Marker);
        row.set("kind", Kind::Str("Number".into()));
        row.set("tz", Kind::Str("New_York".into()));
        row.set("unit", Kind::Str("°C".into()));
        graph.add(row).unwrap();
    }
    let store = HisStore::new();
    store
        .write(
            "p",
            [
                "2024-06-01T04:00:00Z",
                "2024-06-01T12:00:00Z",
                "2024-06-02T03:59:59.999999999Z",
                "2024-06-02T04:00:00Z",
            ]
            .into_iter()
            .enumerate()
            .map(|(i, ts)| HisItem {
                ts: time(ts),
                val: Kind::Number(Number::unitless(i as f64)),
            })
            .collect(),
        )
        .unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(Provider {
        store,
        opens: opens.clone(),
    });
    let builder =
        ApplicationBuilder::new(graph.clone(), Arc::new(Rules), ReadLimits::default()).unwrap();
    let service = HistoryService::new(
        builder.handle().read_service(),
        provider,
        HistoryLimits {
            total_rows,
            batch_rows: 1,
            ..HistoryLimits::default()
        },
    )
    .unwrap();
    // Register the listener first to prove history initialization ordering is
    // owned by application selection rather than builder call order.
    let server = HaystackServer::new(graph)
        .with_scoped_reads(builder.handle())
        .port(0);
    let owner = builder
        .owned_resource(server.into_listener())
        .owned_history(service.clone())
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
    let url = format!(
        "http://{}/api",
        owner.ready().await.unwrap().listeners[0].address
    );
    (owner, url, service, opens)
}
fn request() -> HistoryReadRequest {
    HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01T04:00:00Z GMT,2024-06-02T04:00:00Z GMT".into(),
    }
}
#[tokio::test]
async fn actual_http_and_scoped_client_preserve_partial_result_and_match_embedding() {
    let (owner, url, service, _) = setup(2).await;
    let local = service
        .collect(
            ReadContext::with_timeout(Principal::Anonymous, Duration::from_secs(5)),
            request(),
        )
        .await
        .unwrap();
    assert_eq!(
        local.terminal,
        HistoryTerminal::Limited(HistoryReason::Rows)
    );
    assert_eq!(local.samples.len(), 2);
    for mime in ["text/zinc", "application/json", "application/json;v=3"] {
        let client =
            HaystackClient::from_transport(HttpTransport::with_format(&url, String::new(), mime));
        let result = client.his_read_scoped(&request()).await.unwrap();
        assert_eq!(result.samples, local.samples);
        assert_eq!(result.terminal, local.terminal);
        assert_eq!(result.metadata.history, local.metadata.history);
        assert_eq!(result.metadata.graph, local.metadata.graph);
        assert_eq!(
            result.samples[0].ts.dt.to_rfc3339(),
            "2024-06-01T00:00:00-04:00"
        );
        assert_eq!(result.samples[0].ts.tz_name, "New_York");
        assert!(client.his_read("p", "2024-06-01").await.is_err());
    }
    owner.close().await.unwrap();
    owner.terminated().await;
}
#[tokio::test]
async fn final_fractional_second_is_included_and_following_midnight_is_excluded() {
    let (owner, url, _, _) = setup(100).await;
    let client = HaystackClient::from_transport(HttpTransport::new(&url, String::new()));
    let result = client.his_read_scoped(&request()).await.unwrap();
    assert_eq!(result.terminal, HistoryTerminal::Complete);
    assert_eq!(result.samples.len(), 3);
    assert_eq!(
        result.samples[2].ts.dt.timestamp_subsec_nanos(),
        999_999_999
    );
    owner.close().await.unwrap();
    owner.terminated().await;
}
#[tokio::test]
async fn missing_marker_unsupported_formats_and_denied_points_do_not_open_provider() {
    let (owner, url, _, opens) = setup(100).await;
    let client = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    let zinc = codec_for("text/zinc").unwrap();
    let body = history::encode_request(&request(), zinc).unwrap();
    for mime in ["text/trio", "text/csv", "application/not-real"] {
        let response = client
            .post(format!("{url}/hisRead"))
            .header("Content-Type", "text/zinc")
            .header("Accept", mime)
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let response = client
            .post(format!("{url}/hisRead"))
            .header("Content-Type", mime)
            .header("Accept", "text/zinc")
            .body(body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    let mut unmarked = history::request_grid(&request()).unwrap();
    unmarked.meta = HDict::new();
    let response = client
        .post(format!("{url}/hisRead"))
        .body(zinc.encode_grid(&unmarked).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let mut errors = vec![];
    for id in ["denied", "missing"] {
        let mut request = request();
        request.id = id.into();
        let response = client
            .post(format!("{url}/hisRead"))
            .header("Content-Type", "text/zinc")
            .body(history::encode_request(&request, zinc).unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
        errors.push(response.bytes().await.unwrap());
    }
    assert_eq!(errors[0], errors[1]);
    assert_eq!(opens.load(Ordering::SeqCst), 0);
    let ops = client
        .get(format!("{url}/ops"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let grid: HGrid = zinc.decode_grid(&ops).unwrap();
    assert!(
        grid.rows
            .iter()
            .any(|row| row.get("name") == Some(&Kind::Str("hisRead".into())))
    );
    assert!(
        !grid
            .rows
            .iter()
            .any(|row| row.get("name") == Some(&Kind::Str("hisWrite".into())))
    );
    owner.close().await.unwrap();
    owner.terminated().await;
}

#[tokio::test]
async fn noncanonical_nan_never_silently_changes_through_scoped_http() {
    let (owner, url, service, _) = setup(100).await;
    for value in [
        f64::from_bits(0x7ff8_0000_0000_0001),
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        service
            .provider()
            .his_write(
                "p",
                vec![HisItem {
                    ts: time("2024-06-01T12:00:00Z"),
                    val: Kind::Number(Number::unitless(value)),
                }],
            )
            .await
            .unwrap();
        for mime in ["text/zinc", "application/json", "application/json;v=3"] {
            let client = HaystackClient::from_transport(HttpTransport::with_format(
                &url,
                String::new(),
                mime,
            ));
            let result = client.his_read_scoped(&request()).await.unwrap();
            if value.to_bits() == 0x7ff8_0000_0000_0001 {
                assert_eq!(
                    result.terminal,
                    HistoryTerminal::Failed(HistoryReason::UnsupportedValue)
                );
                assert_eq!(result.samples.len(), 1);
                assert_eq!(result.samples[0].val, Kind::Number(Number::unitless(0.0)));
            } else {
                assert_eq!(result.terminal, HistoryTerminal::Complete);
                assert_eq!(result.samples.len(), 3);
                let Kind::Number(actual) = &result.samples[1].val else {
                    panic!()
                };
                assert_eq!(actual.val.to_bits(), value.to_bits());
            }
        }
    }
    owner.close().await.unwrap();
    owner.terminated().await;
}

#[path = "../../haystack-core/tests/fixtures/finite_numbers.rs"]
mod finite_numbers;
#[tokio::test]
async fn second_review_finite_number_oracle_preserves_bits_through_scoped_http() {
    let (owner, url, service, _) = setup(100).await;
    let originals = finite_numbers::FINITE_NUMBERS;
    service
        .provider()
        .his_write(
            "p",
            originals
                .iter()
                .enumerate()
                .map(|(i, (value, bits))| {
                    assert_eq!(value.to_bits(), *bits, "independent fixture bits");
                    HisItem {
                        ts: time("2024-06-01T04:00:00Z") + chrono::Duration::seconds(i as i64),
                        val: Kind::Number(Number::unitless(*value)),
                    }
                })
                .collect(),
        )
        .await
        .unwrap();
    let request = HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01T04:00:00Z GMT,2024-06-01T04:00:10Z GMT".into(),
    };
    for mime in ["text/zinc", "application/json", "application/json;v=3"] {
        let client =
            HaystackClient::from_transport(HttpTransport::with_format(&url, String::new(), mime));
        let result = client.his_read_scoped(&request).await.unwrap();
        assert_eq!(result.terminal, HistoryTerminal::Complete);
        assert_eq!(result.samples.len(), originals.len());
        for (sample, (_, bits)) in result.samples.iter().zip(originals) {
            let Kind::Number(actual) = &sample.val else {
                panic!()
            };
            assert_eq!(actual.val.to_bits(), bits, "{mime}");
            assert_eq!(actual.unit, None);
        }
    }
    owner.close().await.unwrap();
    owner.terminated().await;
}
