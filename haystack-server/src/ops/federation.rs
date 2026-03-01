//! Federation HTTP endpoints — status and sync for remote connectors.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{HDateTime, Kind, Number};

use crate::connector::TransportMode;
use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// Column definitions for the federation status grid.
fn status_columns() -> Vec<HCol> {
    vec![
        HCol::new("name"),
        HCol::new("entityCount"),
        HCol::new("transport"),
        HCol::new("connected"),
        HCol::new("lastSync"),
    ]
}

/// GET /api/federation/status
///
/// Returns a grid with one row per connector: `name`, `entityCount`,
/// `transport`, `connected`, and `lastSync`.
pub async fn handle_status(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let connectors = &state.federation.connectors;

    let grid = if connectors.is_empty() {
        HGrid::from_parts(HDict::new(), status_columns(), vec![])
    } else {
        let rows: Vec<HDict> = connectors
            .iter()
            .map(|c| {
                let mut row = HDict::new();
                row.set("name", Kind::Str(c.config.name.clone()));
                row.set(
                    "entityCount",
                    Kind::Number(Number::unitless(c.entity_count() as f64)),
                );
                let transport_str = match c.transport_mode() {
                    TransportMode::Http => "http",
                    TransportMode::WebSocket => "ws",
                };
                row.set("transport", Kind::Str(transport_str.to_string()));
                row.set("connected", Kind::Bool(c.is_connected()));
                let last_sync_kind = match c.last_sync_time() {
                    Some(ts) => {
                        let fixed = ts.fixed_offset();
                        Kind::DateTime(HDateTime::new(fixed, "UTC"))
                    }
                    None => Kind::Null,
                };
                row.set("lastSync", last_sync_kind);
                row
            })
            .collect();
        HGrid::from_parts(HDict::new(), status_columns(), rows)
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
        vec![HCol::new("name"), HCol::new("result"), HCol::new("ok")],
        rows,
    );

    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/federation/sync/{name}
///
/// Triggers `sync_one(name)` on a single connector. Returns a grid with
/// `name`, `result` (entity count or error string), and `ok` (Bool).
pub async fn handle_sync_one(
    req: HttpRequest,
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<HttpResponse, HaystackError> {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let name = path.into_inner();
    let result = state.federation.sync_one(&name).await;

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

    let grid = HGrid::from_parts(
        HDict::new(),
        vec![HCol::new("name"), HCol::new("result"), HCol::new("ok")],
        vec![row],
    );

    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}
