//! Server builder and startup.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use haystack_core::auth::{AuthHeader, parse_auth_header};
use haystack_core::graph::SharedGraph;
use haystack_core::ontology::DefNamespace;

use crate::actions::ActionRegistry;
use crate::auth::AuthManager;
use crate::his_store::HisStore;
use crate::ops;
use crate::state::{AppState, SharedState};
use crate::ws;
use crate::ws::WatchManager;

/// Builder for the Haystack HTTP server.
pub struct HaystackServer {
    graph: SharedGraph,
    namespace: DefNamespace,
    auth_manager: AuthManager,
    actions: ActionRegistry,
    custom_router: Option<Router<SharedState>>,
    authenticated_router: Option<Router<SharedState>>,
    history_provider: Option<Box<dyn crate::his_provider::HistoryProvider>>,
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
            custom_router: None,
            authenticated_router: None,
            history_provider: None,
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

    /// Merge additional routes into the server.
    ///
    /// **Note:** Routes added via `with_router()` are NOT protected by the built-in
    /// auth middleware. To protect custom routes, apply your own auth layer to the
    /// router before passing it, or use `with_authenticated_router()` instead.
    ///
    /// The router's routes are merged at the top level, so paths must
    /// include any prefix (e.g. `/custom/endpoint`).
    pub fn with_router(mut self, router: Router<SharedState>) -> Self {
        self.custom_router = Some(router);
        self
    }

    /// Merge additional routes that are protected by the built-in auth middleware.
    ///
    /// Routes added here go through the same authentication and permission
    /// checks as the standard Haystack API endpoints.
    pub fn with_authenticated_router(mut self, router: Router<SharedState>) -> Self {
        self.authenticated_router = Some(router);
        self
    }

    /// Set the history storage provider (default: in-memory [`HisStore`]).
    pub fn with_history_provider(
        mut self,
        provider: Box<dyn crate::his_provider::HistoryProvider>,
    ) -> Self {
        self.history_provider = Some(provider);
        self
    }

    /// Start the HTTP server. This blocks until the server is stopped.
    pub async fn run(self) -> std::io::Result<()> {
        let his: Box<dyn crate::his_provider::HistoryProvider> = self
            .history_provider
            .unwrap_or_else(|| Box::new(HisStore::new()));

        let state: SharedState = Arc::new(AppState {
            graph: self.graph,
            namespace: parking_lot::RwLock::new(self.namespace),
            auth: self.auth_manager,
            watches: WatchManager::new(),
            actions: self.actions,
            his,
            started_at: std::time::Instant::now(),
        });

        let mut core_router = Router::new()
            // GET routes
            .route("/api/about", get(ops::about::handle))
            .route("/api/ops", get(ops::ops_handler::handle))
            .route("/api/formats", get(ops::formats::handle))
            .route("/api/ws", get(ws::ws_handler))
            // POST routes
            .route("/api/read", post(ops::read::handle))
            .route("/api/nav", post(ops::nav::handle))
            .route("/api/defs", post(ops::defs::handle))
            .route("/api/libs", post(ops::defs::handle_libs))
            .route("/api/hisRead", post(ops::his::handle_read))
            .route("/api/hisWrite", post(ops::his::handle_write))
            .route("/api/watchSub", post(ops::watch::handle_sub))
            .route("/api/watchPoll", post(ops::watch::handle_poll))
            .route("/api/watchUnsub", post(ops::watch::handle_unsub))
            .route("/api/pointWrite", post(ops::point_write::handle))
            .route("/api/invokeAction", post(ops::invoke::handle))
            .route("/api/close", post(ops::about::handle_close))
            .route("/api/import", post(ops::data::handle_import))
            .route("/api/export", post(ops::data::handle_export))
            .route("/api/validate", post(ops::libs::handle_validate))
            .route("/api/specs", post(ops::libs::handle_specs))
            .route("/api/spec", post(ops::libs::handle_spec))
            .route("/api/loadLib", post(ops::libs::handle_load_lib))
            .route("/api/unloadLib", post(ops::libs::handle_unload_lib))
            .route("/api/exportLib", post(ops::libs::handle_export_lib))
            .route("/api/changes", post(ops::changes::handle));

        // Merge the authenticated custom router before applying the auth layer,
        // so its routes are also protected by the built-in auth middleware.
        if let Some(auth_router) = self.authenticated_router {
            core_router = core_router.merge(auth_router);
        }

        let mut app = core_router
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
            .with_state(state.clone());

        if let Some(custom) = self.custom_router {
            app = app.merge(custom.with_state(state));
        }

        log::info!("Starting haystack-server on {}:{}", self.host, self.port);

        let listener =
            tokio::net::TcpListener::bind(format!("{}:{}", self.host, self.port)).await?;
        axum::serve(listener, app).await
    }
}

/// Determine the required permission for a given request path.
///
/// Returns `None` if the path does not require permission checking
/// (e.g. public endpoints handled before auth).
fn required_permission(path: &str) -> Option<&'static str> {
    // Write operations
    match path {
        "/api/pointWrite" | "/api/hisWrite" | "/api/invokeAction" | "/api/loadLib"
        | "/api/unloadLib" | "/api/import" => return Some("write"),
        _ => {}
    }

    // Everything else that reaches here is a read-level operation:
    // /api/about, /api/read, /api/nav, /api/defs, /api/libs,
    // /api/hisRead, /api/watchSub, /api/watchPoll, /api/watchUnsub,
    // /api/close, /api/ops, /api/formats, etc.
    Some("read")
}

/// Authentication middleware for Axum.
///
/// - GET /api/about: pass through (about handles auth itself for SCRAM)
/// - GET /api/ops, GET /api/formats: pass through (public info)
/// - All other endpoints: require BEARER token if auth is enabled,
///   then check the user has the required permission for that route.
async fn auth_middleware(
    State(state): State<SharedState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // Allow about endpoint through (it handles auth itself for SCRAM handshake)
    if path == "/api/about" {
        return next.run(req).await;
    }

    // Allow ops and formats through without auth (public endpoints)
    if (path == "/api/ops" || path == "/api/formats") && method == Method::GET {
        return next.run(req).await;
    }

    // Check if auth is enabled
    if !state.auth.is_enabled() {
        return next.run(req).await;
    }

    // Extract and validate BEARER token
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    match auth_header {
        Some(header) => match parse_auth_header(&header) {
            Ok(AuthHeader::Bearer { auth_token }) => {
                match state.auth.validate_token(&auth_token) {
                    Some(auth_user) => {
                        // Check permission for the requested path
                        if let Some(required) = required_permission(&path)
                            && !AuthManager::check_permission(&auth_user, required)
                        {
                            return crate::error::HaystackError::forbidden(format!(
                                "insufficient '{}' permission",
                                required
                            ))
                            .into_response();
                        }

                        // Inject AuthUser into request extensions
                        req.extensions_mut().insert(auth_user);
                        next.run(req).await
                    }
                    None => crate::error::HaystackError::new(
                        "invalid or expired auth token",
                        StatusCode::UNAUTHORIZED,
                    )
                    .into_response(),
                }
            }
            _ => {
                crate::error::HaystackError::new("BEARER token required", StatusCode::UNAUTHORIZED)
                    .into_response()
            }
        },
        None => crate::error::HaystackError::new(
            "Authorization header required",
            StatusCode::UNAUTHORIZED,
        )
        .into_response(),
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
    }
}
