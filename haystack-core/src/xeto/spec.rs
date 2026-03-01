// Xeto resolved Spec and Slot types.

use std::collections::HashMap;

use crate::kinds::Kind;

/// A resolved slot within a Spec.
#[derive(Debug, Clone)]
pub struct Slot {
    /// Slot name.
    pub name: String,
    /// Type reference (e.g. `"Str"`, `"Number"`).
    pub type_ref: Option<String>,
    /// Metadata tags.
    pub meta: HashMap<String, Kind>,
    /// Default value.
    pub default: Option<Kind>,
    /// Whether this slot is a marker (no type/value).
    pub is_marker: bool,
    /// Whether this slot is a query.
    pub is_query: bool,
    /// Nested child slots.
    pub children: Vec<Slot>,
}

impl Slot {
    /// Returns true if this slot has the "maybe" meta tag (optional).
    pub fn is_maybe(&self) -> bool {
        self.meta.contains_key("maybe")
    }
}

/// A resolved Xeto spec (type definition).
#[derive(Debug, Clone)]
pub struct Spec {
    /// Fully qualified name (`"lib::Name"`).
    pub qname: String,
    /// Short name (`"Name"`).
    pub name: String,
    /// Library name (`"lib"`).
    pub lib: String,
    /// Base spec qualified name.
    pub base: Option<String>,
    /// Metadata tags.
    pub meta: HashMap<String, Kind>,
    /// Slots (child tag definitions).
    pub slots: Vec<Slot>,
    /// Whether this spec is abstract.
    pub is_abstract: bool,
    /// Documentation string.
    pub doc: String,
}

impl Spec {
    /// Create a new spec with the given qualified name.
    pub fn new(qname: impl Into<String>, lib: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            qname: qname.into(),
            name: name.into(),
            lib: lib.into(),
            base: None,
            meta: HashMap::new(),
            slots: Vec::new(),
            is_abstract: false,
            doc: String::new(),
        }
    }

    /// All marker slot names.
    pub fn markers(&self) -> Vec<&str> {
        self.slots
            .iter()
            .filter(|s| s.is_marker)
            .map(|s| s.name.as_str())
            .collect()
    }

    /// Mandatory marker slot names (those without "maybe" meta).
    pub fn mandatory_markers(&self) -> Vec<&str> {
        self.slots
            .iter()
            .filter(|s| s.is_marker && !s.is_maybe())
            .map(|s| s.name.as_str())
            .collect()
    }

    /// Collect all slots including inherited ones from the base chain.
    pub fn effective_slots(&self, specs: &HashMap<String, Spec>) -> Vec<Slot> {
        let mut visited = std::collections::HashSet::new();
        self.collect_effective_slots(specs, &mut visited)
    }

    fn collect_effective_slots(
        &self,
        specs: &HashMap<String, Spec>,
        visited: &mut std::collections::HashSet<String>,
    ) -> Vec<Slot> {
        if !visited.insert(self.qname.clone()) {
            return Vec::new();
        }
        let mut inherited: Vec<Slot> = Vec::new();
        if let Some(ref base_name) = self.base {
            if let Some(base_spec) = specs.get(base_name) {
                inherited = base_spec.collect_effective_slots(specs, visited);
            }
        }
        let own_names: std::collections::HashSet<&str> =
            self.slots.iter().map(|s| s.name.as_str()).collect();
        let mut result: Vec<Slot> = inherited
            .into_iter()
            .filter(|s| !own_names.contains(s.name.as_str()))
            .collect();
        result.extend(self.slots.iter().cloned());
        result
    }

    /// Children of the "points" slot, if present.
    pub fn point_specs(&self) -> Vec<&Slot> {
        self.slots
            .iter()
            .find(|s| s.name == "points")
            .map(|s| s.children.iter().collect())
            .unwrap_or_default()
    }
}

/// Convert an AST `SlotDef` into a resolved `Slot`.
impl From<&super::ast::SlotDef> for Slot {
    fn from(def: &super::ast::SlotDef) -> Self {
        Slot {
            name: def.name.clone(),
            type_ref: def.type_ref.clone(),
            meta: def.meta.clone(),
            default: def.default.clone(),
            is_marker: def.is_marker,
            is_query: def.is_query,
            children: def.children.iter().map(Slot::from).collect(),
        }
    }
}

