// Zinc scalar and grid encoder.

use crate::codecs::CodecError;
use crate::codecs::shared;
use crate::data::{HDict, HGrid};
use crate::kinds::Kind;

/// Encode a single Kind value to its Zinc string representation.
pub fn encode_scalar(val: &Kind) -> Result<String, CodecError> {
    match val {
        Kind::Null => Ok("N".to_string()),
        Kind::Bool(true) => Ok("T".to_string()),
        Kind::Bool(false) => Ok("F".to_string()),
        Kind::Marker => Ok("M".to_string()),
        Kind::NA => Ok("NA".to_string()),
        Kind::Remove => Ok("R".to_string()),
        Kind::Number(n) => Ok(encode_number(n)),
        Kind::Str(s) => Ok(encode_str(s)),
        Kind::Ref(r) => Ok(encode_ref(r)),
        Kind::Uri(u) => Ok(format!("`{}`", u.val())),
        Kind::Symbol(s) => Ok(format!("^{}", s.val())),
        Kind::Date(d) => Ok(d.format("%Y-%m-%d").to_string()),
        Kind::Time(t) => Ok(encode_time(t)),
        Kind::DateTime(hdt) => Ok(encode_datetime(hdt)),
        Kind::Coord(c) => Ok(format!("C({},{})", c.lat, c.lng)),
        Kind::XStr(x) => Ok(format!("{}(\"{}\")", x.type_name, escape_str(&x.val))),
        Kind::List(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                parts.push(encode_scalar(item)?);
            }
            Ok(format!("[{}]", parts.join(", ")))
        }
        Kind::Dict(d) => Ok(encode_dict_inline(d)?),
        Kind::Grid(_) => Err(CodecError::Encode(
            "grids cannot be encoded as scalars".to_string(),
        )),
    }
}

/// Encode a Number to its Zinc string representation.
fn encode_number(n: &crate::kinds::Number) -> String {
    let s = shared::format_number_val(n.val);
    match &n.unit {
        Some(u) => format!("{s}{u}"),
        None => s,
    }
}

/// Encode a time value, always including seconds.
fn encode_time(t: &chrono::NaiveTime) -> String {
    shared::format_time(t)
}

use chrono::Timelike;

/// Encode a Haystack DateTime to Zinc format.
fn encode_datetime(hdt: &crate::kinds::HDateTime) -> String {
    let dt_str = hdt.dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    let frac = shared::format_frac_seconds(hdt.dt.nanosecond());
    let offset_str = hdt.dt.format("%:z").to_string();
    let tz = &hdt.tz_name;
    format!("{dt_str}{frac}{offset_str} {tz}")
}

/// Encode a string value (with outer quotes).
fn encode_str(s: &str) -> String {
    format!("\"{}\"", escape_str(s))
}

/// Escape string content for Zinc format (without outer quotes).
pub fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '$' => out.push_str("\\$"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            _ => out.push(ch),
        }
    }
    out
}

/// Encode an HRef to Zinc format.
fn encode_ref(r: &crate::kinds::HRef) -> String {
    match &r.dis {
        Some(dis) => format!("@{} \"{}\"", r.val, escape_str(dis)),
        None => format!("@{}", r.val),
    }
}

/// Encode an HDict inline (for use inside grid cells or nested dicts).
fn encode_dict_inline(d: &HDict) -> Result<String, CodecError> {
    let mut parts = Vec::new();
    for (k, v) in d.sorted_iter() {
        if matches!(v, Kind::Marker) {
            parts.push(k.to_string());
        } else {
            parts.push(format!("{}:{}", k, encode_scalar(v)?));
        }
    }
    Ok(format!("{{{}}}", parts.join(" ")))
}

/// Encode metadata tags in inline format for grid/column metadata.
/// Format: `"tag1 tag2:val2 tag3:val3"`
pub fn encode_meta(d: &HDict) -> Result<String, CodecError> {
    let mut parts = Vec::new();
    for (k, v) in d.sorted_iter() {
        if matches!(v, Kind::Marker) {
            parts.push(k.to_string());
        } else {
            parts.push(format!("{}:{}", k, encode_scalar(v)?));
        }
    }
    Ok(parts.join(" "))
}

