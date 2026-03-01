// Lib -- a library (namespace) of Haystack 4 defs.

use std::collections::HashMap;

use super::def::Def;

/// A library of Haystack 4 definitions.
///
/// Each library groups related defs under a name (e.g. `"phIoT"`) and
/// tracks its version, documentation, and dependencies on other libraries.
#[derive(Debug, Clone)]
pub struct Lib {
    /// Library name, e.g. `"phIoT"`.
    pub name: String,
    /// Version string, e.g. `"4.0.0"`.
    pub version: String,
    /// Library description.
    pub doc: String,
    /// Names of dependent libraries.
    pub depends: Vec<String>,
    /// Symbol -> Def mapping.
    pub defs: HashMap<String, Def>,
}

impl Lib {
    /// Create a new library with the given name and empty defaults.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: String::new(),
            doc: String::new(),
            depends: vec![],
            defs: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::HDict;

    #[test]
    fn new_lib_defaults() {
        let lib = Lib::new("phIoT");
        assert_eq!(lib.name, "phIoT");
        assert_eq!(lib.version, "");
        assert_eq!(lib.doc, "");
        assert!(lib.depends.is_empty());
        assert!(lib.defs.is_empty());
    }

    #[test]
    fn lib_with_defs() {
        let mut lib = Lib::new("test");
        lib.version = "1.0.0".to_string();
        lib.defs.insert(
            "site".to_string(),
            Def {
                symbol: "site".to_string(),
                lib: "test".to_string(),
                is_: vec!["marker".to_string()],
                tag_on: vec![],
                of: None,
                mandatory: false,
                doc: "A site".to_string(),
                tags: HDict::new(),
            },
        );
        assert_eq!(lib.defs.len(), 1);
        assert!(lib.defs.contains_key("site"));
    }
}