/// Convert an AST `SpecDef` into a resolved `Spec`.
pub fn spec_from_def(
    def: &super::ast::SpecDef,
    lib_name: &str,
) -> Spec {
    let qname = format!("{}::{}", lib_name, def.name);
    let is_abstract = def.meta.contains_key("abstract");
    Spec {
        qname,
        name: def.name.clone(),
        lib: lib_name.to_string(),
        base: def.base.clone(),
        meta: def.meta.clone(),
        slots: def.slots.iter().map(Slot::from).collect(),
        is_abstract,
        doc: def.doc.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xeto::ast::SlotDef;

    fn make_marker_slot(name: &str) -> Slot {
        Slot {
            name: name.to_string(),
            type_ref: None,
            meta: HashMap::new(),
            default: None,
            is_marker: true,
            is_query: false,
            children: Vec::new(),
        }
    }

    fn make_typed_slot(name: &str, type_ref: &str) -> Slot {
        Slot {
            name: name.to_string(),
            type_ref: Some(type_ref.to_string()),
            meta: HashMap::new(),
            default: None,
            is_marker: false,
            is_query: false,
            children: Vec::new(),
        }
    }

    fn make_maybe_marker(name: &str) -> Slot {
        let mut meta = HashMap::new();
        meta.insert("maybe".to_string(), Kind::Marker);
        Slot {
            name: name.to_string(),
            type_ref: None,
            meta,
            default: None,
            is_marker: true,
            is_query: false,
            children: Vec::new(),
        }
    }

    #[test]
    fn spec_markers() {
        let mut spec = Spec::new("test::Ahu", "test", "Ahu");
        spec.slots.push(make_marker_slot("hot"));
        spec.slots.push(make_marker_slot("cold"));
        spec.slots.push(make_typed_slot("dis", "Str"));

        let markers = spec.markers();
        assert_eq!(markers, vec!["hot", "cold"]);
    }

    #[test]
    fn spec_mandatory_markers() {
        let mut spec = Spec::new("test::Ahu", "test", "Ahu");
        spec.slots.push(make_marker_slot("hot"));
        spec.slots.push(make_maybe_marker("cold"));

        let mandatory = spec.mandatory_markers();
        assert_eq!(mandatory, vec!["hot"]);
    }

    #[test]
    fn spec_point_specs() {
        let mut spec = Spec::new("test::Ahu", "test", "Ahu");
        let points_slot = Slot {
            name: "points".to_string(),
            type_ref: None,
            meta: HashMap::new(),
            default: None,
            is_marker: false,
            is_query: true,
            children: vec![
                make_typed_slot("temp", "Point"),
                make_typed_slot("flow", "Point"),
            ],
        };
        spec.slots.push(points_slot);

        let points = spec.point_specs();
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].name, "temp");
        assert_eq!(points[1].name, "flow");
    }

    #[test]
    fn spec_point_specs_no_points_slot() {
        let spec = Spec::new("test::Ahu", "test", "Ahu");
        assert!(spec.point_specs().is_empty());
    }

    #[test]
    fn slot_is_maybe() {
        let mut slot = make_marker_slot("optional");
        assert!(!slot.is_maybe());
        slot.meta.insert("maybe".to_string(), Kind::Marker);
        assert!(slot.is_maybe());
    }

    #[test]
    fn slot_from_ast_slot_def() {
        let mut def = SlotDef::new("dis");
        def.type_ref = Some("Str".to_string());
        def.is_marker = false;
        let slot = Slot::from(&def);
        assert_eq!(slot.name, "dis");
        assert_eq!(slot.type_ref.as_deref(), Some("Str"));
    }

    #[test]
    fn spec_from_def_conversion() {
        let mut def = crate::xeto::ast::SpecDef::new("Ahu");
        def.base = Some("Equip".to_string());
        def.meta.insert("abstract".to_string(), Kind::Marker);
        def.doc = "An AHU".to_string();

        let spec = spec_from_def(&def, "phIoT");
        assert_eq!(spec.qname, "phIoT::Ahu");
        assert_eq!(spec.name, "Ahu");
        assert_eq!(spec.lib, "phIoT");
        assert_eq!(spec.base.as_deref(), Some("Equip"));
        assert!(spec.is_abstract);
        assert_eq!(spec.doc, "An AHU");
    }

    #[test]
    fn spec_new() {
        let spec = Spec::new("mylib::Foo", "mylib", "Foo");
        assert_eq!(spec.qname, "mylib::Foo");
        assert_eq!(spec.name, "Foo");
        assert_eq!(spec.lib, "mylib");
        assert!(spec.base.is_none());
        assert!(!spec.is_abstract);
        assert!(spec.slots.is_empty());
    }

    #[test]
    fn effective_slots_cycle_does_not_stackoverflow() {
        // Create two specs that refer to each other as base
        let mut spec_a = Spec::new("test::A", "test", "A");
        spec_a.base = Some("test::B".to_string());
        spec_a.slots.push(make_marker_slot("tagA"));

        let mut spec_b = Spec::new("test::B", "test", "B");
        spec_b.base = Some("test::A".to_string());
        spec_b.slots.push(make_marker_slot("tagB"));

        let mut specs = HashMap::new();
        specs.insert("test::A".to_string(), spec_a.clone());
        specs.insert("test::B".to_string(), spec_b.clone());

        // Should not stack overflow — returns own slots plus whatever it can
        // safely collect before hitting the cycle.
        let slots_a = spec_a.effective_slots(&specs);
        let names_a: Vec<&str> = slots_a.iter().map(|s| s.name.as_str()).collect();
        assert!(names_a.contains(&"tagA"), "A should have its own tagA");

        let slots_b = spec_b.effective_slots(&specs);
        let names_b: Vec<&str> = slots_b.iter().map(|s| s.name.as_str()).collect();
        assert!(names_b.contains(&"tagB"), "B should have its own tagB");
    }

    #[test]
    fn effective_slots_includes_inherited() {
        let mut parent = Spec::new("test::Parent", "test", "Parent");
        parent.slots.push(make_marker_slot("equip"));
        parent.slots.push(make_typed_slot("dis", "Str"));

        let mut child = Spec::new("test::Child", "test", "Child");
        child.base = Some("test::Parent".to_string());
        child.slots.push(make_marker_slot("ahu"));
        // Override dis with own version
        child.slots.push(make_typed_slot("dis", "Str"));

        let mut specs = HashMap::new();
        specs.insert("test::Parent".to_string(), parent);
        specs.insert("test::Child".to_string(), child.clone());

        let effective = child.effective_slots(&specs);
        let names: Vec<&str> = effective.iter().map(|s| s.name.as_str()).collect();

        // Should include inherited "equip" plus own "ahu" and "dis" (own overrides parent)
        assert!(names.contains(&"equip"), "should inherit equip from parent");
        assert!(names.contains(&"ahu"), "should have own ahu");
        assert!(names.contains(&"dis"), "should have dis");
        // No duplicates for overridden "dis"
        let dis_count = names.iter().filter(|n| **n == "dis").count();
        assert_eq!(dis_count, 1, "dis should not be duplicated");
    }
}
