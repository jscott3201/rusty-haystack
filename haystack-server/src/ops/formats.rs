//! The `formats` op — list supported MIME types.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::Kind;

use crate::content;
use crate::state::AppState;

/// GET /api/formats — returns a grid listing supported MIME formats.
pub async fn handle(req: HttpRequest, _state: web::Data<AppState>) -> HttpResponse {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let formats = vec![
        ("text/zinc", "Zinc"),
        ("application/json", "JSON (Haystack 4)"),
        ("text/trio", "Trio"),
        ("application/json;v=3", "JSON (Haystack 3)"),
    ];

    let cols = vec![HCol::new("mime"), HCol::new("receive"), HCol::new("send")];
    let rows: Vec<HDict> = formats
        .into_iter()
        .map(|(mime, _label)| {
            let mut row = HDict::new();
            row.set("mime", Kind::Str(mime.to_string()));
            row.set("receive", Kind::Marker);
            row.set("send", Kind::Marker);
            row
        })
        .collect();

    let grid = HGrid::from_parts(HDict::new(), cols, rows);
    match content::encode_response_grid(&grid, accept) {
        Ok((body, ct)) => HttpResponse::Ok().content_type(ct).body(body),
        Err(e) => {
            log::error!("Failed to encode formats grid: {e}");
            HttpResponse::InternalServerError().body("encoding error")
        }
    }
}
