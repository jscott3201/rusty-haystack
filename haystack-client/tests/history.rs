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
                unit: Some("°C".into()),
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

#[tokio::test]
async fn review_calendar_replies_require_unique_midnights_without_replay() {
    // Independent IANA transition fixtures already used by the application:
    // Havana repeated midnight 2015-11-01; Apia skipped 2011-12-30.
    for (zone, day, next, evaluated, offsets) in [
        (
            "Havana",
            "2015-11-01",
            "2015-11-02",
            "2015-11-01T12:00:00-05:00",
            vec!["-04:00", "-05:00"],
        ),
        (
            "Apia",
            "2011-12-30",
            "2011-12-31",
            "2011-12-31T12:00:00+14:00",
            vec!["-10:00", "+14:00"],
        ),
    ] {
        for offset in offsets {
            for range in [
                day.to_string(),
                format!("{day},{day}"),
                "today".into(),
                "yesterday".into(),
            ] {
                let mut response = result();
                response.samples.clear();
                response.terminal = HistoryTerminal::Complete;
                let m = &mut response.metadata;
                m.requested_range = range.clone();
                m.schema.timezone = zone.into();
                m.start = date(&format!("{day}T00:00:00{offset} {zone}"));
                let end_offset = if zone == "Havana" { "-05:00" } else { "+14:00" };
                m.end = date(&format!("{next}T00:00:00{end_offset} {zone}"));
                m.evaluated_at = date(&format!("{evaluated} {zone}"));
                if range == "yesterday" && zone == "Havana" {
                    m.evaluated_at = date("2015-11-02T12:00:00-05:00 Havana");
                }
                m.coverage = HistoryCoverage {
                    retained_start: None,
                    retained_end: None,
                    retained_count: 0,
                    evicted_through: None,
                };
                // Bypass the response validator, as an independent hostile transport can.
                let mut seed = response.clone();
                seed.metadata.schema.timezone = "UTC".into();
                seed.metadata.start = date("2015-11-01T00:00:00Z UTC");
                seed.metadata.end = date("2015-11-02T00:00:00Z UTC");
                seed.metadata.evaluated_at = date("2015-11-01T12:00:00Z UTC");
                let mut grid = result_grid(&seed).unwrap();
                let Some(Kind::Str(control)) = grid.meta.get("history") else {
                    panic!()
                };
                let Kind::Dict(mut control) = typed::decode(control.as_bytes()).unwrap() else {
                    panic!()
                };
                control.set("timezone", Kind::Str(zone.into()));
                control.set(
                    "evaluatedAt",
                    Kind::DateTime(response.metadata.evaluated_at.clone()),
                );
                grid.meta.set(
                    "history",
                    Kind::Str(
                        String::from_utf8(typed::encode(&Kind::Dict(control)).unwrap()).unwrap(),
                    ),
                );
                grid.meta
                    .set("hisStart", Kind::DateTime(response.metadata.start));
                grid.meta
                    .set("hisEnd", Kind::DateTime(response.metadata.end));
                let calls = Arc::new(AtomicUsize::new(0));
                let client = HaystackClient::from_transport(Fixture {
                    grid,
                    calls: calls.clone(),
                });
                assert!(
                    matches!(
                        client
                            .his_read_scoped(&HistoryReadRequest {
                                id: "p".into(),
                                range: range.clone()
                            })
                            .await,
                        Err(ClientError::Codec(_))
                    ),
                    "{zone} {offset} {range}"
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
            }
        }
    }
}

#[tokio::test]
async fn review_explicit_cross_zone_instants_can_span_repeated_local_midnight() {
    let mut response = result();
    response.samples.clear();
    response.terminal = HistoryTerminal::Complete;
    response.metadata.requested_range = "2015-11-01T04:00:00Z GMT,2015-11-01T05:00:00Z GMT".into();
    response.metadata.schema.timezone = "Havana".into();
    response.metadata.start = date("2015-11-01T00:00:00-04:00 Havana");
    response.metadata.end = date("2015-11-01T00:00:00-05:00 Havana");
    response.metadata.evaluated_at = date("2015-11-01T12:00:00-05:00 Havana");
    response.metadata.coverage = HistoryCoverage {
        retained_start: None,
        retained_end: None,
        retained_count: 0,
        evicted_through: None,
    };
    let request = HistoryReadRequest {
        id: "p".into(),
        range: response.metadata.requested_range.clone(),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let client = HaystackClient::from_transport(Fixture {
        grid: result_grid(&response).unwrap(),
        calls: calls.clone(),
    });
    assert_eq!(client.his_read_scoped(&request).await.unwrap(), response);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
