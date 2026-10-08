//! One capability registry drives both built-in routing and `/api/ops`.
use crate::{ops, state::SharedState, ws};
use axum::routing::{MethodRouter, get, post};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceProfile {
    /// Compatibility API with coarse route permissions and unrestricted handlers.
    LegacyUnrestricted,
    /// Entity and catalog reads use the shared application authorization service.
    ScopedReadService,
}

pub(crate) struct Capability {
    pub name: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
    scoped: bool,
}
macro_rules! capabilities {
    ($(($name:literal, $summary:literal, $scoped:literal)),* $(,)?) => {
        pub(crate) const CAPABILITIES: &[Capability] = &[$(Capability {
            name: $name, path: concat!("/api/", $name), summary: $summary, scoped: $scoped,
        }),*];
    };
}
capabilities! {
    ("about", "Summary information for server", true),
    ("ops", "Operations supported by this server", true),
    ("formats", "Grid data formats supported by this server", true),
    ("read", "Read entity records by id or filter", true),
    ("nav", "Navigate a project for discovery", true),
    ("defs", "Query the definitions namespace", true),
    ("libs", "Query the library namespace", true),
    ("specs", "List specifications", true),
    ("spec", "Read a specification", true),
    ("close", "Close the current session", true),
    ("ws", "WebSocket entity watches", false),
    ("watchSub", "Subscribe to entity changes", false),
    ("watchPoll", "Poll for entity changes", false),
    ("watchUnsub", "Unsubscribe from entity changes", false),
    ("pointWrite", "Write a value to a writable point", false),
    ("hisRead", "Read historical time-series data", false),
    ("hisWrite", "Write historical time-series data", false),
    ("invokeAction", "Invoke an action on an entity", false),
    ("import", "Import entity records", false),
    ("export", "Export entity records", false),
    ("validate", "Validate entities against the catalog", false),
    ("loadLib", "Load a library", false),
    ("unloadLib", "Unload a library", false),
    ("exportLib", "Export a library", false),
    ("changes", "Read entity changes", false),
}
impl Capability {
    pub fn enabled(&self, profile: ServiceProfile) -> bool {
        profile == ServiceProfile::LegacyUnrestricted || self.scoped
    }
    pub fn router(&self, profile: ServiceProfile) -> MethodRouter<SharedState> {
        let scoped = profile == ServiceProfile::ScopedReadService;
        match self.name {
            "about" => get(ops::about::handle),
            "ops" => get(ops::ops_handler::handle),
            "formats" => get(ops::formats::handle),
            "read" if scoped => post(ops::shared_read::read),
            "nav" if scoped => post(ops::shared_read::nav),
            "defs" if scoped => post(ops::shared_read::definitions),
            "libs" if scoped => post(ops::shared_read::libraries),
            "specs" if scoped => post(ops::shared_read::specs),
            "spec" if scoped => post(ops::shared_read::spec),
            "read" => post(ops::read::handle),
            "nav" => post(ops::nav::handle),
            "defs" => post(ops::defs::handle),
            "libs" => post(ops::defs::handle_libs),
            "specs" => post(ops::libs::handle_specs),
            "spec" => post(ops::libs::handle_spec),
            "close" => post(ops::about::handle_close),
            "ws" => get(ws::ws_handler),
            "watchSub" => post(ops::watch::handle_sub),
            "watchPoll" => post(ops::watch::handle_poll),
            "watchUnsub" => post(ops::watch::handle_unsub),
            "pointWrite" => post(ops::point_write::handle),
            "hisRead" => post(ops::his::handle_read),
            "hisWrite" => post(ops::his::handle_write),
            "invokeAction" => post(ops::invoke::handle),
            "import" => post(ops::data::handle_import),
            "export" => post(ops::data::handle_export),
            "validate" => post(ops::libs::handle_validate),
            "loadLib" => post(ops::libs::handle_load_lib),
            "unloadLib" => post(ops::libs::handle_unload_lib),
            "exportLib" => post(ops::libs::handle_export_lib),
            "changes" => post(ops::changes::handle),
            _ => unreachable!("registry and dispatch must agree"),
        }
    }
}
