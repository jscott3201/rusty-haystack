// TrioLoader -- load Haystack 4 defs from Trio format.

use std::collections::HashMap;

use crate::codecs::trio;
use crate::data::HDict;
use crate::kinds::Kind;

use super::OntologyError;
use super::def::Def;
use super::lib::Lib;

/// Parse Trio text and extract defs grouped by library.
///
/// Uses the existing Trio codec to decode the text into an HGrid,
/// then extracts Def records from each row and groups them by library.
pub fn load_trio(source: &str) -> Result<Vec<Lib>, OntologyError> {
    let grid = trio::decode_grid(source)?;

    let mut libs: HashMap<String, HashMap<String, Def>> = HashMap::new();
    let mut lib_defs: HashMap<String, HDict> = HashMap::new();

    for row in &grid.rows {
        if let Some(def) = row_to_def(row) {
            let lib_name = def.lib.clone();
            let symbol = def.symbol.clone();

            if def.kind() == super::def::DefKind::Lib {
                lib_defs.insert(lib_name.clone(), row.clone());
            }

            libs.entry(lib_name).or_default().insert(symbol, def);
        }
    }

    let mut result: Vec<Lib> = Vec::new();
    for (lib_name, defs) in libs {
        let lib_row = lib_defs.get(&lib_name);
        let empty = HDict::new();
        let row = lib_row.unwrap_or(&empty);

        let version = str_val(row, "version", "");
        let doc = str_val(row, "doc", "");
        let depends = symbol_list(row, "depends");

        result.push(Lib {
            name: lib_name,
            version,
            doc,
            depends,
            defs,
        });
    }

    Ok(result)
}

/// Convert an HDict row to a Def.
///
/// Returns `None` if the row has no `def` tag or it cannot be parsed.
fn row_to_def(row: &HDict) -> Option<Def> {
    let raw_def = row.get("def")?;
    let symbol = to_symbol_str(raw_def)?;

    // Extract lib name (strip "lib:" prefix)
    let lib_name = match row.get("lib") {
        Some(val) => {
            let s = to_symbol_str(val).unwrap_or_default();
            s.strip_prefix("lib:").unwrap_or(&s).to_string()
        }
        None => String::new(),
    };

    // Extract is_ supertypes
    let is_ = symbol_list(row, "is");

    // Extract tagOn
    let tag_on = symbol_list(row, "tagOn");

    // Extract of
    let of = row.get("of").and_then(to_symbol_str);

    // Extract mandatory (presence of Marker)
    let mandatory = matches!(row.get("mandatory"), Some(Kind::Marker));

    // Extract doc
    let doc = str_val(row, "doc", "");

    Some(Def {
        symbol,
        lib: lib_name,
        is_,
        tag_on,
        of,
        mandatory,
        doc,
        tags: row.clone(),
    })
}

/// Convert a Symbol, Ref, or Str Kind to a plain string.
///
/// Strips the leading `^` from symbols if present in the stored value
/// (though typically the Symbol type stores the value without the `^`).
fn to_symbol_str(val: &Kind) -> Option<String> {
    match val {
        Kind::Symbol(s) => Some(s.val().to_string()),
        Kind::Ref(r) => Some(r.val.clone()),
        Kind::Str(s) => Some(s.clone()),
        _ => None,
    }
}

/// Extract a list of symbol names from a tag.
///
/// Handles both a single Symbol/Ref value and a List of them.
fn symbol_list(row: &HDict, tag: &str) -> Vec<String> {
    let val = match row.get(tag) {
        Some(v) => v,
        None => return vec![],
    };

    match val {
        Kind::List(items) => {
            let mut result = Vec::new();
            for item in items {
                if let Some(s) = to_symbol_str(item) {
                    result.push(s);
                }
            }
            result
        }
        _ => {
            if let Some(s) = to_symbol_str(val) {
                vec![s]
            } else {
                vec![]
            }
        }
    }
}

