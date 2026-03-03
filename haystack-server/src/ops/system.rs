//! System administration endpoints (admin-only).
//!
//! All routes under `/api/system/` require the "admin" permission, enforced by
//! the auth middleware in `app.rs`.
//!
//! # Endpoints
//!
//! ## `GET /api/system/status`
//!
//! No request grid. Response columns:
//!
//! | Column        | Kind   | Description                           |
//! |---------------|--------|---------------------------------------|
//! | `uptime`      | Number | Seconds since server start (unit `s`) |
//! | `entityCount` | Number | Number of entities in the graph       |
//! | `watchCount`  | Number | Number of active watch subscriptions  |
//!
//! ## `POST /api/system/backup`
//!
//! No request grid. Returns all entities as a JSON-encoded grid
//! (`Content-Type: application/json`), regardless of `Accept` header.
//!
//! ## `POST /api/system/restore`
//!
//! Request body: JSON grid of entities (each row must have an `id` Ref).
//! Existing entities are updated; new entities are added.
//! Response: single-row grid with `count` (Number) of entities loaded.
//!
//! # Errors
//!
//! - **400 Bad Request** — invalid JSON body (restore only).
//! - **500 Internal Server Error** — graph, codec, or encoding error.

use actix_web::{HttpRequest, HttpResponse, web};

use haystack_core::codecs::codec_for;
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::kinds::{Kind, Number};

use crate::error::HaystackError;
use crate::state::AppState;

/// GET /api/system/status
///
/// Returns a single-row grid with server status information:
/// - `uptime`: seconds since the server started (with "s" unit)
/// - `entityCount`: number of entities in the graph
/// - `watchCount`: number of active watch subscriptions
pub async fn handle_status(
    req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let uptime_secs = state.started_at.elapsed().as_secs_f64();
    let entity_count = state.graph.len();
    let watch_count = state.watches.len();

    let mut row = HDict::new();
    row.set(
        "uptime",
        Kind::Number(Number::new(uptime_secs, Some("s".into()))),
    );
    row.set(
        "entityCount",
        Kind::Number(Number::unitless(entity_count as f64)),
    );
    row.set(
        "watchCount",
        Kind::Number(Number::unitless(watch_count as f64)),
    );

    let cols = vec![
        HCol::new("uptime"),
        HCol::new("entityCount"),
        HCol::new("watchCount"),
    ];
    let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);

    let (encoded, ct) = crate::content::encode_response_grid(&grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

/// POST /api/system/backup
///
/// Exports all entities from the graph as JSON. The response body is a raw JSON
/// string with Content-Type `application/json`, regardless of the Accept header.
pub async fn handle_backup(
    _req: HttpRequest,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let grid = state
        .graph
        .read(|g| g.to_grid(""))
        .map_err(|e| HaystackError::internal(format!("backup failed: {e}")))?;

    let codec = codec_for("application/json")
        .ok_or_else(|| HaystackError::internal("JSON codec not available"))?;

    let json = codec
        .encode_grid(&grid)
        .map_err(|e| HaystackError::internal(format!("JSON encoding error: {e}")))?;

    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(json))
}

