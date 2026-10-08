use haystack_client::{
    ClientError, HaystackClient, history::HistoryTransport, transport::Transport,
};
use haystack_core::{
    codecs::{history::*, typed},
    data::HGrid,
    graph::GraphState,
    kinds::{HDateTime, Kind, Number},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn date(text: &str) -> HDateTime {
    use haystack_core::codecs::{Codec, zinc::ZincCodec};
    let Kind::DateTime(value) = ZincCodec.decode_scalar(text).unwrap() else {
        panic!()
    };
    value
}
fn result() -> HistoryReadResult {
    let ts = date("2024-06-01T00:00:00Z UTC");
    HistoryReadResult {
        metadata: HistoryMetadata {
            id: "p".into(),
            requested_range: "2024-06-01".into(),
            evaluated_at: ts.clone(),
            start: ts.clone(),
            end: date("2024-06-02T00:00:00Z UTC"),
            schema: HistorySchema {
                kind: HistoryKind::Number,
                unit: None,
                timezone: "UTC".into(),
            },
            capabilities: HistoryCapabilities::BOUNDED_LIVE,
            bounds: HistoryBounds {
                batch_rows: 2,
                batch_bytes: 4096,
                total_rows: 2,
                total_bytes: 4096,
                total_work: 4096,
                response_bytes: 64 * 1024,
            },
            graph: GraphState {
                incarnation: [1; 16],
                revision: 1,
                catalog_generation: 0,
            },
            policy_observation: [2; 16],
            history: HistoryState {
                authority: [3; 16],
                incarnation: [4; 16],
                generation: 1,
            },
            coverage: HistoryCoverage {
                retained_start: Some(ts.clone()),
                retained_end: Some(date("2024-06-01T01:00:00Z UTC")),
                retained_count: 2,
                evicted_through: None,
            },
        },
        samples: vec![HistorySample {
            ts,
            val: Kind::Number(Number::unitless(1.0)),
        }],
        terminal: HistoryTerminal::Failed(HistoryReason::Provider),
    }
}
struct Fixture {
    grid: HGrid,
    calls: Arc<AtomicUsize>,
}
impl Transport for Fixture {
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        assert_eq!(op, "hisRead");
        assert_eq!(request_from_grid(req).unwrap().id, "p");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.grid.clone())
    }
    async fn close(&self) -> Result<(), ClientError> {
        Ok(())
    }
}
impl HistoryTransport for Fixture {}
fn request() -> HistoryReadRequest {
    HistoryReadRequest {
        id: "p".into(),
        range: "2024-06-01".into(),
    }
}
#[tokio::test]
async fn thin_helper_preserves_partial_failure_and_rejects_foreign_or_inconsistent_envelopes_once()
{
    let expected = result();
    let calls = Arc::new(AtomicUsize::new(0));
    let client = HaystackClient::from_transport(Fixture {
        grid: result_grid(&expected).unwrap(),
        calls: calls.clone(),
    });
    assert_eq!(client.his_read_scoped(&request()).await.unwrap(), expected);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut grids = vec![HGrid::new()];
    let mut foreign = result();
    foreign.metadata.id = "foreign".into();
    grids.push(result_grid(&foreign).unwrap());
    let mut grid = result_grid(&expected).unwrap();
    let Some(Kind::Str(control)) = grid.meta.get("history") else {
        panic!()
    };
    let Kind::Dict(mut control) = typed::decode(control.as_bytes()).unwrap() else {
        panic!()
    };
    control.set("returnedCount", Kind::Str("2".into()));
    grid.meta.set(
        "history",
        Kind::Str(String::from_utf8(typed::encode(&Kind::Dict(control)).unwrap()).unwrap()),
    );
    grids.push(grid);
    let mut grid = result_grid(&expected).unwrap();
    grid.rows[0].set("val", Kind::Int(1));
    grids.push(grid);
    for grid in grids {
        let calls = Arc::new(AtomicUsize::new(0));
        let client = HaystackClient::from_transport(Fixture {
            grid,
            calls: calls.clone(),
        });
        assert!(matches!(
            client.his_read_scoped(&request()).await,
            Err(ClientError::Codec(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
