//! Xeto export — serialize Specs back to .xeto text format.

use crate::kinds::Kind;
use crate::xeto::spec::{Slot, Spec};

/// Export a single Spec to Xeto source text.
pub fn export_spec(spec: &Spec) -> String {
    let mut out = String::new();

    // Doc comment
    if !spec.doc.is_empty() {
        for line in spec.doc.lines() {
            out.push_str(&format!("// {}\n", line));
        }
    }

    // Spec declaration: Name: Base <meta> {
    out.push_str(&spec.name);
    if let Some(ref base) = spec.base {
        out.push_str(": ");
        // Use short name if qualified
        let base_short = base.split("::").last().unwrap_or(base);
        out.push_str(base_short);
    } else {
        out.push_str(": Obj");
    }

    // Meta tags (excluding "abstract" which is handled separately)
    let meta_tags: Vec<String> = spec
        .meta
        .iter()
        .filter(|(k, _)| k.as_str() != "abstract")
        .map(|(k, v)| format_meta_tag(k, v))
        .collect();
    if spec.is_abstract || !meta_tags.is_empty() {
        out.push_str(" <");
        let mut parts = Vec::new();
        if spec.is_abstract {
            parts.push("abstract".to_string());
        }
        parts.extend(meta_tags);
        out.push_str(&parts.join(", "));
        out.push('>');
    }

    // Slots
    if spec.slots.is_empty() {
        out.push('\n');
    } else {
        out.push_str(" {\n");
        for slot in &spec.slots {
            export_slot(&mut out, slot, 2);
        }
        out.push_str("}\n");
    }

    out
}

/// Export a library pragma + all its specs to Xeto source text.
pub fn export_lib(
    _lib_name: &str,
    version: &str,
    doc: &str,
    depends: &[String],
    specs: &[&Spec],
) -> String {
    let mut out = String::new();

    // Pragma
    out.push_str("pragma: Lib <\n");
    if !doc.is_empty() {
        out.push_str(&format!("  doc: \"{}\"\n", escape_xeto_str(doc)));
    }
    out.push_str(&format!("  version: \"{}\"\n", escape_xeto_str(version)));
    if !depends.is_empty() {
        out.push_str("  depends: {\n");
        for dep in depends {
            out.push_str(&format!("    {{ lib: \"{}\" }}\n", escape_xeto_str(dep)));
        }
        out.push_str("  }\n");
    }
    out.push_str(">\n\n");

    // Specs
    for spec in specs {
        out.push_str(&export_spec(spec));
        out.push('\n');
    }

    out
}

fn export_slot(out: &mut String, slot: &Slot, indent: usize) {
    let pad = " ".repeat(indent);
    out.push_str(&pad);

    if slot.is_marker {
        // Marker slot: just the name, with optional ?
        out.push_str(&slot.name);
        if slot.is_maybe() {
            out.push('?');
        }
    } else if slot.is_query {
        // Query slot: name: Query<of:Type, via:"path">
        out.push_str(&slot.name);
        out.push_str(": Query");
        let mut query_parts = Vec::new();
        if let Some(Kind::Str(of)) = slot.meta.get("of") {
            query_parts.push(format!("of:{}", of));
        }
        if let Some(Kind::Str(via)) = slot.meta.get("via") {
            query_parts.push(format!("via:\"{}\"", via));
        }
        if let Some(Kind::Str(inv)) = slot.meta.get("inverse") {
            query_parts.push(format!("inverse:\"{}\"", inv));
        }
        if !query_parts.is_empty() {
            out.push('<');
            out.push_str(&query_parts.join(", "));
            out.push('>');
        }
    } else {
        // Typed slot: name: Type <meta>
        out.push_str(&slot.name);
        out.push_str(": ");
        if let Some(ref t) = slot.type_ref {
            let short = t.split("::").last().unwrap_or(t);
            out.push_str(short);
        } else {
            out.push_str("Obj");
        }
        if slot.is_maybe() {
            out.push('?');
        }

        // Slot meta (excluding "maybe", "of", "via", "inverse" which are rendered differently)
        let slot_meta: Vec<String> = slot
            .meta
            .iter()
            .filter(|(k, _)| {
                let k = k.as_str();
                k != "maybe" && k != "of" && k != "via" && k != "inverse"
            })
            .map(|(k, v)| format_meta_tag(k, v))
            .collect();
        if !slot_meta.is_empty() {
            out.push_str(" <");
            out.push_str(&slot_meta.join(", "));
            out.push('>');
        }
    }

    // Default value
    if let Some(ref val) = slot.default {
        match val {
            Kind::Str(s) => out.push_str(&format!(" \"{}\"", escape_xeto_str(s))),
            Kind::Number(n) => out.push_str(&format!(" {}", n)),
            _ => out.push_str(&format!(" {}", val)),
        }
    }

    out.push('\n');

    // Nested children
    if !slot.children.is_empty() {
        out.push_str(&format!("{}  {{\n", pad));
        for child in &slot.children {
            export_slot(out, child, indent + 4);
        }
        out.push_str(&format!("{}  }}\n", pad));
    }
}

