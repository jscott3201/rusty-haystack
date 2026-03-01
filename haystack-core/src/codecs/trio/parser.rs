// Trio format parser — record-per-entity text format.

use crate::codecs::CodecError;
use crate::codecs::zinc::ZincParser;
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::Kind;

/// Parse Trio-formatted text into an HGrid.
///
/// Each record becomes a row in the grid. Records are separated by `---`
/// (three or more dashes). Columns are derived from all unique tag names
/// across all records.
pub fn decode_grid(input: &str) -> Result<HGrid, CodecError> {
    let records = parse_records(input)?;

    if records.is_empty() {
        return Ok(HGrid::new());
    }

    // Derive columns from all unique tag names, preserving insertion order
    let mut col_names: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for rec in &records {
        // Sort tag names for deterministic column order within each record
        let mut names: Vec<&str> = rec.tag_names().collect();
        names.sort();
        for name in names {
            if seen.insert(name.to_string()) {
                col_names.push(name.to_string());
            }
        }
    }

    let cols: Vec<HCol> = col_names.iter().map(HCol::new).collect();
    Ok(HGrid::from_parts(HDict::new(), cols, records))
}

/// Parse the input text into a list of HDict records.
fn parse_records(input: &str) -> Result<Vec<HDict>, CodecError> {
    let mut records: Vec<HDict> = Vec::new();
    let mut current_tags: Vec<(String, Kind)> = Vec::new();
    let mut multiline_name: Option<String> = None;
    let mut multiline_lines: Vec<String> = Vec::new();

    for line in input.split('\n') {
        let stripped = line.trim();

        // Record separator: line of three or more dashes
        if is_record_separator(stripped) {
            // Flush multiline string if active
            if let Some(name) = multiline_name.take() {
                current_tags.push((name, Kind::Str(multiline_lines.join("\n"))));
                multiline_lines.clear();
            }
            // Flush current record
            if !current_tags.is_empty() {
                records.push(tags_to_dict(current_tags));
                current_tags = Vec::new();
            }
            continue;
        }

        // Comment line
        if stripped.starts_with("//") {
            continue;
        }

        // In multiline string mode
        if multiline_name.is_some() {
            if let Some(content) = line.strip_prefix("  ").or_else(|| line.strip_prefix('\t')) {
                // Indented continuation line
                multiline_lines.push(content.to_string());
                continue;
            } else {
                // Non-indented line ends the multiline
                let name = multiline_name.take().unwrap();
                current_tags.push((name, Kind::Str(multiline_lines.join("\n"))));
                multiline_lines.clear();
                // Fall through to parse this line normally
            }
        }

        // Skip empty lines
        if stripped.is_empty() {
            continue;
        }

        // Parse name:value or marker-only line
        match stripped.find(':') {
            None => {
                // Marker tag (just a name)
                current_tags.push((stripped.to_string(), Kind::Marker));
            }
            Some(colon_idx) => {
                let name = stripped[..colon_idx].trim().to_string();
                let rest = &stripped[colon_idx + 1..];

                if rest.trim().is_empty() {
                    // Empty after colon -> multiline string starts on next line
                    multiline_name = Some(name);
                    multiline_lines.clear();
                } else {
                    // Value follows colon
                    let val_str = rest.trim();
                    let val = parse_scalar_value(val_str);
                    current_tags.push((name, val));
                }
            }
        }
    }

    // Flush final multiline
    if let Some(name) = multiline_name.take() {
        current_tags.push((name, Kind::Str(multiline_lines.join("\n"))));
    }

    // Flush final record
    if !current_tags.is_empty() {
        records.push(tags_to_dict(current_tags));
    }

    Ok(records)
}

/// Try to parse a value string as a Zinc scalar.
/// If parsing fails or the parser doesn't consume all input, treat as a plain string.
fn parse_scalar_value(val_str: &str) -> Kind {
    let mut parser = ZincParser::new(val_str);
    match parser.parse_scalar() {
        Ok(val) => {
            if parser.at_end() {
                val
            } else {
                // Parser didn't consume all input, treat as plain string
                Kind::Str(val_str.to_string())
            }
        }
        Err(_) => {
            // Unparseable as Zinc scalar, treat as plain string
            Kind::Str(val_str.to_string())
        }
    }
}

