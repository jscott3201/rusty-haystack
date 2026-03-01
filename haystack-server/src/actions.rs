//! Action registry for the `invokeAction` op.
//!
//! Provides a trait-based dispatch mechanism so that custom action handlers
//! can be registered at startup and invoked by name at runtime.

use std::collections::HashMap;

use haystack_core::data::{HDict, HGrid};

/// A handler for a named action that can be invoked on entities.
///
/// Implementors provide:
/// - `name()` — the action name used for dispatch (e.g. `"reboot"`)
/// - `invoke()` — the logic that executes the action
pub trait ActionHandler: Send + Sync {
    /// The action name used for dispatch.
    fn name(&self) -> &str;

    /// Invoke the action on the given entity with the supplied arguments.
    ///
    /// - `entity` — the resolved entity dict from the graph
    /// - `action` — the action name (same as `self.name()`)
    /// - `args`   — additional arguments from the request grid row
    ///
    /// Returns a result grid on success, or a human-readable error string.
    fn invoke(&self, entity: &HDict, action: &str, args: &HDict) -> Result<HGrid, String>;
}

/// Registry that maps action names to their handlers.
///
/// Thread-safe because every stored handler is `Send + Sync`.
pub struct ActionRegistry {
    handlers: HashMap<String, Box<dyn ActionHandler>>,
}

impl ActionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler, keyed by `handler.name()`.
    ///
    /// If a handler with the same name already exists it is replaced.
    pub fn register(&mut self, handler: Box<dyn ActionHandler>) {
        let name = handler.name().to_string();
        self.handlers.insert(name, handler);
    }

    /// Dispatch an action invocation to the matching handler.
    ///
    /// Returns `Err` with a descriptive message if no handler is registered
    /// for the requested action name.
    pub fn invoke(&self, entity: &HDict, action: &str, args: &HDict) -> Result<HGrid, String> {
        match self.handlers.get(action) {
            Some(handler) => handler.invoke(entity, action, args),
            None => Err(format!("unknown action: {action}")),
        }
    }

    /// List all registered action names (in arbitrary order).
    pub fn list_actions(&self) -> Vec<String> {
        self.handlers.keys().cloned().collect()
    }
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_core::data::HCol;
    use haystack_core::kinds::{HRef, Kind};

    /// A trivial test handler that always succeeds and returns a single-row grid.
    struct EchoAction;

    impl ActionHandler for EchoAction {
        fn name(&self) -> &str {
            "echo"
        }

        fn invoke(&self, _entity: &HDict, action: &str, _args: &HDict) -> Result<HGrid, String> {
            let mut row = HDict::new();
            row.set("action", Kind::Str(action.to_string()));
            row.set("result", Kind::Str("ok".to_string()));
            let cols = vec![HCol::new("action"), HCol::new("result")];
            Ok(HGrid::from_parts(HDict::new(), cols, vec![row]))
        }
    }

    /// Another trivial handler for testing list_actions.
    struct RebootAction;

    impl ActionHandler for RebootAction {
        fn name(&self) -> &str {
            "reboot"
        }

        fn invoke(&self, _entity: &HDict, _action: &str, _args: &HDict) -> Result<HGrid, String> {
            Ok(HGrid::new())
        }
    }

    #[test]
    fn invoke_known_action() {
        let mut registry = ActionRegistry::new();
        registry.register(Box::new(EchoAction));

        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val("equip-1")));
        entity.set("equip", Kind::Marker);

        let result = registry.invoke(&entity, "echo", &HDict::new());
        assert!(result.is_ok());

        let grid = result.unwrap();
        assert_eq!(grid.len(), 1);
        let row = grid.row(0).unwrap();
        assert_eq!(row.get("action"), Some(&Kind::Str("echo".to_string())));
        assert_eq!(row.get("result"), Some(&Kind::Str("ok".to_string())));
    }

    #[test]
    fn invoke_unknown_action_returns_error() {
        let registry = ActionRegistry::new();

        let entity = HDict::new();
        let result = registry.invoke(&entity, "nonexistent", &HDict::new());

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("unknown action"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn list_actions_returns_registered_names() {
        let mut registry = ActionRegistry::new();
        registry.register(Box::new(EchoAction));
        registry.register(Box::new(RebootAction));

        let mut names = registry.list_actions();
        names.sort();
        assert_eq!(names, vec!["echo".to_string(), "reboot".to_string()]);
    }

    #[test]
    fn empty_registry_has_no_actions() {
        let registry = ActionRegistry::new();
        assert!(registry.list_actions().is_empty());
    }

    #[test]
    fn register_replaces_existing_handler() {
        let mut registry = ActionRegistry::new();
        registry.register(Box::new(EchoAction));
        // Register another handler with the same name
        registry.register(Box::new(EchoAction));

        let names = registry.list_actions();
        assert_eq!(names.len(), 1);
    }
}
