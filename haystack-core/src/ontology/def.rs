// Haystack 4 def record -- a single definition in the ontology.

use crate::data::HDict;

/// Classification of a Haystack 4 def.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefKind {
    /// Marker tag (default).
    Marker,
    /// Value type (val or scalar).
    Val,
    /// Entity type.
    Entity,
    /// Feature namespace.
    Feature,
    /// Compound tag (has "-" in symbol, e.g. "hot-water").
    Conjunct,
    /// Choice option.
    Choice,
    /// Library def.
    Lib,
}

/// A single Haystack 4 definition loaded from Trio.
///
/// Each def has a symbol (name), belongs to a library, and has an
/// inheritance chain via the `is` tag. Additional metadata such as
/// `tagOn`, `of`, `mandatory`, and `doc` describe how the def relates
/// to entity types and other defs.
#[derive(Debug, Clone)]
pub struct Def {
    /// Def name, e.g. `"ahu"` or `"lib:phIoT"`.
    pub symbol: String,
    /// Library name, e.g. `"phIoT"`.
    pub lib: String,
    /// Supertype symbols from the `is` tag.
    pub is_: Vec<String>,
    /// Entity types this tag applies to (`tagOn`).
    pub tag_on: Vec<String>,
    /// Target type for refs/choices (`of` tag).
    pub of: Option<String>,
    /// Whether this tag is mandatory on entities.
    pub mandatory: bool,
    /// Human-readable documentation string.
    pub doc: String,
    /// Full HDict of all meta tags on this def.
    pub tags: HDict,
}

impl Def {
    /// Derive the def kind from its supertype chain.
    ///
    /// Priority order:
    /// 1. `"lib"` in is_ -> `DefKind::Lib`
    /// 2. `"choice"` in is_ -> `DefKind::Choice`
    /// 3. `"entity"` in is_ -> `DefKind::Entity`
    /// 4. `"val"` or `"scalar"` in is_ -> `DefKind::Val`
    /// 5. `"feature"` in is_ -> `DefKind::Feature`
    /// 6. `"-"` in symbol -> `DefKind::Conjunct`
    /// 7. default -> `DefKind::Marker`
    pub fn kind(&self) -> DefKind {
        if self.is_.iter().any(|s| s == "lib") {
            return DefKind::Lib;
        }
        if self.is_.iter().any(|s| s == "choice") {
            return DefKind::Choice;
        }
        if self.is_.iter().any(|s| s == "entity") {
            return DefKind::Entity;
        }
        if self.is_.iter().any(|s| s == "val" || s == "scalar") {
            return DefKind::Val;
        }
        if self.is_.iter().any(|s| s == "feature") {
            return DefKind::Feature;
        }
        if self.symbol.contains('-') {
            return DefKind::Conjunct;
        }
        DefKind::Marker
    }
}

impl PartialEq for Def {
    fn eq(&self, other: &Self) -> bool {
        self.symbol == other.symbol && self.lib == other.lib
    }
}

impl Eq for Def {}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_def(symbol: &str, is_: &[&str]) -> Def {
        Def {
            symbol: symbol.to_string(),
            lib: "test".to_string(),
            is_: is_.iter().map(|s| s.to_string()).collect(),
            tag_on: vec![],
            of: None,
            mandatory: false,
            doc: String::new(),
            tags: HDict::new(),
        }
    }

    #[test]
    fn kind_marker_default() {
        let d = make_def("site", &["marker"]);
        assert_eq!(d.kind(), DefKind::Marker);
    }

    #[test]
    fn kind_lib() {
        let d = make_def("lib:phIoT", &["lib"]);
        assert_eq!(d.kind(), DefKind::Lib);
    }

    #[test]
    fn kind_choice() {
        let d = make_def("ahuZoneDelivery", &["choice"]);
        assert_eq!(d.kind(), DefKind::Choice);
    }

    #[test]
    fn kind_entity() {
        let d = make_def("site", &["entity"]);
        assert_eq!(d.kind(), DefKind::Entity);
    }

    #[test]
    fn kind_val() {
        let d = make_def("number", &["val"]);
        assert_eq!(d.kind(), DefKind::Val);
    }

    #[test]
    fn kind_val_via_scalar() {
        let d = make_def("bool", &["scalar"]);
        assert_eq!(d.kind(), DefKind::Val);
    }

    #[test]
    fn kind_feature() {
        let d = make_def("filetype", &["feature"]);
        assert_eq!(d.kind(), DefKind::Feature);
    }

    #[test]
    fn kind_conjunct() {
        let d = make_def("hot-water", &["marker"]);
        // Even though is_ has "marker", the "-" in symbol wins for conjunct
        // Actually, conjunct only triggers if none of the higher-priority
        // checks match. "marker" doesn't match any priority check, so
        // the "-" in symbol triggers conjunct.
        assert_eq!(d.kind(), DefKind::Conjunct);
    }

    #[test]
    fn kind_lib_takes_priority_over_conjunct() {
        // Lib check has higher priority than conjunct
        let d = make_def("lib:ph-test", &["lib"]);
        assert_eq!(d.kind(), DefKind::Lib);
    }

    #[test]
    fn def_equality() {
        let a = make_def("site", &["marker"]);
        let b = make_def("site", &["marker"]);
        assert_eq!(a, b);
    }

    #[test]
    fn def_inequality() {
        let a = make_def("site", &["marker"]);
        let b = make_def("equip", &["marker"]);
        assert_ne!(a, b);
    }
}
