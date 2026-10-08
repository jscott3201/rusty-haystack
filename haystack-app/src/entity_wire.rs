//! Managed typed envelope adaptation shared by HTTP and embedding consumers.
use crate::{BudgetKind, H4Codec, MutationService, ReadAdmission, ReadError};
use haystack_core::codecs::{
    codec_for,
    entity::{
        self, ChangesRequest, EntityBatchRequest, MutationOutcome, OperationIdentity, UnknownCause,
    },
};
use parking_lot::Mutex;
use std::sync::Arc;
#[derive(Debug, Clone, Copy)]
pub enum EntityWireOperation {
    Batch,
    Receipt,
    Changes,
}
impl MutationService {
    /// Collection was admitted before this call. Decode, source validation,
    /// provider work and response construction retain the same worker ownership.
    pub async fn wire_admitted(
        &self,
        admission: ReadAdmission,
        operation: EntityWireOperation,
        body: Vec<u8>,
        input: H4Codec,
        output: H4Codec,
    ) -> Result<Vec<u8>, ReadError> {
        if !admission.belongs_to(&self.inner.reads) {
            return Err(ReadError::Forbidden);
        }
        if body.len() > entity::MAX_GRID_BYTES {
            return Err(ReadError::Budget(BudgetKind::Input));
        }
        let identity = Arc::new(Mutex::new(None::<OperationIdentity>));
        let worker_identity = identity.clone();
        let service = self.clone();
        let result = admission
            .run_task(move |principal, budget| {
                budget.charge(BudgetKind::Work, body.len())?;
                budget.charge(
                    BudgetKind::Retained,
                    body.len().saturating_mul(32).saturating_add(8192),
                )?;
                let codec = codec_for(input.mime()).ok_or(ReadError::Unavailable)?;
                let output = codec_for(output.mime()).ok_or(ReadError::Unavailable)?;
                match operation {
                    EntityWireOperation::Batch => {
                        let request: EntityBatchRequest = entity::decode_grid(&body, codec)
                            .map_err(|_| {
                                ReadError::InvalidQuery("invalid entity batch envelope")
                            })?;
                        *worker_identity.lock() = Some(request.identity.clone());
                        let outcome = service.inner.submit(principal, request, budget);
                        entity::encode_grid(&outcome, output)
                            .map_err(|_| ReadError::Budget(BudgetKind::Output))
                    }
                    EntityWireOperation::Receipt => {
                        let request: OperationIdentity = entity::decode_grid(&body, codec)
                            .map_err(|_| ReadError::InvalidQuery("invalid receipt envelope"))?;
                        let outcome = service.inner.reconcile(&principal, request, budget)?;
                        entity::encode_grid(&outcome, output)
                            .map_err(|_| ReadError::Budget(BudgetKind::Output))
                    }
                    EntityWireOperation::Changes => {
                        let request: ChangesRequest = entity::decode_grid(&body, codec)
                            .map_err(|_| ReadError::InvalidQuery("invalid changes envelope"))?;
                        let page = service.feed_page(&principal, request, budget)?;
                        entity::encode_grid(&page, output)
                            .map_err(|_| ReadError::Budget(BudgetKind::Output))
                    }
                }
            })
            .await;
        match result {
            Ok(body) => Ok(body),
            Err(error) => {
                if let Some(identity) = identity.lock().take() {
                    let unknown = MutationOutcome::Unknown {
                        identity,
                        cause: UnknownCause::Provider,
                    };
                    entity::encode_grid(
                        &unknown,
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
