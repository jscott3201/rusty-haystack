// Haystack Ref — an entity reference.

use std::fmt;
use std::hash::{Hash, Hasher};

/// Haystack Ref — an entity reference.
///
/// `val` is the identifier (alphanumeric, `_`, `:`, `-`, `.`, `~`).
/// `dis` is an optional display name (cosmetic — ignored in equality/hash).
///
/// Zinc: `@abc-123` or `@abc-123 "Display Name"`.
#[derive(Debug, Clone)]
pub struct HRef {
    pub val: String,
    pub dis: Option<String>,
}

impl HRef {
    pub fn new(val: impl Into<String>, dis: Option<String>) -> Self {
        Self {
            val: val.into(),
            dis,
        }
    }

    pub fn from_val(val: impl Into<String>) -> Self {
        Self {
            val: val.into(),
            dis: None,
        }
    }
}

impl PartialEq for HRef {
    fn eq(&self, other: &Self) -> bool {
        self.val == other.val
    }
}

impl Eq for HRef {}

impl Hash for HRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.val.hash(state);
    }
}

impl fmt::Display for HRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "@{}", self.val)?;
        if let Some(ref dis) = self.dis {
            write!(f, " '{dis}'")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ref_display_without_dis() {
        let r = HRef::from_val("site-1");
        assert_eq!(r.to_string(), "@site-1");
    }

    #[test]
    fn ref_display_with_dis() {
        let r = HRef::new("site-1", Some("Main Site".into()));
        assert_eq!(r.to_string(), "@site-1 'Main Site'");
    }

    #[test]
    fn ref_equality_ignores_dis() {
        let a = HRef::new("site-1", Some("Building A".into()));
        let b = HRef::new("site-1", Some("Different Name".into()));
        let c = HRef::from_val("site-1");
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn ref_hash_ignores_dis() {
        let a = HRef::new("site-1", Some("A".into()));
        let b = HRef::new("site-1", Some("B".into()));
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn ref_inequality() {
        let a = HRef::from_val("site-1");
        let b = HRef::from_val("site-2");
        assert_ne!(a, b);
    }

    #[test]
    fn ref_from_val_convenience() {
        let r = HRef::from_val("abc");
        assert_eq!(r.val, "abc");
        assert_eq!(r.dis, None);
    }
}
