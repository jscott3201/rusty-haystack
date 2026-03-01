// Xeto name resolver -- resolves unqualified spec names to fully qualified names.

use std::collections::{HashMap, HashSet};

use super::XetoError;

/// Resolves unqualified Xeto spec names to fully qualified `lib::Name` forms.
///
/// Resolution order:
/// 1. Already qualified (contains `::`): return as-is
/// 2. Current library's own specs
/// 3. Declared dependencies
/// 4. `"sys"` builtins
/// 5. All known libraries (fallback)
pub struct XetoResolver {
    /// Library name -> set of spec names defined in that library.
    lib_specs: HashMap<String, HashSet<String>>,
    /// Library name -> list of dependency library names.
    lib_depends: HashMap<String, Vec<String>>,
}

impl XetoResolver {
    /// Create a new empty resolver.
    pub fn new() -> Self {
        Self {
            lib_specs: HashMap::new(),
            lib_depends: HashMap::new(),
        }
    }

    /// Register a library with its spec names and dependency list.
    pub fn add_lib(&mut self, lib_name: &str, spec_names: HashSet<String>, depends: Vec<String>) {
        self.lib_specs.insert(lib_name.to_string(), spec_names);
        self.lib_depends.insert(lib_name.to_string(), depends);
    }

    /// Resolve a spec name to a fully qualified name within the context of a library.
    ///
    /// Returns `None` if the name cannot be resolved.
    pub fn resolve(&self, name: &str, context_lib: &str) -> Option<String> {
        // 1. Already qualified
        if name.contains("::") {
            return Some(name.to_string());
        }

        // 2. Current library's own specs
        if let Some(specs) = self.lib_specs.get(context_lib) {
            if specs.contains(name) {
                return Some(format!("{}::{}", context_lib, name));
            }
        }

        // 3. Declared dependencies
        if let Some(deps) = self.lib_depends.get(context_lib) {
            for dep in deps {
                if let Some(specs) = self.lib_specs.get(dep.as_str()) {
                    if specs.contains(name) {
                        return Some(format!("{}::{}", dep, name));
                    }
                }
            }
        }

        // 4. sys builtins
        if let Some(specs) = self.lib_specs.get("sys") {
            if specs.contains(name) {
                return Some(format!("sys::{}", name));
            }
        }

        // 5. All known libraries (fallback)
        for (lib_name, specs) in &self.lib_specs {
            if lib_name == context_lib || lib_name == "sys" {
                continue;
            }
            if specs.contains(name) {
                return Some(format!("{}::{}", lib_name, name));
            }
        }

        None
    }

    /// Compute a topological ordering of libraries based on dependencies.
    ///
    /// Returns an error if a circular dependency is detected.
    pub fn dependency_order(&self) -> Result<Vec<String>, XetoError> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut in_progress = HashSet::new();

        // Process all known libraries
        let all_libs: Vec<String> = self.lib_specs.keys().cloned().collect();
        for lib in &all_libs {
            if !visited.contains(lib.as_str()) {
                self.topo_visit(lib, &mut visited, &mut in_progress, &mut result)?;
            }
        }

        Ok(result)
    }

    /// Recursive DFS for topological sort with cycle detection.
    fn topo_visit(
        &self,
        lib: &str,
        visited: &mut HashSet<String>,
        in_progress: &mut HashSet<String>,
        result: &mut Vec<String>,
    ) -> Result<(), XetoError> {
        if in_progress.contains(lib) {
            return Err(XetoError::Resolve(format!(
                "circular dependency detected involving '{}'",
                lib
            )));
        }
        if visited.contains(lib) {
            return Ok(());
        }

        in_progress.insert(lib.to_string());

        // Visit dependencies first
        if let Some(deps) = self.lib_depends.get(lib) {
            for dep in deps {
                self.topo_visit(dep, visited, in_progress, result)?;
            }
        }

        in_progress.remove(lib);
        visited.insert(lib.to_string());
        result.push(lib.to_string());

        Ok(())
    }
}

impl Default for XetoResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_resolver() -> XetoResolver {
        let mut resolver = XetoResolver::new();

