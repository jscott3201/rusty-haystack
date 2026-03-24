// JSON v3 codec — application/json;v=3 with type-prefix strings.

use crate::codecs::shared;
use crate::codecs::{Codec, CodecError};
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::*;
use chrono::{NaiveDate, NaiveTime, Timelike};
use serde_json::{Map, Value};

/// JSON v3 wire format codec (application/json;v=3).
///
/// Uses type-prefix strings for scalars (e.g., `"m:"`, `"n:72 °F"`, `"s:text"`).
pub struct Json3Codec;

impl Codec for Json3Codec {
    fn mime_type(&self) -> &str {
        "application/json;v=3"
    }

    fn encode_grid(&self, grid: &HGrid) -> Result<String, CodecError> {
        let val = encode_grid_value(grid)?;
        serde_json::to_string(&val).map_err(|e| CodecError::Encode(e.to_string()))
    }

    fn decode_grid(&self, input: &str) -> Result<HGrid, CodecError> {
        let val: Value = serde_json::from_str(input).map_err(|e| CodecError::Parse {
            pos: 0,
            message: e.to_string(),
        })?;
        decode_grid_value(&val)
    }

    fn encode_scalar(&self, val: &Kind) -> Result<String, CodecError> {
        let json = encode_kind(val)?;
        serde_json::to_string(&json).map_err(|e| CodecError::Encode(e.to_string()))
    }

    fn decode_scalar(&self, input: &str) -> Result<Kind, CodecError> {
        let val: Value = serde_json::from_str(input).map_err(|e| CodecError::Parse {
            pos: 0,
            message: e.to_string(),
        })?;
        decode_kind(&val)
    }
}

// ── Encoding ──

/// Encode a Kind value to a serde_json Value in v3 format.
pub fn encode_kind(val: &Kind) -> Result<Value, CodecError> {
    match val {
        Kind::Null => Ok(Value::Null),
        Kind::Bool(b) => Ok(Value::Bool(*b)),
        Kind::Marker => Ok(Value::String("m:".into())),
        Kind::NA => Ok(Value::String("z:".into())),
        Kind::Remove => Ok(Value::String("-:".into())),
        Kind::Number(n) => Ok(encode_number(n)),
        Kind::Str(s) => Ok(Value::String(format!("s:{s}"))),
        Kind::Ref(r) => Ok(encode_ref(r)),
        Kind::Uri(u) => Ok(Value::String(format!("u:{}", u.val()))),
        Kind::Symbol(s) => Ok(Value::String(format!("y:{}", s.val()))),
        Kind::Date(d) => Ok(Value::String(format!("d:{}", d.format("%Y-%m-%d")))),
        Kind::Time(t) => Ok(Value::String(format!("h:{}", encode_time_str(t)))),
        Kind::DateTime(hdt) => Ok(encode_datetime(hdt)),
        Kind::Coord(c) => Ok(Value::String(format!("c:{},{}", c.lat, c.lng))),
        Kind::XStr(x) => Ok(Value::String(format!("x:{}:{}", x.type_name, x.val))),
        Kind::List(items) => {
            let arr: Result<Vec<Value>, CodecError> = items.iter().map(encode_kind).collect();
            Ok(Value::Array(arr?))
        }
        Kind::Dict(d) => encode_dict(d),
        Kind::Grid(g) => {
            let val = encode_grid_value(g)?;
            Ok(Value::Object(val))
        }
    }
}

/// Encode a Number to v3 format: `"n:72 °F"` or `"n:72"`.
fn encode_number(n: &Number) -> Value {
    let val_str = shared::format_number_val(n.val);
    match &n.unit {
        Some(u) => Value::String(format!("n:{val_str} {u}")),
        None => Value::String(format!("n:{val_str}")),
    }
}

/// Encode a Ref to v3 format: `"r:abc Display Name"` or `"r:abc"`.
fn encode_ref(r: &HRef) -> Value {
    match &r.dis {
        Some(dis) => Value::String(format!("r:{} {}", r.val, dis)),
        None => Value::String(format!("r:{}", r.val)),
    }
}

/// Encode a DateTime to v3 format: `"t:2024-01-01T12:30:45-05:00 New_York"`.
fn encode_datetime(hdt: &HDateTime) -> Value {
    let dt_str = hdt.dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    let frac = shared::format_frac_seconds(hdt.dt.nanosecond());
    let offset_str = hdt.dt.format("%:z").to_string();
    if hdt.tz_name.is_empty() {
        Value::String(format!("t:{dt_str}{frac}{offset_str}"))
    } else {
        Value::String(format!("t:{dt_str}{frac}{offset_str} {}", hdt.tz_name))
    }
}

