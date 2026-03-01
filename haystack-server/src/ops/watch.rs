//! Watch ops — subscribe, poll, and unsubscribe for entity changes.

use actix_web::{web, HttpMessage, HttpRequest, HttpResponse};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::auth::AuthUser;
use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// POST /api/watchSub — subscribe to entity changes.
///
/// Request grid has `id` column with Ref values for entities to watch.
/// May also have a `watchId` column to add IDs to an existing watch.
/// Returns the current state of watched entities with `watchId` in grid meta.
pub async fn handle_sub(
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

    let username = req
        .extensions()
        .get::<AuthUser>()
        .map(|u| u.username.clone())
        .unwrap_or_else(|| "anonymous".to_string());

    // Collect entity IDs
    let ids: Vec<String> = request_grid
        .rows
        .iter()
        .filter_map(|row| match row.get("id") {
            Some(Kind::Ref(r)) => Some(r.val.clone()),
            _ => None,
        })
        .collect();

    if ids.is_empty() {
        return Err(HaystackError::bad_request(
            "request must have 'id' column with Ref values",
        ));
    }

    // Check for existing watchId in grid meta (add to existing watch)
    let existing_watch_id = request_grid
        .meta
        .get("watchId")
        .and_then(|v| match v {
            Kind::Str(s) => Some(s.clone()),
            _ => None,
        });

    let watch_id = if let Some(ref wid) = existing_watch_id {
        if state.watches.add_ids(wid, &username, ids.clone()) {
            wid.clone()
        } else {
            return Err(HaystackError::not_found(format!(
                "watch not found: {wid}"
            )));
        }
    } else {
        let graph_version = state.graph.version();
        state
            .watches
            .subscribe(&username, ids.clone(), graph_version)
            .map_err(|e| HaystackError::bad_request(e))?
    };

    // Return current state of watched entities
    let mut rows: Vec<HDict> = Vec::new();
    let mut col_set: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for id in &ids {
        if let Some(entity) = state.graph.get(id) {
            for name in entity.tag_names() {
                if seen.insert(name.to_string()) {
                    col_set.push(name.to_string());
                }
            }
            rows.push(entity);
        }
    }

    col_set.sort();
    let cols: Vec<HCol> = col_set.iter().map(|n| HCol::new(n.as_str())).collect();

    let mut meta = HDict::new();
    meta.set("watchId", Kind::Str(watch_id));

    let grid = HGrid::from_parts(meta, cols, rows);
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/watchPoll — poll for changes since last poll.
///
/// Request grid has `watchId` in the grid meta.
/// Returns changed entities since last poll.
pub async fn handle_poll(
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

    let username = req
        .extensions()
        .get::<AuthUser>()
        .map(|u| u.username.clone())
        .unwrap_or_else(|| "anonymous".to_string());

    let watch_id = request_grid
        .meta
        .get("watchId")
        .and_then(|v| match v {
            Kind::Str(s) => Some(s.clone()),
            _ => None,
        })
        .ok_or_else(|| HaystackError::bad_request("request meta must have 'watchId'"))?;

    let changed = state
        .watches
        .poll(&watch_id, &username, &state.graph)
        .ok_or_else(|| HaystackError::not_found(format!("watch not found: {watch_id}")))?;

    if changed.is_empty() {
        return Ok(respond_grid(&HGrid::new(), accept)?);
    }

    // Build grid from changed entities
    let mut col_set: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for entity in &changed {
        for name in entity.tag_names() {
            if seen.insert(name.to_string()) {
                col_set.push(name.to_string());
            }
        }
    }
    col_set.sort();
    let cols: Vec<HCol> = col_set.iter().map(|n| HCol::new(n.as_str())).collect();

    let grid = HGrid::from_parts(HDict::new(), cols, changed);
    respond_grid(&grid, accept)
}

/// POST /api/watchUnsub — unsubscribe from a watch.
///
/// Request grid has `watchId` in the grid meta.
/// Request rows may have `id` column to remove specific IDs from the watch.
/// If no IDs are present in the request rows, the entire watch is removed.
pub async fn handle_unsub(
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

    let username = req
        .extensions()
        .get::<AuthUser>()
        .map(|u| u.username.clone())
        .unwrap_or_else(|| "anonymous".to_string());

    let watch_id = request_grid
        .meta
        .get("watchId")
        .and_then(|v| match v {
            Kind::Str(s) => Some(s.clone()),
            _ => None,
        })
        .ok_or_else(|| HaystackError::bad_request("request meta must have 'watchId'"))?;

    // Collect entity IDs from request rows for selective removal
    let ids: Vec<String> = request_grid
        .rows
        .iter()
        .filter_map(|row| match row.get("id") {
            Some(Kind::Ref(r)) => Some(r.val.clone()),
            _ => None,
        })
        .collect();

    if ids.is_empty() {
        // No IDs specified — remove the entire watch
        if !state.watches.unsubscribe(&watch_id, &username) {
            return Err(HaystackError::not_found(format!(
                "watch not found: {watch_id}"
            )));
        }
    } else {
        // Selective removal — remove only the specified IDs from the watch
        if !state.watches.remove_ids(&watch_id, &username, &ids) {
            return Err(HaystackError::not_found(format!(
                "watch not found: {watch_id}"
            )));
        }
    }

    respond_grid(&HGrid::new(), accept)
}

fn respond_grid(grid: &HGrid, accept: &str) -> Result<HttpResponse, HaystackError> {
    let (encoded, ct) = content::encode_response_grid(grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;
    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}
