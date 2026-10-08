//! The `ops` op — list all available operations.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::state::SharedState;

/// GET /api/ops — returns a grid listing all available operations.
pub async fn handle(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let accept = headers
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let ops = crate::capabilities::CAPABILITIES
        .iter()
        .filter(|capability| capability.enabled(state.profile));

    let cols = vec![HCol::new("name"), HCol::new("summary")];
    let rows: Vec<HDict> = ops
        .map(|capability| {
            let mut row = HDict::new();
            row.set("name", Kind::Str(capability.name.to_string()));
            row.set("summary", Kind::Str(capability.summary.to_string()));
            row
        })
        .collect();

    let grid = HGrid::from_parts(HDict::new(), cols, rows);
    match content::encode_response_grid(&grid, accept) {
        Ok((body, ct)) => ([(axum::http::header::CONTENT_TYPE, ct)], body).into_response(),
        Err(e) => {
            log::error!("Failed to encode ops grid: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "encoding error").into_response()
        }
    }
}