/// Format a NaiveTime to string (HH:MM:SS with optional fractional seconds).
fn encode_time_str(t: &NaiveTime) -> String {
    shared::format_time(t)
}

/// Encode an HDict as a plain JSON object.
fn encode_dict(d: &HDict) -> Result<Value, CodecError> {
    let mut m = Map::new();
    for (k, v) in d.sorted_tags() {
        m.insert(k.to_string(), encode_kind(v)?);
    }
    Ok(Value::Object(m))
}

/// Encode an HGrid as a v3 JSON object.
fn encode_grid_value(grid: &HGrid) -> Result<Map<String, Value>, CodecError> {
    let mut m = Map::new();

    // meta — always includes ver
    let mut meta_map = Map::new();
    meta_map.insert("ver".into(), Value::String("3.0".into()));
    for (k, v) in grid.meta.sorted_tags() {
        meta_map.insert(k.to_string(), encode_kind(v)?);
    }
    m.insert("meta".into(), Value::Object(meta_map));

    // cols
    let cols: Result<Vec<Value>, CodecError> = grid
        .cols
        .iter()
        .map(|col| {
            let mut cm = Map::new();
            cm.insert("name".into(), Value::String(col.name.clone()));
            // Flatten meta into column object (v3 spec)
            if !col.meta.is_empty()
                && let Value::Object(meta_map) = encode_dict(&col.meta)?
            {
                for (k, v) in meta_map {
                    cm.insert(k, v);
                }
            }
            Ok(Value::Object(cm))
        })
        .collect();
    m.insert("cols".into(), Value::Array(cols?));

    // rows
    let rows: Result<Vec<Value>, CodecError> = grid.rows.iter().map(encode_dict).collect();
    m.insert("rows".into(), Value::Array(rows?));

    Ok(m)
}

// ── Decoding ──

/// Maximum nesting depth for recursive JSON decoding to prevent stack overflow.
const MAX_NESTING_DEPTH: usize = 64;

/// Decode a serde_json Value to a Kind using v3 format.
pub fn decode_kind(val: &Value) -> Result<Kind, CodecError> {
    decode_kind_depth(val, 0)
}

/// Decode a serde_json Value to a Kind using v3 format, tracking recursion depth.
fn decode_kind_depth(val: &Value, depth: usize) -> Result<Kind, CodecError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(CodecError::Parse {
            pos: 0,
            message: "maximum nesting depth exceeded".into(),
        });
    }
    match val {
        Value::Null => Ok(Kind::Null),
        Value::Bool(b) => Ok(Kind::Bool(*b)),
        Value::Number(n) => {
            // Plain JSON numbers decode as Number(val, None)
            let v = n.as_f64().ok_or_else(|| CodecError::Parse {
                pos: 0,
                message: format!("cannot convert JSON number to f64: {n}"),
            })?;
            Ok(Kind::Number(Number::unitless(v)))
        }
        Value::String(s) => decode_prefixed_string(s),
        Value::Array(arr) => {
            let items: Result<Vec<Kind>, CodecError> = arr
                .iter()
                .map(|v| decode_kind_depth(v, depth + 1))
                .collect();
            Ok(Kind::List(items?))
        }
        Value::Object(m) => decode_object_depth(m, depth + 1),
    }
}

/// Decode a type-prefixed string (e.g., `"m:"`, `"n:72 °F"`, `"s:text"`).
fn decode_prefixed_string(s: &str) -> Result<Kind, CodecError> {
    // Check for 2-char prefix patterns (x:)
    if s.len() >= 2 {
        let prefix = &s[..2];
        let rest = &s[2..];
        match prefix {
            "m:" => {
                if rest.is_empty() {
                    return Ok(Kind::Marker);
                }
            }
            "z:" => {
                if rest.is_empty() {
                    return Ok(Kind::NA);
                }
            }
            "-:" => {
                if rest.is_empty() {
                    return Ok(Kind::Remove);
                }
            }
            "s:" => return Ok(Kind::Str(rest.to_string())),
            "n:" => return decode_number_str(rest),
            "r:" => return decode_ref_str(rest),
            "u:" => return Ok(Kind::Uri(Uri::new(rest))),
            "y:" => return Ok(Kind::Symbol(Symbol::new(rest))),
            "d:" => return decode_date_str(rest),
            "h:" => return decode_time_str(rest),
            "t:" => return decode_datetime_str(rest),
            "c:" => return decode_coord_str(rest),
            "x:" => return decode_xstr_str(rest),
            _ => {}
        }
    }
    // No prefix match — treat as plain string.
    // This graceful degradation is by-design for the JSON v3 format: unrecognized
    // or missing type prefixes fall back to plain strings rather than producing
    // errors, allowing forward-compatibility when new type prefixes are introduced.
    Ok(Kind::Str(s.to_string()))
}

