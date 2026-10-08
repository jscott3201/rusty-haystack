//! Thin entity-v1 consumers. HTTP implements the explicit transport opt-in;
//! custom transports must guarantee a single submission with no automatic retry.
use crate::{
    ClientError, HaystackClient,
    transport::{Transport, http::HttpTransport},
};
use haystack_core::codecs::entity::{
    self, ChangesPage, ChangesRequest, EntityBatchRequest, EntityReceipt, MutationOutcome,
    OperationIdentity, UnknownCause,
};
use haystack_core::graph::{CommitSpan, EntityOperation};

/// Opt-in contract for entity extension requests. A `call` sends at most once;
/// it must not retry a mutation after a connection/body/acknowledgement failure.
/// WebSocket transport does not implement this extension contract.
pub trait EntityTransport: Transport {
    /// Check before dispatch that this transport has a no-retry/no-redirect policy.
    fn check_entity_submission(&self) -> Result<(), ClientError> {
        Ok(())
    }
}
impl EntityTransport for HttpTransport {
    fn check_entity_submission(&self) -> Result<(), ClientError> {
        self.check_entity_submission_policy()
    }
}
impl<T: EntityTransport> HaystackClient<T> {
    /// Submit once. Any failure after dispatch may have begun is an unknown
    /// effect preserving the operation identity, including malformed replies.
    /// The caller explicitly invokes `reconcile_entity` to learn the outcome.
    pub async fn submit_entities(
        &self,
        request: &EntityBatchRequest,
    ) -> Result<MutationOutcome, ClientError> {
        self.transport_entity_check()?;
        let grid = entity::to_grid(request).map_err(|e| ClientError::Codec(e.to_string()))?;
        let unknown = |cause| MutationOutcome::Unknown {
            identity: request.identity.clone(),
            cause,
        };
        let response = match self.call("entityBatch", &grid).await {
            Ok(grid) => grid,
            Err(_) => return Ok(unknown(UnknownCause::Transport)),
        };
        let outcome: MutationOutcome = match entity::from_grid(&response) {
            Ok(outcome) => outcome,
            Err(_) => return Ok(unknown(UnknownCause::InvalidAcknowledgement)),
        };
        if outcome.identity() != &request.identity
            || matches!(&outcome,MutationOutcome::Committed(r) if !matches_request(request, r))
        {
            return Ok(unknown(UnknownCause::InvalidAcknowledgement));
        }
        Ok(outcome)
    }
    pub async fn reconcile_entity(
        &self,
        identity: &OperationIdentity,
    ) -> Result<MutationOutcome, ClientError> {
        let grid = entity::to_grid(identity).map_err(|e| ClientError::Codec(e.to_string()))?;
        let unknown = |cause| MutationOutcome::Unknown {
            identity: identity.clone(),
            cause,
        };
        let response = match self.call("entityReceipt", &grid).await {
            Ok(grid) => grid,
            Err(_) => return Ok(unknown(UnknownCause::Transport)),
        };
        let outcome: MutationOutcome = match entity::from_grid(&response) {
            Ok(outcome) => outcome,
            Err(_) => return Ok(unknown(UnknownCause::InvalidAcknowledgement)),
        };
        if outcome.identity() != identity {
            return Ok(unknown(UnknownCause::InvalidAcknowledgement));
        }
        Ok(outcome)
    }
    pub async fn entity_changes(
        &self,
        request: &ChangesRequest,
    ) -> Result<ChangesPage, ClientError> {
        let grid = entity::to_grid(request).map_err(|e| ClientError::Codec(e.to_string()))?;
        let response = self.call("changes", &grid).await?;
        entity::from_grid(&response)
            .map_err(|_| ClientError::Codec("invalid entity changes acknowledgement".into()))
    }
}

/// A committed result must account for every requested revision. Equality of
/// row values does not suppress a nonempty patch; only an empty patch is a no-op.
fn matches_request(request: &EntityBatchRequest, receipt: &EntityReceipt) -> bool {
    let changed = request.operations.iter().filter(|operation| {
        !matches!(operation, EntityOperation::Patch { changes, .. } if changes.is_empty())
    }).count();
    let Ok(changed) = u64::try_from(changed) else {
        return false;
    };
    let Some(after) = request.expected_revision.checked_add(changed) else {
        return false;
    };
    let span = if changed == 0 {
        None
    } else {
        let Some(first) = request.expected_revision.checked_add(1) else {
            return false;
        };
        Some(CommitSpan { first, last: after })
    };
    receipt.before_revision == request.expected_revision
        && receipt.after_revision == after
        && receipt.span == span
}
