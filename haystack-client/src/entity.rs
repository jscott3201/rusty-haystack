//! Thin entity-v1 consumers. HTTP implements the explicit transport opt-in;
//! custom transports must guarantee a single submission with no automatic retry.
use crate::{
    ClientError, HaystackClient,
    transport::{Transport, http::HttpTransport},
};
use haystack_core::codecs::entity::{
    self, ChangesPage, ChangesRequest, EntityBatchRequest, MutationOutcome, OperationIdentity,
    UnknownCause,
};

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
            || matches!(&outcome,MutationOutcome::Committed(r) if r.before_revision!=request.expected_revision)
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
