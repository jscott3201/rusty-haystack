// Haystack singleton types: Marker, NA, Remove

/// Marker tag — boolean presence indicator.
/// In Zinc: `M`. In JSON: `{"_kind": "marker"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Marker;

/// Not Available — missing or invalid data sentinel.
/// In Zinc: `NA`. In JSON: `{"_kind": "na"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NA;

/// Remove — tag removal in diff/update operations.
/// In Zinc: `R`. In JSON: `{"_kind": "remove"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Remove;

impl std::fmt::Display for Marker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "\u{2713}") // ✓
    }
}

impl std::fmt::Display for NA {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NA")
    }
}

impl std::fmt::Display for Remove {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "remove")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_display() {
        assert_eq!(Marker.to_string(), "\u{2713}");
    }

    #[test]
    fn marker_equality() {
        assert_eq!(Marker, Marker);
    }

    #[test]
    fn na_display() {
        assert_eq!(NA.to_string(), "NA");
    }

    #[test]
    fn remove_display() {
        assert_eq!(Remove.to_string(), "remove");
    }

    #[test]
    fn singletons_are_copy() {
        let m = Marker;
        let m2 = m;
        assert_eq!(m, m2);
    }

    #[test]
    fn singletons_are_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Marker);
        assert!(set.contains(&Marker));
    }
}