/// POST /api/system/restore
///
/// Imports entities from a JSON request body. Each row in the decoded grid is
/// added to (or updated in) the entity graph. Returns a single-row grid with
/// the count of entities loaded.
pub async fn handle_restore(
    req: HttpRequest,
    body: String,
    state: web::Data<AppState>,
) -> Result<HttpResponse, HaystackError> {
    let accept = req
        .headers()
        .get("Accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let codec = codec_for("application/json")
        .ok_or_else(|| HaystackError::internal("JSON codec not available"))?;

    let grid = codec
        .decode_grid(&body)
        .map_err(|e| HaystackError::bad_request(format!("failed to decode JSON body: {e}")))?;

    let mut count: usize = 0;

    for row in &grid.rows {
        let ref_val = match row.id() {
            Some(r) => r.val.clone(),
            None => continue, // skip rows without a valid Ref id
        };

        if state.graph.contains(&ref_val) {
            state.graph.update(&ref_val, row.clone()).map_err(|e| {
                HaystackError::internal(format!("update failed for {ref_val}: {e}"))
            })?;
        } else {
            state
                .graph
                .add(row.clone())
                .map_err(|e| HaystackError::internal(format!("add failed for {ref_val}: {e}")))?;
        }

        count += 1;
    }

    let mut result_row = HDict::new();
    result_row.set("count", Kind::Number(Number::unitless(count as f64)));

    let cols = vec![HCol::new("count")];
    let result_grid = HGrid::from_parts(HDict::new(), cols, vec![result_row]);

    let (encoded, ct) = crate::content::encode_response_grid(&result_grid, accept)
        .map_err(|e| HaystackError::internal(format!("encoding error: {e}")))?;

    Ok(HttpResponse::Ok().content_type(ct).body(encoded))
}

#[cfg(test)]
mod tests {
    use actix_web::App;
    use actix_web::test as actix_test;
    use actix_web::web;

    use haystack_core::codecs::codec_for;
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
            his: Box::new(HisStore::new()),
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

    fn decode_grid_zinc(body: &str) -> HGrid {
        let codec = codec_for("text/zinc").unwrap();
        codec.decode_grid(body).unwrap()
    }

    fn decode_grid_json(body: &str) -> HGrid {
        let codec = codec_for("application/json").unwrap();
        codec.decode_grid(body).unwrap()
    }

    // ── Status ──

    #[actix_web::test]
    async fn status_returns_server_info() {
        let state = test_app_state();

        // Pre-populate graph with two entities
        state.graph.add(make_site("site-1")).unwrap();
        state.graph.add(make_site("site-2")).unwrap();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/status", web::get().to(super::handle_status)),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/api/system/status")
            .insert_header(("Accept", "text/zinc"))
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body = actix_test::read_body(resp).await;
        let body_str = std::str::from_utf8(&body).unwrap();
        let grid = decode_grid_zinc(body_str);
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();

        // Check entityCount
        match row.get("entityCount") {
            Some(Kind::Number(n)) => assert_eq!(n.val as usize, 2),
            other => panic!("expected Number entityCount, got {other:?}"),
        }

        // Check watchCount
        match row.get("watchCount") {
            Some(Kind::Number(n)) => assert_eq!(n.val as usize, 0),
            other => panic!("expected Number watchCount, got {other:?}"),
        }

        // Check uptime is a non-negative number
        match row.get("uptime") {
            Some(Kind::Number(n)) => assert!(n.val >= 0.0),
            other => panic!("expected Number uptime, got {other:?}"),
        }
    }

    #[actix_web::test]
    async fn status_empty_graph() {
        let state = test_app_state();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/status", web::get().to(super::handle_status)),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/api/system/status")
            .insert_header(("Accept", "text/zinc"))
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body = actix_test::read_body(resp).await;
        let body_str = std::str::from_utf8(&body).unwrap();
        let grid = decode_grid_zinc(body_str);
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();
        match row.get("entityCount") {
            Some(Kind::Number(n)) => assert_eq!(n.val as usize, 0),
            other => panic!("expected Number entityCount=0, got {other:?}"),
        }
    }

    #[actix_web::test]
    async fn status_reflects_watch_count() {
        let state = test_app_state();

        // Add two watches
        state
            .watches
            .subscribe("admin", vec!["site-1".into()], 0)
            .unwrap();
        state
            .watches
            .subscribe("admin", vec!["site-2".into()], 0)
            .unwrap();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/status", web::get().to(super::handle_status)),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/api/system/status")
            .insert_header(("Accept", "text/zinc"))
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body = actix_test::read_body(resp).await;
        let body_str = std::str::from_utf8(&body).unwrap();
        let grid = decode_grid_zinc(body_str);
        let row = grid.row(0).unwrap();

        match row.get("watchCount") {
            Some(Kind::Number(n)) => assert_eq!(n.val as usize, 2),
            other => panic!("expected watchCount=2, got {other:?}"),
        }
    }

    // ── Backup ──

    #[actix_web::test]
    async fn backup_empty_graph() {
        let state = test_app_state();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/backup", web::post().to(super::handle_backup)),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/api/system/backup")
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            ct.contains("application/json"),
            "expected JSON content-type, got {ct}"
        );

        let body = actix_test::read_body(resp).await;
        let body_str = std::str::from_utf8(&body).unwrap();
        let grid = decode_grid_json(body_str);
        assert!(grid.is_empty());
    }

    #[actix_web::test]
    async fn backup_with_entities() {
        let state = test_app_state();
        state.graph.add(make_site("site-1")).unwrap();
        state.graph.add(make_site("site-2")).unwrap();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/backup", web::post().to(super::handle_backup)),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/api/system/backup")
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body = actix_test::read_body(resp).await;
        let body_str = std::str::from_utf8(&body).unwrap();
        let grid = decode_grid_json(body_str);
        assert_eq!(grid.len(), 2);
    }

    // ── Restore ──

    #[actix_web::test]
    async fn restore_adds_entities() {
        let state = test_app_state();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/restore", web::post().to(super::handle_restore)),
        )
        .await;

        // Build a JSON-encoded grid with two entities
        let site1 = make_site("site-1");
        let site2 = make_site("site-2");
        let cols = vec![
            HCol::new("area"),
            HCol::new("dis"),
            HCol::new("id"),
            HCol::new("site"),
        ];
        let grid = HGrid::from_parts(HDict::new(), cols, vec![site1, site2]);

        let codec = codec_for("application/json").unwrap();
        let json_body = codec.encode_grid(&grid).unwrap();

        let req = actix_test::TestRequest::post()
            .uri("/api/system/restore")
            .insert_header(("Content-Type", "application/json"))
            .insert_header(("Accept", "text/zinc"))
            .set_payload(json_body)
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body = actix_test::read_body(resp).await;
        let body_str = std::str::from_utf8(&body).unwrap();
        let result_grid = decode_grid_zinc(body_str);
        assert_eq!(result_grid.len(), 1);

        // Verify count
        let count_row = result_grid.row(0).unwrap();
        match count_row.get("count") {
            Some(Kind::Number(n)) => assert_eq!(n.val as usize, 2),
            other => panic!("expected Number count=2, got {other:?}"),
        }

        // Verify graph has entities
        assert_eq!(state.graph.len(), 2);
        assert!(state.graph.contains("site-1"));
        assert!(state.graph.contains("site-2"));
    }

    #[actix_web::test]
    async fn restore_updates_existing_entities() {
        let state = test_app_state();

        // Pre-populate with one entity
        state.graph.add(make_site("site-1")).unwrap();
        assert_eq!(state.graph.len(), 1);

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/restore", web::post().to(super::handle_restore)),
        )
        .await;

        // Build an updated version
        let mut updated = HDict::new();
        updated.set("id", Kind::Ref(HRef::from_val("site-1")));
        updated.set("site", Kind::Marker);
        updated.set("dis", Kind::Str("Updated Site".into()));
        updated.set(
            "area",
            Kind::Number(Number::new(9000.0, Some("ft\u{00b2}".into()))),
        );

        let cols = vec![
            HCol::new("area"),
            HCol::new("dis"),
            HCol::new("id"),
            HCol::new("site"),
        ];
        let grid = HGrid::from_parts(HDict::new(), cols, vec![updated]);

        let codec = codec_for("application/json").unwrap();
        let json_body = codec.encode_grid(&grid).unwrap();

        let req = actix_test::TestRequest::post()
            .uri("/api/system/restore")
            .insert_header(("Content-Type", "application/json"))
            .insert_header(("Accept", "text/zinc"))
            .set_payload(json_body)
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        // Still only 1 entity
        assert_eq!(state.graph.len(), 1);

        // Verify it was updated
        let entity = state.graph.get("site-1").unwrap();
        assert_eq!(entity.get("dis"), Some(&Kind::Str("Updated Site".into())));
    }

    #[actix_web::test]
    async fn backup_then_restore_roundtrip() {
        let state = test_app_state();
        state.graph.add(make_site("site-1")).unwrap();
        state.graph.add(make_site("site-2")).unwrap();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/backup", web::post().to(super::handle_backup))
                .route("/api/system/restore", web::post().to(super::handle_restore)),
        )
        .await;

        // Backup
        let backup_req = actix_test::TestRequest::post()
            .uri("/api/system/backup")
            .to_request();
        let backup_resp = actix_test::call_service(&app, backup_req).await;
        assert_eq!(backup_resp.status(), 200);

        let backup_body = actix_test::read_body(backup_resp).await;
        let backup_str = std::str::from_utf8(&backup_body).unwrap().to_string();

        // Restore into a fresh state
        let state2 = test_app_state();
        let app2 = actix_test::init_service(
            App::new()
                .app_data(state2.clone())
                .route("/api/system/restore", web::post().to(super::handle_restore)),
        )
        .await;

        let restore_req = actix_test::TestRequest::post()
            .uri("/api/system/restore")
            .insert_header(("Content-Type", "application/json"))
            .insert_header(("Accept", "text/zinc"))
            .set_payload(backup_str)
            .to_request();

        let restore_resp = actix_test::call_service(&app2, restore_req).await;
        assert_eq!(restore_resp.status(), 200);

        // Verify the new state has the same entities
        assert_eq!(state2.graph.len(), 2);
        assert!(state2.graph.contains("site-1"));
        assert!(state2.graph.contains("site-2"));
    }

    #[actix_web::test]
    async fn restore_invalid_json_returns_400() {
        let state = test_app_state();

        let app = actix_test::init_service(
            App::new()
                .app_data(state.clone())
                .route("/api/system/restore", web::post().to(super::handle_restore)),
        )
        .await;

        let req = actix_test::TestRequest::post()
            .uri("/api/system/restore")
            .insert_header(("Content-Type", "application/json"))
            .set_payload("{not valid json}")
            .to_request();

        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 400);
    }
}
