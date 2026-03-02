//! The `read` op — read entities by filter or by id.
//!
//! # Overview
//!
//! `POST /api/read` returns entity records matching a filter expression or a
//! list of explicit IDs. Results include local entities and, when federation is
//! enabled, matching entities from remote connectors.
//!
//! # Request Grid Columns
//!
//! Two mutually-exclusive request forms:
//!
//! **Filter read** — first row contains:
//!
//! | Column   | Kind   | Description                                      |
//! |----------|--------|--------------------------------------------------|
//! | `filter` | Str    | Haystack filter expression (e.g. `"site"`, `"point and siteRef==@s1"`) |
//! | `limit`  | Number | *(optional)* Max entities to return (0 = no limit) |
//!
//! **ID read** — each row contains:
//!
//! | Column | Kind | Description      |
//! |--------|------|------------------|
//! | `id`   | Ref  | Entity reference |
//!
//! # Response Grid Columns
//!
//! The response grid has one column per unique tag across all matched entities.
//! For ID reads, rows for unknown IDs appear as stubs with only the `id` column.
//!
//! # Errors
//!
//! - **400 Bad Request** — missing `filter`/`id` column, invalid filter syntax, or
//!   request decode failure.
//! - **500 Internal Server Error** — graph or encoding error.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{HRef, Kind};

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// POST /api/read
///
/// Request grid may have:
/// - A `filter` column with a filter expression string, and optional `limit` column
/// - An `id` column with Ref values for reading specific entities
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

    let result_grid = if request_grid.col("id").is_some() {
        // Read by ID
        read_by_id(&request_grid, &state)
    } else if request_grid.col("filter").is_some() {
        // Read by filter
        read_by_filter(&request_grid, &state)
    } else {
        Err(HaystackError::bad_request(
            "request must have 'id' or 'filter' column",
        ))
    }?;

    let (encoded, ct) = content::encode_response_grid(&result_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// Read entities by filter expression.
///
/// After reading from the local graph, also includes matching entities from
/// any federated remote connectors.
fn read_by_filter(request_grid: &HGrid, state: &AppState) -> Result<HGrid, HaystackError> {
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

    // Read from local graph.
    let mut results: Vec<HDict> = if is_wildcard {
        // Wildcard: return all entities from the graph.
        state.graph.all_entities()
    } else {
        let local_grid = state
            .graph
            .read_filter(filter, limit)
            .map_err(|e| HaystackError::bad_request(format!("filter error: {e}")))?;
        local_grid.rows
    };

    // Apply limit to local results.
    if results.len() > effective_limit {
        results.truncate(effective_limit);
    }

    // Merge federated entities if we have not yet hit the limit.
    if results.len() < effective_limit {
        let remaining = effective_limit - results.len();
        if is_wildcard {
            // Wildcard: include all federated entities up to the limit.
            let federated = state.federation.all_cached_entities();
            for entity in federated {
                if results.len() >= effective_limit {
                    break;
                }
                results.push(entity);
            }
        } else {
            // Use bitmap-accelerated filter on federated caches.
            match state.federation.filter_cached_entities(filter, remaining) {
                Ok(federated) => results.extend(federated),
                Err(e) => {
                    return Err(HaystackError::bad_request(format!(
                        "federation filter error: {e}"
                    )));
                }
            }
        }
    }

    if results.is_empty() {
        return Ok(HGrid::new());
    }

    // Build column set from all result entities using borrowed &str.
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

    Ok(HGrid::from_parts(HDict::new(), cols, results))
}

/// Read entities by ID references.
///
/// First pass: resolve IDs from local graph.
/// Second pass: batch-fetch remaining IDs from federation connectors
/// (grouped by connector for O(1) indexed lookup per ID).
fn read_by_id(request_grid: &HGrid, state: &AppState) -> Result<HGrid, HaystackError> {
    let mut results: Vec<HDict> = Vec::new();
    let mut unknown_ids: Vec<String> = Vec::new();

    // First pass: resolve from local graph, collect unknowns.
    for row in request_grid.rows.iter() {
        let ref_val = match row.get("id") {
            Some(Kind::Ref(r)) => &r.val,
            _ => continue,
        };

        if let Some(entity) = state.graph.get(ref_val) {
            results.push(entity);
        } else {
            unknown_ids.push(ref_val.clone());
        }
    }

    // Second pass: batch fetch unknowns from federation.
    if !unknown_ids.is_empty() && !state.federation.connectors.is_empty() {
        let id_refs: Vec<&str> = unknown_ids.iter().map(|s| s.as_str()).collect();
        let (found, still_missing) = state.federation.batch_read_by_id(id_refs);
        results.extend(found);

        // Add missing stubs for IDs not found anywhere.
        for id in still_missing {
            let mut missing = HDict::new();
            missing.set("id", Kind::Ref(HRef::from_val(id.as_str())));
            results.push(missing);
        }
    } else {
        // No federation — add missing stubs directly.
        for id in unknown_ids {
            let mut missing = HDict::new();
            missing.set("id", Kind::Ref(HRef::from_val(id.as_str())));
            results.push(missing);
        }
    }

    if results.is_empty() {
        return Ok(HGrid::new());
    }

    // Build column set using borrowed &str from entities.
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
    Ok(HGrid::from_parts(HDict::new(), cols, results))
}
