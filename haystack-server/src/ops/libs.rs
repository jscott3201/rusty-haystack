//! Library and spec management endpoints.
//!
//! # Overview
//!
//! These endpoints manage Xeto-based ontology libraries at runtime:
//! listing specs, inspecting individual specs, loading/unloading libraries,
//! exporting library source, and validating entities against the ontology.
//!
//! # Endpoints
//!
//! - `POST /api/specs` — list specs, optionally filtered by `lib` (Str).
//!   Response: `qname`, `name`, `lib`, `base`, `doc`, `abstract`.
//! - `POST /api/spec` — get single spec by `qname` (Str).
//!   Response: `qname`, `name`, `lib`, `base`, `doc`, `abstract`, `slots`.
//! - `POST /api/loadLib` — load library from `name` (Str) and `source` (Str).
//!   Response: `loaded`, `specs`.
//! - `POST /api/unloadLib` — unload library by `name` (Str).
//!   Response: `unloaded`.
//! - `POST /api/exportLib` — export library by `name` (Str) to Xeto source.
//!   Response: `name`, `source`.
//! - `POST /api/validate` — validate entity rows against the ontology.
//!   Response: `entity`, `issueType`, `detail`.
//!
//! # Errors
//!
//! - **400 Bad Request** — missing required columns, spec not found, load/unload
//!   error, or request decode failure.
//! - **500 Internal Server Error** — encoding error.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// POST /api/specs — list specs, optionally filtered by library.
///
/// Request grid may have a `lib` (Str) column to filter by library name.
/// Returns a grid of specs sorted by `qname`, with columns:
/// `qname`, `name`, `lib`, `base`, `doc`, and `abstract` (Marker).
pub async fn handle_specs(
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

    let ns = state.namespace.read();

    // Parse optional lib filter from request
    let lib_filter: Option<String> = if body.trim().is_empty() {
        None
    } else {
        let grid = content::decode_request_grid(&body, content_type)
            .map_err(|e| HaystackError::bad_request(format!("decode error: {e}")))?;
        grid.row(0).and_then(|row| match row.get("lib") {
            Some(Kind::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        })
    };

    let specs = ns.specs(lib_filter.as_deref());
    let cols = vec![
        HCol::new("qname"),
        HCol::new("name"),
        HCol::new("lib"),
        HCol::new("base"),
        HCol::new("doc"),
        HCol::new("abstract"),
    ];

    let mut rows: Vec<HDict> = specs
        .iter()
        .map(|spec| {
            let mut row = HDict::new();
            row.set("qname", Kind::Str(spec.qname.clone()));
            row.set("name", Kind::Str(spec.name.clone()));
            row.set("lib", Kind::Str(spec.lib.clone()));
            if let Some(ref base) = spec.base {
                row.set("base", Kind::Str(base.clone()));
            }
            row.set("doc", Kind::Str(spec.doc.clone()));
            if spec.is_abstract {
                row.set("abstract", Kind::Marker);
            }
            row
        })
        .collect();

    rows.sort_by(|a, b| {
        let a_name = match a.get("qname") {
            Some(Kind::Str(s)) => s.as_str(),
            _ => "",
        };
        let b_name = match b.get("qname") {
            Some(Kind::Str(s)) => s.as_str(),
            _ => "",
        };
        a_name.cmp(b_name)
    });

    let grid = HGrid::from_parts(HDict::new(), cols, rows);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/spec — get a single spec by qualified name.
///
/// Request grid must have a `qname` (Str) column with the fully-qualified
/// spec name. Returns a single-row grid with `qname`, `name`, `lib`,
/// `base`, `doc`, `abstract`, and `slots` (comma-separated slot names).
pub async fn handle_spec(
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

    let grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("decode error: {e}")))?;
    let row = grid
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;
    let qname = match row.get("qname") {
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("qname column required")),
    };

    let ns = state.namespace.read();
    let spec = ns
        .get_spec(&qname)
        .ok_or_else(|| HaystackError::bad_request(format!("spec '{}' not found", qname)))?;

    let cols = vec![
        HCol::new("qname"),
        HCol::new("name"),
        HCol::new("lib"),
        HCol::new("base"),
        HCol::new("doc"),
        HCol::new("abstract"),
        HCol::new("slots"),
    ];

    let mut result = HDict::new();
    result.set("qname", Kind::Str(spec.qname.clone()));
    result.set("name", Kind::Str(spec.name.clone()));
    result.set("lib", Kind::Str(spec.lib.clone()));
    if let Some(ref base) = spec.base {
        result.set("base", Kind::Str(base.clone()));
    }
    result.set("doc", Kind::Str(spec.doc.clone()));
    if spec.is_abstract {
        result.set("abstract", Kind::Marker);
    }
    // Encode slots as a comma-separated string for simplicity
    let slot_names: Vec<String> = spec.slots.iter().map(|s| s.name.clone()).collect();
    result.set("slots", Kind::Str(slot_names.join(",")));

    let grid = HGrid::from_parts(HDict::new(), cols, vec![result]);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/loadLib — load a library from Xeto source text.
