// CSV wire format codec — encode-only CSV output for Haystack grids.

use super::{Codec, CodecError};
use crate::codecs::zinc;
use crate::data::HGrid;
use crate::kinds::Kind;

/// CSV wire format codec (encode only).
pub struct CsvCodec;

/// Escape a value for inclusion in a CSV cell.
///
/// The value is always wrapped in double quotes, and any internal
/// double-quote characters are escaped by doubling them (`""`).
fn csv_quote(val: &str) -> String {
    let mut out = String::with_capacity(val.len() + 2);
    out.push('"');
    for ch in val.chars() {
        if ch == '"' {
            out.push_str("\"\"");
        } else {
            out.push(ch);
        }
    }
    out.push('"');
    out
}

/// Encode an HGrid to CSV format.
fn encode_grid(grid: &HGrid) -> Result<String, CodecError> {
    let mut buf = String::new();

    // Header row: quoted column names
    let headers: Vec<String> = grid.cols.iter().map(|col| csv_quote(&col.name)).collect();
    buf.push_str(&headers.join(","));
    buf.push('\n');

    // Data rows: Zinc-encoded scalar values, quoted for CSV
    for row in &grid.rows {
        let cells: Result<Vec<String>, CodecError> = grid
            .cols
            .iter()
            .map(|col| {
                let val = match row.get(&col.name) {
                    Some(v) => v,
                    None => &Kind::Null,
                };
                let zinc_str = zinc::encode_scalar(val)?;
                Ok(csv_quote(&zinc_str))
            })
            .collect();
        buf.push_str(&cells?.join(","));
        buf.push('\n');
    }

    Ok(buf)
}

impl Codec for CsvCodec {
    fn mime_type(&self) -> &str {
        "text/csv"
    }

    fn encode_grid(&self, grid: &HGrid) -> Result<String, CodecError> {
        encode_grid(grid)
    }

    fn decode_grid(&self, _input: &str) -> Result<HGrid, CodecError> {
        Err(CodecError::Parse {
            pos: 0,
            message: "CSV decode not supported".into(),
        })
    }

    fn encode_scalar(&self, val: &Kind) -> Result<String, CodecError> {
        zinc::encode_scalar(val)
    }

    fn decode_scalar(&self, input: &str) -> Result<Kind, CodecError> {
        zinc::decode_scalar(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{HCol, HDict, HGrid};
    use crate::kinds::*;
    use chrono::NaiveDate;

    #[test]
    fn encode_grid_mixed_types() {
        let cols = vec![
            HCol::new("dis"),
            HCol::new("area"),
            HCol::new("built"),
            HCol::new("site"),
        ];

        let mut row1 = HDict::new();
        row1.set("dis", Kind::Str("Alpha".into()));
        row1.set(
            "area",
            Kind::Number(Number::new(3500.0, Some("ft\u{00B2}".into()))),
        );
        row1.set(
            "built",
            Kind::Date(NaiveDate::from_ymd_opt(2020, 6, 15).unwrap()),
        );
        row1.set("site", Kind::Marker);

        let mut row2 = HDict::new();
        row2.set("dis", Kind::Str("Beta".into()));
        row2.set("area", Kind::Number(Number::unitless(2100.0)));
        // built is missing in row2
        row2.set("site", Kind::Bool(false));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);
        let csv = encode_grid(&grid).unwrap();
        let lines: Vec<&str> = csv.lines().collect();

        assert_eq!(lines[0], r#""dis","area","built","site""#);
        // Zinc encodes Kind::Str("Alpha") as "Alpha" (with quotes).
        // csv_quote doubles the inner " chars: """Alpha"""
        assert_eq!(
            lines[1],
            "\"\"\"Alpha\"\"\",\"3500ft\u{00B2}\",\"2020-06-15\",\"M\""
        );
        assert_eq!(lines[2], "\"\"\"Beta\"\"\",\"2100\",\"N\",\"F\"");
    }

    #[test]
    fn encode_empty_grid() {
        let grid = HGrid::new();
        let csv = encode_grid(&grid).unwrap();
        // Empty grid has no columns so the header row is just a newline
        assert_eq!(csv, "\n");
    }

    #[test]
    fn encode_grid_with_commas_and_quotes_in_strings() {
        let cols = vec![HCol::new("name"), HCol::new("notes")];

        let mut row = HDict::new();
        row.set("name", Kind::Str("O'Brien, James".into()));
        row.set("notes", Kind::Str("He said \"hello\"".into()));

        let grid = HGrid::from_parts(HDict::new(), cols, vec![row]);
        let csv = encode_grid(&grid).unwrap();
        let lines: Vec<&str> = csv.lines().collect();

        assert_eq!(lines[0], "\"name\",\"notes\"");

        // The Zinc encoder encodes the strings with Zinc escaping (\" for
        // double quotes), and then csv_quote wraps the result in CSV
        // double quotes, doubling any literal " that appear.
        //
        // Kind::Str("O'Brien, James") -> zinc encode -> "O'Brien, James"
        //   (with outer zinc quotes and backslash-escaped inner content)
        // csv_quote on that -> wrap in CSV quotes, doubling the " chars:
        //   """O'Brien, James"""
        //
        // Kind::Str("He said \"hello\"") -> zinc encode -> "He said \"hello\""
        // csv_quote on that:
        //   """He said \""hello\""""

        // The name cell: zinc produces "O'Brien, James" (with quotes),
        // csv_quote doubles the quotes -> """O'Brien, James"""
        assert!(lines[1].starts_with("\"\"\"O'Brien, James\"\"\""));

        // The notes cell: zinc produces "He said \"hello\""
        // csv_quote doubles the literal " chars
        let notes_cell = lines[1].split(',').skip(1).collect::<Vec<_>>().join(",");
        assert!(notes_cell.contains("He said"));
        assert!(notes_cell.contains("hello"));
    }

    #[test]
    fn decode_grid_not_supported() {
        let codec = CsvCodec;
        let result = codec.decode_grid("anything");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("CSV decode not supported"));
    }

    #[test]
    fn scalar_delegates_to_zinc() {
        let codec = CsvCodec;
        // encode
        let encoded = codec
            .encode_scalar(&Kind::Number(Number::unitless(42.0)))
            .unwrap();
        assert_eq!(encoded, "42");

        // decode
        let decoded = codec.decode_scalar("42").unwrap();
        assert_eq!(decoded, Kind::Number(Number::unitless(42.0)));
    }

    #[test]
    fn mime_type() {
        let codec = CsvCodec;
        assert_eq!(codec.mime_type(), "text/csv");
    }

    #[test]
    fn encode_grid_cols_no_rows() {
        let cols = vec![HCol::new("a"), HCol::new("b")];
        let grid = HGrid::from_parts(HDict::new(), cols, vec![]);
        let csv = encode_grid(&grid).unwrap();
        assert_eq!(csv, "\"a\",\"b\"\n");
    }

    #[test]
    fn csv_quote_escapes_double_quotes() {
        assert_eq!(csv_quote("hello"), "\"hello\"");
        assert_eq!(csv_quote("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_quote(""), "\"\"");
        assert_eq!(csv_quote("a,b"), "\"a,b\"");
    }
}
