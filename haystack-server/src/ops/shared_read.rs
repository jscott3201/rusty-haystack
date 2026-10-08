//! HTTP authenticates and collects a bounded body; the application service owns
//! authorization, query evaluation, projection, cursor state, and wire encoding.
use crate::{auth::AuthUser, content, error::HaystackError, state::SharedState};
use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use haystack_app::{
    BudgetKind, CancellationToken, H4Codec, Principal, ReadContext, ReadError, ReadOperation,
    ReadOutput,
};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(crate) struct ReadStarted(pub Instant);

macro_rules! handler {
    ($name:ident, $op:ident) => {
        pub async fn $name(
            State(state): State<SharedState>,
            request: Request<Body>,
        ) -> Result<Response, HaystackError> {
            handle(state, request, ReadOperation::$op)
                .await
                .map_err(http_error)
        }
    };
}
handler!(read, Read);
handler!(nav, Nav);
handler!(definitions, Definitions);
handler!(libraries, Libraries);
handler!(specs, Specs);
handler!(spec, Spec);

async fn handle(
    state: SharedState,
    request: Request<Body>,
    operation: ReadOperation,
) -> Result<Response, ReadError> {
    let service = state.read_service.as_ref().ok_or(ReadError::Unavailable)?;
    let started = request
        .extensions()
        .get::<ReadStarted>()
        .map(|s| s.0)
        .unwrap_or_else(Instant::now);
    let principal = request
        .extensions()
        .get::<AuthUser>()
        .map(|user| Principal::authenticated(user.username.clone(), user.permissions.clone()))
        .unwrap_or(Principal::Anonymous);
    let context = ReadContext::new(
        principal,
        started + service.limits().max_duration,
        CancellationToken::new(),
    );
    let guard = request
        .extensions()
        .get::<std::sync::Arc<haystack_app::WorkGuard>>()
        .ok_or(ReadError::Closed)?
        .child();
    let admission = service.begin_admitted(context, guard).await?;
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
    // Bound content negotiation allocation before parsing weighted entries.
    if content_type.len().saturating_add(accept.len()) > service.limits().max_input_bytes {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    let input = codec(content::normalize_content_type(content_type));
    let output = codec(content::parse_accept(accept));
    if output == H4Codec::Trio {
        return Err(ReadError::InvalidQuery("codec cannot carry page metadata"));
    }
    let bytes = tokio::select! {
        biased;
        _ = admission.cancelled() => return Err(ReadError::Cancelled),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(admission.deadline())) => return Err(ReadError::Deadline),
        result = to_bytes(body, service.limits().max_input_bytes) => result.map_err(|_| ReadError::Budget(BudgetKind::Input))?,
    };
    // Exactly one permit passes from collection to the worker. Dropping this
    // future during either stage cancels it without releasing a live worker.
    let page = admission
        .read_wire(operation, bytes.to_vec(), input, output)
        .await?;
    match page.output {
        ReadOutput::H4 { body, codec } => {
            Ok(([(axum::http::header::CONTENT_TYPE, codec.mime())], body).into_response())
        }
        ReadOutput::Typed(_) => Err(ReadError::Projection),
    }
}
pub(super) fn codec(mime: &str) -> H4Codec {
    match mime {
        "application/json" => H4Codec::Json,
        "application/json;v=3" => H4Codec::JsonV3,
        "text/trio" => H4Codec::Trio,
        _ => H4Codec::Zinc,
    }
}
pub(super) fn http_error(error: ReadError) -> HaystackError {
    let status = match error {
        ReadError::NotReady | ReadError::Closed => StatusCode::SERVICE_UNAVAILABLE,
        ReadError::InvalidQuery(_) => StatusCode::BAD_REQUEST,
        ReadError::Unavailable => StatusCode::NOT_FOUND,
        ReadError::Forbidden => StatusCode::FORBIDDEN,
        ReadError::StaleCursor => StatusCode::CONFLICT,
        ReadError::Capacity => StatusCode::TOO_MANY_REQUESTS,
        ReadError::Cancelled | ReadError::Deadline => StatusCode::REQUEST_TIMEOUT,
        ReadError::Budget(BudgetKind::Input) => StatusCode::PAYLOAD_TOO_LARGE,
        ReadError::Budget(_) | ReadError::Projection | ReadError::UnitTooLarge => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        ReadError::InvalidLimits => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HaystackError::new(error.to_string(), status)
}