/// Encode an HGrid to the Zinc wire format.
pub fn encode_grid(grid: &HGrid) -> Result<String, CodecError> {
    let mut buf = String::new();

    // Line 1: version + grid meta
    buf.push_str("ver:\"3.0\"");
    if !grid.meta.is_empty() {
        buf.push(' ');
        buf.push_str(&encode_meta(&grid.meta)?);
    }
    buf.push('\n');

    // Line 2: columns
    if grid.cols.is_empty() {
        buf.push_str("empty\n");
    } else {
        let col_parts: Result<Vec<String>, CodecError> = grid
            .cols
            .iter()
            .map(|col| {
                let mut s = col.name.clone();
                if !col.meta.is_empty() {
                    s.push(' ');
                    s.push_str(&encode_meta(&col.meta)?);
                }
                Ok(s)
            })
            .collect();
        buf.push_str(&col_parts?.join(","));
        buf.push('\n');
    }

    // Rows
    for row in &grid.rows {
        let cells: Result<Vec<String>, CodecError> = grid
            .cols
            .iter()
            .map(|col| match row.get(&col.name) {
                Some(val) => encode_scalar(val),
                None => Ok("N".to_string()),
            })
            .collect();
        buf.push_str(&cells?.join(","));
        buf.push('\n');
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{HCol, HDict, HGrid};
    use crate::kinds::*;
    use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};

    #[test]
    fn encode_null() {
        assert_eq!(encode_scalar(&Kind::Null).unwrap(), "N");
    }

    #[test]
    fn encode_bool_true() {
        assert_eq!(encode_scalar(&Kind::Bool(true)).unwrap(), "T");
    }

    #[test]
    fn encode_bool_false() {
        assert_eq!(encode_scalar(&Kind::Bool(false)).unwrap(), "F");
    }

    #[test]
    fn encode_marker() {
        assert_eq!(encode_scalar(&Kind::Marker).unwrap(), "M");
    }

    #[test]
    fn encode_na() {
        assert_eq!(encode_scalar(&Kind::NA).unwrap(), "NA");
    }

    #[test]
    fn encode_remove() {
        assert_eq!(encode_scalar(&Kind::Remove).unwrap(), "R");
    }

    #[test]
    fn encode_number_zero() {
        let k = Kind::Number(Number::unitless(0.0));
        assert_eq!(encode_scalar(&k).unwrap(), "0");
    }

    #[test]
    fn encode_number_integer() {
        let k = Kind::Number(Number::unitless(42.0));
        assert_eq!(encode_scalar(&k).unwrap(), "42");
    }

    #[test]
    fn encode_number_float() {
        let k = Kind::Number(Number::unitless(72.5));
        assert_eq!(encode_scalar(&k).unwrap(), "72.5");
    }

    #[test]
    fn encode_number_negative() {
        let k = Kind::Number(Number::unitless(-23.45));
        assert_eq!(encode_scalar(&k).unwrap(), "-23.45");
    }

    #[test]
    fn encode_number_with_unit() {
        let k = Kind::Number(Number::new(72.5, Some("\u{00B0}F".into())));
        assert_eq!(encode_scalar(&k).unwrap(), "72.5\u{00B0}F");
    }

    #[test]
    fn encode_number_inf() {
        let k = Kind::Number(Number::unitless(f64::INFINITY));
        assert_eq!(encode_scalar(&k).unwrap(), "INF");
    }

    #[test]
    fn encode_number_neg_inf() {
        let k = Kind::Number(Number::unitless(f64::NEG_INFINITY));
        assert_eq!(encode_scalar(&k).unwrap(), "-INF");
    }

    #[test]
    fn encode_number_nan() {
        let k = Kind::Number(Number::unitless(f64::NAN));
        assert_eq!(encode_scalar(&k).unwrap(), "NaN");
    }

    #[test]
    fn encode_string_simple() {
        let k = Kind::Str("hello".into());
        assert_eq!(encode_scalar(&k).unwrap(), "\"hello\"");
    }

    #[test]
    fn encode_string_empty() {
        let k = Kind::Str(String::new());
        assert_eq!(encode_scalar(&k).unwrap(), "\"\"");
    }

    #[test]
    fn encode_string_escapes() {
        let k = Kind::Str("line1\nline2\ttab\\slash\"quote$dollar".into());
        let encoded = encode_scalar(&k).unwrap();
        assert_eq!(
            encoded,
            "\"line1\\nline2\\ttab\\\\slash\\\"quote\\$dollar\""
        );
    }

    #[test]
    fn encode_ref_simple() {
        let k = Kind::Ref(HRef::from_val("site-1"));
        assert_eq!(encode_scalar(&k).unwrap(), "@site-1");
    }

    #[test]
    fn encode_ref_with_dis() {
        let k = Kind::Ref(HRef::new("site-1", Some("Main Site".into())));
        assert_eq!(encode_scalar(&k).unwrap(), "@site-1 \"Main Site\"");
    }

    #[test]
    fn encode_uri() {
        let k = Kind::Uri(Uri::new("http://example.com"));
        assert_eq!(encode_scalar(&k).unwrap(), "`http://example.com`");
    }

    #[test]
    fn encode_symbol() {
        let k = Kind::Symbol(Symbol::new("hot-water"));
        assert_eq!(encode_scalar(&k).unwrap(), "^hot-water");
    }

    #[test]
    fn encode_date() {
        let k = Kind::Date(NaiveDate::from_ymd_opt(2024, 3, 13).unwrap());
        assert_eq!(encode_scalar(&k).unwrap(), "2024-03-13");
    }

    #[test]
    fn encode_time_no_frac() {
        let k = Kind::Time(NaiveTime::from_hms_opt(8, 12, 5).unwrap());
        assert_eq!(encode_scalar(&k).unwrap(), "08:12:05");
    }

    #[test]
    fn encode_time_with_frac() {
        let k = Kind::Time(NaiveTime::from_hms_milli_opt(14, 30, 0, 123).unwrap());
        assert_eq!(encode_scalar(&k).unwrap(), "14:30:00.123");
    }

    #[test]
    fn encode_datetime() {
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 8, 12, 5).unwrap();
        let hdt = HDateTime::new(dt, "New_York");
        let k = Kind::DateTime(hdt);
        assert_eq!(
            encode_scalar(&k).unwrap(),
            "2024-01-01T08:12:05-05:00 New_York"
        );
    }

    #[test]
    fn encode_datetime_utc() {
        let offset = FixedOffset::east_opt(0).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap();
        let hdt = HDateTime::new(dt, "UTC");
        let k = Kind::DateTime(hdt);
        assert_eq!(encode_scalar(&k).unwrap(), "2024-06-15T12:00:00+00:00 UTC");
    }

    #[test]
    fn encode_coord() {
        let k = Kind::Coord(Coord::new(37.5458266, -77.4491888));
        assert_eq!(encode_scalar(&k).unwrap(), "C(37.5458266,-77.4491888)");
    }

    #[test]
    fn encode_xstr() {
        let k = Kind::XStr(XStr::new("Color", "red"));
        assert_eq!(encode_scalar(&k).unwrap(), "Color(\"red\")");
    }

    #[test]
    fn encode_list_empty() {
        let k = Kind::List(vec![]);
        assert_eq!(encode_scalar(&k).unwrap(), "[]");
    }

    #[test]
    fn encode_list_mixed() {
        let k = Kind::List(vec![
            Kind::Number(Number::unitless(1.0)),
            Kind::Str("two".into()),
            Kind::Marker,
        ]);
        assert_eq!(encode_scalar(&k).unwrap(), "[1, \"two\", M]");
    }

    #[test]
    fn encode_dict_empty() {
        let k = Kind::Dict(Box::new(HDict::new()));
        assert_eq!(encode_scalar(&k).unwrap(), "{}");
    }

    #[test]
    fn encode_dict_with_values() {
        let mut d = HDict::new();
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str("Main".into()));
        let k = Kind::Dict(Box::new(d));
        let encoded = encode_scalar(&k).unwrap();
        // Sorted keys: dis, site
        assert_eq!(encoded, "{dis:\"Main\" site}");
    }

    #[test]
    fn encode_grid_error() {
        let k = Kind::Grid(Box::new(HGrid::new()));
        assert!(encode_scalar(&k).is_err());
    }

    #[test]
    fn encode_grid_empty() {
        let g = HGrid::new();
        let encoded = encode_grid(&g).unwrap();
        assert_eq!(encoded, "ver:\"3.0\"\nempty\n");
    }

    #[test]
    fn encode_grid_with_data() {
        let cols = vec![HCol::new("dis"), HCol::new("area")];
        let mut row1 = HDict::new();
        row1.set("dis", Kind::Str("Site One".into()));
        row1.set("area", Kind::Number(Number::unitless(4500.0)));
        let mut row2 = HDict::new();
        row2.set("dis", Kind::Str("Site Two".into()));
        // area missing in row2

        let g = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);
        let encoded = encode_grid(&g).unwrap();
        let lines: Vec<&str> = encoded.lines().collect();
        assert_eq!(lines[0], "ver:\"3.0\"");
        assert_eq!(lines[1], "dis,area");
        assert_eq!(lines[2], "\"Site One\",4500");
        assert_eq!(lines[3], "\"Site Two\",N");
    }

    #[test]
    fn encode_grid_with_meta() {
        let mut meta = HDict::new();
        meta.set("err", Kind::Marker);
        meta.set("dis", Kind::Str("some error".into()));

        let g = HGrid::from_parts(meta, vec![], vec![]);
        let encoded = encode_grid(&g).unwrap();
        let first_line = encoded.lines().next().unwrap();
        assert!(first_line.starts_with("ver:\"3.0\" "));
        assert!(first_line.contains("err"));
        assert!(first_line.contains("dis:\"some error\""));
    }

    #[test]
    fn encode_grid_with_col_meta() {
        let mut col_meta = HDict::new();
        col_meta.set("unit", Kind::Str("kW".into()));
        let cols = vec![HCol::new("name"), HCol::with_meta("power", col_meta)];
        let g = HGrid::from_parts(HDict::new(), cols, vec![]);
        let encoded = encode_grid(&g).unwrap();
        let lines: Vec<&str> = encoded.lines().collect();
        assert_eq!(lines[1], "name,power unit:\"kW\"");
    }

    #[test]
    fn encode_escape_str() {
        assert_eq!(escape_str("hello"), "hello");
        assert_eq!(escape_str("a\\b"), "a\\\\b");
        assert_eq!(escape_str("a\"b"), "a\\\"b");
        assert_eq!(escape_str("a\nb"), "a\\nb");
        assert_eq!(escape_str("a\rb"), "a\\rb");
        assert_eq!(escape_str("a\tb"), "a\\tb");
        assert_eq!(escape_str("a$b"), "a\\$b");
        assert_eq!(escape_str("a\u{0008}b"), "a\\bb");
        assert_eq!(escape_str("a\u{000C}b"), "a\\fb");
    }
}
