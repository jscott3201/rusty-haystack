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

struct Acknowledging {
    submits: Arc<AtomicUsize>,
    outcome: MutationOutcome,
}
impl EntityTransport for Acknowledging {}
impl Transport for Acknowledging {
    async fn call(&self, op: &str, _: &HGrid) -> Result<HGrid, ClientError> {
        assert_eq!(op, "entityBatch");
        self.submits.fetch_add(1, Ordering::SeqCst);
        Ok(entity::to_grid(&self.outcome).unwrap())
    }
    async fn close(&self) -> Result<(), ClientError> {
        Ok(())
    }
}
fn acknowledgement_request(
    expected_revision: u64,
    operations: Vec<EntityOperation>,
) -> EntityBatchRequest {
    EntityBatchRequest {
        identity: OperationIdentity {
            operation_id: "ack-bound".into(),
            dataset: [1; 16],
            incarnation: [2; 16],
        },
        expected_revision,
        operations,
    }
}
async fn expect_acknowledgement(request: EntityBatchRequest, after: u64, valid: bool) {
    let receipt = EntityReceipt {
        identity: request.identity.clone(),
        before_revision: request.expected_revision,
        after_revision: after,
        span: if after == request.expected_revision {
            None
        } else {
            Some(CommitSpan {
                first: request.expected_revision + 1,
                last: after,
            })
        },
        qualification: ReceiptQualification::EphemeralMemory,
    };
    let count = Arc::new(AtomicUsize::new(0));
    let acknowledged = MutationOutcome::Committed(receipt);
    let client = HaystackClient::from_transport(Acknowledging {
        submits: count.clone(),
        outcome: acknowledged.clone(),
    });
    let actual = client.submit_entities(&request).await.unwrap();
    let expected = if valid {
        acknowledged
    } else {
        MutationOutcome::Unknown {
            identity: request.identity.clone(),
            cause: UnknownCause::InvalidAcknowledgement,
        }
    };
    assert_eq!(actual, expected);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn committed_acknowledgement_must_match_exact_request_change_count() {
    for after in [10, 12] {
        expect_acknowledgement(
            acknowledgement_request(
                10,
                vec![EntityOperation::Remove {
                    id: "removed".into(),
                }],
            ),
            after,
            false,
        )
        .await;
    }
    expect_acknowledgement(
        acknowledgement_request(
            10,
            vec![EntityOperation::Remove {
                id: "removed".into(),
            }],
        ),
        11,
        true,
    )
    .await;
    let empty = || EntityOperation::Patch {
        id: "empty".into(),
        changes: HDict::new(),
    };
    expect_acknowledgement(acknowledgement_request(10, vec![empty()]), 10, true).await;
    expect_acknowledgement(acknowledgement_request(10, vec![empty()]), 11, false).await;
    let mut added = HDict::new();
    added.set("id", Kind::Ref(HRef::from_val("new")));
    let mut equal_patch = HDict::new();
    equal_patch.set("site", Kind::Marker);
    let mixed = vec![
        EntityOperation::Add(added),
        empty(),
        EntityOperation::Patch {
            id: "existing".into(),
            changes: equal_patch,
        },
        EntityOperation::Remove { id: "old".into() },
    ];
    expect_acknowledgement(acknowledgement_request(10, mixed.clone()), 13, true).await;
    expect_acknowledgement(acknowledgement_request(10, mixed), 12, false).await;
}
#[tokio::test]
async fn committed_acknowledgement_checks_revision_overflow_without_replay() {
    expect_acknowledgement(
        acknowledgement_request(
            u64::MAX,
            vec![EntityOperation::Remove {
                id: "overflow".into(),
            }],
        ),
        u64::MAX,
        false,
    )
    .await;
    expect_acknowledgement(
        acknowledgement_request(
            u64::MAX,
            vec![EntityOperation::Patch {
                id: "empty".into(),
                changes: HDict::new(),
            }],
        ),
        u64::MAX,
        true,
    )
    .await;
    expect_acknowledgement(
        acknowledgement_request(
            u64::MAX - 1,
            vec![EntityOperation::Remove { id: "last".into() }],
        ),
        u64::MAX,
        true,
    )
    .await;
    expect_acknowledgement(
        acknowledgement_request(
            u64::MAX - 1,
            vec![
                EntityOperation::Remove { id: "one".into() },
                EntityOperation::Remove { id: "two".into() },
            ],
        ),
        u64::MAX,
        false,
    )
    .await;
}
