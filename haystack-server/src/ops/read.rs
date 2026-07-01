//! The `read` op — read entities by filter or by id.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{HRef, Kind};
use std::sync::Arc;

use crate::content;
use crate::error::HaystackError;
use crate::state::SharedState;

/// Helper to build an Axum response from encoded grid output.
fn haystack_response(body: Vec<u8>, content_type: &str) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, content_type.to_string())],
        body,
    )
        .into_response()
}

/// POST /api/read
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

    let result_grid = if request_grid.col("id").is_some() {
        read_by_id(&request_grid, &state)
    } else if request_grid.col("filter").is_some() {
        read_by_filter(&request_grid, &state)
    } else {
        Err(HaystackError::bad_request(
            "request must have 'id' or 'filter' column",
        ))
    }?;

    let (encoded, ct) = content::encode_response_grid(&result_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(haystack_response(encoded, ct))
}

/// Read entities by filter expression.
fn read_by_filter(request_grid: &HGrid, state: &SharedState) -> Result<HGrid, HaystackError> {
    let row = request_grid
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;

    let filter = match row.get("filter") {
        Some(Kind::Str(s)) => s.as_str(),
        _ => return Err(HaystackError::bad_request("filter must be a Str value")),
    };

    let limit = match row.get("limit") {
        Some(Kind::Number(n)) => n.val as usize,
        _ => 0, // 0 means no limit
    };

    let effective_limit = if limit == 0 { usize::MAX } else { limit };
    let is_wildcard = filter == "*";

    let mut results: Vec<Arc<HDict>> = if is_wildcard {
        state
            .graph
            .all_entities()
            .into_iter()
            .map(Arc::new)
            .collect()
    } else {
        let local_grid = state
            .graph
            .read_filter(filter, limit)
            .map_err(|e| HaystackError::bad_request(format!("filter error: {e}")))?;
        local_grid.rows.into_iter().map(Arc::new).collect()
    };

    if results.len() > effective_limit {
        results.truncate(effective_limit);
    }

    if results.is_empty() {
        return Ok(HGrid::new());
    }

    // Build column set from all result entities.
    let mut col_set: Vec<&str> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for entity in &results {
        for name in entity.tag_names() {
            if seen.insert(name) {
                col_set.push(name);
            }
        }
    }
    col_set.sort_unstable();
    let cols: Vec<HCol> = col_set.iter().map(|&n| HCol::new(n)).collect();

    Ok(HGrid::from_parts_arc(HDict::new(), cols, results))
}

/// Read entities by ID references.
fn read_by_id(request_grid: &HGrid, state: &SharedState) -> Result<HGrid, HaystackError> {
    let mut results: Vec<Arc<HDict>> = Vec::new();

    for row in request_grid.rows.iter() {
        let ref_val = match row.get("id") {
            Some(Kind::Ref(r)) => &r.val,
            _ => continue,
        };

        if let Some(entity) = state.graph.get(ref_val) {
            results.push(Arc::new(entity));
        } else {
            // Add missing stub for IDs not found.
            let mut missing = HDict::new();
            missing.set("id", Kind::Ref(HRef::from_val(ref_val.as_str())));
            results.push(Arc::new(missing));
        }
    }

    if results.is_empty() {
        return Ok(HGrid::new());
    }

    // Build column set.
    let mut col_set: Vec<&str> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for entity in &results {
        for name in entity.tag_names() {
            if seen.insert(name) {
                col_set.push(name);
            }
        }
    }

    col_set.sort_unstable();
    let cols: Vec<HCol> = col_set.iter().map(|&n| HCol::new(n)).collect();
    Ok(HGrid::from_parts_arc(HDict::new(), cols, results))
}
