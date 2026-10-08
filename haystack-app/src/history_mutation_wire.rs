//! The original request admission owns bounded decode, publication and encoding.
use crate::{BudgetKind, H4Codec, HistoryMutationService, ReadAdmission, ReadError};
use haystack_core::codecs::{
    codec_for,
    history_mutation::{self as wire, *},
};
use parking_lot::Mutex;
use std::sync::Arc;
#[derive(Debug, Clone, Copy)]
pub enum HistoryMutationWireOperation {
    Submit,
    Receipt,
}
impl HistoryMutationService {
    pub async fn wire_admitted(
        &self,
        admission: ReadAdmission,
        operation: HistoryMutationWireOperation,
        body: Vec<u8>,
        input: H4Codec,
        output: H4Codec,
    ) -> Result<Vec<u8>, ReadError> {
        if !admission.belongs_to(self.read_service()) {
            return Err(ReadError::Forbidden);
        }
        if body.len() > wire::MAX_GRID_BYTES.min(self.read_service().limits().max_input_bytes) {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let identity = Arc::new(Mutex::new(None::<HistoryOperationIdentity>));
        let worker_identity = identity.clone();
        let service = self.inner.clone();
        let result = admission
            .run_task(move |principal, budget| {
                budget.charge(BudgetKind::Work, body.len())?;
                budget.charge(
                    BudgetKind::Retained,
                    body.len()
                        .saturating_mul(32)
                        .saturating_add(wire::MAX_RECEIPT_BYTES),
                )?;
                let codec = codec_for(input.mime()).ok_or(ReadError::Unavailable)?;
                let outcome = match operation {
                    HistoryMutationWireOperation::Submit => {
                        let request = wire::decode_request(&body, codec).map_err(|_| {
                            ReadError::InvalidQuery("invalid scoped history write envelope")
                        })?;
                        *worker_identity.lock() = Some(request.identity.clone());
                        service.submit(principal, request, budget)
                    }
                    HistoryMutationWireOperation::Receipt => {
                        let identity = wire::decode_lookup(&body, codec).map_err(|_| {
                            ReadError::InvalidQuery("invalid history receipt envelope")
                        })?;
                        service.reconcile(&principal, identity, budget)?
                    }
                };
                wire::encode_outcome(
                    &outcome,
                    codec_for(output.mime()).ok_or(ReadError::Unavailable)?,
                )
                .map_err(|_| ReadError::Budget(BudgetKind::Output))
            })
            .await;
        match result {
            Ok(body) => Ok(body),
            Err(error) => {
                if let Some(identity) = identity.lock().take() {
                    wire::encode_outcome(
                        &HistoryWriteOutcome::Unknown {
                            identity,
                            cause: HistoryWriteUnknown::Provider,
                        },
                        codec_for(output.mime()).ok_or(ReadError::Unavailable)?,
                    )
                    .map_err(|_| ReadError::Budget(BudgetKind::Output))
                } else {
                    Err(error)
                }
            }
        }
    }
}
