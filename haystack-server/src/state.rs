//! Application state shared across all request handlers.

use std::sync::Arc;

use haystack_core::graph::SharedGraph;
use haystack_core::ontology::DefNamespace;

use crate::actions::ActionRegistry;
use crate::auth::AuthManager;
use haystack_app::HistoryProvider;

/// Type alias for the shared state used by Axum extractors.
pub type SharedState = Arc<AppState>;

/// Shared application state injected into every Axum handler via `State`.
pub struct AppState {
    /// Application lifetime for requests and upgraded connections.
    pub application: Option<haystack_app::ApplicationHandle>,
    /// Thread-safe entity graph.
    pub graph: SharedGraph,
    /// Application read authority, present only in the scoped service profile.
    pub read_service: Option<haystack_app::ReadService>,
    /// Selection captured when routing is built; discovery uses the same value.
    pub mutation_service: Option<haystack_app::MutationService>,
    /// Built-in capability profile, also used by the ops advertisement.
    pub profile: crate::capabilities::ServiceProfile,
    /// Serializes legacy library updates. Catalog construction happens outside
    /// the graph lock, and publication compares the captured catalog generation.
    pub lib_mutations: parking_lot::Mutex<()>,
    /// SCRAM authentication manager.
    pub auth: AuthManager,
    /// One application owner shared by HTTP and every WebSocket attachment.
    pub subscription_service: Option<haystack_app::StateSubscriptionService>,
    /// Action dispatch registry for the `invokeAction` op.
    pub actions: ActionRegistry,
    /// Pluggable time-series history store for hisRead/hisWrite.
    pub his: Option<Arc<dyn HistoryProvider>>,
    /// Authorized scoped writes captured with the same history selection.
    pub history_mutation_service: Option<haystack_app::HistoryMutationService>,
    /// Exact history selection captured from the owning application.
    pub history_service: Option<haystack_app::HistoryService>,
    /// Instant when the server was started, used for uptime calculation.
    pub started_at: std::time::Instant,
}

impl AppState {
    /// Capture the graph's sole immutable catalog authority.
    pub fn namespace(&self) -> Arc<DefNamespace> {
        self.graph
            .read(|graph| graph.namespace_arc().cloned())
            .unwrap_or_else(|| Arc::new(DefNamespace::new()))
    }
}
