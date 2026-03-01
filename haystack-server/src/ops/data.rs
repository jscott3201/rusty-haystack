//! The `export` and `import` ops — bulk data import/export.

use actix_web::{web, HttpRequest, HttpResponse};

use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{Kind, Number};

use crate::content;
use crate::error::HaystackError;
use crate::state::AppState;

/// POST /api/export
///
/// Reads all entities from the graph and returns them as a grid.
/// The Accept header determines the response encoding format.
pub async fn handle_export(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let grid = state
        .graph
        .read(|g| g.to_grid(""))
        .map_err(|e| HaystackError::internal(format!("export failed: {e}")))?;

    let (encoded, ct) = content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/import
///
/// Decodes the request body as a grid and adds/updates each row as an entity.
/// Each row must have an `id` tag with a Ref value.
/// Existing entities are updated; new entities are added.
/// Returns a grid with the count of imported entities.
pub async fn handle_import(
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

    let mut count: usize = 0;

    for row in &request_grid.rows {
        let ref_val = match row.id() {
            Some(r) => r.val.clone(),
            None => {
                // Skip rows without a valid Ref id
                continue;
            }
        };

        if state.graph.contains(&ref_val) {
            // Update existing entity
            state
                .graph
                .update(&ref_val, row.clone())
                .map_err(|e| HaystackError::internal(format!("update failed for {ref_val}: {e}")))?;
        } else {
            // Add new entity
            state
                .graph
                .add(row.clone())
                .map_err(|e| HaystackError::internal(format!("add failed for {ref_val}: {e}")))?;
        }

        count += 1;
    }

    // Build response grid with count
    let mut row = HDict::new();
    row.set("count", Kind::Number(Number::new(count as f64, None)));

    let cols = vec![HCol::new("count")];
    let result_grid = HGrid::from_parts(HDict::new(), cols, vec![row]);

    let (encoded, ct) = content::encode_response_grid(&result_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

#[cfg(test)]
mod tests {
    use actix_web::test as actix_test;
    use actix_web::web;
    use actix_web::App;

    use haystack_core::data::{HCol, HDict, HGrid};
    use haystack_core::graph::{EntityGraph, SharedGraph};
    use haystack_core::kinds::{HRef, Kind, Number};

    use crate::actions::ActionRegistry;
    use crate::auth::AuthManager;
    use crate::his_store::HisStore;
    use crate::state::AppState;
    use crate::ws::WatchManager;

    fn test_app_state() -> web::Data<AppState> {
        web::Data::new(AppState {
            graph: SharedGraph::new(EntityGraph::new()),
            namespace: parking_lot::RwLock::new(haystack_core::ontology::DefNamespace::new()),
            auth: AuthManager::empty(),
            watches: WatchManager::new(),
            actions: ActionRegistry::new(),
            his: HisStore::new(),
            started_at: std::time::Instant::now(),
            federation: crate::federation::Federation::new(),
        })
    }

    fn make_site(id: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str(format!("Site {id}")));
        d.set(
            "area",
            Kind::Number(Number::new(4500.0, Some("ft\u{00b2}".into()))),
        );
        d
    }

    fn make_equip(id: &str, site_ref: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d.set("equip", Kind::Marker);
        d.set("dis", Kind::Str(format!("Equip {id}")));
        d.set("siteRef", Kind::Ref(HRef::from_val(site_ref)));
        d
    }

    fn encode_grid_zinc(grid: &HGrid) -> String {
        let codec = haystack_core::codecs::codec_for("text/zinc").unwrap();
        codec.encode_grid(grid).unwrap()
    }

    fn decode_grid_zinc(body: &str) -> HGrid {
        let codec = haystack_core::codecs::codec_for("text/zinc").unwrap();
        codec.decode_grid(body).unwrap()
    }

    #[actix_web::test]
    async fn export_empty_graph() {
        let state = test_app_state();
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/export", web::post().to(super::handle_export)),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/api/export")
            .insert_header(("Accept", "text/zinc"))
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body = actix_test::read_body(resp).await;
        let body_str = std::str::from_utf8(&body).unwrap();
        let grid = decode_grid_zinc(body_str);
        assert!(grid.is_empty());
    }

    #[actix_web::test]
    async fn import_entities() {
        let state = test_app_state();
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/import", web::post().to(super::handle_import)),
        )
        .await;

        // Build a grid with two entities
        let site = make_site("site-1");
        let equip = make_equip("equip-1", "site-1");
        let cols = vec![
            HCol::new("area"),
            HCol::new("dis"),
            HCol::new("equip"),
            HCol::new("id"),
            HCol::new("site"),
            HCol::new("siteRef"),
        ];
        let import_grid = HGrid::from_parts(HDict::new(), cols, vec![site, equip]);
        let body = encode_grid_zinc(&import_grid);

        let req = actix_test::TestRequest::post()
            .uri("/api/import")
            .insert_header(("Content-Type", "text/zinc"))
            .insert_header(("Accept", "text/zinc"))
            .set_payload(body)
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let resp_body = actix_test::read_body(resp).await;
        let resp_str = std::str::from_utf8(&resp_body).unwrap();
        let result_grid = decode_grid_zinc(resp_str);
        assert_eq!(result_grid.len(), 1);

        // Verify count
        let count_row = result_grid.row(0).unwrap();
        match count_row.get("count") {
            Some(Kind::Number(n)) => assert_eq!(n.val as usize, 2),
            other => panic!("expected Number count, got {other:?}"),
        }

        // Verify graph has entities
        assert_eq!(state.graph.len(), 2);
        assert!(state.graph.contains("site-1"));
        assert!(state.graph.contains("equip-1"));
    }

    #[actix_web::test]
    async fn import_updates_existing_entities() {
        let state = test_app_state();

        // Pre-populate the graph with a site
        state.graph.add(make_site("site-1")).unwrap();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/import", web::post().to(super::handle_import)),
        )
        .await;

        // Import an updated version of site-1
        let mut updated_site = HDict::new();
        updated_site.set("id", Kind::Ref(HRef::from_val("site-1")));
        updated_site.set("site", Kind::Marker);
        updated_site.set("dis", Kind::Str("Updated Site".into()));
        updated_site.set(
            "area",
            Kind::Number(Number::new(9000.0, Some("ft\u{00b2}".into()))),
        );

        let cols = vec![
            HCol::new("area"),
            HCol::new("dis"),
            HCol::new("id"),
            HCol::new("site"),
        ];
        let import_grid = HGrid::from_parts(HDict::new(), cols, vec![updated_site]);
        let body = encode_grid_zinc(&import_grid);

        let req = actix_test::TestRequest::post()
            .uri("/api/import")
            .insert_header(("Content-Type", "text/zinc"))
            .insert_header(("Accept", "text/zinc"))
            .set_payload(body)
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        // Still only 1 entity (updated, not duplicated)
        assert_eq!(state.graph.len(), 1);

        // Verify the entity was updated
        let entity = state.graph.get("site-1").unwrap();
        assert_eq!(
            entity.get("dis"),
            Some(&Kind::Str("Updated Site".into()))
        );
    }

    #[actix_web::test]
    async fn import_then_export_roundtrip() {
        let state = test_app_state();
        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/import", web::post().to(super::handle_import))
                .route("/api/export", web::post().to(super::handle_export)),
        )
        .await;

        // Import two entities
        let site = make_site("site-1");
        let equip = make_equip("equip-1", "site-1");
        let cols = vec![
            HCol::new("area"),
            HCol::new("dis"),
            HCol::new("equip"),
            HCol::new("id"),
            HCol::new("site"),
            HCol::new("siteRef"),
        ];
        let import_grid = HGrid::from_parts(HDict::new(), cols, vec![site, equip]);
        let body = encode_grid_zinc(&import_grid);

        let import_req = actix_test::TestRequest::post()
            .uri("/api/import")
            .insert_header(("Content-Type", "text/zinc"))
            .insert_header(("Accept", "text/zinc"))
            .set_payload(body)
            .to_request();

        let import_resp = actix_test::call_service(&app, import_req).await;
        assert_eq!(import_resp.status(), 200);

        // Now export
        let export_req = actix_test::TestRequest::post()
            .uri("/api/export")
            .insert_header(("Accept", "text/zinc"))
            .to_request();

        let export_resp = actix_test::call_service(&app, export_req).await;
        assert_eq!(export_resp.status(), 200);

        let export_body = actix_test::read_body(export_resp).await;
        let export_str = std::str::from_utf8(&export_body).unwrap();
        let exported_grid = decode_grid_zinc(export_str);

        // Should have 2 rows
        assert_eq!(exported_grid.len(), 2);

        // Verify the exported grid has the expected columns
        assert!(exported_grid.col("id").is_some());
        assert!(exported_grid.col("dis").is_some());

        // Verify we can find both entities by checking id refs in the rows
        let mut ids: Vec<String> = exported_grid
            .rows
            .iter()
            .filter_map(|r| r.id().map(|r| r.val.clone()))
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["equip-1", "site-1"]);
    }
}
