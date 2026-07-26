//! Application state shared across all request handlers.

use std::sync::Arc;

use haystack_core::graph::SharedGraph;
use haystack_core::ontology::DefNamespace;

use crate::actions::ActionRegistry;
use crate::auth::AuthManager;
use crate::his_provider::HistoryProvider;
use crate::ws::WatchManager;

/// Type alias for the shared state used by Axum extractors.
pub type SharedState = Arc<AppState>;

/// Shared application state injected into every Axum handler via `State`.
pub struct AppState {
    /// Thread-safe entity graph.
    pub graph: SharedGraph,
    /// Haystack 4 ontology namespace for def/spec operations.
    pub namespace: parking_lot::RwLock<DefNamespace>,
    /// Serializes library load/unload end to end.
    ///
    /// A lib mutation updates two things: this `namespace`, and the ontology
    /// snapshot every graph holds. Those cannot be done under one lock — holding
    /// `namespace` while taking the graph lock fixes a namespace-then-graph order
    /// that a custom router can invert, which is an AB/BA deadlock. But updating
    /// them independently lets two concurrent loads publish snapshots out of order,
    /// leaving the graph permanently older than the namespace.
    ///
    /// This lock resolves both: mutations are serialized, so publishes are ordered,
    /// while the namespace lock is still released before the graph lock is taken, so
    /// the two are never held at once.
    pub lib_mutations: parking_lot::Mutex<()>,
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
}
