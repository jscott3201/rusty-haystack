// Xeto AST nodes -- the output of parsing a `.xeto` file.

use std::collections::HashMap;

use crate::kinds::Kind;

/// A slot (child tag) definition within a spec.
#[derive(Debug, Clone)]
pub struct SlotDef {
    /// Slot name.
    pub name: String,
    /// Type reference (e.g. `"Str"`, `"Number"`, `"Ref"`).
    pub type_ref: Option<String>,
    /// Metadata tags from angle-bracket meta section.
    pub meta: HashMap<String, Kind>,
    /// Default value.
    pub default: Option<Kind>,
    /// True if this slot is a marker (no type, no value).
    pub is_marker: bool,
    /// True if this slot is a query (type is `Query<...>`).
    pub is_query: bool,
    /// True if this slot has the `?` suffix (optional).
    pub is_maybe: bool,
    /// True if prefixed with `*` (global slot).
    pub is_global: bool,
    /// For query slots: the `of` parameter type.
    pub query_of: Option<String>,
    /// For query slots: the `via` parameter path.
    pub query_via: Option<String>,
    /// For query slots: the `inverse` parameter path.
    pub query_inverse: Option<String>,
    /// Nested child slots.
    pub children: Vec<SlotDef>,
    /// Doc comment text (collected from `//` comments before this slot).
    pub doc: String,
}

impl SlotDef {
    /// Create a new empty slot definition with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_ref: None,
            meta: HashMap::new(),
            default: None,
            is_marker: false,
            is_query: false,
            is_maybe: false,
            is_global: false,
            query_of: None,
            query_via: None,
            query_inverse: None,
            children: Vec::new(),
            doc: String::new(),
        }
    }
}

/// A top-level spec (type) definition.
#[derive(Debug, Clone)]
pub struct SpecDef {
    /// Spec name.
    pub name: String,
    /// Base type reference (after the `:`).
    pub base: Option<String>,
    /// Metadata tags from angle-bracket meta section.
    pub meta: HashMap<String, Kind>,
    /// Child slot definitions within the `{ }` body.
    pub slots: Vec<SlotDef>,
    /// Doc comment text.
    pub doc: String,
    /// Default value.
    pub default: Option<Kind>,
}

impl SpecDef {
    /// Create a new empty spec definition with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base: None,
            meta: HashMap::new(),
            slots: Vec::new(),
            doc: String::new(),
            default: None,
        }
    }
}

/// Library pragma at the top of a `.xeto` file.
#[derive(Debug, Clone)]
pub struct LibPragma {
    /// Library name.
    pub name: String,
    /// Library version string.
    pub version: String,
    /// Doc comment text.
    pub doc: String,
    /// Dependent library names.
    pub depends: Vec<String>,
    /// Additional metadata tags.
    pub meta: HashMap<String, Kind>,
}

/// Parsed representation of a `.xeto` file.
#[derive(Debug, Clone)]
pub struct XetoFile {
    /// Optional library pragma.
    pub pragma: Option<LibPragma>,
    /// Top-level spec definitions.
    pub specs: Vec<SpecDef>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_def_new() {
        let slot = SlotDef::new("discharge");
        assert_eq!(slot.name, "discharge");
        assert!(slot.type_ref.is_none());
        assert!(!slot.is_marker);
        assert!(!slot.is_query);
        assert!(!slot.is_maybe);
        assert!(!slot.is_global);
        assert!(slot.children.is_empty());
    }

    #[test]
    fn spec_def_new() {
        let spec = SpecDef::new("Ahu");
        assert_eq!(spec.name, "Ahu");
        assert!(spec.base.is_none());
        assert!(spec.slots.is_empty());
    }

    #[test]
    fn xeto_file_empty() {
        let file = XetoFile {
            pragma: None,
            specs: vec![],
        };
        assert!(file.pragma.is_none());
        assert!(file.specs.is_empty());
    }
}
