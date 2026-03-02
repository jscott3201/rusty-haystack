//! Integration tests for federation-enhanced endpoints.
//!
//! These tests use Actix Web's test infrastructure with pre-populated
//! federation caches — no real remote servers are involved.

use actix_web::test as actix_test;
use actix_web::{App, web};

use haystack_core::codecs;
use haystack_core::data::{HCol, HDict, HGrid};
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::kinds::{HRef, Kind, Number};

use haystack_server::Federation;
use haystack_server::actions::ActionRegistry;
use haystack_server::auth::AuthManager;
use haystack_server::connector::ConnectorConfig;
use haystack_server::his_store::HisStore;
use haystack_server::ops;
use haystack_server::state::AppState;
use haystack_server::ws::WatchManager;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn encode_grid_zinc(grid: &HGrid) -> String {
    let codec = codecs::codec_for("text/zinc").unwrap();
    codec.encode_grid(grid).unwrap()
}

fn decode_grid_zinc(s: &str) -> HGrid {
    let codec = codecs::codec_for("text/zinc").unwrap();
    codec.decode_grid(s).unwrap()
}

/// Build an `AppState` with an empty graph and an empty federation.
fn test_app_state() -> web::Data<AppState> {
    web::Data::new(AppState {
        graph: SharedGraph::new(EntityGraph::new()),
        namespace: parking_lot::RwLock::new(haystack_core::ontology::DefNamespace::new()),
        auth: AuthManager::empty(),
        watches: WatchManager::new(),
        actions: ActionRegistry::new(),
        his: HisStore::new(),
        started_at: std::time::Instant::now(),
        federation: Federation::new(),
    })
}

/// Build an `AppState` with a federation containing the given connectors
/// (already configured). The caller is responsible for populating caches
/// via `state.federation.connectors[i].update_cache(...)`.
fn test_app_state_with_federation(federation: Federation) -> web::Data<AppState> {
    web::Data::new(AppState {
        graph: SharedGraph::new(EntityGraph::new()),
        namespace: parking_lot::RwLock::new(haystack_core::ontology::DefNamespace::new()),
        auth: AuthManager::empty(),
        watches: WatchManager::new(),
        actions: ActionRegistry::new(),
        his: HisStore::new(),
        started_at: std::time::Instant::now(),
        federation,
    })
}

/// Create a site entity with the given id.
fn make_site(id: &str) -> HDict {
    let mut d = HDict::new();
    d.set("id", Kind::Ref(HRef::from_val(id)));
    d.set("site", Kind::Marker);
    d.set("dis", Kind::Str(format!("Site {id}")));
    d.set(
        "area",
        Kind::Number(Number::new(4500.0, Some("ft\u{b2}".into()))),
    );
    d
}

/// Shorthand for creating a `ConnectorConfig`.
fn connector_config(name: &str, url: &str, prefix: Option<&str>) -> ConnectorConfig {
    ConnectorConfig {
        name: name.to_string(),
        url: url.to_string(),
        username: "fed".to_string(),
        password: "pass".to_string(),
        id_prefix: prefix.map(|s| s.to_string()),
        ws_url: None,
        sync_interval_secs: None,
        client_cert: None,
        client_key: None,
        ca_cert: None,
        domain: None,
    }
}

/// Build a read-by-filter request grid.
fn read_filter_grid(filter: &str) -> HGrid {
    let mut row = HDict::new();
    row.set("filter", Kind::Str(filter.to_string()));
    HGrid::from_parts(HDict::new(), vec![HCol::new("filter")], vec![row])
}

