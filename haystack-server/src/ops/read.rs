//! The `read` op — read entities by filter or by id.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::filter::{matches, parse_filter};
use haystack_core::kinds::Kind;

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

    // Read from local graph.
    let local_grid = state
        .graph
        .read_filter(filter, limit)
        .map_err(|e| HaystackError::bad_request(format!("filter error: {e}")))?;

    let mut results: Vec<HDict> = local_grid.rows;

    // Merge federated entities if we have not yet hit the limit.
    if results.len() < effective_limit {
        let federated = state.federation.all_cached_entities();
        if !federated.is_empty() {
            let ast = parse_filter(filter)
                .map_err(|e| HaystackError::bad_request(format!("filter error: {e}")))?;

            for entity in federated {
                if results.len() >= effective_limit {
                    break;
                }
                if matches(&ast, &entity, None) {
                    results.push(entity);
                }
            }
        }
    }

    if results.is_empty() {
        return Ok(HGrid::new());
    }

    // Build column set from all result entities.
    let mut col_set: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entity in &results {
        for name in entity.tag_names() {
            if seen.insert(name.to_string()) {
                col_set.push(name.to_string());
            }
        }
    }
    col_set.sort();
    let cols: Vec<HCol> = col_set.iter().map(|n| HCol::new(n.as_str())).collect();

    Ok(HGrid::from_parts(HDict::new(), cols, results))
}

/// Read entities by ID references.
fn read_by_id(request_grid: &HGrid, state: &AppState) -> Result<HGrid, HaystackError> {
    let mut results: Vec<HDict> = Vec::new();
    let mut col_set: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for row in request_grid.rows.iter() {
        let ref_val = match row.get("id") {
            Some(Kind::Ref(r)) => &r.val,
            _ => continue,
        };

        if let Some(entity) = state.graph.get(ref_val) {
            for name in entity.tag_names() {
                if seen.insert(name.to_string()) {
                    col_set.push(name.to_string());
                }
            }
            results.push(entity);
        } else {
            // Entity not found: add empty row with just the id marker
            // (per Haystack spec, missing entities are still returned)
            let mut missing = HDict::new();
            missing.set(
                "id",
                Kind::Ref(haystack_core::kinds::HRef::from_val(ref_val.as_str())),
            );
            if seen.insert("id".to_string()) {
                col_set.push("id".to_string());
            }
            results.push(missing);
        }
    }

    if results.is_empty() {
        return Ok(HGrid::new());
    }

    col_set.sort();
    let cols: Vec<HCol> = col_set.iter().map(|n| HCol::new(n.as_str())).collect();
    Ok(HGrid::from_parts(HDict::new(), cols, results))
}
