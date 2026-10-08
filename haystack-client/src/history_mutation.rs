//! One submission, explicit receipt lookup and no automatic mutation replay.
use crate::{
    ClientError, HaystackClient,
    transport::{Transport, http::HttpTransport},
};
use haystack_core::codecs::history_mutation::{self as wire, *};
/// An explicit transport opt-in. Calls dispatch at most once, do not redirect or
/// replay, bound complete response bodies, and preserve typed receipt metadata.
/// WebSocket does not implement this extension contract.
pub trait HistoryMutationTransport: Transport {
    fn check_history_submission(&self) -> Result<(), ClientError> {
        Ok(())
    }
}
impl HistoryMutationTransport for HttpTransport {
    fn check_history_submission(&self) -> Result<(), ClientError> {
        self.check_history_submission_policy()
    }
}
impl<T: HistoryMutationTransport> HaystackClient<T> {
    /// Submit exactly once. An unreadable, malformed, empty or foreign reply is
    /// Unknown with the original identity. Reconcile explicitly; never replay.
    pub async fn his_write_scoped(
        &self,
        request: &HistoryWriteRequest,
    ) -> Result<HistoryWriteOutcome, ClientError> {
        self.transport_history_mutation_check()?;
        let grid =
            wire::request_grid(request).map_err(|error| ClientError::Codec(error.to_string()))?;
        let unknown = |cause| HistoryWriteOutcome::Unknown {
            identity: request.identity.clone(),
            cause,
        };
        let grid = match self.call("hisWrite", &grid).await {
            Ok(grid) => grid,
            Err(_) => return Ok(unknown(HistoryWriteUnknown::Transport)),
        };
        let outcome = match wire::outcome_from_grid(&grid) {
            Ok(outcome) => outcome,
            Err(_) => return Ok(unknown(HistoryWriteUnknown::InvalidAcknowledgement)),
        };
        if wire::validate_for_request(&outcome, request).is_err() {
            return Ok(unknown(HistoryWriteUnknown::InvalidAcknowledgement));
        }
        Ok(outcome)
    }
    pub async fn reconcile_history(
        &self,
        identity: &HistoryOperationIdentity,
    ) -> Result<HistoryWriteOutcome, ClientError> {
        self.transport_history_mutation_check()?;
        let grid =
            wire::lookup_grid(identity).map_err(|error| ClientError::Codec(error.to_string()))?;
        let unknown = |cause| HistoryWriteOutcome::Unknown {
            identity: identity.clone(),
            cause,
        };
        let grid = match self.call("hisReceipt", &grid).await {
            Ok(grid) => grid,
            Err(_) => return Ok(unknown(HistoryWriteUnknown::Transport)),
        };
        let outcome = match wire::outcome_from_grid(&grid) {
            Ok(outcome) => outcome,
            Err(_) => return Ok(unknown(HistoryWriteUnknown::InvalidAcknowledgement)),
        };
        if outcome.identity() != identity {
            return Ok(unknown(HistoryWriteUnknown::InvalidAcknowledgement));
        }
        Ok(outcome)
    }
}
