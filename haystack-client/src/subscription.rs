//! Explicit state-subscription-v1 operations. The caller owns creation keys,
//! prepared deliveries, acknowledgement and renewal decisions across reconnects.
use crate::{
    ClientError, HaystackClient,
    transport::{Transport, http::HttpTransport, subscription_ws::SubscriptionWsTransport},
};
use haystack_core::codecs::subscription::{self as wire, SubscriptionOutcome, SubscriptionRequest};
/// Opt-in contract: bounded complete responses, at most one dispatch per call,
/// no redirects, automatic reconnect, resubscription, acknowledgement or renewal.
pub trait SubscriptionTransport: Transport {
    fn check_subscriptions(&self) -> Result<(), ClientError> {
        Ok(())
    }
}
impl SubscriptionTransport for HttpTransport {
    fn check_subscriptions(&self) -> Result<(), ClientError> {
        self.check_subscription_policy()
    }
}
impl SubscriptionTransport for SubscriptionWsTransport {}
impl<T: SubscriptionTransport> HaystackClient<T> {
    /// Dispatch exactly once. Any unverified result after dispatch is Unknown;
    /// retrying with the same caller-known creation key is an explicit decision.
    pub async fn state_subscription(
        &self,
        request: &SubscriptionRequest,
    ) -> Result<SubscriptionOutcome, ClientError> {
        self.transport_subscription_check()?;
        let grid = wire::to_grid(request)
            .map_err(|_| ClientError::Codec("invalid subscription request".into()))?;
        let response = match self.call(request.operation(), &grid).await {
            Ok(response) => response,
            Err(_) => return Ok(SubscriptionOutcome::Unknown),
        };
        let outcome = match wire::from_grid::<SubscriptionOutcome>(&response) {
            Ok(outcome) => outcome,
            Err(_) => return Ok(SubscriptionOutcome::Unknown),
        };
        if wire::validate_for_request(&outcome, request).is_err() {
            return Ok(SubscriptionOutcome::Unknown);
        }
        Ok(outcome)
    }
}
