//! Bounded shared history reads and explicit legacy native history writes.
use super::shared_read::{ReadStarted, http_error};
use crate::{auth::AuthUser, content, error::HaystackError, state::SharedState};
use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request},
    response::{IntoResponse, Response},
};
use haystack_app::{
    BudgetKind, CancellationToken, H4Codec, HisItem, Principal, ReadContext, ReadError,
};
use haystack_core::{codecs::history, data::HGrid, kinds::Kind};
use std::time::Instant;

pub async fn handle_scoped_read(
    State(state): State<SharedState>,
    request: Request<Body>,
) -> Result<Response, HaystackError> {
    read(state, request, true).await.map_err(http_error)
}
pub async fn handle_read(
    State(state): State<SharedState>,
    request: Request<Body>,
) -> Result<Response, HaystackError> {
    read(state, request, false).await.map_err(http_error)
}
fn history_codec(header: &str) -> Result<H4Codec, ReadError> {
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
                    "unsupported history format parameter",
                ));
            }
        }
    }
    match (mime, version) {
        ("" | "text/zinc" | "*/*", None) => Ok(H4Codec::Zinc),
        ("application/json", Some(3)) => Ok(H4Codec::JsonV3),
        ("application/json", None | Some(4)) => Ok(H4Codec::Json),
        _ => Err(ReadError::InvalidQuery(
            "history requires Zinc or JSON v3/v4",
        )),
    }
}
async fn read(
    state: SharedState,
    request: Request<Body>,
    scoped: bool,
) -> Result<Response, ReadError> {
    let service = state
        .history_service
        .as_ref()
        .ok_or(ReadError::Unavailable)?;
    let reads = service.read_service();
    let started = request
        .extensions()
        .get::<ReadStarted>()
        .map(|value| value.0)
        .unwrap_or_else(Instant::now);
    let principal = request
        .extensions()
        .get::<AuthUser>()
        .map(|user| Principal::authenticated(user.username.clone(), user.permissions.clone()))
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
    let input = parts
        .headers
        .get("Content-Type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let output = parts
        .headers
        .get("Accept")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if input.len().saturating_add(output.len()) > reads.limits().max_input_bytes {
        return Err(ReadError::Budget(BudgetKind::Input));
    }
    // Admission rejects unsupported negotiation before collecting/provider work.
    let input = history_codec(input)?;
    let output = history_codec(output)?;
    let bytes = tokio::select! {
        biased;
        _ = admission.cancelled() => return Err(ReadError::Cancelled),
        _ = tokio::time::sleep_until(admission.deadline().into()) => return Err(ReadError::Deadline),
        result = to_bytes(body, history::MAX_REQUEST_BYTES.min(reads.limits().max_input_bytes)) => result.map_err(|_| ReadError::Budget(BudgetKind::Input))?,
    };
    let bytes = if scoped {
        service
            .wire_admitted(admission, bytes.to_vec(), input, output)
            .await?
    } else {
        service
            .legacy_wire_admitted(admission, bytes.to_vec(), input, output)
            .await?
    };
    Ok(([(axum::http::header::CONTENT_TYPE, output.mime())], bytes).into_response())
}

// ---------------------------------------------------------------------------
// hisWrite
// ---------------------------------------------------------------------------

const MAX_HIS_WRITE_ROWS: usize = 100_000;

/// POST /api/hisWrite
pub async fn handle_write(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, HaystackError> {
    let content_type = headers
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let request_grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

    if request_grid.rows.len() > MAX_HIS_WRITE_ROWS {
        return Err(HaystackError::bad_request("too many history rows"));
    }

    let id = match request_grid.meta.get("id") {
        Some(Kind::Ref(r)) => r.val.clone(),
        _ => {
            return Err(HaystackError::bad_request(
                "hisWrite: grid meta must contain 'id' Ref",
            ));
        }
    };

    // Parse rows into HisItems.
    let mut items = Vec::with_capacity(request_grid.len());
    for (i, row) in request_grid.iter().enumerate() {
        let ts = match row.get("ts") {
            Some(Kind::DateTime(hdt)) => hdt.dt,
            _ => {
                return Err(HaystackError::bad_request(format!(
                    "hisWrite: row {i} missing or invalid 'ts' DateTime"
                )));
            }
        };
        let val = row.get("val").cloned().unwrap_or(Kind::Null);

        items.push(HisItem { ts, val });
    }

    let count = items.len();
    state
        .his
        .as_ref()
        .ok_or_else(|| HaystackError::bad_request("history unavailable"))?
        .his_write(&id, items)
        .await
        .map_err(|_| HaystackError::internal("history provider write failed"))?;

    log::info!("hisWrite: stored {} items for point {}", count, id);
    let grid = HGrid::new();
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(([(axum::http::header::CONTENT_TYPE, ct)], encoded).into_response())
}
