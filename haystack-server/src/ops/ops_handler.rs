//! The `ops` op — list all available operations.

use actix_web::{web, HttpRequest, HttpResponse};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::state::AppState;

/// GET /api/ops — returns a grid listing all available operations.
pub async fn handle(req: HttpRequest, _state: web::Data<AppState>) -> HttpResponse {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let ops = vec![
        ("about", "Summary information for server"),
        ("ops", "Operations supported by this server"),
        ("formats", "Grid data formats supported by this server"),
        ("read", "Read entity records by id or filter"),
        ("nav", "Navigate a project for discovery"),
        ("defs", "Query the definitions namespace"),
        ("libs", "Query the library namespace"),
        ("watchSub", "Subscribe to entity changes"),
        ("watchPoll", "Poll for entity changes"),
        ("watchUnsub", "Unsubscribe from entity changes"),
        ("pointWrite", "Write a value to a writable point"),
        ("hisRead", "Read historical time-series data"),
        ("hisWrite", "Write historical time-series data"),
        ("invokeAction", "Invoke an action on an entity"),
        ("close", "Close the current session"),
    ];

    let cols = vec![HCol::new("name"), HCol::new("summary")];
    let rows: Vec<HDict> = ops
        .into_iter()
        .map(|(name, summary)| {
            let mut row = HDict::new();
            row.set("name", Kind::Str(name.to_string()));
            row.set("summary", Kind::Str(summary.to_string()));
            row
        })
        .collect();

    let grid = HGrid::from_parts(HDict::new(), cols, rows);
    match content::encode_response_grid(&grid, accept) {
        Ok((body, ct)) => HttpResponse::Ok().content_type(ct).body(body),
        Err(e) => {
            log::error!("Failed to encode ops grid: {e}");
            HttpResponse::InternalServerError().body("encoding error")
        }
    }
}
