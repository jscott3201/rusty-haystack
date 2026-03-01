use std::fmt;

/// Reference to a def/tag definition name.
/// Zinc: `^hot-water`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol(pub String);

impl Symbol {
    pub fn new(val: impl Into<String>) -> Self {
        Self(val.into())
    }

    pub fn val(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "^{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_display() {
        let s = Symbol::new("hot-water");
        assert_eq!(s.to_string(), "^hot-water");
    }

    #[test]
    fn symbol_equality() {
        assert_eq!(Symbol::new("site"), Symbol::new("site"));
        assert_ne!(Symbol::new("site"), Symbol::new("equip"));
    }

    #[test]
    fn symbol_val() {
        assert_eq!(Symbol::new("ahu").val(), "ahu");
    }
}
