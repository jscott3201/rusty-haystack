// RDF export — Turtle and JSON-LD serialization for Haystack entities.

use crate::data::HDict;
use crate::kinds::Kind;

const PH_PREFIX: &str = "https://project-haystack.org/def/ph/";
const PH_IOT_PREFIX: &str = "https://project-haystack.org/def/phIoT/";
const ENTITY_PREFIX: &str = "urn:haystack:entity/";
const XSD_PREFIX: &str = "http://www.w3.org/2001/XMLSchema#";

/// Encode a slice of entity dicts as RDF Turtle.
pub fn to_turtle(entities: &[HDict]) -> String {
    let mut out = String::new();

    // Prefixes
    out.push_str(&format!("@prefix ph: <{PH_PREFIX}> .\n"));
    out.push_str(&format!("@prefix phIoT: <{PH_IOT_PREFIX}> .\n"));
    out.push_str(&format!("@prefix entity: <{ENTITY_PREFIX}> .\n"));
    out.push_str(&format!("@prefix xsd: <{XSD_PREFIX}> .\n"));

    for entity in entities {
        let id = match entity.id() {
            Some(r) => &r.val,
            None => continue, // skip entities without an id
        };

        out.push('\n');
        out.push_str(&format!("entity:{id}\n"));

        // Collect tags, excluding "id", sorted for deterministic output
        let mut tags: Vec<(&str, &Kind)> =
            entity.iter().filter(|(name, _)| *name != "id").collect();
        tags.sort_by_key(|(name, _)| *name);

        for (i, (name, val)) in tags.iter().enumerate() {
            let is_last = i == tags.len() - 1;
            let terminator = if is_last { " ." } else { " ;" };

            let value_str = kind_to_turtle(val);
            out.push_str(&format!("  ph:{name} {value_str}{terminator}\n"));
        }
    }

    out
}

/// Convert a Kind value to its Turtle representation.
fn kind_to_turtle(val: &Kind) -> String {
    match val {
        Kind::Marker => "ph:Marker".to_string(),
        Kind::Str(s) => {
            let escaped = escape_turtle_string(s);
            format!("\"{escaped}\"^^xsd:string")
        }
        Kind::Number(n) => {
            let v = n.val;
            let lit = format!("\"{v}\"^^xsd:double");
            if let Some(ref u) = n.unit {
                format!("{lit} # unit: {u}")
            } else {
                lit
            }
        }
        Kind::Bool(b) => format!("\"{b}\"^^xsd:boolean"),
        Kind::Ref(r) => format!("entity:{}", r.val),
        // Fallback: encode as string literal
        other => {
            let escaped = escape_turtle_string(&other.to_string());
            format!("\"{escaped}\"^^xsd:string")
        }
    }
}

/// Escape special characters in a Turtle string literal.
fn escape_turtle_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Encode a slice of entity dicts as JSON-LD.
pub fn to_jsonld(entities: &[HDict]) -> String {
    let mut graph = Vec::new();

    for entity in entities {
        let id = match entity.id() {
            Some(r) => &r.val,
            None => continue,
        };

        let mut node = serde_json::Map::new();
        node.insert(
            "@id".to_string(),
            serde_json::Value::String(format!("entity:{id}")),
        );

        // Collect tags, excluding "id", sorted for deterministic output
        let mut tags: Vec<(&str, &Kind)> =
            entity.iter().filter(|(name, _)| *name != "id").collect();
        tags.sort_by_key(|(name, _)| *name);

        for (name, val) in &tags {
            let key = format!("ph:{name}");
            let json_val = kind_to_jsonld(val);
            node.insert(key, json_val);
        }

        graph.push(serde_json::Value::Object(node));
    }

    let mut context = serde_json::Map::new();
    context.insert(
        "ph".to_string(),
        serde_json::Value::String(PH_PREFIX.to_string()),
    );
    context.insert(
        "phIoT".to_string(),
        serde_json::Value::String(PH_IOT_PREFIX.to_string()),
    );
    context.insert(
        "entity".to_string(),
        serde_json::Value::String(ENTITY_PREFIX.to_string()),
    );
    context.insert(
        "xsd".to_string(),
        serde_json::Value::String(XSD_PREFIX.to_string()),
    );

    let mut doc = serde_json::Map::new();
    doc.insert("@context".to_string(), serde_json::Value::Object(context));
    doc.insert("@graph".to_string(), serde_json::Value::Array(graph));

    serde_json::to_string_pretty(&serde_json::Value::Object(doc))
        .expect("JSON-LD serialization should not fail")
}