        let mut sys_specs = HashSet::new();
        sys_specs.insert("Str".to_string());
        sys_specs.insert("Number".to_string());
        sys_specs.insert("Ref".to_string());
        sys_specs.insert("Bool".to_string());
        resolver.add_lib("sys", sys_specs, vec![]);

        let mut ph_specs = HashSet::new();
        ph_specs.insert("Entity".to_string());
        ph_specs.insert("Site".to_string());
        resolver.add_lib("ph", ph_specs, vec!["sys".to_string()]);

        let mut phiot_specs = HashSet::new();
        phiot_specs.insert("Equip".to_string());
        phiot_specs.insert("Point".to_string());
        phiot_specs.insert("Ahu".to_string());
        resolver.add_lib(
            "phIoT",
            phiot_specs,
            vec!["ph".to_string(), "sys".to_string()],
        );

        resolver
    }

    #[test]
    fn resolve_qualified_returns_as_is() {
        let resolver = make_resolver();
        assert_eq!(
            resolver.resolve("ph::Entity", "phIoT"),
            Some("ph::Entity".to_string())
        );
    }

    #[test]
    fn resolve_finds_in_current_lib() {
        let resolver = make_resolver();
        assert_eq!(
            resolver.resolve("Ahu", "phIoT"),
            Some("phIoT::Ahu".to_string())
        );
    }

    #[test]
    fn resolve_finds_in_dependencies() {
        let resolver = make_resolver();
        assert_eq!(
            resolver.resolve("Entity", "phIoT"),
            Some("ph::Entity".to_string())
        );
    }

    #[test]
    fn resolve_finds_in_sys() {
        let resolver = make_resolver();
        assert_eq!(
            resolver.resolve("Str", "phIoT"),
            Some("sys::Str".to_string())
        );
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let resolver = make_resolver();
        assert_eq!(resolver.resolve("UnknownType", "phIoT"), None);
    }

    #[test]
    fn resolve_fallback_to_all_libs() {
        // ph is not in phIoT's deps directly but has Entity
        // Since ph IS in deps, let's test with a lib that
        // doesn't depend on ph but can still find it via fallback
        let mut resolver = XetoResolver::new();

        let mut ph_specs = HashSet::new();
        ph_specs.insert("Entity".to_string());
        resolver.add_lib("ph", ph_specs, vec![]);

        let mut other_specs = HashSet::new();
        other_specs.insert("Foo".to_string());
        resolver.add_lib("other", other_specs, vec![]); // no deps

        // "Entity" is not in other's deps, but should be found via fallback
        let result = resolver.resolve("Entity", "other");
        assert_eq!(result, Some("ph::Entity".to_string()));
    }

    #[test]
    fn dependency_ordering() {
        let resolver = make_resolver();
        let order = resolver.dependency_order().unwrap();

        // sys should come before ph, ph before phIoT
        let sys_pos = order.iter().position(|s| s == "sys").unwrap();
        let ph_pos = order.iter().position(|s| s == "ph").unwrap();
        let phiot_pos = order.iter().position(|s| s == "phIoT").unwrap();

        assert!(sys_pos < ph_pos);
        assert!(ph_pos < phiot_pos);
    }

    #[test]
    fn circular_dependency_detected() {
        let mut resolver = XetoResolver::new();

        let a_specs = HashSet::new();
        resolver.add_lib("a", a_specs, vec!["b".to_string()]);

        let b_specs = HashSet::new();
        resolver.add_lib("b", b_specs, vec!["a".to_string()]);

        let result = resolver.dependency_order();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("circular dependency"));
    }

    #[test]
    fn dependency_order_single_lib() {
        let mut resolver = XetoResolver::new();
        let specs = HashSet::new();
        resolver.add_lib("solo", specs, vec![]);

        let order = resolver.dependency_order().unwrap();
        assert_eq!(order, vec!["solo"]);
    }

    #[test]
    fn resolve_sys_builtin_from_any_context() {
        let resolver = make_resolver();
        assert_eq!(
            resolver.resolve("Number", "ph"),
            Some("sys::Number".to_string())
        );
    }
}
