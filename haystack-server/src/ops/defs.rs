//! The `defs` and `libs` ops — query the definition namespace.
//!
//! # defs (`POST /api/defs`)
//!
//! Returns definitions from the ontology namespace, optionally filtered
//! by a substring match on the def symbol.
//!
//! ## Request Grid Columns
//!
//! | Column   | Kind | Description                                    |
//! |----------|------|------------------------------------------------|
//! | `filter` | Str  | *(optional)* Substring to match against def symbols |
//!
//! An empty body returns all definitions.
//!
//! ## Response Grid Columns
//!
//! | Column | Kind   | Description              |
//! |--------|--------|--------------------------|
//! | `def`  | Symbol | Definition symbol        |
//! | `lib`  | Symbol | Owning library name      |
//! | `doc`  | Str    | Documentation string     |
//!
//! Rows are sorted by `def` symbol.
//!
//! # libs (`POST /api/libs`)
//!
//! Returns all loaded ontology libraries.
//!
//! ## Response Grid Columns
//!
//! | Column    | Kind | Description       |
//! |-----------|------|-------------------|
//! | `name`    | Str  | Library name      |
//! | `version` | Str  | Library version   |
//!
//! Rows are sorted by `name`.
//!
//! # Errors
//!
//! - **400 Bad Request** — request grid decode failure.
//! - **500 Internal Server Error** — encoding error.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// POST /api/defs
///
/// Request may have a `filter` column with a filter string.
/// Returns matching def records as a grid.
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

    let ns = state.namespace.read();

    // Parse optional filter from request
    let filter: Option<String> = if body.trim().is_empty() {
        None
    } else {
        let request_grid = content::decode_request_grid(&body, content_type)
            .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

        request_grid.row(0).and_then(|row| match row.get("filter") {
            Some(Kind::Str(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        })
    };

    // Build def grid
    let cols = vec![HCol::new("def"), HCol::new("lib"), HCol::new("doc")];

    let defs = ns.defs();
    let mut rows: Vec<HDict> = Vec::new();

    for (symbol, def) in defs {
        // If a filter is provided, only include defs whose symbol contains the filter
        if let Some(ref f) = filter
            && !symbol.contains(f.as_str())
        {
            continue;
        }

        let mut row = HDict::new();
        row.set(
            "def",
            Kind::Symbol(haystack_core::kinds::Symbol::new(symbol)),
        );
        row.set(
            "lib",
            Kind::Symbol(haystack_core::kinds::Symbol::new(&def.lib)),
        );
        row.set("doc", Kind::Str(def.doc.clone()));
        rows.push(row);
    }

    // Sort by symbol for deterministic output
    rows.sort_by(|a, b| {
        let a_name = match a.get("def") {
            Some(Kind::Symbol(s)) => s.val(),
            _ => "",
        };
        let b_name = match b.get("def") {
            Some(Kind::Symbol(s)) => s.val(),
            _ => "",
        };
        a_name.cmp(b_name)
    });

    let grid = HGrid::from_parts(HDict::new(), cols, rows);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/libs — returns a grid of library names.
pub async fn handle_libs(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let ns = state.namespace.read();

    let cols = vec![HCol::new("name"), HCol::new("version")];
    let libs = ns.libs();
    let mut rows: Vec<HDict> = libs
        .values()
        .map(|lib| {
            let mut row = HDict::new();
            row.set("name", Kind::Str(lib.name.clone()));
            row.set("version", Kind::Str(lib.version.clone()));
            row
        })
        .collect();

    // Sort by name for deterministic output
    rows.sort_by(|a, b| {
        let a_name = match a.get("name") {
            Some(Kind::Str(s)) => s.as_str(),
            _ => "",
        };
        let b_name = match b.get("name") {
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
