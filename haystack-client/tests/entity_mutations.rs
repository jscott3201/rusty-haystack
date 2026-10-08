use haystack_client::{ClientError, HaystackClient, entity::EntityTransport, transport::Transport};
use haystack_core::{
    codecs::entity::{self, *},
    data::{HDict, HGrid},
    graph::{CommitSpan, EntityOperation},
    kinds::{HRef, Kind},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
struct Counting {
    submits: Arc<AtomicUsize>,
    receipt: EntityReceipt,
    invalid: bool,
}
impl EntityTransport for Counting {}
impl Transport for Counting {
    async fn call(&self, op: &str, _: &HGrid) -> Result<HGrid, ClientError> {
        match op {
            "entityBatch" => {
                self.submits.fetch_add(1, Ordering::SeqCst);
                if self.invalid {
                    Ok(HGrid::new())
                } else {
                    Err(ClientError::Transport("lost committed response".into()))
                }
            }
            "entityReceipt" => {
                Ok(entity::to_grid(&MutationOutcome::Committed(self.receipt.clone())).unwrap())
            }
            _ => panic!("unexpected op"),
        }
    }
    async fn close(&self) -> Result<(), ClientError> {
        Ok(())
    }
}
#[tokio::test]
async fn lost_or_invalid_ack_preserves_identity_and_reconciles_without_submit_replay() {
    for invalid in [false, true] {
        let identity = OperationIdentity {
            operation_id: "once".into(),
            dataset: [1; 16],
            incarnation: [2; 16],
        };
        let before = (1u64 << 53) + 9;
        let receipt = EntityReceipt {
            identity: identity.clone(),
            before_revision: before,
            after_revision: before + 1,
            span: Some(CommitSpan {
                first: before + 1,
                last: before + 1,
            }),
            qualification: ReceiptQualification::ProviderProtocol,
        };
        let count = Arc::new(AtomicUsize::new(0));
        let client = HaystackClient::from_transport(Counting {
            submits: count.clone(),
            receipt: receipt.clone(),
            invalid,
        });
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("one")));
        row.set("exact", Kind::Int(i64::MAX));
        let request = EntityBatchRequest {
            identity: identity.clone(),
            expected_revision: before,
            operations: vec![EntityOperation::Add(row)],
        };
        assert!(
            matches!(client.submit_entities(&request).await.unwrap(),MutationOutcome::Unknown{identity:i,..} if i==identity)
        );
        assert_eq!(
            client.reconcile_entity(&identity).await.unwrap(),
            MutationOutcome::Committed(receipt)
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