/// Decode a number from the v3 `n:` prefix body: `"72 °F"` or `"72"`.
fn decode_number_str(s: &str) -> Result<Kind, CodecError> {
    // Format: "val" or "val unit" (space-separated)
    // Special values: INF, -INF, NaN (with optional unit after space)
    let (val_str, unit) = match s.find(' ') {
        Some(pos) => {
            let val_part = &s[..pos];
            let unit_part = &s[pos + 1..];
            (
                val_part,
                if unit_part.is_empty() {
                    None
                } else {
                    Some(unit_part.to_string())
                },
            )
        }
        None => (s, None),
    };

    let v = match val_str {
        "INF" => f64::INFINITY,
        "-INF" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        _ => val_str.parse::<f64>().map_err(|e| CodecError::Parse {
            pos: 0,
            message: format!("invalid v3 number: {e}"),
        })?,
    };
    Ok(Kind::Number(Number::new(v, unit)))
}

/// Decode a ref from the v3 `r:` prefix body: `"abc Display Name"` or `"abc"`.
fn decode_ref_str(s: &str) -> Result<Kind, CodecError> {
    // First token is the ref val, rest (after first space) is display name
    match s.find(' ') {
        Some(pos) => {
            let val = &s[..pos];
            let dis = &s[pos + 1..];
            Ok(Kind::Ref(HRef::new(val, Some(dis.to_string()))))
        }
        None => Ok(Kind::Ref(HRef::from_val(s))),
    }
}

/// Decode a date from the v3 `d:` prefix body: `"2024-01-01"`.
fn decode_date_str(s: &str) -> Result<Kind, CodecError> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|e| CodecError::Parse {
        pos: 0,
        message: format!("invalid v3 date: {e}"),
    })?;
    Ok(Kind::Date(d))
}

