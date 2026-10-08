//! Server builder and startup.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};

use haystack_core::auth::{AuthHeader, parse_auth_header};
use haystack_core::graph::SharedGraph;
use haystack_core::ontology::DefNamespace;

use crate::actions::ActionRegistry;
use crate::auth::AuthManager;
use crate::capabilities::{CAPABILITIES, ServiceProfile};
use crate::cors::CorsPolicy;
use crate::his_store::HisStore;
use crate::ops;
use crate::state::{AppState, SharedState};
use crate::ws::WatchManager;

/// Builder for the Haystack HTTP server.
pub struct HaystackServer {
    graph: SharedGraph,
    namespace: Option<DefNamespace>,
    read_service: Option<haystack_app::ReadService>,
    trusted_external_routes: bool,
    auth_manager: AuthManager,
    actions: ActionRegistry,
    custom_router: Option<Router<SharedState>>,
    authenticated_router: Option<Router<SharedState>>,
    history_provider: Option<Box<dyn crate::his_provider::HistoryProvider>>,
    cors: CorsPolicy,
    port: u16,
    host: String,
}

impl HaystackServer {
    /// Create a new server with the given entity graph.
    pub fn new(graph: SharedGraph) -> Self {
        Self {
            graph,
            namespace: None,
            read_service: None,
            trusted_external_routes: false,
            auth_manager: AuthManager::empty(),
            actions: ActionRegistry::new(),
            custom_router: None,
            authenticated_router: None,
            history_provider: None,
            cors: CorsPolicy::default(),
            port: 8080,
            host: "127.0.0.1".to_string(),
        }
    }

    /// Set the ontology namespace for def/spec operations.
    pub fn with_namespace(mut self, ns: DefNamespace) -> Self {
        self.namespace = Some(ns);
        self
    }

    /// Serve bounded, policy-authorized reads using the supplied service and graph.
    /// Unsupported built-in operations are absent in this profile.
    pub fn with_scoped_reads(mut self, service: haystack_app::ReadService) -> Self {
        self.graph = service.graph();
        self.read_service = Some(service);
        self
    }

    /// Select the legacy coarse-permission API (also the compatibility default).
    pub fn with_legacy_unrestricted(mut self) -> Self {
        self.read_service = None;
        self
    }

    /// Explicitly authorize custom routes as a separate trusted authority.
    /// The read service cannot police arbitrary handlers receiving `AppState`.
    pub fn with_trusted_external_routes(mut self) -> Self {
        self.trusted_external_routes = true;
        self
    }

    /// Set the authentication manager.
    pub fn with_auth(mut self, auth: AuthManager) -> Self {
        self.auth_manager = auth;
        self
    }

    /// Set the cross-origin policy (default: [`CorsPolicy::Disabled`]).
    pub fn with_cors(mut self, cors: CorsPolicy) -> Self {
        self.cors = cors;
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
    /// **Note:** Routes added via `with_router()` are merged after the built-in
    /// middleware stack, so they are protected by NEITHER the auth middleware NOR
    /// the 2 MB request-body limit. To protect custom routes, apply your own auth
    /// and body-size layers to the router before passing it, or use
    /// `with_authenticated_router()` instead (which is also covered by the body limit).
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
        self.run_reporting_addr(|_| {}).await
    }

    /// Run, invoking `on_bound` with the address actually bound.
    ///
    /// The callback fires after a successful bind and before the first connection
    /// is accepted, so a caller that prints or publishes the address cannot race a
    /// client that reads it. This is what makes `--port 0` usable: the kernel picks
    /// the port and this is the only place it can be observed.
    pub async fn run_reporting_addr<F>(self, on_bound: F) -> std::io::Result<()>
    where
        F: FnOnce(std::net::SocketAddr),
    {
        let (host, port) = (self.host.clone(), self.port);
        let app = self.build_router()?;

        log::info!("Starting haystack-server on {host}:{port}");

        let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;

        // Report the address actually bound, not the one requested. With `--port 0`
        // the kernel picks the port, and nothing outside the process could discover
        // it — so callers had to guess a free port and race the bind (issue #35).
        let bound = listener.local_addr()?;
        log::info!("haystack-server listening on {bound}");
        on_bound(bound);

        axum::serve(listener, app).await
    }

