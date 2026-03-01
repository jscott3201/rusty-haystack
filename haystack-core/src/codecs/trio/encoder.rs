// Trio format encoder — encode HGrid to record-per-entity text format.

use crate::codecs::CodecError;
use crate::codecs::zinc;
use crate::data::HGrid;
use crate::kinds::Kind;

/// Encode an HGrid to Trio format.
///
/// Each row in the grid becomes a record. Records are separated by `---`.
/// For each tag in the row:
/// - Marker values: just the tag name
/// - Multi-line strings (containing \n): `name:` then each line indented with 2 spaces
/// - All other values: `name: ` + zinc-encoded scalar
pub fn encode_grid(grid: &HGrid) -> Result<String, CodecError> {
    let mut parts: Vec<String> = Vec::new();

    for row in &grid.rows {
        parts.push(encode_dict(row)?);
    }

    Ok(parts.join("\n---\n"))
}

/// Encode a single dict (row) as Trio-formatted lines.
fn encode_dict(d: &crate::data::HDict) -> Result<String, CodecError> {
    let mut lines: Vec<String> = Vec::new();

    // Sort keys for deterministic output
    let mut keys: Vec<&String> = d.tags().keys().collect();
    keys.sort();

    for name in keys {
        let val = &d.tags()[name];
        match val {
            Kind::Marker => {
                lines.push(name.clone());
            }
            Kind::Str(s) if s.contains('\n') => {
                // Multi-line string: name followed by colon, then indented lines
                lines.push(format!("{name}:"));
                for line in s.split('\n') {
                    lines.push(format!("  {line}"));
                }
            }
            _ => {
                let encoded = zinc::encode_scalar(val)?;
                lines.push(format!("{name}: {encoded}"));
            }
        }
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{HCol, HDict, HGrid};
    use crate::kinds::{HRef, Kind, Number};

    #[test]
    fn encode_empty_grid() {
        let g = HGrid::new();
        let encoded = encode_grid(&g).unwrap();
        assert_eq!(encoded, "");
    }

    #[test]
    fn encode_single_record_with_marker() {
        let cols = vec![HCol::new("site")];
        let mut row = HDict::new();
        row.set("site", Kind::Marker);
        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);

        let encoded = encode_grid(&g).unwrap();
        assert_eq!(encoded, "site");
    }

    #[test]
    fn encode_single_record_with_values() {
        let cols = vec![HCol::new("dis"), HCol::new("area"), HCol::new("site")];
        let mut row = HDict::new();
        row.set("dis", Kind::Str("Main Site".into()));
        row.set("site", Kind::Marker);
        row.set("area", Kind::Number(Number::new(5000.0, Some("ft\u{00B2}".into()))));
        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);

        let encoded = encode_grid(&g).unwrap();
        // Keys are sorted: area, dis, site
        let lines: Vec<&str> = encoded.lines().collect();
        assert_eq!(lines[0], "area: 5000ft\u{00B2}");
        assert_eq!(lines[1], "dis: \"Main Site\"");
        assert_eq!(lines[2], "site");
    }

    #[test]
    fn encode_multiple_records() {
        let cols = vec![HCol::new("dis"), HCol::new("site")];
        let mut row1 = HDict::new();
        row1.set("dis", Kind::Str("Site A".into()));
        row1.set("site", Kind::Marker);
        let mut row2 = HDict::new();
        row2.set("dis", Kind::Str("Site B".into()));
        row2.set("site", Kind::Marker);
        let g = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);

        let encoded = encode_grid(&g).unwrap();
        assert!(encoded.contains("---"));
        let records: Vec<&str> = encoded.split("\n---\n").collect();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn encode_multiline_string() {
        let cols = vec![HCol::new("doc")];
        let mut row = HDict::new();
        row.set("doc", Kind::Str("Line 1\nLine 2\nLine 3".into()));
        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);

        let encoded = encode_grid(&g).unwrap();
        let lines: Vec<&str> = encoded.lines().collect();
        assert_eq!(lines[0], "doc:");
        assert_eq!(lines[1], "  Line 1");
        assert_eq!(lines[2], "  Line 2");
        assert_eq!(lines[3], "  Line 3");
    }

    #[test]
    fn encode_ref_value() {
        let cols = vec![HCol::new("id")];
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("site-1")));
        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);

        let encoded = encode_grid(&g).unwrap();
        assert_eq!(encoded, "id: @site-1");
    }

    #[test]
    fn encode_number_with_unit() {
        let cols = vec![HCol::new("temp")];
        let mut row = HDict::new();
        row.set(
            "temp",
            Kind::Number(Number::new(72.5, Some("\u{00B0}F".into()))),
        );
        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);

        let encoded = encode_grid(&g).unwrap();
        assert_eq!(encoded, "temp: 72.5\u{00B0}F");
    }

    #[test]
    fn encode_bool_values() {
        let cols = vec![HCol::new("active"), HCol::new("deleted")];
        let mut row = HDict::new();
        row.set("active", Kind::Bool(true));
        row.set("deleted", Kind::Bool(false));
        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);

        let encoded = encode_grid(&g).unwrap();
        let lines: Vec<&str> = encoded.lines().collect();
        assert_eq!(lines[0], "active: T");
        assert_eq!(lines[1], "deleted: F");
    }
}