/// Extract a string value from a tag, with a default fallback.
fn str_val(row: &HDict, tag: &str, default: &str) -> String {
    match row.get(tag) {
        Some(Kind::Str(s)) => s.clone(),
        _ => default.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::Symbol;

    #[test]
    fn to_symbol_str_from_symbol() {
        let val = Kind::Symbol(Symbol::new("site"));
        assert_eq!(to_symbol_str(&val), Some("site".to_string()));
    }

    #[test]
    fn to_symbol_str_from_ref() {
        let val = Kind::Ref(crate::kinds::HRef::from_val("site"));
        assert_eq!(to_symbol_str(&val), Some("site".to_string()));
    }

    #[test]
    fn to_symbol_str_from_str() {
        let val = Kind::Str("site".to_string());
        assert_eq!(to_symbol_str(&val), Some("site".to_string()));
    }

    #[test]
    fn to_symbol_str_from_number_is_none() {
        let val = Kind::Number(crate::kinds::Number::unitless(42.0));
        assert_eq!(to_symbol_str(&val), None);
    }

    #[test]
    fn load_small_trio_snippet() {
        let input = "\
def:^site
doc:\"A site is a geographic location\"
is:[^entity]
lib:^lib:ph
mandatory
---
def:^equip
doc:\"A piece of equipment\"
is:[^entity]
lib:^lib:phIoT
---
def:^ahu
doc:\"Air Handling Unit\"
is:[^equip]
lib:^lib:phIoT
mandatory
tagOn:[^site]
---
def:^lib:ph
doc:\"Project Haystack core definitions\"
is:[^lib]
lib:^lib:ph
version:\"4.0.0\"
---
def:^lib:phIoT
doc:\"Project Haystack IoT definitions\"
is:[^lib]
lib:^lib:phIoT
version:\"4.0.0\"
depends:[^lib:ph]
";
        let libs = load_trio(input).unwrap();

        // Should have 2 libs: ph and phIoT
        assert_eq!(libs.len(), 2);

        // Find the libs
        let ph = libs.iter().find(|l| l.name == "ph").unwrap();
        let phiot = libs.iter().find(|l| l.name == "phIoT").unwrap();

        // Check ph lib
        assert_eq!(ph.version, "4.0.0");
        assert!(ph.defs.contains_key("site"));
        assert!(ph.defs.contains_key("lib:ph"));

        // Check phIoT lib
        assert_eq!(phiot.version, "4.0.0");
        assert!(phiot.defs.contains_key("equip"));
        assert!(phiot.defs.contains_key("ahu"));
        assert!(phiot.defs.contains_key("lib:phIoT"));
        assert_eq!(phiot.depends, vec!["lib:ph"]);

        // Check def details
        let site_def = &ph.defs["site"];
        assert_eq!(site_def.symbol, "site");
        assert_eq!(site_def.is_, vec!["entity"]);
        assert!(site_def.mandatory);

        let ahu_def = &phiot.defs["ahu"];
        assert_eq!(ahu_def.symbol, "ahu");
        assert_eq!(ahu_def.is_, vec!["equip"]);
        assert!(ahu_def.mandatory);
        assert_eq!(ahu_def.tag_on, vec!["site"]);
    }

    #[test]
    fn load_empty_trio() {
        let libs = load_trio("").unwrap();
        assert!(libs.is_empty());
    }

    #[test]
    fn load_trio_with_of_tag() {
        let input = "\
def:^ahuRef
doc:\"AHU reference\"
is:[^ref]
lib:^lib:phIoT
of:^ahu
";
        let libs = load_trio(input).unwrap();
        assert_eq!(libs.len(), 1);

        let lib = &libs[0];
        let def = &lib.defs["ahuRef"];
        assert_eq!(def.of, Some("ahu".to_string()));
    }

    #[test]
    fn symbol_list_single() {
        let mut row = HDict::new();
        row.set("is", Kind::Symbol(Symbol::new("marker")));

        let result = symbol_list(&row, "is");
        assert_eq!(result, vec!["marker"]);
    }

    #[test]
    fn symbol_list_multiple() {
        let mut row = HDict::new();
        row.set(
            "is",
            Kind::List(vec![
                Kind::Symbol(Symbol::new("equip")),
                Kind::Symbol(Symbol::new("elec-input")),
            ]),
        );

        let result = symbol_list(&row, "is");
        assert_eq!(result, vec!["equip", "elec-input"]);
    }

    #[test]
    fn symbol_list_missing_tag() {
        let row = HDict::new();
        let result = symbol_list(&row, "is");
        assert!(result.is_empty());
    }

    #[test]
    fn str_val_present() {
        let mut row = HDict::new();
        row.set("version", Kind::Str("4.0.0".to_string()));
        assert_eq!(str_val(&row, "version", ""), "4.0.0");
    }

    #[test]
    fn str_val_missing() {
        let row = HDict::new();
        assert_eq!(str_val(&row, "version", "unknown"), "unknown");
    }
}
