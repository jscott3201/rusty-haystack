//! Domain scoping for federated queries.

use std::collections::HashSet;

/// Scopes federation queries to specific connector domains.
///
/// A `None` domains set means wildcard — all connectors are in scope.
/// An empty set means no connectors are in scope.
#[derive(Debug, Clone)]
pub struct DomainScope {
    domains: Option<HashSet<String>>,
}

impl DomainScope {
    /// Create a wildcard scope that includes all connectors.
    pub fn all() -> Self {
        Self { domains: None }
    }

    /// Create a scope limited to specific domains.
    pub fn scoped(domains: impl IntoIterator<Item = String>) -> Self {
        Self {
            domains: Some(domains.into_iter().collect()),
        }
    }

    /// Check if a domain is included in this scope.
    /// Connectors with `None` domain are always included in any scope.
    pub fn includes(&self, domain: Option<&str>) -> bool {
        match (&self.domains, domain) {
            (None, _) => true, // wildcard scope includes everything
            (_, None) => true, // unscoped connector is always included
            (Some(set), Some(d)) => set.contains(d),
        }
    }

    /// Check if this is a wildcard scope.
    pub fn is_wildcard(&self) -> bool {
        self.domains.is_none()
    }

    /// Get the domain set, if scoped.
    pub fn domains(&self) -> Option<&HashSet<String>> {
        self.domains.as_ref()
    }
}

impl Default for DomainScope {
    fn default() -> Self {
        Self::all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_includes_everything() {
        let scope = DomainScope::all();
        assert!(scope.includes(Some("a")));
        assert!(scope.includes(Some("b")));
        assert!(scope.includes(None));
    }

    #[test]
    fn scoped_includes_matching() {
        let scope = DomainScope::scoped(["a".to_string(), "b".to_string()]);
        assert!(scope.includes(Some("a")));
        assert!(scope.includes(Some("b")));
    }

    #[test]
    fn scoped_excludes_non_matching() {
        let scope = DomainScope::scoped(["a".to_string()]);
        assert!(!scope.includes(Some("c")));
    }

    #[test]
    fn unscoped_connector_always_included() {
        let scope = DomainScope::scoped(["a".to_string()]);
        assert!(scope.includes(None));
    }

    #[test]
    fn default_is_wildcard() {
        let scope = DomainScope::default();
        assert!(scope.is_wildcard());
    }

    #[test]
    fn empty_scope_includes_unscoped() {
        let scope = DomainScope::scoped(std::iter::empty::<String>());
        assert!(scope.includes(None));
        assert!(!scope.includes(Some("any")));
    }
}
