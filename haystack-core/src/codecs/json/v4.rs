// JSON v4 codec — application/json with `_kind` discriminator.

use crate::codecs::shared;
use crate::codecs::{Codec, CodecError};
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::*;
use chrono::{NaiveDate, NaiveTime, Timelike};
use serde_json::{Map, Value};

/// JSON v4 wire format codec (application/json).
///
/// Uses `_kind` discriminator for type-tagged values.
pub struct Json4Codec;

impl Codec for Json4Codec {
    fn mime_type(&self) -> &str {
        "application/json"
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

/// Encode a Kind value to a serde_json Value.
pub fn encode_kind(val: &Kind) -> Result<Value, CodecError> {
    match val {
        Kind::Null => Ok(Value::Null),
        Kind::Bool(b) => Ok(Value::Bool(*b)),
        Kind::Marker => Ok(kind_obj("marker", |_| {})),
        Kind::NA => Ok(kind_obj("na", |_| {})),
        Kind::Remove => Ok(kind_obj("remove", |_| {})),
        Kind::Number(n) => Ok(encode_number(n)),
        Kind::Str(s) => Ok(Value::String(s.clone())),
        Kind::Ref(r) => Ok(encode_ref(r)),
        Kind::Uri(u) => Ok(kind_obj("uri", |m| {
            m.insert("val".into(), Value::String(u.val().to_string()));
        })),
        Kind::Symbol(s) => Ok(kind_obj("symbol", |m| {
            m.insert("val".into(), Value::String(s.val().to_string()));
        })),
        Kind::Date(d) => Ok(kind_obj("date", |m| {
            m.insert(
                "val".into(),
                Value::String(d.format("%Y-%m-%d").to_string()),
            );
        })),
        Kind::Time(t) => Ok(kind_obj("time", |m| {
            m.insert("val".into(), Value::String(encode_time_str(t)));
        })),
        Kind::DateTime(hdt) => Ok(encode_datetime(hdt)),
        Kind::Coord(c) => Ok(kind_obj("coord", |m| {
            m.insert("lat".into(), json_number(c.lat));
            m.insert("lng".into(), json_number(c.lng));
        })),
        Kind::XStr(x) => Ok(kind_obj("xstr", |m| {
            m.insert("type".into(), Value::String(x.type_name.clone()));
            m.insert("val".into(), Value::String(x.val.clone()));
        })),
        Kind::List(items) => {
            let arr: Result<Vec<Value>, CodecError> = items.iter().map(encode_kind).collect();
            Ok(Value::Array(arr?))
        }
        Kind::Dict(d) => encode_dict(d),
        Kind::Grid(g) => encode_grid_value(g).map(Value::Object),
    }
}

/// Build a `{"_kind": kind_name, ...}` JSON object.
fn kind_obj(kind_name: &str, f: impl FnOnce(&mut Map<String, Value>)) -> Value {
    let mut m = Map::new();
    m.insert("_kind".into(), Value::String(kind_name.into()));
    f(&mut m);
    Value::Object(m)
}

/// Encode a Number to JSON v4 format.
fn encode_number(n: &Number) -> Value {
    let mut m = Map::new();
    m.insert("_kind".into(), Value::String("number".into()));
    if n.val.is_infinite() {
        if n.val > 0.0 {
            m.insert("val".into(), Value::String("INF".into()));
        } else {
            m.insert("val".into(), Value::String("-INF".into()));
        }
    } else if n.val.is_nan() {
        m.insert("val".into(), Value::String("NaN".into()));
    } else {
        m.insert("val".into(), json_number(n.val));
    }
    if let Some(ref u) = n.unit {
        m.insert("unit".into(), Value::String(u.clone()));
    }
    Value::Object(m)
}

/// Encode a Ref to JSON v4 format.
fn encode_ref(r: &HRef) -> Value {
    kind_obj("ref", |m| {
        m.insert("val".into(), Value::String(r.val.clone()));
        if let Some(ref dis) = r.dis {
            m.insert("dis".into(), Value::String(dis.clone()));
        }
    })
}

/// Encode a DateTime to JSON v4 format.
fn encode_datetime(hdt: &HDateTime) -> Value {
    let dt_str = hdt.dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    let frac = shared::format_frac_seconds(hdt.dt.nanosecond());
    let offset_str = hdt.dt.format("%:z").to_string();
    let val_str = format!("{dt_str}{frac}{offset_str}");

    kind_obj("dateTime", |m| {
        m.insert("val".into(), Value::String(val_str));
        if !hdt.tz_name.is_empty() {
            m.insert("tz".into(), Value::String(hdt.tz_name.clone()));
        }
    })
}

/// Format a NaiveTime to string (HH:MM:SS with optional fractional seconds).
fn encode_time_str(t: &NaiveTime) -> String {
    shared::format_time(t)
}

/// Encode an HDict as a plain JSON object (no _kind key).
fn encode_dict(d: &HDict) -> Result<Value, CodecError> {
    let mut m = Map::new();
    for (k, v) in d.sorted_iter() {
        m.insert(k.to_string(), encode_kind(v)?);
    }
    Ok(Value::Object(m))
}

/// Encode an HGrid as a JSON object (with `_kind: "grid"`).
fn encode_grid_value(grid: &HGrid) -> Result<Map<String, Value>, CodecError> {
    let mut m = Map::new();
    m.insert("_kind".into(), Value::String("grid".into()));

    // meta — only emit when non-empty
    if !grid.meta.is_empty() {
        let meta_val = encode_dict(&grid.meta)?;
        m.insert("meta".into(), meta_val);
    }

    // cols
    let cols: Result<Vec<Value>, CodecError> = grid
        .cols
        .iter()
        .map(|col| {
            let mut cm = Map::new();
            cm.insert("name".into(), Value::String(col.name.clone()));
            if !col.meta.is_empty() {
                let meta_val = encode_dict(&col.meta)?;
                cm.insert("meta".into(), meta_val);
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

/// Create a serde_json Number from an f64, handling integer display.
fn json_number(v: f64) -> Value {
    // serde_json::Number::from_f64 returns None for NaN/Infinity
    match serde_json::Number::from_f64(v) {
        Some(n) => Value::Number(n),
        None => Value::String(format!("{v}")),
    }
}

// ── Decoding ──

/// Decode a serde_json Value to a Kind.
pub fn decode_kind(val: &Value) -> Result<Kind, CodecError> {
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
        Value::String(s) => {
            // Plain JSON strings decode as Str
            Ok(Kind::Str(s.clone()))
        }
        Value::Array(arr) => {
            let items: Result<Vec<Kind>, CodecError> = arr.iter().map(decode_kind).collect();
            Ok(Kind::List(items?))
        }
        Value::Object(m) => decode_object(m),
    }
}

/// Decode a JSON object (may be a typed value with _kind, a grid, or a plain dict).
fn decode_object(m: &Map<String, Value>) -> Result<Kind, CodecError> {
    match m.get("_kind") {
        Some(Value::String(k)) => decode_typed(k, m),
        _ => {
            // Plain dict — decode all values
            let mut dict = HDict::new();
            for (key, val) in m {
                dict.set(key.clone(), decode_kind(val)?);
            }
            Ok(Kind::Dict(Box::new(dict)))
        }
    }
}

/// Decode a type-tagged JSON object.
fn decode_typed(kind: &str, m: &Map<String, Value>) -> Result<Kind, CodecError> {
    match kind {
        "marker" => Ok(Kind::Marker),
        "na" => Ok(Kind::NA),
        "remove" => Ok(Kind::Remove),
        "number" => decode_number(m),
        "ref" => decode_ref(m),
        "uri" => {
            let val = get_str(m, "val")?;
            Ok(Kind::Uri(Uri::new(val)))
        }
        "symbol" => {
            let val = get_str(m, "val")?;
            Ok(Kind::Symbol(Symbol::new(val)))
        }
        "date" => {
            let val = get_str(m, "val")?;
            let d = NaiveDate::parse_from_str(&val, "%Y-%m-%d").map_err(|e| CodecError::Parse {
                pos: 0,
                message: format!("invalid date: {e}"),
            })?;
            Ok(Kind::Date(d))
        }
        "time" => {
            let val = get_str(m, "val")?;
            let t = parse_time(&val)?;
            Ok(Kind::Time(t))
        }
        "dateTime" => decode_datetime(m),
        "coord" => decode_coord(m),
        "xstr" => {
            let type_name = get_str(m, "type")?;
            let val = get_str(m, "val")?;
            Ok(Kind::XStr(XStr::new(type_name, val)))
        }
        "grid" => {
            let grid = decode_grid_value(&Value::Object(m.clone()))?;
            Ok(Kind::Grid(Box::new(grid)))
        }
        other => Err(CodecError::Parse {
            pos: 0,
            message: format!("unknown _kind: {other}"),
        }),
    }
}

/// Decode a number from a `_kind: "number"` object.
fn decode_number(m: &Map<String, Value>) -> Result<Kind, CodecError> {
    let val = m.get("val").ok_or_else(|| CodecError::Parse {
        pos: 0,
        message: "number missing 'val' field".into(),
    })?;
    let v = match val {
        Value::Number(n) => n.as_f64().ok_or_else(|| CodecError::Parse {
            pos: 0,
            message: "cannot convert number val to f64".into(),
        })?,
        Value::String(s) => match s.as_str() {
            "INF" => f64::INFINITY,
            "-INF" => f64::NEG_INFINITY,
            "NaN" => f64::NAN,
            _ => s.parse::<f64>().map_err(|e| CodecError::Parse {
                pos: 0,
                message: format!("invalid number string: {e}"),
            })?,
        },
        _ => {
            return Err(CodecError::Parse {
                pos: 0,
                message: "number 'val' must be a number or string".into(),
            });
        }
    };
    let unit = match m.get("unit") {
        Some(Value::String(u)) => Some(u.clone()),
        _ => None,
    };
    Ok(Kind::Number(Number::new(v, unit)))
}

/// Decode a ref from a `_kind: "ref"` object.
fn decode_ref(m: &Map<String, Value>) -> Result<Kind, CodecError> {
    let val = get_str(m, "val")?;
    let dis = match m.get("dis") {
        Some(Value::String(d)) => Some(d.clone()),
        _ => None,
    };
    Ok(Kind::Ref(HRef::new(val, dis)))
}

/// Decode a datetime from a `_kind: "dateTime"` object.
fn decode_datetime(m: &Map<String, Value>) -> Result<Kind, CodecError> {
    let val = get_str(m, "val")?;
    let dt = chrono::DateTime::parse_from_rfc3339(&val)
        .or_else(|_| {
            // Try a more lenient format
            chrono::DateTime::parse_from_str(&val, "%Y-%m-%dT%H:%M:%S%:z")
        })
        .map_err(|e| CodecError::Parse {
            pos: 0,
            message: format!("invalid datetime: {e}"),
        })?;
    let tz = match m.get("tz") {
        Some(Value::String(t)) => t.clone(),
        _ => String::new(),
    };
    Ok(Kind::DateTime(HDateTime::new(dt, tz)))
}

/// Decode a coord from a `_kind: "coord"` object.
fn decode_coord(m: &Map<String, Value>) -> Result<Kind, CodecError> {
    let lat = get_f64(m, "lat")?;
    let lng = get_f64(m, "lng")?;
    Ok(Kind::Coord(Coord::new(lat, lng)))
}

/// Decode a grid from a JSON Value.
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

    // meta
    let meta = match m.get("meta") {
        Some(Value::Object(meta_map)) => {
            let mut dict = HDict::new();
            for (key, val) in meta_map {
                if key == "_kind" {
                    continue; // skip _kind in meta if present
                }
                dict.set(key.clone(), decode_kind(val)?);
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
                let name = get_str(col_obj, "name")?;
                let col_meta = match col_obj.get("meta") {
                    Some(Value::Object(meta_map)) => {
                        let mut dict = HDict::new();
                        for (key, val) in meta_map {
                            dict.set(key.clone(), decode_kind(val)?);
                        }
                        dict
                    }
                    _ => HDict::new(),
                };
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
                    dict.set(key.clone(), decode_kind(val)?);
                }
                rows.push(dict);
            }
            rows
        }
        _ => Vec::new(),
    };

    Ok(HGrid::from_parts(meta, cols, rows))
}

/// Parse a time string (HH:MM:SS with optional fractional seconds).
fn parse_time(s: &str) -> Result<NaiveTime, CodecError> {
    // Try with fractional seconds first, then without
    NaiveTime::parse_from_str(s, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .map_err(|e| CodecError::Parse {
            pos: 0,
            message: format!("invalid time: {e}"),
        })
}

/// Helper: get a string field from a JSON object.
fn get_str(m: &Map<String, Value>, key: &str) -> Result<String, CodecError> {
    match m.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(CodecError::Parse {
            pos: 0,
            message: format!("missing or invalid string field '{key}'"),
        }),
    }
}

/// Helper: get an f64 field from a JSON object.
fn get_f64(m: &Map<String, Value>, key: &str) -> Result<f64, CodecError> {
    match m.get(key) {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| CodecError::Parse {
            pos: 0,
            message: format!("cannot convert '{key}' to f64"),
        }),
        _ => Err(CodecError::Parse {
            pos: 0,
            message: format!("missing or invalid number field '{key}'"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{HCol, HDict, HGrid};
    use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};

    fn roundtrip_scalar(kind: Kind) -> Kind {
        let codec = Json4Codec;
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
        let codec = Json4Codec;
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
        let codec = Json4Codec;
        assert_eq!(codec.encode_scalar(&Kind::Bool(true)).unwrap(), "true");
        assert_eq!(codec.encode_scalar(&Kind::Bool(false)).unwrap(), "false");
    }

    // ── Marker ──

    #[test]
    fn marker_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::Marker), Kind::Marker);
    }

    #[test]
    fn marker_encodes_with_kind() {
        let codec = Json4Codec;
        let encoded = codec.encode_scalar(&Kind::Marker).unwrap();
        assert!(encoded.contains("\"_kind\""));
        assert!(encoded.contains("\"marker\""));
    }

    // ── NA ──

    #[test]
    fn na_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::NA), Kind::NA);
    }

    // ── Remove ──

    #[test]
    fn remove_roundtrip() {
        assert_eq!(roundtrip_scalar(Kind::Remove), Kind::Remove);
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
        let codec = Json4Codec;
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
    fn number_integer_roundtrip() {
        let k = Kind::Number(Number::unitless(42.0));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    #[test]
    fn plain_json_number_decodes_as_number() {
        let codec = Json4Codec;
        let decoded = codec.decode_scalar("42.5").unwrap();
        assert_eq!(decoded, Kind::Number(Number::unitless(42.5)));
    }

    #[test]
    fn plain_json_integer_decodes_as_number() {
        let codec = Json4Codec;
        let decoded = codec.decode_scalar("100").unwrap();
        assert_eq!(decoded, Kind::Number(Number::unitless(100.0)));
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
    fn string_encodes_as_plain_json_string() {
        let codec = Json4Codec;
        let encoded = codec.encode_scalar(&Kind::Str("hello".into())).unwrap();
        assert_eq!(encoded, "\"hello\"");
    }

    #[test]
    fn plain_json_string_decodes_as_str() {
        let codec = Json4Codec;
        let decoded = codec.decode_scalar("\"world\"").unwrap();
        assert_eq!(decoded, Kind::Str("world".into()));
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
        let rt = roundtrip_scalar(k.clone());
        match rt {
            Kind::Ref(r) => {
                assert_eq!(r.val, "site-1");
                assert_eq!(r.dis, Some("Main Site".into()));
            }
            other => panic!("expected Ref, got {other:?}"),
        }
    }

    // ── Uri ──

    #[test]
    fn uri_roundtrip() {
        let k = Kind::Uri(Uri::new("http://example.com/api"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    // ── Symbol ──

    #[test]
    fn symbol_roundtrip() {
        let k = Kind::Symbol(Symbol::new("hot-water"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    // ── Date ──

    #[test]
    fn date_roundtrip() {
        let k = Kind::Date(NaiveDate::from_ymd_opt(2024, 3, 13).unwrap());
        assert_eq!(roundtrip_scalar(k.clone()), k);
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

    // ── Coord ──

    #[test]
    fn coord_roundtrip() {
        let k = Kind::Coord(Coord::new(37.5458266, -77.4491888));
        assert_eq!(roundtrip_scalar(k.clone()), k);
    }

    // ── XStr ──

    #[test]
    fn xstr_roundtrip() {
        let k = Kind::XStr(XStr::new("Color", "red"));
        assert_eq!(roundtrip_scalar(k.clone()), k);
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

    #[test]
    fn dict_no_kind_key() {
        // A dict should not have _kind in output — it's a plain JSON object
        let codec = Json4Codec;
        let mut d = HDict::new();
        d.set("site", Kind::Marker);
        let k = Kind::Dict(Box::new(d));
        let encoded = codec.encode_scalar(&k).unwrap();
        // The _kind should only appear inside the marker value, not at the dict level
        let val: Value = serde_json::from_str(&encoded).unwrap();
        let obj = val.as_object().unwrap();
        assert!(obj.get("_kind").is_none());
    }

    // ── Grid ──

    #[test]
    fn grid_empty_roundtrip() {
        let codec = Json4Codec;
        let g = HGrid::new();
        let encoded = codec.encode_grid(&g).unwrap();
        let decoded = codec.decode_grid(&encoded).unwrap();
        assert!(decoded.is_empty());
        assert_eq!(decoded.num_cols(), 0);
    }

    #[test]
    fn grid_with_data_roundtrip() {
        let codec = Json4Codec;

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
        let codec = Json4Codec;

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
        let codec = Json4Codec;

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
    fn grid_with_missing_cells() {
        let codec = Json4Codec;

        let cols = vec![HCol::new("a"), HCol::new("b")];
        let mut row1 = HDict::new();
        row1.set("a", Kind::Number(Number::unitless(1.0)));
        // b missing in row1

        let g = HGrid::from_parts(HDict::new(), cols, vec![row1]);
        let encoded = codec.encode_grid(&g).unwrap();
        let decoded = codec.decode_grid(&encoded).unwrap();

        let r = decoded.row(0).unwrap();
        assert!(r.has("a"));
        assert!(r.missing("b"));
    }

    #[test]
    fn grid_nested_in_scalar() {
        let codec = Json4Codec;

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

    // ── Edge cases ──

    #[test]
    fn decode_plain_object_as_dict() {
        let codec = Json4Codec;
        let decoded = codec.decode_scalar(r#"{"a": 1, "b": "hello"}"#).unwrap();
        match decoded {
            Kind::Dict(d) => {
                assert_eq!(d.get("a"), Some(&Kind::Number(Number::unitless(1.0))));
                assert_eq!(d.get("b"), Some(&Kind::Str("hello".into())));
            }
            other => panic!("expected Dict, got {other:?}"),
        }
    }

    #[test]
    fn number_with_unit_encoding_format() {
        let codec = Json4Codec;
        let k = Kind::Number(Number::new(72.5, Some("\u{00B0}F".into())));
        let encoded = codec.encode_scalar(&k).unwrap();
        let val: Value = serde_json::from_str(&encoded).unwrap();
        let obj = val.as_object().unwrap();
        assert_eq!(obj.get("_kind").unwrap(), "number");
        assert_eq!(obj.get("val").unwrap(), 72.5);
        assert_eq!(obj.get("unit").unwrap(), "\u{00B0}F");
    }

    #[test]
    fn number_inf_encoding_format() {
        let codec = Json4Codec;
        let k = Kind::Number(Number::unitless(f64::INFINITY));
        let encoded = codec.encode_scalar(&k).unwrap();
        let val: Value = serde_json::from_str(&encoded).unwrap();
        let obj = val.as_object().unwrap();
        assert_eq!(obj.get("val").unwrap(), "INF");
    }

    #[test]
    fn number_nan_encoding_format() {
        let codec = Json4Codec;
        let k = Kind::Number(Number::unitless(f64::NAN));
        let encoded = codec.encode_scalar(&k).unwrap();
        let val: Value = serde_json::from_str(&encoded).unwrap();
        let obj = val.as_object().unwrap();
        assert_eq!(obj.get("val").unwrap(), "NaN");
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
}
