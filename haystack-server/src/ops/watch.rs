//! Watch HTTP adaptation over the selected application owner.
use super::shared_read::{ReadStarted, codec, http_error};
use crate::{content, error::HaystackError, state::SharedState};
use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::Request,
    response::{IntoResponse, Response},
};
use haystack_app::{BudgetKind, CancellationToken, ReadContext, ReadError, SubscriptionSession};
use std::time::Instant;
macro_rules! handler {
    ($name:ident,$op:literal) => {
        pub async fn $name(
            State(state): State<SharedState>,
            request: Request<Body>,
        ) -> Result<Response, HaystackError> {
            handle(state, request, $op).await.map_err(http_error)
        }
    };
}
handler!(handle_sub, "watchSub");
handler!(handle_poll, "watchPoll");
handler!(handle_unsub, "watchUnsub");
handler!(handle_info, "watchInfo");
handler!(handle_ack, "watchAck");
handler!(handle_renew, "watchRenew");
async fn handle(
    state: SharedState,
    request: Request<Body>,
    operation: &'static str,
) -> Result<Response, ReadError> {
    let service = state
        .subscription_service
        .clone()
        .ok_or(ReadError::Unavailable)?;
    let legacy = state.profile == crate::capabilities::ServiceProfile::LegacyUnrestricted;
    let session = request
        .extensions()
        .get::<SubscriptionSession>()
        .cloned()
        .or_else(|| legacy.then(|| service.anonymous_legacy_session()))
        .ok_or(ReadError::Forbidden)?;
    let reads = service.read_service();
    let started = request
        .extensions()
        .get::<ReadStarted>()
        .map(|s| s.0)
        .unwrap_or_else(Instant::now);
    let context = ReadContext::new(
        session.principal().clone(),
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
    let (input, output) = if legacy {
        (
            codec(content::normalize_content_type(content_type)),
            codec(content::parse_accept(accept)),
        )
    } else {
        (scoped_codec(content_type)?, scoped_codec(accept)?)
    };
    let bytes = tokio::select! {biased;_=session.closed()=>return Err(ReadError::Forbidden),_=admission.cancelled()=>return Err(ReadError::Cancelled),_=tokio::time::sleep_until(admission.deadline().into())=>return Err(ReadError::Deadline),result=to_bytes(body,haystack_core::codecs::subscription::MAX_WIRE_BYTES)=>result.map_err(|_|ReadError::Budget(BudgetKind::Input))?};
    let body = service
        .wire_admitted(
            admission,
            session,
            haystack_app::SubscriptionWireRequest {
                operation,
                body: bytes.to_vec(),
                input,
                output,
                legacy_allowed: legacy,
            },
        )
        .await?;
    Ok(([(axum::http::header::CONTENT_TYPE, output.mime())], body).into_response())
}

fn scoped_codec(header: &str) -> Result<haystack_app::H4Codec, ReadError> {
    let mut fields = header.split(';');
    let mime = fields.next().unwrap_or("").trim();
    let mut version = None;
    for field in fields {
        match field.trim() {
            "v=3" if version.is_none() => version = Some(3),
            "v=4" if version.is_none() => version = Some(4),
            "charset=utf-8" | "charset=UTF-8" => {}
            _ => {
                return Err(ReadError::InvalidQuery(
                    "unsupported subscription format parameter",
                ));
            }
        }
    }
    match (mime, version) {
        ("" | "text/zinc" | "*/*", None) => Ok(haystack_app::H4Codec::Zinc),
        ("application/json", Some(3)) => Ok(haystack_app::H4Codec::JsonV3),
        ("application/json", None | Some(4)) => Ok(haystack_app::H4Codec::Json),
        _ => Err(ReadError::InvalidQuery(
            "subscriptions require Zinc or JSON v3/v4",
        )),
    }
}
