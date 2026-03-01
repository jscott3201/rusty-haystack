//! The `pointWrite` op — write a value to a writable point.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// POST /api/pointWrite
///
/// Request grid has `id`, `level`, and `val` columns.
/// Writes the value to the entity in the graph as a simple curVal update.
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

    for row in request_grid.rows.iter() {
        let ref_val = match row.get("id") {
            Some(Kind::Ref(r)) => r.val.clone(),
            _ => continue,
        };

        let level = match row.get("level") {
            Some(Kind::Number(n)) => n.val as u32,
            _ => 17, // Default level
        };

        if !(1..=17).contains(&level) {
            return Err(HaystackError::bad_request(format!(
                "level must be between 1 and 17, got {level}"
            )));
        }

        // Check that the target entity has the `writable` marker tag
        let entity = state
            .graph
            .get(&ref_val)
            .ok_or_else(|| HaystackError::not_found(format!("entity not found: {ref_val}")))?;
        if !entity.has("writable") {
            return Err(HaystackError::bad_request(format!(
                "entity '{ref_val}' is not writable"
            )));
        }

        // Get the value to write
        if let Some(val) = row.get("val") {
            // Simple implementation: update the entity's curVal tag at the given level
            let mut changes = HDict::new();
            changes.set("curVal", val.clone());
            changes.set(
                "writeLevel",
                Kind::Number(haystack_core::kinds::Number::unitless(level as f64)),
            );
            state
                .graph
                .update(&ref_val, changes)
                .map_err(|e| HaystackError::bad_request(format!("write failed: {e}")))?;
        }
    }

    // Return empty grid on success
    let grid = HGrid::new();
    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}
