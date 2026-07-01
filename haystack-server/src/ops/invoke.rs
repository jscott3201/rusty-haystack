//! The `invokeAction` op — invoke an action on an entity.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use haystack_core::kinds::Kind;

use crate::content;
use crate::error::HaystackError;
use crate::state::SharedState;

/// POST /api/invokeAction
pub async fn handle(
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

    let row = request_grid
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;

    let ref_val = match row.get("id") {
        Some(Kind::Ref(r)) => &r.val,
        _ => {
            return Err(HaystackError::bad_request(
                "request row must have an 'id' column with a Ref value",
            ));
        }
    };

    let action = match row.get("action") {
        Some(Kind::Str(s)) => s.as_str(),
        _ => {
            return Err(HaystackError::bad_request(
                "request row must have an 'action' column with a Str value",
            ));
        }
    };

    // Resolve entity from the graph
    let entity = state
        .graph
        .get(ref_val)
        .ok_or_else(|| HaystackError::not_found(format!("entity not found: {ref_val}")))?;

    // The remaining tags in the row serve as arguments
    let args = row.clone();

    log::info!("invokeAction: id={ref_val} action={action}");

    // Dispatch to the action registry
    let result_grid = state
        .actions
        .invoke(&entity, action, &args)
        .map_err(HaystackError::bad_request)?;

    let (encoded, ct) = content::encode_response_grid(&result_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(([(axum::http::header::CONTENT_TYPE, ct)], encoded).into_response())
}