fn escape_xeto_str(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn format_meta_tag(key: &str, val: &Kind) -> String {
    match val {
        Kind::Marker => key.to_string(),
        Kind::Str(s) => format!("{}: \"{}\"", key, escape_xeto_str(s)),
        Kind::Number(n) => format!("{}: {}", key, n),
        Kind::Bool(b) => format!("{}: {}", key, b),
        _ => format!("{}: {}", key, val),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn export_simple_spec() {
        let mut spec = Spec::new("test::Foo", "test", "Foo");
        spec.slots.push(Slot {
            name: "active".into(),
            type_ref: None,
            meta: HashMap::new(),
            default: None,
            is_marker: true,
            is_query: false,
            children: vec![],
        });
        let output = export_spec(&spec);
        assert!(output.contains("Foo: Obj"));
        assert!(output.contains("active"));
    }

    #[test]
    fn export_abstract_spec() {
        let mut spec = Spec::new("test::Base", "test", "Base");
        spec.is_abstract = true;
        let output = export_spec(&spec);
        assert!(output.contains("<abstract>"));
    }

    #[test]
    fn export_spec_with_typed_slots() {
        let mut spec = Spec::new("test::Site", "test", "Site");
        spec.slots.push(Slot {
            name: "dis".into(),
            type_ref: Some("Str".into()),
            meta: HashMap::new(),
            default: None,
            is_marker: false,
            is_query: false,
            children: vec![],
        });
        let output = export_spec(&spec);
        assert!(output.contains("dis: Str"));
    }

    #[test]
    fn export_spec_with_query_slot() {
        let mut meta = HashMap::new();
        meta.insert("of".into(), Kind::Str("Point".into()));
        meta.insert("via".into(), Kind::Str("equipRef+".into()));
        let mut spec = Spec::new("test::Equip", "test", "Equip");
        spec.slots.push(Slot {
            name: "points".into(),
            type_ref: None,
            meta,
            default: None,
            is_marker: false,
            is_query: true,
            children: vec![],
        });
        let output = export_spec(&spec);
        assert!(output.contains("Query"));
        assert!(output.contains("of:Point"));
        assert!(output.contains("via:\"equipRef+\""));
    }

    #[test]
    fn export_lib_with_pragma() {
        let spec = Spec::new("mylib::Thing", "mylib", "Thing");
        let output = export_lib(
            "mylib",
            "2.0.0",
            "My library",
            &["sys".into()],
            &[&spec],
        );
        assert!(output.contains("pragma: Lib"));
        assert!(output.contains("version: \"2.0.0\""));
        assert!(output.contains("doc: \"My library\""));
        assert!(output.contains("lib: \"sys\""));
        assert!(output.contains("Thing: Obj"));
    }

    #[test]
    fn export_roundtrip() {
        use crate::xeto::parser::parse_xeto;

        let source = "Foo: Obj {\n  active\n  dis: Str\n}\n";
        let xf = parse_xeto(source).unwrap();
        let spec = crate::xeto::spec::spec_from_def(&xf.specs[0], "test");
        let exported = export_spec(&spec);
        // Re-parse the exported text
        let xf2 = parse_xeto(&exported).unwrap();
        assert_eq!(xf2.specs[0].name, "Foo");
        assert_eq!(xf2.specs[0].slots.len(), 2);
    }

    #[test]
    fn export_spec_with_doc() {
        let mut spec = Spec::new("test::Foo", "test", "Foo");
        spec.doc = "A foo thing\nWith multiple lines".into();
        let output = export_spec(&spec);
        assert!(output.contains("// A foo thing"));
        assert!(output.contains("// With multiple lines"));
    }

    #[test]
    fn export_maybe_slot() {
        let mut meta = HashMap::new();
        meta.insert("maybe".into(), Kind::Marker);
        let mut spec = Spec::new("test::Foo", "test", "Foo");
        spec.slots.push(Slot {
            name: "optional".into(),
            type_ref: None,
            meta: meta.clone(),
            default: None,
            is_marker: true,
            is_query: false,
            children: vec![],
        });
        spec.slots.push(Slot {
            name: "optStr".into(),
            type_ref: Some("Str".into()),
            meta,
            default: None,
            is_marker: false,
            is_query: false,
            children: vec![],
        });
        let output = export_spec(&spec);
        assert!(output.contains("optional?"));
        assert!(output.contains("optStr: Str?"));
    }

    #[test]
    fn format_meta_tag_escapes_strings() {
        let result = format_meta_tag("doc", &Kind::Str("has \"quotes\" and \\backslash".to_string()));
        assert_eq!(result, r#"doc: "has \"quotes\" and \\backslash""#);
    }

    #[test]
    fn format_meta_tag_escapes_newlines_and_tabs() {
        let result = format_meta_tag("note", &Kind::Str("line1\nline2\there".to_string()));
        assert_eq!(result, r#"note: "line1\nline2\there""#);
    }

    #[test]
    fn export_slot_default_value_escapes_strings() {
        let mut spec = Spec::new("test::Esc", "test", "Esc");
        spec.slots.push(Slot {
            name: "greeting".into(),
            type_ref: Some("Str".into()),
            meta: HashMap::new(),
            default: Some(Kind::Str("say \"hello\"".to_string())),
            is_marker: false,
            is_query: false,
            children: vec![],
        });
        let output = export_spec(&spec);
        assert!(
            output.contains(r#""say \"hello\"""#),
            "default value string should be escaped, got: {}",
            output
        );
    }
}
