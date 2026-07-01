//! The `changes` op — return graph changelog entries since a given version.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::graph::changelog::DiffOp;
use haystack_core::kinds::Kind;

use crate::content;
use crate::error::HaystackError;
use crate::state::SharedState;

/// Maximum number of change rows returned in a single response.
const MAX_CHANGE_ROWS: usize = 10_000;

/// POST /api/changes
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
            return Ok(([(axum::http::header::CONTENT_TYPE, ct)], encoded).into_response());
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
        return Ok(([(axum::http::header::CONTENT_TYPE, ct)], encoded).into_response());
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

    Ok(([(axum::http::header::CONTENT_TYPE, ct)], encoded).into_response())
}