/// Decode a time from the v3 `h:` prefix body: `"12:30:45"`.
fn decode_time_str(s: &str) -> Result<Kind, CodecError> {
    NaiveTime::parse_from_str(s, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .map(Kind::Time)
        .map_err(|e| CodecError::Parse {
            pos: 0,
            message: format!("invalid v3 time: {e}"),
        })
}

/// Decode a datetime from the v3 `t:` prefix body:
/// `"2024-01-01T12:30:45-05:00 New_York"` or `"2024-01-01T12:30:45-05:00"`.
fn decode_datetime_str(s: &str) -> Result<Kind, CodecError> {
    // The tz name is separated by a space after the offset.
    // We need to find the tz name carefully — the offset ends after the timezone
    // offset pattern (e.g., "-05:00" or "+00:00" or "Z").
    // Strategy: try to find the last space that comes after the datetime part.

    let (dt_str, tz_name) = split_datetime_tz(s);

    let dt = chrono::DateTime::parse_from_rfc3339(dt_str)
        .or_else(|_| chrono::DateTime::parse_from_str(dt_str, "%Y-%m-%dT%H:%M:%S%:z"))
        .or_else(|_| chrono::DateTime::parse_from_str(dt_str, "%Y-%m-%dT%H:%M:%S%.f%:z"))
        .map_err(|e| CodecError::Parse {
            pos: 0,
            message: format!("invalid v3 datetime: {e}"),
        })?;

    Ok(Kind::DateTime(HDateTime::new(dt, tz_name)))
}

/// Split a datetime string into the ISO datetime part and optional timezone name.
///
/// Input formats:
/// - `"2024-01-01T12:30:45-05:00 New_York"` -> `("2024-01-01T12:30:45-05:00", "New_York")`
/// - `"2024-01-01T12:30:45+00:00"` -> `("2024-01-01T12:30:45+00:00", "")`
fn split_datetime_tz(s: &str) -> (&str, &str) {
    // Look for the offset pattern — it's either +HH:MM, -HH:MM, or Z
    // After the offset, there may be a space followed by the tz name.

    // Find the offset: scan for + or - after the T
    if let Some(t_pos) = s.find('T') {
        let after_t = &s[t_pos..];
        // Find the last +/- in the string after T (for the offset)
        // The offset is the last sign followed by HH:MM
        let offset_end = find_offset_end(after_t);
        if let Some(end) = offset_end {
            let abs_end = t_pos + end;
            if abs_end < s.len() {
                let rest = &s[abs_end..];
                if let Some(space_pos) = rest.find(' ') {
                    let dt_part = &s[..abs_end + space_pos];
                    let tz_part = &s[abs_end + space_pos + 1..];
                    return (dt_part, tz_part);
                }
            }
            return (s, "");
        }
    }
    (s, "")
}

/// Find the end position (relative to input) of the UTC offset in a datetime string.
/// Returns the position after the offset (e.g., after "-05:00" or "Z" or "+00:00").
fn find_offset_end(s: &str) -> Option<usize> {
    // Look for Z
    if s.ends_with('Z') {
        return Some(s.len());
    }
    // Look for +HH:MM or -HH:MM at the end or before a space
    // Find the last occurrence of +/- that could be an offset
    for (i, c) in s.char_indices().rev() {
        if (c == '+' || c == '-') && i + 6 <= s.len() {
            // Check if this looks like an offset: +HH:MM or -HH:MM
            let candidate = &s[i..];
            if candidate.len() >= 6 {
                let hh = &candidate[1..3];
                let colon = &candidate[3..4];
                let mm = &candidate[4..6];
                if colon == ":"
                    && hh.chars().all(|c| c.is_ascii_digit())
                    && mm.chars().all(|c| c.is_ascii_digit())
                {
                    return Some(i + 6);
                }
            }
        }
    }
    None
}

/// Decode a coord from the v3 `c:` prefix body: `"37.5,-77.4"`.
fn decode_coord_str(s: &str) -> Result<Kind, CodecError> {
    let parts: Vec<&str> = s.splitn(2, ',').collect();
    if parts.len() != 2 {
        return Err(CodecError::Parse {
            pos: 0,
            message: format!("invalid v3 coord: expected 'lat,lng', got '{s}'"),
        });
    }
    let lat = parts[0].parse::<f64>().map_err(|e| CodecError::Parse {
        pos: 0,
        message: format!("invalid v3 coord lat: {e}"),
    })?;
    let lng = parts[1].parse::<f64>().map_err(|e| CodecError::Parse {
        pos: 0,
        message: format!("invalid v3 coord lng: {e}"),
    })?;
    Ok(Kind::Coord(Coord::new(lat, lng)))
}

/// Decode an xstr from the v3 `x:` prefix body: `"Type:value"`.
fn decode_xstr_str(s: &str) -> Result<Kind, CodecError> {
    match s.find(':') {
        Some(pos) => {
            let type_name = &s[..pos];
            let val = &s[pos + 1..];
            Ok(Kind::XStr(XStr::new(type_name, val)))
        }
        None => Err(CodecError::Parse {
            pos: 0,
            message: format!("invalid v3 xstr: expected 'Type:value', got '{s}'"),
        }),
    }
}

/// Decode a JSON object, tracking recursion depth.
fn decode_object_depth(m: &Map<String, Value>, depth: usize) -> Result<Kind, CodecError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(CodecError::Parse {
            pos: 0,
            message: "maximum nesting depth exceeded".into(),
        });
    }
    // Check if this is a v3 grid: has both "meta" and "cols" keys
    if m.contains_key("meta") && m.contains_key("cols") {
        let grid = decode_grid_from_map_depth(m, depth)?;
        return Ok(Kind::Grid(Box::new(grid)));
    }
    // Otherwise it's a plain dict
    let mut dict = HDict::new();
    for (key, val) in m {
        dict.set(key.clone(), decode_kind_depth(val, depth + 1)?);
    }
    Ok(Kind::Dict(Box::new(dict)))
}

/// Decode a v3 grid from a JSON Value.
pub fn decode_grid_value(val: &Value) -> Result<HGrid, CodecError> {
    let m = match val {
        Value::Object(m) => m,
        _ => {
            return Err(CodecError::Parse {
                pos: 0,
                message: "grid must be a JSON object".into(),
            });
        }
    };
    decode_grid_from_map_depth(m, 0)
}

