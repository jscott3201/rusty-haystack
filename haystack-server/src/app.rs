//! Server builder and startup.

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::middleware::from_fn;
use actix_web::{App, HttpMessage, HttpServer, web};

use haystack_core::auth::{AuthHeader, parse_auth_header};
use haystack_core::graph::SharedGraph;
use haystack_core::ontology::DefNamespace;

use crate::actions::ActionRegistry;
use crate::auth::AuthManager;
use crate::federation::Federation;
use crate::his_store::HisStore;
use crate::ops;
use crate::state::AppState;
use crate::ws;
use crate::ws::WatchManager;

/// Builder for the Haystack HTTP server.
pub struct HaystackServer {
    graph: SharedGraph,
    namespace: DefNamespace,
    auth_manager: AuthManager,
    actions: ActionRegistry,
    federation: Federation,
    port: u16,
    host: String,
}

impl HaystackServer {
    /// Create a new server with the given entity graph.
    pub fn new(graph: SharedGraph) -> Self {
        Self {
            graph,
            namespace: DefNamespace::new(),
            auth_manager: AuthManager::empty(),
            actions: ActionRegistry::new(),
            federation: Federation::new(),
            port: 8080,
            host: "127.0.0.1".to_string(),
        }
    }

    /// Set the ontology namespace for def/spec operations.
    pub fn with_namespace(mut self, ns: DefNamespace) -> Self {
        self.namespace = ns;
        self
    }

    /// Set the authentication manager.
    pub fn with_auth(mut self, auth: AuthManager) -> Self {
        self.auth_manager = auth;
        self
    }

    /// Set the port to listen on (default: 8080).
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Set the host to bind to (default: "127.0.0.1").
    pub fn host(mut self, host: &str) -> Self {
        self.host = host.to_string();
        self
    }

    /// Set the action registry for the `invokeAction` op.
    pub fn with_actions(mut self, actions: ActionRegistry) -> Self {
        self.actions = actions;
        self
    }

    /// Set the federation manager for remote connector queries.
    pub fn with_federation(mut self, federation: Federation) -> Self {
        self.federation = federation;
        self
    }

    /// Start the HTTP server. This blocks until the server is stopped.
    pub async fn run(self) -> std::io::Result<()> {
        let state = web::Data::new(AppState {
            graph: self.graph,
            namespace: parking_lot::RwLock::new(self.namespace),
            auth: self.auth_manager,
            watches: WatchManager::new(),
            actions: self.actions,
            his: HisStore::new(),
            started_at: std::time::Instant::now(),
            federation: self.federation,
        });

        log::info!("Starting haystack-server on {}:{}", self.host, self.port);

        HttpServer::new(move || {
            App::new()
                .app_data(state.clone())
                .app_data(actix_web::web::PayloadConfig::default().limit(2 * 1024 * 1024))
                .wrap(from_fn(auth_middleware))
                .configure(ops::configure)
                .route("/api/ws", web::get().to(ws::ws_handler))
        })
        .bind((self.host.as_str(), self.port))?
        .run()
        .await
    }
}

/// Determine the required permission for a given request path.
///
/// Returns `None` if the path does not require permission checking
/// (e.g. public endpoints handled before auth).
fn required_permission(path: &str) -> Option<&'static str> {
    // System / admin endpoints
    if path.starts_with("/api/system/") {
        return Some("admin");
    }

    // Write operations
    match path {
        "/api/pointWrite"
        | "/api/hisWrite"
        | "/api/invokeAction"
        | "/api/loadLib"
        | "/api/unloadLib"
        | "/api/import"
        | "/api/federation/sync" => return Some("write"),
        _ => {}
    }

    // Everything else that reaches here is a read-level operation:
    // /api/about, /api/read, /api/nav, /api/defs, /api/libs,
    // /api/hisRead, /api/watchSub, /api/watchPoll, /api/watchUnsub,
    // /api/close, /api/ops, /api/formats, etc.
    Some("read")
}