/// Build a read-by-id request grid for a single id.
fn read_id_grid(id: &str) -> HGrid {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    HGrid::from_parts(HDict::new(), vec![HCol::new("id")], vec![row])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Test 1: Federation status endpoint with connectors and populated cache.
#[actix_web::test]
async fn federation_status_with_connectors() {
    let mut fed = Federation::new();
    let _ = fed.add(connector_config(
        "Remote A",
        "http://remote-a:8080/api",
        Some("ra-"),
    ));
    let _ = fed.add(connector_config(
        "Remote B",
        "http://remote-b:8080/api",
        Some("rb-"),
    ));

    // Populate cache for Remote A with 2 entities.
    fed.connectors[0].update_cache(vec![make_site("ra-site-1"), make_site("ra-site-2")]);
    // Populate cache for Remote B with 1 entity.
    fed.connectors[1].update_cache(vec![make_site("rb-site-1")]);

    let state = test_app_state_with_federation(fed);

    let app = actix_test::init_service(App::new().app_data(state.clone()).route(
        "/api/federation/status",
        web::get().to(ops::federation::handle_status),
    ))
    .await;

    let req = actix_test::TestRequest::get()
        .uri("/api/federation/status")
        .insert_header(("Accept", "text/zinc"))
        .to_request();

    let resp = actix_test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body = actix_test::read_body(resp).await;
    let body_str = std::str::from_utf8(&body).expect("valid utf-8");
    let grid = decode_grid_zinc(body_str);

    // Should have 5 columns.
    assert!(grid.col("name").is_some(), "missing 'name' column");
    assert!(
        grid.col("entityCount").is_some(),
        "missing 'entityCount' column"
    );
    assert!(
        grid.col("transport").is_some(),
        "missing 'transport' column"
    );
    assert!(
        grid.col("connected").is_some(),
        "missing 'connected' column"
    );
    assert!(grid.col("lastSync").is_some(), "missing 'lastSync' column");

    // Should have 2 rows (one per connector).
    assert_eq!(grid.rows.len(), 2);

    // Verify Remote A row.
    let row_a = &grid.rows[0];
    assert_eq!(row_a.get("name"), Some(&Kind::Str("Remote A".to_string())));
    assert_eq!(
        row_a.get("entityCount"),
        Some(&Kind::Number(Number::unitless(2.0)))
    );

    // Verify Remote B row.
    let row_b = &grid.rows[1];
    assert_eq!(row_b.get("name"), Some(&Kind::Str("Remote B".to_string())));
    assert_eq!(
        row_b.get("entityCount"),
        Some(&Kind::Number(Number::unitless(1.0)))
    );
}

/// Test 2: Status endpoint with no connectors returns empty grid with correct columns.
#[actix_web::test]
async fn federation_status_no_connectors() {
    let state = test_app_state();

    let app = actix_test::init_service(App::new().app_data(state.clone()).route(
        "/api/federation/status",
        web::get().to(ops::federation::handle_status),
    ))
    .await;

    let req = actix_test::TestRequest::get()
        .uri("/api/federation/status")
        .insert_header(("Accept", "text/zinc"))
        .to_request();

    let resp = actix_test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body = actix_test::read_body(resp).await;
    let body_str = std::str::from_utf8(&body).expect("valid utf-8");
    let grid = decode_grid_zinc(body_str);

    // Should have the standard columns even with no connectors.
    assert!(grid.col("name").is_some());
    assert!(grid.col("entityCount").is_some());
    assert!(grid.col("transport").is_some());
    assert!(grid.col("connected").is_some());
    assert!(grid.col("lastSync").is_some());

    // No rows.
    assert!(grid.rows.is_empty());
}

/// Test 3: Filter read merges local and federated entities.
#[actix_web::test]
async fn filter_read_merges_federated_entities() {
    let mut fed = Federation::new();
    let _ = fed.add(connector_config(
        "Remote A",
        "http://remote-a:8080/api",
        Some("ra-"),
    ));

    // Populate federation cache with a remote site.
    fed.connectors[0].update_cache(vec![make_site("ra-site-1")]);

    let state = test_app_state_with_federation(fed);

    // Add a local site entity to the graph.
    let local_site = make_site("local-site-1");
    state.graph.add(local_site).expect("add local site");

    let app = actix_test::init_service(
        App::new()
            .app_data(state.clone())
            .route("/api/read", web::post().to(ops::read::handle)),
    )
    .await;

    let request_grid = read_filter_grid("site");
    let payload = encode_grid_zinc(&request_grid);

    let req = actix_test::TestRequest::post()
        .uri("/api/read")
        .insert_header(("Content-Type", "text/zinc"))
        .insert_header(("Accept", "text/zinc"))
        .set_payload(payload)
        .to_request();

    let resp = actix_test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body = actix_test::read_body(resp).await;
    let body_str = std::str::from_utf8(&body).expect("valid utf-8");
    let grid = decode_grid_zinc(body_str);

    // Should have both local and federated entities.
    assert_eq!(
        grid.rows.len(),
        2,
        "expected 2 rows (1 local + 1 federated)"
    );

    // Collect the IDs from the result.
    let ids: Vec<String> = grid
        .rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Kind::Ref(r)) => Some(r.val.clone()),
            _ => None,
        })
        .collect();

    assert!(
        ids.contains(&"local-site-1".to_string()),
        "missing local entity"
    );
    assert!(
        ids.contains(&"ra-site-1".to_string()),
        "missing federated entity"
    );
}