/// Convert a Kind value to its JSON-LD representation.
fn kind_to_jsonld(val: &Kind) -> serde_json::Value {
    match val {
        Kind::Marker => {
            let mut m = serde_json::Map::new();
            m.insert(
                "@value".to_string(),
                serde_json::Value::String("marker".to_string()),
            );
            m.insert(
                "@type".to_string(),
                serde_json::Value::String("ph:Marker".to_string()),
            );
            serde_json::Value::Object(m)
        }
        Kind::Str(s) => serde_json::Value::String(s.clone()),
        Kind::Number(n) => {
            let mut m = serde_json::Map::new();
            m.insert("@value".to_string(), serde_json::json!(n.val));
            m.insert(
                "@type".to_string(),
                serde_json::Value::String("xsd:double".to_string()),
            );
            serde_json::Value::Object(m)
        }
        Kind::Bool(b) => serde_json::Value::Bool(*b),
        Kind::Ref(r) => {
            let mut m = serde_json::Map::new();
            m.insert(
                "@id".to_string(),
                serde_json::Value::String(format!("entity:{}", r.val)),
            );
            serde_json::Value::Object(m)
        }
        // Fallback: encode as string
        other => serde_json::Value::String(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{HRef, Number};

    /// Helper to create an entity dict with an id.
    fn make_entity(id: &str) -> HDict {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(id)));
        d
    }

    // ── Turtle tests ──

    #[test]
    fn turtle_empty_entities() {
        let result = to_turtle(&[]);
        assert!(result.contains("@prefix ph:"));
        assert!(result.contains("@prefix phIoT:"));
        assert!(result.contains("@prefix entity:"));
        assert!(result.contains("@prefix xsd:"));
        // No entity blocks after the prefix section
        let lines: Vec<&str> = result.lines().collect();
        let non_prefix_lines: Vec<&&str> = lines
            .iter()
            .filter(|l| !l.starts_with("@prefix") && !l.trim().is_empty())
            .collect();
        assert!(non_prefix_lines.is_empty());
    }

    #[test]
    fn turtle_single_entity_with_markers() {
        let mut entity = make_entity("demo-site");
        entity.set("site", Kind::Marker);
        entity.set("dis", Kind::Str("Demo Building".to_string()));

        let result = to_turtle(&[entity]);
        assert!(result.contains("entity:demo-site"));
        assert!(result.contains("ph:site ph:Marker"));
        assert!(result.contains("ph:dis \"Demo Building\"^^xsd:string"));
    }

    #[test]
    fn turtle_ref_tags_produce_entity_links() {
        let mut entity = make_entity("demo-ahu-1");
        entity.set("ahu", Kind::Marker);
        entity.set("siteRef", Kind::Ref(HRef::from_val("demo-site")));

        let result = to_turtle(&[entity]);
        assert!(result.contains("entity:demo-ahu-1"));
        assert!(result.contains("ph:siteRef entity:demo-site"));
    }

    #[test]
    fn turtle_number_tags() {
        let mut entity = make_entity("demo-site");
        entity.set(
            "area",
            Kind::Number(Number::new(50000.0, Some("ft²".to_string()))),
        );
        entity.set("floors", Kind::Number(Number::unitless(3.0)));

        let result = to_turtle(&[entity]);
        assert!(result.contains("ph:area \"50000\"^^xsd:double"));
        assert!(result.contains("# unit: ft²"));
        assert!(result.contains("ph:floors \"3\"^^xsd:double"));
    }

    // ── JSON-LD tests ──

    #[test]
    fn jsonld_empty_entities() {
        let result = to_jsonld(&[]);
        let doc: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(doc["@context"].is_object());
        assert!(doc["@graph"].is_array());
        assert_eq!(doc["@graph"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn jsonld_single_entity() {
        let mut entity = make_entity("demo-site");
        entity.set("dis", Kind::Str("Demo Building".to_string()));
        entity.set("site", Kind::Marker);

        let result = to_jsonld(&[entity]);
        let doc: serde_json::Value = serde_json::from_str(&result).unwrap();
        let graph = doc["@graph"].as_array().unwrap();
        assert_eq!(graph.len(), 1);

        let node = &graph[0];
        assert_eq!(node["@id"], "entity:demo-site");
        assert_eq!(node["ph:dis"], "Demo Building");
    }

    #[test]
    fn jsonld_ref_links() {
        let mut entity = make_entity("demo-ahu-1");
        entity.set("siteRef", Kind::Ref(HRef::from_val("demo-site")));

        let result = to_jsonld(&[entity]);
        let doc: serde_json::Value = serde_json::from_str(&result).unwrap();
        let node = &doc["@graph"][0];
        let site_ref = &node["ph:siteRef"];
        assert_eq!(site_ref["@id"], "entity:demo-site");
    }

    #[test]
    fn jsonld_marker_encoding() {
        let mut entity = make_entity("demo-site");
        entity.set("site", Kind::Marker);

        let result = to_jsonld(&[entity]);
        let doc: serde_json::Value = serde_json::from_str(&result).unwrap();
        let node = &doc["@graph"][0];
        let marker = &node["ph:site"];
        assert_eq!(marker["@value"], "marker");
        assert_eq!(marker["@type"], "ph:Marker");
    }
}