/// Authentication middleware.
///
/// - GET /api/about: pass through (about handles auth itself for SCRAM)
/// - GET /api/ops, GET /api/formats: pass through (public info)
/// - All other endpoints: require BEARER token if auth is enabled,
///   then check the user has the required permission for that route.
async fn auth_middleware(
    req: ServiceRequest,
    next: actix_web::middleware::Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let path = req.path().to_string();
    let method = req.method().clone();

    // Allow about endpoint through (it handles auth itself for SCRAM handshake)
    if path == "/api/about" {
        return next.call(req).await;
    }

    // Allow ops and formats through without auth (public endpoints)
    if (path == "/api/ops" || path == "/api/formats") && method == actix_web::http::Method::GET {
        return next.call(req).await;
    }

    // Check if auth is enabled
    let auth_enabled = {
        let state = req
            .app_data::<web::Data<AppState>>()
            .expect("AppState must be configured");
        state.auth.is_enabled()
    };

    if !auth_enabled {
        // Auth is not enabled, pass through
        return next.call(req).await;
    }

    // Extract and validate BEARER token
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    match auth_header {
        Some(header) => {
            match parse_auth_header(&header) {
                Ok(AuthHeader::Bearer { auth_token }) => {
                    let user = {
                        let state = req
                            .app_data::<web::Data<AppState>>()
                            .expect("AppState must be configured");
                        state.auth.validate_token(&auth_token)
                    };

                    match user {
                        Some(auth_user) => {
                            // Check permission for the requested path
                            if let Some(required) = required_permission(&path)
                                && !AuthManager::check_permission(&auth_user, required)
                            {
                                return Err(crate::error::HaystackError::forbidden(format!(
                                    "user '{}' lacks '{}' permission",
                                    auth_user.username, required
                                ))
                                .into());
                            }

                            // Inject AuthUser into request extensions
                            req.extensions_mut().insert(auth_user);
                            next.call(req).await
                        }
                        None => Err(crate::error::HaystackError::new(
                            "invalid or expired auth token",
                            actix_web::http::StatusCode::UNAUTHORIZED,
                        )
                        .into()),
                    }
                }
                _ => Err(crate::error::HaystackError::new(
                    "BEARER token required",
                    actix_web::http::StatusCode::UNAUTHORIZED,
                )
                .into()),
            }
        }
        None => Err(crate::error::HaystackError::new(
            "Authorization header required",
            actix_web::http::StatusCode::UNAUTHORIZED,
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_permission_read_ops() {
        assert_eq!(required_permission("/api/read"), Some("read"));
        assert_eq!(required_permission("/api/nav"), Some("read"));
        assert_eq!(required_permission("/api/defs"), Some("read"));
        assert_eq!(required_permission("/api/libs"), Some("read"));
        assert_eq!(required_permission("/api/hisRead"), Some("read"));
        assert_eq!(required_permission("/api/watchSub"), Some("read"));
        assert_eq!(required_permission("/api/watchPoll"), Some("read"));
        assert_eq!(required_permission("/api/watchUnsub"), Some("read"));
        assert_eq!(required_permission("/api/close"), Some("read"));
        assert_eq!(required_permission("/api/about"), Some("read"));
        assert_eq!(required_permission("/api/ops"), Some("read"));
        assert_eq!(required_permission("/api/formats"), Some("read"));
    }

    #[test]
    fn required_permission_write_ops() {
        assert_eq!(required_permission("/api/pointWrite"), Some("write"));
        assert_eq!(required_permission("/api/hisWrite"), Some("write"));
        assert_eq!(required_permission("/api/invokeAction"), Some("write"));
        assert_eq!(required_permission("/api/import"), Some("write"));
        assert_eq!(required_permission("/api/federation/sync"), Some("write"));
    }

    #[test]
    fn required_permission_admin_ops() {
        assert_eq!(required_permission("/api/system/backup"), Some("admin"));
        assert_eq!(required_permission("/api/system/restart"), Some("admin"));
        assert_eq!(required_permission("/api/system/"), Some("admin"));
    }

    // ---- Integration tests for the auth middleware ----

    use crate::auth::users::hash_password;
    use crate::ws::WatchManager;
    use actix_web::dev::Service;
    use actix_web::middleware::from_fn;
    use actix_web::test as actix_test;
    use actix_web::{App, HttpResponse};

    /// Build an AppState with auth enabled: one admin user and one read-only viewer.
    fn test_state() -> web::Data<AppState> {
        let hash = hash_password("s3cret");
        let toml_str = format!(
            r#"
[users.admin]
password_hash = "{hash}"
permissions = ["read", "write", "admin"]

[users.viewer]
password_hash = "{hash}"
permissions = ["read"]
"#
        );
        let auth = AuthManager::from_toml_str(&toml_str).unwrap();
        web::Data::new(AppState {
            graph: haystack_core::graph::SharedGraph::new(haystack_core::graph::EntityGraph::new()),
            namespace: parking_lot::RwLock::new(DefNamespace::new()),
            auth,
            watches: WatchManager::new(),
            actions: ActionRegistry::new(),
            his: HisStore::new(),
            started_at: std::time::Instant::now(),
            federation: Federation::new(),
        })
    }

    /// Insert a token directly into the AuthManager and return it.
    fn insert_token(
        state: &web::Data<AppState>,
        username: &str,
        permissions: Vec<String>,
    ) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        let user = crate::auth::AuthUser {
            username: username.to_string(),
            permissions,
        };
        state.auth.inject_token(token.clone(), user);
        token
    }

    /// Minimal read handler.
    async fn dummy_handler() -> HttpResponse {
        HttpResponse::Ok().body("ok")
    }

    /// Create a test app with auth middleware and dummy routes.
    fn test_app(
        state: web::Data<AppState>,
    ) -> App<
        impl actix_web::dev::ServiceFactory<
            ServiceRequest,
            Config = (),
            Response = ServiceResponse<impl MessageBody>,
            Error = actix_web::Error,
            InitError = (),
        >,
    > {
        App::new()
            .app_data(state)
            .wrap(from_fn(auth_middleware))
            .route("/api/about", web::get().to(dummy_handler))
            .route("/api/ops", web::get().to(dummy_handler))
            .route("/api/formats", web::get().to(dummy_handler))
            .route("/api/read", web::post().to(dummy_handler))
            .route("/api/hisRead", web::post().to(dummy_handler))
            .route("/api/pointWrite", web::post().to(dummy_handler))
            .route("/api/hisWrite", web::post().to(dummy_handler))
            .route("/api/invokeAction", web::post().to(dummy_handler))
            .route("/api/close", web::post().to(dummy_handler))
            .route("/api/system/backup", web::post().to(dummy_handler))
    }

    /// Helper: call the service and extract the status code, handling both
    /// success responses and middleware errors (which implement ResponseError).
    async fn call_status(
        app: &impl Service<
            actix_http::Request,
            Response = ServiceResponse<impl MessageBody>,
            Error = actix_web::Error,
        >,
        req: actix_http::Request,
    ) -> u16 {
        match app.call(req).await {
            Ok(resp) => resp.status().as_u16(),
            Err(err) => err.as_response_error().status_code().as_u16(),
        }
    }

    #[actix_rt::test]
    async fn about_passes_through_without_auth() {
        let state = test_state();
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::get()
            .uri("/api/about")
            .to_request();
        assert_eq!(call_status(&app, req).await, 200);
    }

    #[actix_rt::test]
    async fn ops_passes_through_without_auth() {
        let state = test_state();
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::get().uri("/api/ops").to_request();
        assert_eq!(call_status(&app, req).await, 200);
    }

    #[actix_rt::test]
    async fn formats_passes_through_without_auth() {
        let state = test_state();
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::get()
            .uri("/api/formats")
            .to_request();
        assert_eq!(call_status(&app, req).await, 200);
    }

    #[actix_rt::test]
    async fn protected_endpoint_without_token_returns_401() {
        let state = test_state();
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/read")
            .to_request();
        assert_eq!(call_status(&app, req).await, 401);
    }

    #[actix_rt::test]
    async fn viewer_can_read() {
        let state = test_state();
        let token = insert_token(&state, "viewer", vec!["read".to_string()]);
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/read")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 200);
    }

    #[actix_rt::test]
    async fn viewer_cannot_write() {
        let state = test_state();
        let token = insert_token(&state, "viewer", vec!["read".to_string()]);
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/pointWrite")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 403);
    }

    #[actix_rt::test]
    async fn viewer_cannot_invoke_action() {
        let state = test_state();
        let token = insert_token(&state, "viewer", vec!["read".to_string()]);
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/invokeAction")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 403);
    }

    #[actix_rt::test]
    async fn viewer_cannot_his_write() {
        let state = test_state();
        let token = insert_token(&state, "viewer", vec!["read".to_string()]);
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/hisWrite")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 403);
    }

    #[actix_rt::test]
    async fn writer_can_write() {
        let state = test_state();
        let token = insert_token(
            &state,
            "writer",
            vec!["read".to_string(), "write".to_string()],
        );
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/pointWrite")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 200);
    }

    #[actix_rt::test]
    async fn admin_can_access_system() {
        let state = test_state();
        let token = insert_token(&state, "admin", vec!["admin".to_string()]);
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/system/backup")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 200);
    }

    #[actix_rt::test]
    async fn viewer_cannot_access_system() {
        let state = test_state();
        let token = insert_token(&state, "viewer", vec!["read".to_string()]);
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/system/backup")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 403);
    }

    #[actix_rt::test]
    async fn writer_cannot_access_system() {
        let state = test_state();
        let token = insert_token(
            &state,
            "writer",
            vec!["read".to_string(), "write".to_string()],
        );
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/system/backup")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 403);
    }

    #[actix_rt::test]
    async fn viewer_can_close() {
        let state = test_state();
        let token = insert_token(&state, "viewer", vec!["read".to_string()]);
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/close")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 200);
    }

    #[actix_rt::test]
    async fn viewer_can_his_read() {
        let state = test_state();
        let token = insert_token(&state, "viewer", vec!["read".to_string()]);
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/hisRead")
            .insert_header(("Authorization", format!("BEARER authToken={token}")))
            .to_request();
        assert_eq!(call_status(&app, req).await, 200);
    }

    #[actix_rt::test]
    async fn invalid_token_returns_401() {
        let state = test_state();
        let app = actix_test::init_service(test_app(state)).await;

        let req = actix_test::TestRequest::post()
            .uri("/api/read")
            .insert_header(("Authorization", "BEARER authToken=bogus-token"))
            .to_request();
        assert_eq!(call_status(&app, req).await, 401);
    }
}
