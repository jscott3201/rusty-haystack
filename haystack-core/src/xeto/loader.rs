//! Xeto library loader — parses .xeto source, resolves names, produces Specs.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::ontology::{DefNamespace, Lib};
use crate::xeto::XetoError;
use crate::xeto::parser::parse_xeto;
use crate::xeto::resolver::XetoResolver;
use crate::xeto::spec::{Spec, spec_from_def};

/// Load a Xeto library from source text.
///
/// Parses the source, resolves names against already-loaded libraries in `ns`,
/// and returns the library metadata plus resolved specs.
pub fn load_xeto_source(
    source: &str,
    lib_name: &str,
    ns: &DefNamespace,
) -> Result<(Lib, Vec<Spec>), XetoError> {
    let xeto_file = parse_xeto(source)?;
    load_from_ast(xeto_file, lib_name, ns)
}

/// Load a Xeto library from a directory containing .xeto files.
///
/// Reads all .xeto files in the directory, concatenates them (pragma from
/// the first file that has one), and processes as a single library.
pub fn load_xeto_dir(dir: &Path, ns: &DefNamespace) -> Result<(String, Lib, Vec<Spec>), XetoError> {
    let mut all_source = String::new();
    let mut lib_name: Option<String> = None;

    // Read .xeto files sorted by name for deterministic ordering
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| XetoError::Load(format!("cannot read directory: {e}")))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "xeto"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        return Err(XetoError::Load("no .xeto files found in directory".into()));
    }

    for entry in &entries {
        let content = std::fs::read_to_string(entry.path())
            .map_err(|e| XetoError::Load(format!("cannot read {:?}: {e}", entry.path())))?;

        // Try to extract lib name from pragma if we haven't found one yet
        if lib_name.is_none()
            && let Ok(xf) = parse_xeto(&content)
            && let Some(ref pragma) = xf.pragma
        {
            lib_name = Some(pragma.name.clone());
        }

        all_source.push_str(&content);
        all_source.push('\n');
    }

    // Fall back to directory name if no pragma found
    let name = lib_name.unwrap_or_else(|| {
        dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    });

    let (lib, specs) = load_xeto_source(&all_source, &name, ns)?;
    Ok((name, lib, specs))
}

/// Internal: convert a parsed XetoFile into a Lib + Vec<Spec>.
fn load_from_ast(
    xeto_file: crate::xeto::ast::XetoFile,
    lib_name: &str,
    ns: &DefNamespace,
) -> Result<(Lib, Vec<Spec>), XetoError> {
    // Build resolver with known libs
    let mut resolver = XetoResolver::new();
    for (name, lib) in ns.libs() {
        let mut all_names: HashSet<String> = ns
            .specs(Some(name))
            .iter()
            .map(|s| s.name.clone())
            .collect();
        // Also include def names for resolution
        for def_name in lib.defs.keys() {
            all_names.insert(def_name.clone());
        }
        resolver.add_lib(name, all_names, lib.depends.clone());
    }

    // Register this library's spec names for self-resolution
    let own_names: HashSet<String> = xeto_file.specs.iter().map(|s| s.name.clone()).collect();
    let depends: Vec<String> = xeto_file
        .pragma
        .as_ref()
        .map(|p| p.depends.clone())
        .unwrap_or_default();
    resolver.add_lib(lib_name, own_names, depends.clone());

    // Validate dependencies exist
    for dep in &depends {
        if !ns.libs().contains_key(dep.as_str()) {
            return Err(XetoError::Load(format!(
                "library '{}' depends on '{}' which is not loaded",
                lib_name, dep
            )));
        }
    }

    // Resolve names and convert to Specs
    let mut specs = Vec::new();
    for spec_def in &xeto_file.specs {
        let mut resolved = spec_from_def(spec_def, lib_name);

        // Resolve base type name
        if let Some(ref base) = resolved.base
            && let Some(resolved_name) = resolver.resolve(base, lib_name)
        {
            resolved.base = Some(resolved_name);
        }

        // Resolve slot type_refs
        for slot in &mut resolved.slots {
            if let Some(ref type_ref) = slot.type_ref
                && let Some(resolved_name) = resolver.resolve(type_ref, lib_name)
            {
                slot.type_ref = Some(resolved_name);
            }
        }

        specs.push(resolved);
    }

    // Build Lib metadata
    let pragma = xeto_file.pragma.as_ref();
    let lib = Lib {
        name: lib_name.to_string(),
        version: pragma
            .map(|p| p.version.clone())
            .unwrap_or_else(|| "0.0.0".into()),
        doc: pragma.map(|p| p.doc.clone()).unwrap_or_default(),
        depends,
        defs: HashMap::new(), // Specs are registered separately
    };

    Ok((lib, specs))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_ns() -> DefNamespace {
        DefNamespace::new()
    }

    #[test]
    fn load_simple_spec() {
        let source = r#"
Foo: Obj {
  name: Str
  active
}
"#;
        let ns = empty_ns();
        let (lib, specs) = load_xeto_source(source, "test", &ns).unwrap();
        assert_eq!(lib.name, "test");
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].qname, "test::Foo");
        assert_eq!(specs[0].slots.len(), 2);
    }

    #[test]
    fn load_with_pragma() {
        let source = r#"
pragma: Lib <
  doc: "Test library"
  version: "1.0.0"
>

Bar: Obj {
  count: Number
}
"#;
        let ns = empty_ns();
        let (lib, specs) = load_xeto_source(source, "testlib", &ns).unwrap();
        assert_eq!(lib.version, "1.0.0");
        assert_eq!(lib.doc, "Test library");
        assert_eq!(specs.len(), 1);
    }

    #[test]
    fn load_multiple_specs() {
        let source = r#"
Parent: Obj {
  equip
}

Child: Parent {
  ahu
  dis: Str
}
"#;
        let ns = empty_ns();
        let (_, specs) = load_xeto_source(source, "test", &ns).unwrap();
        assert_eq!(specs.len(), 2);
        let child = specs.iter().find(|s| s.name == "Child").unwrap();
        assert_eq!(child.base.as_deref(), Some("test::Parent"));
    }

    #[test]
    fn load_registers_in_namespace() {
        let source = "Baz: Obj { tag }";
        let mut ns = DefNamespace::new();
        let qnames = ns.load_xeto_str(source, "mylib").unwrap();
        assert_eq!(qnames, vec!["mylib::Baz"]);
        assert!(ns.get_spec("mylib::Baz").is_some());
    }

    #[test]
    fn load_missing_dependency_fails() {
        let source = r#"
pragma: Lib <
  doc: "Needs base"
  version: "1.0.0"
  depends: { { lib: "nonexistent" } }
>

Foo: Obj { tag }
"#;
        let ns = empty_ns();
        let result = load_xeto_source(source, "test", &ns);
        assert!(result.is_err());
    }

    #[test]
    fn load_and_unload_roundtrip() {
        let source = "Foo: Obj { marker }";
        let mut ns = DefNamespace::new();
        ns.load_xeto_str(source, "temp").unwrap();
        assert!(ns.get_spec("temp::Foo").is_some());
        ns.unload_lib("temp").unwrap();
        assert!(ns.get_spec("temp::Foo").is_none());
    }
}
