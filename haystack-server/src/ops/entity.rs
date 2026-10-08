//! Thin HTTP entity extension adapter; the managed application owns execution.
use super::shared_read::{ReadStarted, codec, http_error};
use crate::{auth::AuthUser, content, error::HaystackError, state::SharedState};
use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::Request,
    response::{IntoResponse, Response},
};
use haystack_app::{
    BudgetKind, CancellationToken, EntityWireOperation, Principal, ReadContext, ReadError,
};
use haystack_core::codecs::entity::MAX_GRID_BYTES;
use std::time::Instant;
macro_rules! handler {
    ($name:ident,$op:ident) => {
        pub async fn $name(
            State(state): State<SharedState>,
            request: Request<Body>,
        ) -> Result<Response, HaystackError> {
            handle(state, request, EntityWireOperation::$op)
                .await
                .map_err(http_error)
        }
    };
}
handler!(batch, Batch);
handler!(receipt, Receipt);
handler!(changes, Changes);
async fn handle(
    state: SharedState,
    request: Request<Body>,
    operation: EntityWireOperation,
) -> Result<Response, ReadError> {
    let service = state
        .mutation_service
        .clone()
        .ok_or(ReadError::Unavailable)?;
    let reads = service.read_service();
    let started = request
        .extensions()
        .get::<ReadStarted>()
        .map(|s| s.0)
        .unwrap_or_else(Instant::now);
    let principal = request
        .extensions()
        .get::<AuthUser>()
        .map(|u| Principal::authenticated(u.username.clone(), u.permissions.clone()))
        .unwrap_or(Principal::Anonymous);
    let context = ReadContext::new(
        principal,
        started + reads.limits().max_duration,
        CancellationToken::new(),
    );
    let guard = request
        .extensions()
        .get::<std::sync::Arc<haystack_app::WorkGuard>>()
        .ok_or(ReadError::Closed)?
        .child();
    let admission = reads.begin_admitted(context, guard).await?;
    let (parts, body) = request.into_parts();
    let content_type = parts
        .headers
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = parts
        .headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if content_type.len().saturating_add(accept.len()) > reads.limits().max_input_bytes {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    let input = codec(content::normalize_content_type(content_type));
    let output = codec(content::parse_accept(accept));
    let bytes = tokio::select! {biased;_=admission.cancelled()=>return Err(ReadError::Cancelled),_=tokio::time::sleep_until(admission.deadline().into())=>return Err(ReadError::Deadline),result=to_bytes(body,MAX_GRID_BYTES)=>result.map_err(|_|ReadError::Budget(BudgetKind::Input))?};
    let body = service
        .wire_admitted(admission, operation, bytes.to_vec(), input, output)
        .await?;
    Ok(([(axum::http::header::CONTENT_TYPE, output.mime())], body).into_response())
}
