use super::*;
use crate::{
    ApiError, CommandRequest, SimulatedCommandService, TypedInvocationResponse, registry::Registry,
    typed_http,
};
pub(super) struct Prepared {
    registry: Arc<Registry>,
    wire: typed_http::Request,
    request: CommandRequest,
    session: crate::SubscriptionSession,
    policy_scope: String,
}
impl Prepared {
    pub fn new(
        registry: Arc<Registry>,
        mut wire: typed_http::Request,
        args: &HDict,
        session: crate::SubscriptionSession,
        policy_scope: &str,
        budget: &mut Budget,
    ) -> Result<Self, ApiError> {
        let request = CommandRequest::decode(args)?;
        wire.args = HDict::new();
        Ok(Self {
            registry,
            wire,
            request,
            session,
            policy_scope: budget.copy_string(policy_scope)?,
        })
    }
    pub async fn execute(
        self,
        mut admission: ReadAdmission,
        service: Option<SimulatedCommandService>,
    ) -> Result<TypedInvocationResponse, ApiError> {
        let service = service.ok_or(ApiError::Unavailable)?;
        admission.budget_mut().typed_catalog = Some(self.registry.observation().clone());
        let attempt = admission
            .budget_mut()
            .command_attempt
            .clone()
            .ok_or(ApiError::Internal)?;
        let result = admission
            .run_task(move |principal, budget| {
                Ok(service.execute(
                    &principal,
                    &self.session,
                    self.request,
                    Some(&self.policy_scope),
                    budget,
                    |grid, budget| {
                        let value = Kind::Grid(Box::new(grid));
                        self.registry
                            .catalog()
                            .fit_result("ph.api::pointWrite", &value)
                            .map_err(|_| ApiError::Internal)?;
                        let profile = self
                            .registry
                            .observation()
                            .callable("ph.api::pointWrite")
                            .ok_or(ApiError::Unavailable)?;
                        typed_http::encode(value, &self.wire, profile, budget)
                    },
                ))
            })
            .await
            .map_err(ApiError::from)
            .and_then(|r| r);
        result.map_err(|e| attempt.classify(e))
    }
}
