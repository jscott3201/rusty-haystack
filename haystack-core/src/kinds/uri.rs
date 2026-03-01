use std::fmt;

/// Universal Resource Identifier.
/// Zinc: `` `http://example.com` ``
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Uri(pub String);

impl Uri {
    pub fn new(val: impl Into<String>) -> Self {
        Self(val.into())
    }

    pub fn val(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Uri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_display() {
        let u = Uri::new("http://example.com");
        assert_eq!(u.to_string(), "http://example.com");
    }

    #[test]
    fn uri_equality() {
        assert_eq!(Uri::new("http://a.com"), Uri::new("http://a.com"));
        assert_ne!(Uri::new("http://a.com"), Uri::new("http://b.com"));
    }

    #[test]
    fn uri_val() {
        let u = Uri::new("http://example.com");
        assert_eq!(u.val(), "http://example.com");
    }
}