/// Decode a v3 grid from a JSON object map, tracking recursion depth.
fn decode_grid_from_map_depth(m: &Map<String, Value>, depth: usize) -> Result<HGrid, CodecError> {
    if depth > MAX_NESTING_DEPTH {
        return Err(CodecError::Parse {
            pos: 0,
            message: "maximum nesting depth exceeded".into(),
        });
    }

    // meta (skip "ver" key)
    let meta = match m.get("meta") {
        Some(Value::Object(meta_map)) => {
            let mut dict = HDict::new();
            for (key, val) in meta_map {
                if key == "ver" {
                    continue;
                }
                dict.set(key.clone(), decode_kind_depth(val, depth + 1)?);
            }
            dict
        }
        _ => HDict::new(),
    };

    // cols
    let cols = match m.get("cols") {
        Some(Value::Array(arr)) => {
            let mut cols = Vec::with_capacity(arr.len());
            for col_val in arr {
                let col_obj = col_val.as_object().ok_or_else(|| CodecError::Parse {
                    pos: 0,
                    message: "col must be a JSON object".into(),
                })?;
                let name = match col_obj.get("name") {
                    Some(Value::String(n)) => n.clone(),
                    _ => {
                        return Err(CodecError::Parse {
                            pos: 0,
                            message: "col missing 'name' field".into(),
                        });
                    }
                };
                let mut col_meta = HDict::new();
                for (key, val) in col_obj {
                    if key != "name" {
                        col_meta.set(key.clone(), decode_kind_depth(val, depth + 1)?);
                    }
                }
                cols.push(HCol::with_meta(name, col_meta));
            }
            cols
        }
        _ => Vec::new(),
    };

    // rows
    let rows = match m.get("rows") {
        Some(Value::Array(arr)) => {
            let mut rows = Vec::with_capacity(arr.len());
            for row_val in arr {
                let row_obj = row_val.as_object().ok_or_else(|| CodecError::Parse {
                    pos: 0,
                    message: "row must be a JSON object".into(),
                })?;
                let mut dict = HDict::new();
                for (key, val) in row_obj {
                    dict.set(key.clone(), decode_kind_depth(val, depth + 1)?);
                }
                rows.push(dict);
            }
            rows
        }
        _ => Vec::new(),
    };

    Ok(HGrid::from_parts(meta, cols, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{HCol, HDict, HGrid};
    use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};

    fn roundtrip_scalar(kind: Kind) -> Kind {
        let codec = Json3Codec;
        let encoded = codec.encode_scalar(&kind).unwrap();
        codec.decode_scalar(&encoded).unwrap()
    }

    // ── Null ──

    #[test]
    fn null_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::Null), Kind::Null);
    }

    #[test]
    fn null_encodes_to_json_null() {
        let codec = Json3Codec;
        assert_eq!(codec.encode_scalar(&Kind::Null).unwrap(), "null");
    }

    // ── Bool ──

    #[test]
    fn bool_true_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::Bool(true)), Kind::Bool(true));
    }

    #[test]
    fn bool_false_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::Bool(false)), Kind::Bool(false));
    }

    #[test]
    fn bool_encodes_to_json_bool() {
        let codec = Json3Codec;
        assert_eq!(codec.encode_scalar(&Kind::Bool(true)).unwrap(), "true");
        assert_eq!(codec.encode_scalar(&Kind::Bool(false)).unwrap(), "false");
    }

    // ── Marker ──

    #[test]
    fn marker_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::Marker), Kind::Marker);
    }

    #[test]
    fn marker_encodes_as_prefix() {
        let codec = Json3Codec;
        assert_eq!(codec.encode_scalar(&Kind::Marker).unwrap(), "\"m:\"");
    }

    // ── NA ──

    #[test]
    fn na_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::NA), Kind::NA);
    }

    #[test]
    fn na_encodes_as_prefix() {
        let codec = Json3Codec;
        assert_eq!(codec.encode_scalar(&Kind::NA).unwrap(), "\"z:\"");
    }

    // ── Remove ──

    #[test]
    fn remove_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::Remove), Kind::Remove);
    }

    #[test]
    fn remove_encodes_as_prefix() {
        let codec = Json3Codec;
        assert_eq!(codec.encode_scalar(&Kind::Remove).unwrap(), "\"-:\"");
    }

    // ── Number ──

    #[test]
    fn number_unitless_roundtrip() {
        let k = Kind::Number(Number::unitless(72.5));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn number_with_unit_roundtrip() {
        let k = Kind::Number(Number::new(72.5, Some("\u{00B0}F".into())));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn number_zero_roundtrip() {
        let k = Kind::Number(Number::unitless(0.0));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn number_negative_roundtrip() {
        let k = Kind::Number(Number::new(-23.45, Some("m\u{00B2}".into())));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn number_integer_roundtrip() {
        let k = Kind::Number(Number::unitless(42.0));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn number_inf_roundtrip() {
        let k = Kind::Number(Number::unitless(f64::INFINITY));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn number_neg_inf_roundtrip() {
        let k = Kind::Number(Number::unitless(f64::NEG_INFINITY));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn number_nan_roundtrip() {
        let codec = Json3Codec;
        let k = Kind::Number(Number::unitless(f64::NAN));
        let encoded = codec.encode_scalar(&k).unwrap();
        let decoded = codec.decode_scalar(&encoded).unwrap();
        match decoded {
            Kind::Number(n) => {
                assert!(n.val.is_nan());
                assert_eq!(n.unit, None);
            }
            other => panic!("expected Number, got {other:?}"),
        }
    }

    #[test]
    fn number_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Number(Number::new(72.5, Some("\u{00B0}F".into())));
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"n:72.5 \u{00B0}F\"");
    }

    #[test]
    fn number_unitless_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Number(Number::unitless(42.0));
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"n:42\"");
    }

    #[test]
    fn number_inf_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Number(Number::unitless(f64::INFINITY));
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"n:INF\"");
    }

    #[test]
    fn plain_json_number_decodes_as_number() {
        let codec = Json3Codec;
        let decoded = codec.decode_scalar("42.5").unwrap();
        assert_eq!(decoded, Kind::Number(Number::unitless(42.5)));
    }

    // ── String ──

    #[test]
    fn string_simple_roundtrip() {
        let k = Kind::Str("hello".into());
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn string_empty_roundtrip() {
        let k = Kind::Str(String::new());
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn string_with_special_chars_roundtrip() {
        let k = Kind::Str("line1\nline2\ttab".into());
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn string_encodes_with_s_prefix() {
        let codec = Json3Codec;
        assert_eq!(
            codec.encode_scalar(&Kind::Str("hello".into())).unwrap(),
            "\"s:hello\""
        );
    }

    #[test]
    fn string_empty_encodes_with_s_prefix() {
        let codec = Json3Codec;
        assert_eq!(
            codec.encode_scalar(&Kind::Str(String::new())).unwrap(),
            "\"s:\""
        );
    }

    // ── Ref ──

    #[test]
    fn ref_simple_roundtrip() {
        let k = Kind::Ref(HRef::from_val("site-1"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn ref_with_dis_roundtrip() {
        let k = Kind::Ref(HRef::new("site-1", Some("Main Site".into())));
        let rt = roundtrip_scalar(k);
        match rt {
            Kind::Ref(r) => {
                assert_eq!(r.val, "site-1");
                assert_eq!(r.dis, Some("Main Site".into()));
            }
            other => panic!("expected Ref, got {other:?}"),
        }
    }

    #[test]
    fn ref_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Ref(HRef::new("abc", Some("Display Name".into())));
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"r:abc Display Name\"");
    }

    // ── Uri ──

    #[test]
    fn uri_roundtrip() {
        let k = Kind::Uri(Uri::new("http://example.com/api"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn uri_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Uri(Uri::new("http://example.com"));
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"u:http://example.com\"");
    }

    // ── Symbol ──

    #[test]
    fn symbol_roundtrip() {
        let k = Kind::Symbol(Symbol::new("hot-water"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn symbol_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Symbol(Symbol::new("hot-water"));
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"y:hot-water\"");
    }

    // ── Date ──

    #[test]
    fn date_roundtrip() {
        let k = Kind::Date(NaiveDate::from_ymd_opt(2024, 3, 13).unwrap());
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn date_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Date(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"d:2024-01-01\"");
    }

    // ── Time ──

    #[test]
    fn time_roundtrip() {
        let k = Kind::Time(NaiveTime::from_hms_opt(8, 12, 5).unwrap());
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn time_with_frac_roundtrip() {
        let k = Kind::Time(NaiveTime::from_hms_milli_opt(14, 30, 0, 123).unwrap());
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn time_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Time(NaiveTime::from_hms_opt(12, 30, 45).unwrap());
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"h:12:30:45\"");
    }

    // ── DateTime ──

    #[test]
    fn datetime_roundtrip() {
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 8, 12, 5).unwrap();
        let k = Kind::DateTime(HDateTime::new(dt, "New_York"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn datetime_utc_roundtrip() {
        let offset = FixedOffset::east_opt(0).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 6, 15, 12, 0, 0).unwrap();
        let k = Kind::DateTime(HDateTime::new(dt, "UTC"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn datetime_encoding_format() {
        let codec = Json3Codec;
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 12, 30, 45).unwrap();
        let k = Kind::DateTime(HDateTime::new(dt, "New_York"));
        assert_eq!(
            codec.encode_scalar(&k).unwrap(),
            "\"t:2024-01-01T12:30:45-05:00 New_York\""
        );
    }

    // ── Coord ──

    #[test]
    fn coord_roundtrip() {
        let k = Kind::Coord(Coord::new(37.5458266, -77.4491888));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn coord_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::Coord(Coord::new(40.7, -74.0));
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"c:40.7,-74\"");
    }

    // ── XStr ──

    #[test]
    fn xstr_roundtrip() {
        let k = Kind::XStr(XStr::new("Color", "red"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn xstr_encoding_format() {
        let codec = Json3Codec;
        let k = Kind::XStr(XStr::new("Color", "red"));
        assert_eq!(codec.encode_scalar(&k).unwrap(), "\"x:Color:red\"");
    }

    // ── List ──

    #[test]
    fn list_empty_roundtrip() {
        let k = Kind::List(vec![]);
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn list_mixed_roundtrip() {
        let k = Kind::List(vec![
            Kind::Number(Number::unitless(1.0)),
            Kind::Str("two".into()),
            Kind::Marker,
            Kind::Bool(true),
            Kind::Null,
        ]);
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn list_nested_roundtrip() {
        let k = Kind::List(vec![
            Kind::List(vec![Kind::Number(Number::unitless(1.0))]),
            Kind::List(vec![Kind::Str("inner".into())]),
        ]);
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    // ── Dict ──

    #[test]
    fn dict_empty_roundtrip() {
        let k = Kind::Dict(Box::new(HDict::new()));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn dict_with_values_roundtrip() {
        let mut d = HDict::new();
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str("Main".into()));
        d.set(
            "area",
            Kind::Number(Number::new(4500.0, Some("ft\u{00B2}".into()))),
        );
        let k = Kind::Dict(Box::new(d));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    // ── Grid ──

    #[test]
    fn grid_empty_roundtrip() {
        let codec = Json3Codec;
        let g = HGrid::new();
        let encoded = codec.encode_grid(&g).unwrap();
        let decoded = codec.decode_grid(&encoded).unwrap();
        assert!(decoded.is_empty());
        assert_eq!(decoded.num_cols(), 0);
    }

    #[test]
    fn grid_with_data_roundtrip() {
        let codec = Json3Codec;

        let cols = vec![HCol::new("dis"), HCol::new("area")];
        let mut row1 = HDict::new();
        row1.set("dis", Kind::Str("Site One".into()));
        row1.set("area", Kind::Number(Number::unitless(4500.0)));
        let mut row2 = HDict::new();
        row2.set("dis", Kind::Str("Site Two".into()));
        row2.set("area", Kind::Number(Number::unitless(3200.0)));

        let g = HGrid::from_parts(HDict::new(), cols, vec![row1, row2]);
        let encoded = codec.encode_grid(&g).unwrap();
        let decoded = codec.decode_grid(&encoded).unwrap();

        assert_eq!(decoded.num_cols(), 2);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded.col_names().collect::<Vec<_>>(), vec!["dis", "area"]);
        assert_eq!(
            decoded.row(0).unwrap().get("dis"),
            Some(&Kind::Str("Site One".into()))
        );
        assert_eq!(
            decoded.row(1).unwrap().get("dis"),
            Some(&Kind::Str("Site Two".into()))
        );
    }

    #[test]
    fn grid_with_meta_roundtrip() {
        let codec = Json3Codec;

        let mut meta = HDict::new();
        meta.set("err", Kind::Marker);
        meta.set("dis", Kind::Str("some error".into()));

        let g = HGrid::from_parts(meta, vec![], vec![]);
        let encoded = codec.encode_grid(&g).unwrap();
        let decoded = codec.decode_grid(&encoded).unwrap();

        assert!(decoded.is_err());
        assert_eq!(
            decoded.meta.get("dis"),
            Some(&Kind::Str("some error".into()))
        );
    }

    #[test]
    fn grid_with_col_meta_roundtrip() {
        let codec = Json3Codec;

        let mut col_meta = HDict::new();
        col_meta.set("unit", Kind::Str("kW".into()));

        let cols = vec![HCol::new("name"), HCol::with_meta("power", col_meta)];
        let g = HGrid::from_parts(HDict::new(), cols, vec![]);
        let encoded = codec.encode_grid(&g).unwrap();
        let decoded = codec.decode_grid(&encoded).unwrap();

        assert_eq!(decoded.num_cols(), 2);
        let power_col = decoded.col("power").unwrap();
        assert_eq!(power_col.meta.get("unit"), Some(&Kind::Str("kW".into())));
    }

    #[test]
    fn grid_encoding_has_ver() {
        let codec = Json3Codec;
        let g = HGrid::new();
        let encoded = codec.encode_grid(&g).unwrap();
        let val: Value = serde_json::from_str(&encoded).unwrap();
        let meta = val.get("meta").unwrap().as_object().unwrap();
        assert_eq!(meta.get("ver").unwrap(), "3.0");
    }

    #[test]
    fn grid_missing_cells() {
        let codec = Json3Codec;

        let cols = vec![HCol::new("a"), HCol::new("b")];
        let mut row1 = HDict::new();
        row1.set("a", Kind::Number(Number::unitless(1.0)));
        // b missing

        let g = HGrid::from_parts(HDict::new(), cols, vec![row1]);
        let encoded = codec.encode_grid(&g).unwrap();
        let decoded = codec.decode_grid(&encoded).unwrap();

        let r = decoded.row(0).unwrap();
        assert!(r.has("a"));
        assert!(r.missing("b"));
    }

    // ── Edge cases ──

    #[test]
    fn disambiguation_str_vs_marker() {
        // "m:" should decode as Marker, not Str
        let codec = Json3Codec;
        let decoded = codec.decode_scalar("\"m:\"").unwrap();
        assert_eq!(decoded, Kind::Marker);
    }

    #[test]
    fn disambiguation_str_with_colon() {
        // "s:hello:world" should decode as Str("hello:world")
        let codec = Json3Codec;
        let decoded = codec.decode_scalar("\"s:hello:world\"").unwrap();
        assert_eq!(decoded, Kind::Str("hello:world".into()));
    }

    #[test]
    fn string_that_looks_like_prefix() {
        // A string that starts with "m:" but was encoded properly as "s:m:"
        let k = Kind::Str("m:".into());
        let rt = roundtrip_scalar(k.clone());
        assert_eq!(rt, k);
    }

    #[test]
    fn list_with_all_types() {
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 8, 0, 0).unwrap();
        let k = Kind::List(vec![
            Kind::Null,
            Kind::Marker,
            Kind::NA,
            Kind::Remove,
            Kind::Bool(true),
            Kind::Number(Number::new(42.0, Some("kW".into()))),
            Kind::Str("hello".into()),
            Kind::Ref(HRef::new("x", Some("Dis".into()))),
            Kind::Uri(Uri::new("http://a.com")),
            Kind::Symbol(Symbol::new("tag")),
            Kind::Date(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            Kind::Time(NaiveTime::from_hms_opt(12, 0, 0).unwrap()),
            Kind::DateTime(HDateTime::new(dt, "New_York")),
            Kind::Coord(Coord::new(37.5, -77.4)),
            Kind::XStr(XStr::new("Color", "red")),
        ]);
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn nested_dict_with_typed_values() {
        let mut inner = HDict::new();
        inner.set(
            "temp",
            Kind::Number(Number::new(72.5, Some("\u{00B0}F".into()))),
        );
        inner.set("site", Kind::Ref(HRef::from_val("s1")));
        let k = Kind::Dict(Box::new(inner));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn grid_nested_in_scalar() {
        let codec = Json3Codec;
        let cols = vec![HCol::new("x")];
        let mut row = HDict::new();
        row.set("x", Kind::Number(Number::unitless(42.0)));
        let g = HGrid::from_parts(HDict::new(), cols, vec![row]);

        let k = Kind::Grid(Box::new(g));
        let encoded = codec.encode_scalar(&k).unwrap();
        let decoded = codec.decode_scalar(&encoded).unwrap();
        match decoded {
            Kind::Grid(g) => {
                assert_eq!(g.len(), 1);
                assert_eq!(g.num_cols(), 1);
            }
            other => panic!("expected Grid, got {other:?}"),
        }
    }
}