/// Check if a line is a record separator (three or more dashes only).
fn is_record_separator(stripped: &str) -> bool {
    !stripped.is_empty() && stripped.len() >= 3 && stripped.chars().all(|ch| ch == '-')
}

/// Convert an ordered list of (name, value) pairs into an HDict.
fn tags_to_dict(tags: Vec<(String, Kind)>) -> HDict {
    let mut dict = HDict::new();
    for (name, val) in tags {
        dict.set(name, val);
    }
    dict
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kinds::{Coord, HRef, Number};
    use chrono::NaiveDate;

    #[test]
    fn parse_empty_input() {
        let grid = decode_grid("").unwrap();
        assert!(grid.is_empty());
        assert_eq!(grid.num_cols(), 0);
    }

    #[test]
    fn parse_whitespace_only() {
        let grid = decode_grid("   \n  \n  ").unwrap();
        assert!(grid.is_empty());
    }

    #[test]
    fn parse_single_record_with_markers_and_values() {
        let input = "dis: \"Site 1\"\nsite\narea: 3702ft\u{00B2}\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();
        assert_eq!(row.get("dis"), Some(&Kind::Str("Site 1".into())));
        assert_eq!(row.get("site"), Some(&Kind::Marker));
        assert_eq!(
            row.get("area"),
            Some(&Kind::Number(Number::new(
                3702.0,
                Some("ft\u{00B2}".into())
            )))
        );
    }

    #[test]
    fn parse_multiple_records() {
        let input = "dis: \"Site A\"\nsite\n---\ndis: \"Site B\"\nsite\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 2);

        assert_eq!(
            grid.row(0).unwrap().get("dis"),
            Some(&Kind::Str("Site A".into()))
        );
        assert_eq!(
            grid.row(1).unwrap().get("dis"),
            Some(&Kind::Str("Site B".into()))
        );
    }

    #[test]
    fn parse_comments_skipped() {
        let input = "// This is a comment\ndis: \"Site\"\nsite\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 1);
        assert_eq!(
            grid.row(0).unwrap().get("dis"),
            Some(&Kind::Str("Site".into()))
        );
        assert!(grid.row(0).unwrap().missing("//"));
    }

    #[test]
    fn parse_multiline_string() {
        let input = "dis: \"Test\"\ndoc:\n  This is line 1\n  This is line 2\nsite\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();
        assert_eq!(
            row.get("doc"),
            Some(&Kind::Str("This is line 1\nThis is line 2".into()))
        );
        assert_eq!(row.get("site"), Some(&Kind::Marker));
    }

    #[test]
    fn parse_multiline_string_with_tab_indent() {
        let input = "doc:\n\tLine A\n\tLine B\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();
        assert_eq!(row.get("doc"), Some(&Kind::Str("Line A\nLine B".into())));
    }

    #[test]
    fn parse_multiline_string_at_end_of_input() {
        let input = "doc:\n  Last line";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();
        assert_eq!(row.get("doc"), Some(&Kind::Str("Last line".into())));
    }

    #[test]
    fn parse_markers_alone() {
        let input = "site\nequip\nahu\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();
        assert_eq!(row.get("site"), Some(&Kind::Marker));
        assert_eq!(row.get("equip"), Some(&Kind::Marker));
        assert_eq!(row.get("ahu"), Some(&Kind::Marker));
    }

    #[test]
    fn parse_blank_lines_between_tags() {
        let input = "dis: \"Test\"\n\nsite\n\narea: 100\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();
        assert_eq!(row.get("dis"), Some(&Kind::Str("Test".into())));
        assert_eq!(row.get("site"), Some(&Kind::Marker));
        assert_eq!(
            row.get("area"),
            Some(&Kind::Number(Number::unitless(100.0)))
        );
    }

    #[test]
    fn parse_ref_values() {
        let input = "id: @site-1\nsiteRef: @alpha\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 1);

        let row = grid.row(0).unwrap();
        assert_eq!(row.get("id"), Some(&Kind::Ref(HRef::from_val("site-1"))));
        assert_eq!(
            row.get("siteRef"),
            Some(&Kind::Ref(HRef::from_val("alpha")))
        );
    }

    #[test]
    fn parse_date_value() {
        let input = "installed: 2024-03-13\n";
        let grid = decode_grid(input).unwrap();
        let row = grid.row(0).unwrap();
        assert_eq!(
            row.get("installed"),
            Some(&Kind::Date(NaiveDate::from_ymd_opt(2024, 3, 13).unwrap()))
        );
    }

    #[test]
    fn parse_coord_value() {
        let input = "geoCoord: C(37.5458,-77.4491)\n";
        let grid = decode_grid(input).unwrap();
        let row = grid.row(0).unwrap();
        assert_eq!(
            row.get("geoCoord"),
            Some(&Kind::Coord(Coord::new(37.5458, -77.4491)))
        );
    }

    #[test]
    fn parse_bool_values() {
        let input = "active: T\ndeleted: F\n";
        let grid = decode_grid(input).unwrap();
        let row = grid.row(0).unwrap();
        assert_eq!(row.get("active"), Some(&Kind::Bool(true)));
        assert_eq!(row.get("deleted"), Some(&Kind::Bool(false)));
    }

    #[test]
    fn parse_number_with_unit() {
        let input = "temp: 72.5\u{00B0}F\nflow: 350gal/min\n";
        let grid = decode_grid(input).unwrap();
        let row = grid.row(0).unwrap();
        assert_eq!(
            row.get("temp"),
            Some(&Kind::Number(Number::new(72.5, Some("\u{00B0}F".into()))))
        );
        assert_eq!(
            row.get("flow"),
            Some(&Kind::Number(Number::new(350.0, Some("gal/min".into()))))
        );
    }

    #[test]
    fn parse_separator_with_more_dashes() {
        let input = "site\n-----\nequip\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 2);
        assert_eq!(grid.row(0).unwrap().get("site"), Some(&Kind::Marker));
        assert_eq!(grid.row(1).unwrap().get("equip"), Some(&Kind::Marker));
    }

    #[test]
    fn parse_columns_derived_from_all_records() {
        let input = "dis: \"A\"\nsite\n---\ndis: \"B\"\narea: 100\n";
        let grid = decode_grid(input).unwrap();

        // Columns should include tags from both records
        let col_names: Vec<&str> = grid.col_names().collect();
        assert!(col_names.contains(&"dis"));
        assert!(col_names.contains(&"site"));
        assert!(col_names.contains(&"area"));
    }

    #[test]
    fn parse_complex_trio_file() {
        let input = "\
// Alpha Office
id: @alpha
dis: \"Alpha Office\"
site
geoAddr: \"600 N 2nd St, Richmond VA 23219\"
geoCoord: C(37.5407,-77.4360)
area: 120000ft\u{00B2}
---
// Floor 1
id: @floor1
dis: \"Floor 1\"
floor
siteRef: @alpha
---
id: @ahu1
dis: \"AHU-1\"
equip
ahu
siteRef: @alpha
floorRef: @floor1
";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 3);

        let site = grid.row(0).unwrap();
        assert_eq!(site.get("dis"), Some(&Kind::Str("Alpha Office".into())));
        assert_eq!(site.get("site"), Some(&Kind::Marker));
        assert_eq!(site.get("id"), Some(&Kind::Ref(HRef::from_val("alpha"))));
        assert_eq!(
            site.get("area"),
            Some(&Kind::Number(Number::new(
                120000.0,
                Some("ft\u{00B2}".into())
            )))
        );

        let floor = grid.row(1).unwrap();
        assert_eq!(floor.get("dis"), Some(&Kind::Str("Floor 1".into())));
        assert_eq!(floor.get("floor"), Some(&Kind::Marker));

        let ahu = grid.row(2).unwrap();
        assert_eq!(ahu.get("dis"), Some(&Kind::Str("AHU-1".into())));
        assert_eq!(ahu.get("equip"), Some(&Kind::Marker));
        assert_eq!(ahu.get("ahu"), Some(&Kind::Marker));
    }

    #[test]
    fn parse_multiline_between_records() {
        let input = "dis: \"A\"\ndoc:\n  Hello world\n  Second line\n---\ndis: \"B\"\n";
        let grid = decode_grid(input).unwrap();
        assert_eq!(grid.len(), 2);

        assert_eq!(
            grid.row(0).unwrap().get("doc"),
            Some(&Kind::Str("Hello world\nSecond line".into()))
        );
        assert_eq!(
            grid.row(1).unwrap().get("dis"),
            Some(&Kind::Str("B".into()))
        );
    }

    #[test]
    fn roundtrip_encode_decode() {
        use crate::codecs::trio::encode_grid;
        use crate::data::HCol;

        let cols = vec![
            HCol::new("area"),
            HCol::new("dis"),
            HCol::new("id"),
            HCol::new("site"),
        ];
        let mut row1 = HDict::new();
        row1.set("dis", Kind::Str("My Site".into()));
        row1.set("site", Kind::Marker);
        row1.set(
            "area",
            Kind::Number(Number::new(1000.0, Some("ft\u{00B2}".into()))),
        );
        row1.set("id", Kind::Ref(HRef::from_val("site-1")));

        let mut row2 = HDict::new();
        row2.set("dis", Kind::Str("AHU-1".into()));
        row2.set("id", Kind::Ref(HRef::from_val("ahu-1")));

        let g = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);
        let encoded = encode_grid(&g).unwrap();
        let decoded = decode_grid(&encoded).unwrap();

        assert_eq!(decoded.len(), 2);

        let r0 = decoded.row(0).unwrap();
        assert_eq!(r0.get("dis"), Some(&Kind::Str("My Site".into())));
        assert_eq!(r0.get("site"), Some(&Kind::Marker));
        assert_eq!(
            r0.get("area"),
            Some(&Kind::Number(Number::new(
                1000.0,
                Some("ft\u{00B2}".into())
            )))
        );
        assert_eq!(r0.get("id"), Some(&Kind::Ref(HRef::from_val("site-1"))));

        let r1 = decoded.row(1).unwrap();
        assert_eq!(r1.get("dis"), Some(&Kind::Str("AHU-1".into())));
        assert_eq!(r1.get("id"), Some(&Kind::Ref(HRef::from_val("ahu-1"))));
    }

    #[test]
    fn roundtrip_multiline_string() {
        use crate::codecs::trio::encode_grid;
        use crate::data::HCol;

        let cols = vec![HCol::new("dis"), HCol::new("doc")];
        let mut row = HDict::new();
        row.set("dis", Kind::Str("Test".into()));
        row.set("doc", Kind::Str("Line 1\nLine 2\nLine 3".into()));

        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);
        let encoded = encode_grid(&g).unwrap();
        let decoded = decode_grid(&encoded).unwrap();

        assert_eq!(decoded.len(), 1);
        let r = decoded.row(0).unwrap();
        assert_eq!(r.get("dis"), Some(&Kind::Str("Test".into())));
        assert_eq!(
            r.get("doc"),
            Some(&Kind::Str("Line 1\nLine 2\nLine 3".into()))
        );
    }

    #[test]
    fn parse_uri_value() {
        use crate::kinds::Uri;

        let input = "href: `http://example.com/api`\n";
        let grid = decode_grid(input).unwrap();
        let row = grid.row(0).unwrap();
        assert_eq!(
            row.get("href"),
            Some(&Kind::Uri(Uri::new("http://example.com/api")))
        );
    }

    #[test]
    fn codec_for_registry() {
        use crate::codecs::codec_for;

        let trio = codec_for("text/trio").expect("trio codec should be registered");
        assert_eq!(trio.mime_type(), "text/trio");

        let zinc = codec_for("text/zinc").expect("zinc codec should be registered");
        assert_eq!(zinc.mime_type(), "text/zinc");

        assert!(codec_for("text/json").is_none());
    }

    #[test]
    fn trio_codec_trait_impl() {
        use crate::codecs::Codec;
        use crate::codecs::trio::TrioCodec;

        let codec = TrioCodec;
        assert_eq!(codec.mime_type(), "text/trio");

        // Scalar encoding/decoding delegates to Zinc
        let val = Kind::Number(Number::unitless(42.0));
        let encoded = codec.encode_scalar(&val).unwrap();
        assert_eq!(encoded, "42");
        let decoded = codec.decode_scalar(&encoded).unwrap();
        assert_eq!(decoded, val);
    }
}
