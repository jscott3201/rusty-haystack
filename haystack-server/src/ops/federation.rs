//! Federation HTTP endpoints — status and sync for remote connectors.

use actix_web::{web, HttpRequest, HttpResponse};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{Kind, Number};

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// GET /api/federation/status
///
/// Returns a grid with one row per connector: `name` and `entityCount`.
pub async fn handle_status(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let status = state.federation.status();

    let grid = if status.is_empty() {
        HGrid::from_parts(
            HDict::new(),
            vec![HCol::new("name"), HCol::new("entityCount")],
            vec![],
        )
    } else {
        let rows: Vec<HDict> = status
            .into_iter()
            .map(|(name, count)| {
                let mut row = HDict::new();
                row.set("name", Kind::Str(name));
                row.set("entityCount", Kind::Number(Number::unitless(count as f64)));
                row
            })
            .collect();
        HGrid::from_parts(
            HDict::new(),
            vec![HCol::new("name"), HCol::new("entityCount")],
            rows,
        )
    };

    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/federation/sync
///
/// Triggers `sync_all()` on all connectors. Returns a grid with `name`,
/// `result` (entity count or error string), and `ok` (Bool).
pub async fn handle_sync(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let results = state.federation.sync_all().await;

    let rows: Vec<HDict> = results
        .into_iter()
        .map(|(name, result)| {
            let mut row = HDict::new();
            row.set("name", Kind::Str(name));
            match result {
                Ok(count) => {
                    row.set("result", Kind::Str(format!("{count} entities")));
                    row.set("ok", Kind::Bool(true));
                }
                Err(err) => {
                    row.set("result", Kind::Str(err));
                    row.set("ok", Kind::Bool(false));
                }
            }
            row
        })
        .collect();

    let grid = HGrid::from_parts(
        HDict::new(),
        vec![
            HCol::new("name"),
            HCol::new("result"),
            HCol::new("ok"),
        ],
        rows,
    );

    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}
