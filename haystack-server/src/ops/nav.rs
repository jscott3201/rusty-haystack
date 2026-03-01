//! The `nav` op — navigate a project for entity discovery.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// POST /api/nav
///
/// Request may have a `navId` column:
/// - No navId or empty: return top-level sites
/// - navId is a site ref: return children (equips/spaces with siteRef)
/// - navId is an equip ref: return children (points with equipRef)
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

    // Try to decode request grid; if body is empty, treat as no navId
    let nav_id: Option<String> = if body.trim().is_empty() {
        None
    } else {
        let request_grid = content::decode_request_grid(&body, content_type)
            .map_err(|e| HaystackError::bad_request(format!("failed to decode request: {e}")))?;

        request_grid.row(0).and_then(|row| match row.get("navId") {
            Some(Kind::Str(s)) if !s.is_empty() => Some(s.clone()),
            Some(Kind::Ref(r)) => Some(r.val.clone()),
            _ => None,
        })
    };

    let result_grid = match nav_id {
        None => {
            // Return top-level sites
            let sites = state
                .graph
                .read_all("site", 0)
                .map_err(|e| HaystackError::internal(format!("graph error: {e}")))?;

            build_nav_grid(sites)
        }
        Some(ref parent_id) => {
            // Check if the parent entity exists
            let parent = state.graph.get(parent_id);
            if parent.is_none() {
                return Err(HaystackError::not_found(format!(
                    "entity not found: {parent_id}"
                )));
            }

            // Find children that reference this parent
            let child_refs = state.graph.refs_to(parent_id, None);
            let mut children = Vec::new();
            for ref_val in child_refs {
                if let Some(entity) = state.graph.get(&ref_val) {
                    children.push(entity);
                }
            }
            build_nav_grid(children)
        }
    };

    let (encoded, ct) = content::encode_response_grid(&result_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// Build a navigation grid from a list of entity dicts.
fn build_nav_grid(entities: Vec<HDict>) -> HGrid {
    if entities.is_empty() {
        return HGrid::new();
    }

    let cols = vec![HCol::new("id"), HCol::new("dis"), HCol::new("navId")];
    let rows: Vec<HDict> = entities
        .into_iter()
        .map(|entity| {
            let mut row = HDict::new();
            if let Some(id_ref) = entity.id() {
                row.set("id", Kind::Ref(id_ref.clone()));
                row.set("navId", Kind::Str(id_ref.val.clone()));
            }
            if let Some(dis) = entity.dis() {
                row.set("dis", Kind::Str(dis.to_string()));
            }
            row
        })
        .collect();

    HGrid::from_parts(HDict::new(), cols, rows)
}
