use haystack_client::{
    ClientError, HaystackClient, history_mutation::HistoryMutationTransport, transport::Transport,
};
use haystack_core::{
    codecs::{Codec, history::HistorySample, history_mutation::*, zinc::ZincCodec},
    data::HGrid,
    kinds::{Kind, Number},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn request() -> HistoryWriteRequest {
    let Kind::DateTime(ts) = ZincCodec.decode_scalar("2024-06-01T00:00:00Z UTC").unwrap() else {
        panic!()
    };
    HistoryWriteRequest {
        identity: HistoryOperationIdentity {
            authority: [1; 16],
            point: "p".into(),
            incarnation: [2; 16],
            operation_id: "once".into(),
        },
        expected_generation: (1 << 53) + 1,
        samples: vec![
            HistorySample {
                ts: ts.clone(),
                val: Kind::Number(Number::unitless(1.0)),
            },
            HistorySample { ts, val: Kind::NA },
        ],
    }
}
fn receipt(request: &HistoryWriteRequest) -> HistoryWriteReceipt {
    HistoryWriteReceipt {
        identity: request.identity.clone(),
        before_generation: request.expected_generation,
        after_generation: request.expected_generation + 1,
        change_sequence: 5,
        submitted_samples: 2,
        unique_samples: 1,
        retained_samples: 1,
        evicted_samples: 0,
        qualification: HistoryReceiptQualification::ProviderProtocol,
    }
}
struct Counting {
    calls: Arc<AtomicUsize>,
    receipt: HistoryWriteReceipt,
    mode: usize,
}
impl HistoryMutationTransport for Counting {}
impl Transport for Counting {
    async fn call(&self, op: &str, _: &HGrid) -> Result<HGrid, ClientError> {
        if op == "hisReceipt" {
            return Ok(
                outcome_grid(&HistoryWriteOutcome::Committed(self.receipt.clone())).unwrap(),
            );
        }
        assert_eq!(op, "hisWrite");
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut receipt = self.receipt.clone();
        match self.mode {
            0 => return Err(ClientError::Transport("injected unreadable body".into())),
            1 => return Ok(HGrid::new()),
            2 => receipt.identity.point = "other".into(),
            3 => receipt.unique_samples = 2,
            4 => {
                receipt.before_generation += 1;
                receipt.after_generation += 1;
            }
            _ => {}
        }
        let mut grid = outcome_grid(&HistoryWriteOutcome::Committed(receipt)).unwrap();
        grid.meta.set("err", Kind::Marker);
        Ok(grid)
    }
    async fn close(&self) -> Result<(), ClientError> {
        Ok(())
    }
}
#[tokio::test]
async fn lost_empty_foreign_and_inconsistent_acknowledgements_are_unknown_with_no_replay() {
    let request = request();
    let receipt = receipt(&request);
    for mode in 0..5 {
        let calls = Arc::new(AtomicUsize::new(0));
        let client = HaystackClient::from_transport(Counting {
            calls: calls.clone(),
            receipt: receipt.clone(),
            mode,
        });
        assert!(
            matches!(client.his_write_scoped(&request).await.unwrap(), HistoryWriteOutcome::Unknown { identity, .. } if identity == request.identity)
        );
        assert_eq!(
            client.reconcile_history(&request.identity).await.unwrap(),
            HistoryWriteOutcome::Committed(receipt.clone())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn typed_committed_outcome_survives_error_marker_and_native_projection_never_dispatches() {
    let mut request = request();
    let receipt = receipt(&request);
    let calls = Arc::new(AtomicUsize::new(0));
    let client = HaystackClient::from_transport(Counting {
        calls: calls.clone(),
        receipt: receipt.clone(),
        mode: 5,
    });
    assert_eq!(
        client.his_write_scoped(&request).await.unwrap(),
        HistoryWriteOutcome::Committed(receipt)
    );
    request.samples[0].val = Kind::Int(1);
    assert!(client.his_write_scoped(&request).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn review_unrepresentable_timestamp_offsets_never_dispatch() {
    let mut original = request();
    let calls = Arc::new(AtomicUsize::new(0));
    let client = HaystackClient::from_transport(Counting {
        calls: calls.clone(),
        receipt: receipt(&original),
        mode: 5,
    });
    for (source, zone) in [
        ("1880-06-01T00:00:00Z UTC", "New_York"),
        ("1850-01-01T00:00:00Z UTC", "Calcutta"),
    ] {
        let Kind::DateTime(mut timestamp) = ZincCodec.decode_scalar(source).unwrap() else {
            panic!()
        };
        let offset = haystack_core::kinds::offset_at(zone, timestamp.dt).unwrap();
        assert_ne!(offset.local_minus_utc() % 60, 0);
        timestamp.dt = timestamp.dt.with_timezone(&offset);
        timestamp.tz_name = zone.into();
        original.samples[0].ts = timestamp;
        let canonical = canonical_request(&original).unwrap();
        assert!(matches!(
            client.his_write_scoped(&original).await,
            Err(ClientError::Codec(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(canonical_request(&original).unwrap(), canonical);
    }
}

fn native_timestamp(
    seconds: i64,
    nanos: u32,
    offset: i32,
    zone: &str,
) -> haystack_core::kinds::HDateTime {
    let source = format!(
        r#"{{"version":1,"value":{{"kind":"dateTime","seconds":"{seconds}","nanos":{nanos},"offset":{offset},"timezone":"{zone}"}}}}"#
    );
    let Kind::DateTime(value) = haystack_core::codecs::typed::decode(source.as_bytes()).unwrap()
    else {
        panic!()
    };
    value
}

#[tokio::test]
async fn second_review_unrepresentable_timestamp_fields_never_dispatch() {
    let mut original = request();
    let calls = Arc::new(AtomicUsize::new(0));
    let client = HaystackClient::from_transport(Counting {
        calls: calls.clone(),
        receipt: receipt(&original),
        mode: 5,
    });
    for (seconds, nanos, offset, zone) in [
        (58, 1_500_000_000, 0, "UTC"),
        (253_402_300_800, 500_000_000, 0, "UTC"),
        (8_210_266_876_799, 0, 3600, "GMT-1"),
    ] {
        original.samples[0].ts = native_timestamp(seconds, nanos, offset, zone);
        let canonical = canonical_request(&original).unwrap();
        assert!(
            matches!(
                client.his_write_scoped(&original).await,
                Err(ClientError::Codec(_))
            ),
            "seconds={seconds}, nanos={nanos}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(canonical_request(&original).unwrap(), canonical);
    }
}
