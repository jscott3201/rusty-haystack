//! Application state shared across all request handlers.

use haystack_core::graph::SharedGraph;
use haystack_core::ontology::DefNamespace;

use crate::actions::ActionRegistry;
use crate::auth::AuthManager;
use crate::federation::Federation;
use crate::his_provider::HistoryProvider;
use crate::ws::WatchManager;

/// Shared application state injected into every Actix handler via `web::Data`.
pub struct AppState {
    /// Thread-safe entity graph.
    pub graph: SharedGraph,
    /// Haystack 4 ontology namespace for def/spec operations.
    pub namespace: parking_lot::RwLock<DefNamespace>,
    /// SCRAM authentication manager.
    pub auth: AuthManager,
    /// Watch subscription manager for change polling.
    pub watches: WatchManager,
    /// Action dispatch registry for the `invokeAction` op.
    pub actions: ActionRegistry,
    /// Pluggable time-series history store for hisRead/hisWrite.
    pub his: Box<dyn HistoryProvider>,
    /// Instant when the server was started, used for uptime calculation.
    pub started_at: std::time::Instant,
    /// Federation manager for remote connector queries.
    pub federation: Federation,
}
