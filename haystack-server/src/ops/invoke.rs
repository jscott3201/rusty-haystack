//! The `invokeAction` op — invoke an action on an entity.
//!
//! Parses `id` and `action` columns from the request grid, resolves
//! the entity from the graph, and dispatches to the ActionRegistry.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::kinds::Kind;

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// POST /api/invokeAction
///
/// Request grid must have `id` (Ref) and `action` (Str) columns in the
/// first row.  Additional columns are passed as arguments to the handler.
pub async fn handle(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let content_type = req
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let request_grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

    // Extract id and action from the first row
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

    // Check federation: if entity is not in local graph, proxy to remote.
    if !state.graph.contains(ref_val) {
        if let Some(connector) = state.federation.owner_of(ref_val) {
            let args = row.clone();
            let grid = connector
                .proxy_invoke_action(ref_val, action, args)
                .await
                .map_err(|e| {
                    HaystackError::internal(format!("federation proxy error: {e}"))
                })?;
            let (encoded, ct) = content::encode_response_grid(&grid, accept)
                .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
            return Ok(HttpResponse::Ok().content_type(ct).body(encoded));
        }
        return Err(HaystackError::not_found(format!(
            "entity not found: {ref_val}"
        )));
    }

    // Resolve entity from the graph
    let entity = state
        .graph
        .get(ref_val)
        .ok_or_else(|| HaystackError::not_found(format!("entity not found: {ref_val}")))?;

    // The remaining tags in the row serve as arguments (clone row as args dict)
    let args = row.clone();

    log::info!("invokeAction: id={ref_val} action={action}");

    // Dispatch to the action registry
    let result_grid = state
        .actions
        .invoke(&entity, action, &args)
        .map_err(HaystackError::bad_request)?;

    let (encoded, ct) = content::encode_response_grid(&result_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}
