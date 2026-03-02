//! The `changes` op — return graph changelog entries since a given version.
//!
//! Used by federation connectors for incremental delta sync instead of full
//! `read("*")` on every interval.
//!
//! # Request Grid Columns
//!
//! | Column    | Kind   | Description                                     |
//! |-----------|--------|-------------------------------------------------|
//! | `version` | Number | Graph version to query changes since (0 for all) |
//!
//! # Response
//!
//! Grid meta contains `curVer` (Number) — the current graph version.
//!
//! | Column    | Kind   | Description                                    |
//! |-----------|--------|------------------------------------------------|
//! | `version` | Number | Version after this mutation                    |
//! | `op`      | Str    | `"add"`, `"update"`, or `"remove"`             |
//! | `ref`     | Str    | Entity ref value                               |
//! | `entity`  | Dict   | Entity data (present for add/update only)      |
//!
//! # Errors
//!
//! - **400 Bad Request** — request grid decode failure.
//! - **500 Internal Server Error** — encoding error.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::graph::changelog::DiffOp;
use haystack_core::kinds::Kind;

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// Maximum number of change rows returned in a single response.
const MAX_CHANGE_ROWS: usize = 10_000;

/// POST /api/changes
///
/// Request grid should have a single row with a `version` column (Number).
/// Returns a grid of changelog entries since that version, each with:
/// - `version`: Number — the version after the mutation
/// - `op`: Str — "add", "update", or "remove"
/// - `ref`: Str — the entity ref value
/// - `entity`: the entity dict (for add/update; absent for remove)
///
/// Also includes `curVer` in the response meta with the current graph version,
/// so the caller can store it for the next delta sync.
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

    let since_version = request_grid
        .row(0)
        .and_then(|row| row.get("version"))
        .and_then(|k| {
            if let Kind::Number(n) = k {
                Some(n.val as u64)
            } else {
                None
            }
        })
        .unwrap_or(0);

    let current_version = state.graph.version();
    let diffs = match state.graph.changes_since(since_version) {
        Ok(d) => d,
        Err(gap) => {
            // Subscriber fell behind. Return an error grid with the gap info
            // so the caller knows to do a full resync.
            let mut err_meta = HDict::new();
            err_meta.set(
                "curVer",
                Kind::Number(haystack_core::kinds::Number::unitless(
                    current_version as f64,
                )),
            );
            err_meta.set(
                "err",
                Kind::Str(format!(
                    "changelog gap: requested version {}, floor is {}",
                    gap.subscriber_version, gap.floor_version
                )),
            );
            let grid = HGrid::from_parts(err_meta, Vec::new(), Vec::new());
            let (encoded, ct) = content::encode_response_grid(&grid, accept)
                .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
            return Ok(HttpResponse::Ok().content_type(ct).body(encoded));
        }
    };

    let mut meta = HDict::new();
    meta.set(
        "curVer",
        Kind::Number(haystack_core::kinds::Number::unitless(
            current_version as f64,
        )),
    );

    if diffs.is_empty() {
        let grid = HGrid::from_parts(meta, Vec::new(), Vec::new());
        let (encoded, ct) = content::encode_response_grid(&grid, accept)
            .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
        return Ok(HttpResponse::Ok().content_type(ct).body(encoded));
    }

    let cols = vec![
        HCol::new("version"),
        HCol::new("op"),
        HCol::new("ref"),
        HCol::new("ts"),
        HCol::new("entity"),
    ];

    let mut rows: Vec<HDict> = diffs
        .iter()
        .map(|diff| {
            let mut row = HDict::new();
            row.set(
                "version",
                Kind::Number(haystack_core::kinds::Number::unitless(diff.version as f64)),
            );
            row.set(
                "op",
                Kind::Str(match diff.op {
                    DiffOp::Add => "add".to_string(),
                    DiffOp::Update => "update".to_string(),
                    DiffOp::Remove => "remove".to_string(),
                }),
            );
            row.set("ref", Kind::Str(diff.ref_val.clone()));
            row.set(
                "ts",
                Kind::Number(haystack_core::kinds::Number::unitless(
                    diff.timestamp as f64,
                )),
            );
            // Include entity data: full entity for Add, changed_tags for Update.
            if let Some(entity) = &diff.new {
                row.set("entity", Kind::Dict(Box::new(entity.clone())));
            } else if let Some(changed) = &diff.changed_tags {
                row.set("entity", Kind::Dict(Box::new(changed.clone())));
            }
            row
        })
        .collect();

    let truncated = rows.len() > MAX_CHANGE_ROWS;
    if truncated {
        rows.truncate(MAX_CHANGE_ROWS);
        meta.set("truncated", Kind::Marker);
        meta.set(
            "maxRows",
            Kind::Number(haystack_core::kinds::Number::unitless(
                MAX_CHANGE_ROWS as f64,
            )),
        );
    }

    let grid = HGrid::from_parts(meta, cols, rows);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}