    /// Assemble the router and its middleware stack, without binding a socket.
    ///
    /// Split out from [`Self::run_reporting_addr`] so the stack can be driven
    /// directly in tests. Layer *order* below is load-bearing rather than
    /// incidental, and an assertion about it in a comment is worth only as much
    /// as the test that exercises it.
    fn build_router(self) -> std::io::Result<Router> {
        let profile = if self.read_service.is_some() {
            ServiceProfile::ScopedReadService
        } else {
            ServiceProfile::LegacyUnrestricted
        };
        if profile == ServiceProfile::ScopedReadService
            && (self.custom_router.is_some() || self.authenticated_router.is_some())
            && !self.trusted_external_routes
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "scoped reads require explicit trusted external authority for custom routes",
            ));
        }
        // Initialize only when absent; preserve an embedding's existing catalog.
        self.graph.write(|graph| {
            if let Some(namespace) = self.namespace {
                graph.set_namespace(Arc::new(namespace));
            } else if graph.namespace_arc().is_none() {
                graph.set_namespace(Arc::new(DefNamespace::new()));
            }
        });
        let his: Box<dyn crate::his_provider::HistoryProvider> = self
            .history_provider
            .unwrap_or_else(|| Box::new(HisStore::new()));

        let state: SharedState = Arc::new(AppState {
            graph: self.graph,
            read_service: self.read_service,
            profile,
            lib_mutations: parking_lot::Mutex::new(()),
            auth: self.auth_manager,
            watches: WatchManager::new(),
            actions: self.actions,
            his,
            started_at: std::time::Instant::now(),
        });

        let mut core_router = Router::new();
        for capability in CAPABILITIES
            .iter()
            .filter(|capability| capability.enabled(profile))
        {
            core_router = core_router.route(capability.path, capability.router(profile));
        }

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

        // Outermost, and after the custom-router merge so those routes are
        // covered too. The position is load-bearing, not stylistic. A browser
        // sends preflight as OPTIONS with no Authorization header, and
        // `route_layer` above *does* run for it: `/api/read` matches as a path,
        // the method router is what rejects OPTIONS, and that is late enough
        // that auth has already seen the request. So with auth enabled and
        // CorsLayer any deeper, every preflight to an authenticated route is a
        // 401 and no cross-origin call ever reaches its handler.
        //
        // Measured, not assumed: with `CorsPolicy::Disabled` an OPTIONS to
        // `/api/read` on an auth-enabled server returns 401, which is exactly
        // what `cors_layering::disabled_grants_nothing` pins down. (`/api/about`
        // is bypassed by the middleware regardless of method, and an
        // auth-disabled server passes everything, so the 401 needs both an
        // authenticated route and live auth to show up.)
        if let Some(cors) = self.cors.layer() {
            app = app.layer(cors);
        }

        Ok(app)
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
    req.extensions_mut()
        .insert(ops::shared_read::ReadStarted(std::time::Instant::now()));
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

    mod cors_layering {
        use super::*;
        use crate::auth::users::hash_password;
        use axum::http::header;
        use haystack_core::graph::EntityGraph;
        use tower::ServiceExt;

        const ORIGIN: &str = "https://ops.example.com";

        /// A server with auth genuinely enabled — the whole point of these
        /// tests is that CORS answers *before* auth, so auth must be able to
        /// reject in the first place.
        fn server_with_auth_and_cors(cors: CorsPolicy) -> HaystackServer {
            let hash = hash_password("s3cret");
            let auth = AuthManager::from_toml_str(&format!(
                "[users.admin]\npassword_hash = \"{hash}\"\npermissions = [\"read\", \"write\"]\n"
            ))
            .unwrap();
            assert!(auth.is_enabled(), "auth must be live for these tests");

            HaystackServer::new(SharedGraph::new(EntityGraph::new()))
                .with_auth(auth)
                .with_cors(cors)
        }

        fn preflight(origin: &str) -> Request<Body> {
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/read")
                .header(header::ORIGIN, origin)
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
                .body(Body::empty())
                .unwrap()
        }

        /// The baseline the CORS tests below are meaningful against: an
        /// unauthenticated POST really is refused.
        #[tokio::test]
        async fn unauthenticated_post_is_rejected() {
            let app = server_with_auth_and_cors(CorsPolicy::Allow(vec![ORIGIN.to_string()]))
                .build_router()
                .unwrap();

            let res = app
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/read")
                        .header(header::ORIGIN, ORIGIN)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        /// The load-bearing one. A browser sends preflight with no
        /// `Authorization` header; if `CorsLayer` sat inside `auth_middleware`
        /// this would be a 401 and every cross-origin call would fail.
        #[tokio::test]
        async fn preflight_is_answered_without_authentication() {
            let app = server_with_auth_and_cors(CorsPolicy::Allow(vec![ORIGIN.to_string()]))
                .build_router()
                .unwrap();

            let res = app.oneshot(preflight(ORIGIN)).await.unwrap();

            assert_ne!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "preflight reached the auth middleware — CorsLayer is nested too deep"
            );
            assert!(res.status().is_success(), "status was {}", res.status());
            assert_eq!(
                res.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .map(|v| v.to_str().unwrap()),
                Some(ORIGIN)
            );
        }

        /// `POST` is what every Haystack op but four uses, and `Authorization`
        /// is how auth crosses origins at all. Both must survive preflight.
        #[tokio::test]
        async fn preflight_allows_post_and_the_authorization_header() {
            let app = server_with_auth_and_cors(CorsPolicy::Allow(vec![ORIGIN.to_string()]))
                .build_router()
                .unwrap();

            let res = app.oneshot(preflight(ORIGIN)).await.unwrap();
            let headers = res.headers();

            let methods = headers
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap()
                .to_str()
                .unwrap()
                .to_ascii_uppercase();
            assert!(methods.contains("POST"), "methods were {methods}");
            assert!(methods.contains("GET"), "methods were {methods}");

            let allowed = headers
                .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
                .unwrap()
                .to_str()
                .unwrap()
                .to_ascii_lowercase();
            assert!(allowed.contains("authorization"), "headers were {allowed}");
            assert!(allowed.contains("content-type"), "headers were {allowed}");
        }

        /// Auth is header-based and no cookie is ever set, so granting
        /// credentials would widen the policy for nothing.
        ///
        /// The success and allow-origin assertions are load-bearing: without
        /// them this passes when preflight is refused outright, since a 401
        /// carries no `Allow-Credentials` header either.
        #[tokio::test]
        async fn preflight_does_not_allow_credentials() {
            let app = server_with_auth_and_cors(CorsPolicy::Allow(vec![ORIGIN.to_string()]))
                .build_router()
                .unwrap();

            let res = app.oneshot(preflight(ORIGIN)).await.unwrap();

            assert!(res.status().is_success(), "status was {}", res.status());
            assert!(
                res.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .is_some(),
                "CORS did not answer, so the absence below proves nothing"
            );
            assert!(
                res.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
                    .is_none()
            );
        }

        /// One bad entry must not cost the good ones their access. Asserting
        /// the grant rather than "a layer exists" is what makes this fail if
        /// the valid origins are dropped alongside the invalid one.
        #[tokio::test]
        async fn a_valid_origin_survives_an_allowlist_holding_an_invalid_one() {
            let app = server_with_auth_and_cors(CorsPolicy::Allow(vec![
                ORIGIN.to_string(),
                "bad\norigin".to_string(),
            ]))
            .build_router()
            .unwrap();

            let res = app.oneshot(preflight(ORIGIN)).await.unwrap();

            assert!(res.status().is_success(), "status was {}", res.status());
            assert_eq!(
                res.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .map(|v| v.to_str().unwrap()),
                Some(ORIGIN)
            );
        }

        /// `*` reaches `AllowOrigin::list` only if nothing filters it, and
        /// tower-http panics there rather than returning an error — so this
        /// would take the server down after it had already loaded its data.
        #[tokio::test]
        async fn a_wildcard_origin_is_refused_rather_than_panicking() {
            let app = server_with_auth_and_cors(CorsPolicy::Allow(vec!["*".to_string()]))
                .build_router()
                .unwrap();

            let res = app.oneshot(preflight(ORIGIN)).await.unwrap();

            assert!(
                res.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .is_none(),
                "a wildcard must not grant a concrete origin"
            );
        }

        #[tokio::test]
        async fn an_origin_off_the_allowlist_is_not_granted() {
            let app = server_with_auth_and_cors(CorsPolicy::Allow(vec![ORIGIN.to_string()]))
                .build_router()
                .unwrap();

            let res = app
                .oneshot(preflight("https://attacker.example.com"))
                .await
                .unwrap();

            assert!(
                res.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .is_none(),
                "an unlisted origin was granted access"
            );
        }

        /// The default must behave exactly as the server did before CORS
        /// existed. The 401 is the interesting half: it shows the preflight ran
        /// all the way into the auth middleware because no CORS layer was there
        /// to answer it first, which is simultaneously the evidence that the
        /// layer's outermost position in `build_router` is load-bearing. An
        /// assertion on the missing header alone would also pass if a layer
        /// were installed that merely granted nothing.
        #[tokio::test]
        async fn disabled_grants_nothing_and_preflight_falls_through_to_auth() {
            let app = server_with_auth_and_cors(CorsPolicy::Disabled)
                .build_router()
                .unwrap();

            let res = app.oneshot(preflight(ORIGIN)).await.unwrap();

            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
            assert!(
                res.headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .is_none()
            );
        }
    }
}

#[cfg(test)]
#[path = "scoped_body_tests.rs"]
mod scoped_body_tests;
