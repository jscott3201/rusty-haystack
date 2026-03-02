//! Xeto library loader — parses .xeto source, resolves names, produces Specs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Maximum size of a single .xeto file (10 MB).
const MAX_XETO_FILE_SIZE: u64 = 10 * 1024 * 1024;

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
        // Skip symlinks for security (consistent with scan_xeto_dir)
        let file_type = entry
            .file_type()
            .map_err(|e| XetoError::Load(format!("cannot read file type: {e}")))?;
        if file_type.is_symlink() {
            continue;
        }

        // Check file size
        let metadata = entry
            .metadata()
            .map_err(|e| XetoError::Load(format!("cannot read metadata: {e}")))?;
        if metadata.len() > MAX_XETO_FILE_SIZE {
            return Err(XetoError::Load(format!(
                "file too large ({} bytes): {}",
                metadata.len(),
                entry.path().display()
            )));
        }

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

/// Scan a directory for its Xeto library name and dependency list without
/// fully loading or resolving.  Returns `(lib_name, depends, source)`.
fn scan_xeto_dir(dir: &Path) -> Result<(String, Vec<String>, String), XetoError> {
    let mut all_source = String::new();
    let mut lib_name: Option<String> = None;
    let mut depends: Vec<String> = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| XetoError::Load(format!("cannot read directory {:?}: {e}", dir)))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "xeto"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        return Err(XetoError::Load(format!(
            "no .xeto files found in {:?}",
            dir
        )));
    }

    for entry in &entries {
        // Check for symlinks
        let file_type = entry
            .file_type()
            .map_err(|e| XetoError::Load(format!("cannot read file type: {e}")))?;
        if file_type.is_symlink() {
            continue; // Skip symlinks for security
        }

        // Check file size
        let metadata = entry
            .metadata()
            .map_err(|e| XetoError::Load(format!("cannot read metadata: {e}")))?;
        if metadata.len() > MAX_XETO_FILE_SIZE {
            return Err(XetoError::Load(format!(
                "file too large ({} bytes): {}",
                metadata.len(),
                entry.path().display()
            )));
        }

        let content = std::fs::read_to_string(entry.path())
            .map_err(|e| XetoError::Load(format!("cannot read {:?}: {e}", entry.path())))?;

        if lib_name.is_none()
            && let Ok(xf) = parse_xeto(&content)
            && let Some(ref pragma) = xf.pragma
        {
            if !pragma.name.is_empty() {
                lib_name = Some(pragma.name.clone());
            }
            depends = pragma.depends.clone();
        }

        all_source.push_str(&content);
        all_source.push('\n');
    }

    let name = lib_name.unwrap_or_else(|| {
        dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    });

    Ok((name, depends, all_source))
}

