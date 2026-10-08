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
        .filter(|capability| {
            capability.enabled(
                state.profile,
                state.mutation_service.is_some(),
                state.history_service.is_some(),
                state.history_mutation_service.is_some(),
                state.subscription_service.is_some(),
            )
        });

    let cols = vec![HCol::new("name"), HCol::new("summary")];
    let mut rows: Vec<HDict> = ops
        .map(|capability| {
            let mut row = HDict::new();
            row.set("name", Kind::Str(capability.name.to_string()));
            row.set("summary", Kind::Str(capability.summary.to_string()));
            row
        })
        .collect();

    if let Some(application) = &state.application {
        for function in application.read_service().typed_functions() {
            if crate::capabilities::CAPABILITIES
                .iter()
                .any(|capability| capability.name == function.name)
            {
                continue;
            }
            let mut row = HDict::new();
            row.set("name", Kind::Str(function.name.to_owned()));
            row.set(
                "summary",
                Kind::Str(function.doc.unwrap_or(function.signature).to_owned()),
            );
            rows.push(row);
        }
    }
    let grid = HGrid::from_parts(HDict::new(), cols, rows);
    match content::encode_response_grid(&grid, accept) {
        Ok((body, ct)) => ([(axum::http::header::CONTENT_TYPE, ct)], body).into_response(),
        Err(e) => {
            log::error!("Failed to encode ops grid: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "encoding error").into_response()
        }
    }
}