///
/// Request grid must have `name` (Str) and `source` (Str) columns.
/// Returns a single-row grid with `loaded` (library name) and `specs`
/// (comma-separated list of loaded spec qualified names).
pub async fn handle_load_lib(
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

    let grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("decode error: {e}")))?;
    let row = grid
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;
    let name = match row.get("name") {
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("name column required")),
    };
    let source = match row.get("source") {
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("source column required")),
    };

    let mut ns = state.namespace.write();
    let qnames = ns
        .load_xeto_str(&source, &name)
        .map_err(|e| HaystackError::bad_request(format!("load error: {e}")))?;

    let cols = vec![HCol::new("loaded"), HCol::new("specs")];
    let mut result = HDict::new();
    result.set("loaded", Kind::Str(name));
    result.set("specs", Kind::Str(qnames.join(",")));
    let grid = HGrid::from_parts(HDict::new(), cols, vec![result]);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/unloadLib — unload a library by name.
///
/// Request grid must have a `name` (Str) column.
/// Returns a single-row grid with `unloaded` (the library name).
pub async fn handle_unload_lib(
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

    let grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("decode error: {e}")))?;
    let row = grid
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;
    let name = match row.get("name") {
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("name column required")),
    };

    let mut ns = state.namespace.write();
    ns.unload_lib(&name).map_err(HaystackError::bad_request)?;

    let cols = vec![HCol::new("unloaded")];
    let mut result = HDict::new();
    result.set("unloaded", Kind::Str(name));
    let grid = HGrid::from_parts(HDict::new(), cols, vec![result]);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/exportLib — export a library to Xeto source text.
///
/// Request grid must have a `name` (Str) column.
/// Returns a single-row grid with `name` and `source` (Xeto text).
pub async fn handle_export_lib(
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

    let grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("decode error: {e}")))?;
    let row = grid
        .row(0)
        .ok_or_else(|| HaystackError::bad_request("request grid has no rows"))?;
    let name = match row.get("name") {
        Some(Kind::Str(s)) => s.clone(),
        _ => return Err(HaystackError::bad_request("name column required")),
    };

    let ns = state.namespace.read();
    let xeto_text = ns
        .export_lib_xeto(&name)
        .map_err(HaystackError::bad_request)?;

    let cols = vec![HCol::new("name"), HCol::new("source")];
    let mut result = HDict::new();
    result.set("name", Kind::Str(name));
    result.set("source", Kind::Str(xeto_text));
    let grid = HGrid::from_parts(HDict::new(), cols, vec![result]);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/validate — validate entities against the ontology.
///
/// Each row in the request grid is an entity dict to validate.
/// Returns a grid of validation issues with columns: `entity` (Str),
/// `issueType` (Str), and `detail` (Str). An empty grid means all
/// entities passed validation.
pub async fn handle_validate(
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

    let grid = content::decode_request_grid(&body, content_type)
        .map_err(|e| HaystackError::bad_request(format!("decode error: {e}")))?;

    let ns = state.namespace.read();

    let cols = vec![
        HCol::new("entity"),
        HCol::new("issueType"),
        HCol::new("detail"),
    ];
    let mut rows: Vec<HDict> = Vec::new();

    for entity in &grid.rows {
        let issues = ns.validate_entity(entity);
        for issue in issues {
            let mut row = HDict::new();
            if let Some(ref e) = issue.entity {
                row.set("entity", Kind::Str(e.clone()));
            }
            row.set("issueType", Kind::Str(issue.issue_type));
            row.set("detail", Kind::Str(issue.detail));
            rows.push(row);
        }
    }

    let grid = HGrid::from_parts(HDict::new(), cols, rows);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}