/// Load multiple Xeto libraries from directories with automatic dependency resolution.
///
/// Scans each directory for library metadata (name and dependencies from pragmas),
/// performs topological sort to determine load order, detects circular dependencies,
/// and loads each library into `ns` in the correct order.
///
/// Returns the library names in the order they were loaded.
pub fn load_xeto_with_deps(
    dirs: &[PathBuf],
    ns: &mut DefNamespace,
) -> Result<Vec<String>, XetoError> {
    // Phase 1: Scan all directories for lib names and dependencies
    let mut scanned: Vec<(String, Vec<String>, String, PathBuf)> = Vec::new();
    let mut seen_names = HashSet::new();

    for dir in dirs {
        let canonical_dir = dir
            .canonicalize()
            .map_err(|e| XetoError::Load(format!("cannot resolve path {}: {e}", dir.display())))?;
        let (name, depends, source) = scan_xeto_dir(&canonical_dir)?;
        // Verify resolved path is still under the canonical directory
        let file_canonical = canonical_dir
            .canonicalize()
            .map_err(|e| XetoError::Load(format!("cannot resolve: {e}")))?;
        if !file_canonical.starts_with(&canonical_dir) {
            return Err(XetoError::Load(format!(
                "path traversal detected: {}",
                dir.display()
            )));
        }

        if !seen_names.insert(name.clone()) {
            return Err(XetoError::Load(format!(
                "duplicate library name '{}' in {:?}",
                name, dir
            )));
        }
        scanned.push((name, depends, source, canonical_dir));
    }

    // Phase 2: Build resolver and compute topological order
    let mut resolver = XetoResolver::new();

    // Add already-loaded libs from the namespace
    for (name, lib) in ns.libs() {
        let all_names: HashSet<String> = ns
            .specs(Some(name))
            .iter()
            .map(|s| s.name.clone())
            .collect();
        resolver.add_lib(name, all_names, lib.depends.clone());
    }

    // Add scanned libs (with empty spec sets — we only need deps for ordering)
    for (name, depends, _, _) in &scanned {
        resolver.add_lib(name, HashSet::new(), depends.clone());
    }

    let order = resolver.dependency_order()?;

    // Phase 3: Load in dependency order (skip libs already in namespace)
    let scanned_map: HashMap<String, (String, PathBuf)> = scanned
        .into_iter()
        .map(|(name, _, source, dir)| (name.clone(), (source, dir)))
        .collect();

    let mut loaded = Vec::new();
    for lib_name in &order {
        if ns.libs().contains_key(lib_name.as_str()) {
            continue; // already loaded (e.g. bundled libs)
        }
        if let Some((source, _)) = scanned_map.get(lib_name) {
            ns.load_xeto_str(source, lib_name)?;
            loaded.push(lib_name.clone());
        }
        // libs not in scanned_map and not in ns are transitive deps
        // that must already be loaded — the load_xeto_source call will
        // validate this via the dependency check
    }

    Ok(loaded)
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

    #[test]
    fn load_with_deps_single_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mylib");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("lib.xeto"),
            r#"
pragma: Lib <
  doc: "My lib"
  version: "1.0.0"
>

Widget: Obj {
  label: Str
}
"#,
        )
        .unwrap();

        let mut ns = DefNamespace::new();
        let loaded = load_xeto_with_deps(&[dir], &mut ns).unwrap();
        assert_eq!(loaded, vec!["mylib"]);
        assert!(ns.get_spec("mylib::Widget").is_some());
    }

    #[test]
    fn load_with_deps_respects_order() {
        let tmp = tempfile::tempdir().unwrap();

        // Base lib — no dependencies
        let base = tmp.path().join("base");
        std::fs::create_dir(&base).unwrap();
        std::fs::write(
            base.join("lib.xeto"),
            r#"
pragma: Lib <
  doc: "Base"
  version: "1.0.0"
>

BaseType: Obj { core }
"#,
        )
        .unwrap();

        // App lib — depends on base
        let app = tmp.path().join("app");
        std::fs::create_dir(&app).unwrap();
        std::fs::write(
            app.join("lib.xeto"),
            r#"
pragma: Lib <
  doc: "App"
  version: "1.0.0"
  depends: { { lib: "base" } }
>

AppType: BaseType { extra }
"#,
        )
        .unwrap();

        // Pass dirs in reverse order — topo sort should still load base first
        let mut ns = DefNamespace::new();
        let loaded = load_xeto_with_deps(&[app, base], &mut ns).unwrap();
        assert_eq!(loaded, vec!["base", "app"]);
        assert!(ns.get_spec("base::BaseType").is_some());
        assert!(ns.get_spec("app::AppType").is_some());
    }

    #[test]
    fn load_with_deps_circular_detected() {
        let tmp = tempfile::tempdir().unwrap();

        let lib_a = tmp.path().join("a");
        std::fs::create_dir(&lib_a).unwrap();
        std::fs::write(
            lib_a.join("lib.xeto"),
            r#"
pragma: Lib <
  doc: "A"
  version: "1.0.0"
  depends: { { lib: "b" } }
>

A: Obj { tag }
"#,
        )
        .unwrap();

        let lib_b = tmp.path().join("b");
        std::fs::create_dir(&lib_b).unwrap();
        std::fs::write(
            lib_b.join("lib.xeto"),
            r#"
pragma: Lib <
  doc: "B"
  version: "1.0.0"
  depends: { { lib: "a" } }
>

B: Obj { tag }
"#,
        )
        .unwrap();

        let mut ns = DefNamespace::new();
        let result = load_xeto_with_deps(&[lib_a, lib_b], &mut ns);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("circular"), "expected circular error: {err}");
    }

    #[test]
    fn load_with_deps_duplicate_name() {
        let tmp = tempfile::tempdir().unwrap();

        // Two dirs that both resolve to lib name "samename" (via dir name, no pragma)
        let dir1 = tmp.path().join("samename");
        std::fs::create_dir(&dir1).unwrap();
        std::fs::write(dir1.join("a.xeto"), "Foo: Obj { x }").unwrap();

        // Use a pragma with name: "samename" to produce the duplicate
        let dir2 = tmp.path().join("other");
        std::fs::create_dir(&dir2).unwrap();
        std::fs::write(
            dir2.join("lib.xeto"),
            "pragma: Lib < name: \"samename\", version: \"1.0.0\" >\nBar: Obj { y }",
        )
        .unwrap();

        let mut ns = DefNamespace::new();
        let result = load_xeto_with_deps(&[dir1, dir2], &mut ns);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }
}