/// Test 4: ID read returns a federated entity by its prefixed ID.
#[actix_web::test]
async fn id_read_returns_federated_entity() {
    let mut fed = Federation::new();
    let _ = fed.add(connector_config(
        "Remote A",
        "http://remote-a:8080/api",
        Some("ra-"),
    ));

    // Populate cache with a known entity.
    fed.connectors[0].update_cache(vec![make_site("ra-site-1")]);

    let state = test_app_state_with_federation(fed);

    let app = actix_test::init_service(
        App::new()
            .app_data(state.clone())
            .route("/api/read", web::post().to(ops::read::handle)),
    )
    .await;

    let request_grid = read_id_grid("ra-site-1");
    let payload = encode_grid_zinc(&request_grid);

    let req = actix_test::TestRequest::post()
        .uri("/api/read")
        .insert_header(("Content-Type", "text/zinc"))
        .insert_header(("Accept", "text/zinc"))
        .set_payload(payload)
        .to_request();

    let resp = actix_test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body = actix_test::read_body(resp).await;
    let body_str = std::str::from_utf8(&body).expect("valid utf-8");
    let grid = decode_grid_zinc(body_str);

    assert_eq!(grid.rows.len(), 1);

    let row = &grid.rows[0];
    match row.get("id") {
        Some(Kind::Ref(r)) => assert_eq!(r.val, "ra-site-1"),
        other => panic!("expected Ref id, got {other:?}"),
    }
    // Should also have the site marker and dis.
    assert_eq!(row.get("site"), Some(&Kind::Marker));
    assert_eq!(
        row.get("dis"),
        Some(&Kind::Str("Site ra-site-1".to_string()))
    );
}

/// Test 5: ID read prefers local entity over federated entity with same ID.
#[actix_web::test]
async fn id_read_prefers_local_over_federated() {
    let mut fed = Federation::new();
    let _ = fed.add(connector_config(
        "Remote A",
        "http://remote-a:8080/api",
        Some("ra-"),
    ));

    // Put entity with id "shared-1" in federation cache.
    let mut fed_entity = HDict::new();
    fed_entity.set("id", Kind::Ref(HRef::from_val("shared-1")));
    fed_entity.set("site", Kind::Marker);
    fed_entity.set("dis", Kind::Str("Federated Version".to_string()));
    fed.connectors[0].update_cache(vec![fed_entity]);

    let state = test_app_state_with_federation(fed);

    // Add entity with same id to local graph.
    let mut local_entity = HDict::new();
    local_entity.set("id", Kind::Ref(HRef::from_val("shared-1")));
    local_entity.set("site", Kind::Marker);
    local_entity.set("dis", Kind::Str("Local Version".to_string()));
    state.graph.add(local_entity).expect("add local entity");

    let app = actix_test::init_service(
        App::new()
            .app_data(state.clone())
            .route("/api/read", web::post().to(ops::read::handle)),
    )
    .await;

    let request_grid = read_id_grid("shared-1");
    let payload = encode_grid_zinc(&request_grid);

    let req = actix_test::TestRequest::post()
        .uri("/api/read")
        .insert_header(("Content-Type", "text/zinc"))
        .insert_header(("Accept", "text/zinc"))
        .set_payload(payload)
        .to_request();

    let resp = actix_test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body = actix_test::read_body(resp).await;
    let body_str = std::str::from_utf8(&body).expect("valid utf-8");
    let grid = decode_grid_zinc(body_str);

    assert_eq!(grid.rows.len(), 1);

    // Local version should win.
    let row = &grid.rows[0];
    assert_eq!(
        row.get("dis"),
        Some(&Kind::Str("Local Version".to_string())),
        "local entity should take precedence over federated"
    );
}

/// Test 6: Per-connector sync with non-existent name returns error.
#[actix_web::test]
async fn sync_one_nonexistent_connector() {
    let mut fed = Federation::new();
    let _ = fed.add(connector_config(
        "Remote A",
        "http://remote-a:8080/api",
        Some("ra-"),
    ));

    let state = test_app_state_with_federation(fed);

    let app = actix_test::init_service(App::new().app_data(state.clone()).route(
        "/api/federation/sync/{name}",
        web::post().to(ops::federation::handle_sync_one),
    ))
    .await;

    let req = actix_test::TestRequest::post()
        .uri("/api/federation/sync/NonExistent")
        .insert_header(("Accept", "text/zinc"))
        .to_request();

    let resp = actix_test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);

    let body = actix_test::read_body(resp).await;
    let body_str = std::str::from_utf8(&body).expect("valid utf-8");
    let grid = decode_grid_zinc(body_str);

    // Should have 1 row with ok=false.
    assert_eq!(grid.rows.len(), 1);
    let row = &grid.rows[0];
    assert_eq!(row.get("name"), Some(&Kind::Str("NonExistent".to_string())));
    assert_eq!(row.get("ok"), Some(&Kind::Bool(false)));

    // The result should contain an error message about connector not found.
    match row.get("result") {
        Some(Kind::Str(s)) => {
            assert!(
                s.contains("connector not found"),
                "expected 'connector not found' in error, got: {s}"
            );
        }
        other => panic!("expected Str result, got {other:?}"),
    }
}
