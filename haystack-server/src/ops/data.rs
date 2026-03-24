//! The `export` and `import` ops — bulk data import/export.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{Kind, Number};

use crate::content;
use crate::error::HaystackError;
use crate::state::SharedState;

/// POST /api/export
pub async fn handle_export(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Response, HaystackError> {
    let accept = headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let grid = state
        .graph
        .read(|g| g.to_grid(""))
        .map_err(|e| HaystackError::internal(format!("export failed: {e}")))?;

    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(([(axum::http::header::CONTENT_TYPE, ct)], encoded).into_response())
}

/// POST /api/import
pub async fn handle_import(
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

    let mut count: usize = 0;

    for row in &request_grid.rows {
        let ref_val = match row.id() {
            Some(r) => r.val.clone(),
            None => {
                // Skip rows without a valid Ref id
                continue;
            }
        };

        if state.graph.contains(&ref_val) {
            // Update existing entity
            state.graph.update(&ref_val, row.clone()).map_err(|e| {
                HaystackError::internal(format!("update failed for {ref_val}: {e}"))
            })?;
        } else {
            // Add new entity
            state
                .graph
                .add(row.clone())
                .map_err(|e| HaystackError::internal(format!("add failed for {ref_val}: {e}")))?;
        }

        count += 1;
    }

    // Build response grid with count
    let mut row = HDict::new();
    row.set("count", Kind::Number(Number::new(count as f64, None)));

    let cols = vec![HCol::new("count")];
    let result_grid = HGrid::from_parts(HDict::new(), cols, vec![row]);

    let (encoded, ct) = content::encode_response_grid(&result_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(([(axum::http::header::CONTENT_TYPE, ct)], encoded).into_response())
}
