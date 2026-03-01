use std::fmt;

/// Extended typed string — vendor-specific scalar.
/// Zinc: `Color("red")`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct XStr {
    pub type_name: String,
    pub val: String,
}

impl XStr {
    pub fn new(type_name: impl Into<String>, val: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            val: val.into(),
        }
    }
}

impl fmt::Display for XStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(\"{}\")", self.type_name, self.val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xstr_display() {
        let x = XStr::new("Color", "red");
        assert_eq!(x.to_string(), "Color(\"red\")");
    }

    #[test]
    fn xstr_equality() {
        assert_eq!(XStr::new("Color", "red"), XStr::new("Color", "red"));
        assert_ne!(XStr::new("Color", "red"), XStr::new("Color", "blue"));
    }
}
