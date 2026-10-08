//! Project-owned typed payload v1; this is **not** Jeto or Haystack JSON.
//!
//! Decoding is staged into an owned value. Duplicate keys, unknown fields,
//! noncanonical scalar representations and resource-limit violations fail before
//! a value is returned. No graph is mutated by this module.
use std::collections::BTreeMap;
use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, FixedOffset};
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::data::{HCol, HDict, HGrid};
use crate::kinds::{Coord, Float, HDateTime, HRef, Kind, NominalScalar, Number, Symbol, Uri, XStr};

/// Limits apply to the JSON document, including the envelope and scalar fields.
/// Nodes count JSON values (containers included); depth counts nested values,
/// with the envelope at depth zero. Maximum configurable depth is 64.
#[derive(Debug, Clone, Copy)]
pub struct PayloadLimits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_nodes: usize,
}
impl Default for PayloadLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1_048_576,
            max_depth: 64,
            max_nodes: 100_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TypedPayloadError {
    #[error("invalid typed payload limits (positive byte/node limits and depth 1..=64 required)")]
    InvalidLimits,
    #[error("typed payload exceeds byte limit {limit}")]
    ByteLimit { limit: usize },
    #[error("invalid typed payload: {0}")]
    Invalid(String),
    #[error("unsupported typed payload version {0}")]
    UnsupportedVersion(u32),
}
fn invalid(message: impl Into<String>) -> TypedPayloadError {
    TypedPayloadError::Invalid(message.into())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    value: WireValue,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum WireValue {
    Null {},
    None {},
    Marker {},
    Na {},
    Remove {},
    Bool {
        value: bool,
    },
    Int {
        value: String,
    },
    Float {
        bits: String,
    },
    Number {
        bits: String,
        unit: Option<String>,
    },
    Str {
        value: String,
    },
    Ref {
        value: String,
        display: Option<String>,
    },
    Uri {
        value: String,
    },
    Symbol {
        value: String,
    },
    Date {
        value: String,
    },
    Time {
        value: String,
    },
    DateTime {
        seconds: String,
        nanos: u32,
        offset: i32,
        timezone: String,
    },
    Coord {
        lat: String,
        lng: String,
    },
    Xstr {
        name: String,
        value: String,
    },
    Buf {
        base64: String,
    },
    Nominal {
        spec: String,
        catalog: String,
        revision: String,
        value: String,
    },
    List {
        items: Vec<WireValue>,
    },
    Dict {
        tags: BTreeMap<String, WireValue>,
    },
    Grid {
        meta: BTreeMap<String, WireValue>,
        cols: Vec<WireColumn>,
        rows: Vec<BTreeMap<String, WireValue>>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireColumn {
    name: String,
    meta: BTreeMap<String, WireValue>,
}

/// Encode with deterministic dictionary order and default document bounds.
/// Lists, grid columns and rows retain their original order. All float fields
/// use exactly 16 lowercase hexadecimal digits, including Number and Coord.
pub fn encode(value: &Kind) -> Result<Vec<u8>, TypedPayloadError> {
    let wire = Envelope {
        version: 1,
        value: to_wire(value, 0)?,
    };
    let bytes = serde_json::to_vec(&wire).map_err(|e| invalid(e.to_string()))?;
    // Apply the same document limits as the default decoder to our output.
    bounded_json(&bytes, PayloadLimits::default())?;
    Ok(bytes)
}

pub fn decode(bytes: &[u8]) -> Result<Kind, TypedPayloadError> {
    decode_with_limits(bytes, PayloadLimits::default())
}

pub fn decode_with_limits(bytes: &[u8], limits: PayloadLimits) -> Result<Kind, TypedPayloadError> {
    let json = bounded_json(bytes, limits)?;
    // Check version before interpreting any version-specific variant.
    let version = json
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| invalid("version must be a u32"))?;
    if version != 1 {
        return Err(TypedPayloadError::UnsupportedVersion(version));
    }
    let envelope: Envelope = serde_json::from_value(json).map_err(|e| invalid(e.to_string()))?;
    from_wire(envelope.value)
}

fn bits(value: f64) -> String {
    format!("{:016x}", value.to_bits())
}
fn parse_bits(text: &str) -> Result<f64, TypedPayloadError> {
    if text.len() != 16
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid(
            "float bits require 16 lowercase hexadecimal digits",
        ));
    }
    u64::from_str_radix(text, 16)
        .map(f64::from_bits)
        .map_err(|e| invalid(e.to_string()))
}
fn parse_int(text: &str) -> Result<i64, TypedPayloadError> {
    let value = text
        .parse::<i64>()
        .map_err(|_| invalid("integer outside signed 64-bit range or invalid decimal"))?;
    if value.to_string() != text {
        return Err(invalid(
            "integer must be canonical decimal (no plus, leading zeros or negative zero)",
        ));
    }
    Ok(value)
}
fn to_tags(dict: &HDict, depth: usize) -> Result<BTreeMap<String, WireValue>, TypedPayloadError> {
    dict.iter()
        .map(|(tag, value)| Ok((tag.into(), to_wire(value, depth + 1)?)))
        .collect()
}
fn to_wire(value: &Kind, depth: usize) -> Result<WireValue, TypedPayloadError> {
    if depth > 64 {
        return Err(invalid("value nesting exceeds 64"));
    }
    Ok(match value {
        Kind::Null => WireValue::Null {},
        Kind::None => WireValue::None {},
        Kind::Marker => WireValue::Marker {},
        Kind::NA => WireValue::Na {},
        Kind::Remove => WireValue::Remove {},
        Kind::Bool(v) => WireValue::Bool { value: *v },
        Kind::Int(v) => WireValue::Int {
            value: v.to_string(),
        },
        Kind::Float(v) => WireValue::Float {
            bits: format!("{:016x}", v.bits()),
        },
        Kind::Number(v) => WireValue::Number {
            bits: bits(v.val),
            unit: v.unit.clone(),
        },
        Kind::Str(v) => WireValue::Str { value: v.clone() },
        Kind::Ref(v) => WireValue::Ref {
            value: v.val.clone(),
            display: v.dis.clone(),
        },
        Kind::Uri(v) => WireValue::Uri {
            value: v.val().into(),
        },
        Kind::Symbol(v) => WireValue::Symbol {
            value: v.val().into(),
        },
        Kind::Date(v) => WireValue::Date {
            value: v.to_string(),
        },
        Kind::Time(v) => WireValue::Time {
            value: v.to_string(),
        },
        Kind::DateTime(v) => WireValue::DateTime {
            seconds: v.dt.timestamp().to_string(),
            nanos: v.dt.timestamp_subsec_nanos(),
            offset: v.dt.offset().local_minus_utc(),
            timezone: v.tz_name.clone(),
        },
        Kind::Coord(v) => WireValue::Coord {
            lat: bits(v.lat),
            lng: bits(v.lng),
        },
        Kind::XStr(v) => WireValue::Xstr {
            name: v.type_name.clone(),
            value: v.val.clone(),
        },
        Kind::Buf(v) => WireValue::Buf {
            base64: STANDARD.encode(v),
        },
        Kind::Nominal(v) => WireValue::Nominal {
            spec: v.spec().into(),
            catalog: v.catalog().into(),
            revision: v.revision().into(),
            value: v.text().into(),
        },
        Kind::List(v) => WireValue::List {
            items: v
                .iter()
                .map(|v| to_wire(v, depth + 1))
                .collect::<Result<_, _>>()?,
        },
        Kind::Dict(v) => WireValue::Dict {
            tags: to_tags(v, depth)?,
        },
        Kind::Grid(v) => {
            let mut names = std::collections::HashSet::new();
            if v.cols.iter().any(|c| !names.insert(&c.name)) {
                return Err(invalid("duplicate grid column"));
            }
            WireValue::Grid {
                meta: to_tags(&v.meta, depth + 1)?,
                cols: v
                    .cols
                    .iter()
                    .map(|c| {
                        Ok(WireColumn {
                            name: c.name.clone(),
                            meta: to_tags(&c.meta, depth + 1)?,
                        })
                    })
                    .collect::<Result<_, TypedPayloadError>>()?,
                rows: v
                    .rows
                    .iter()
                    .map(|r| to_tags(r, depth + 1))
                    .collect::<Result<_, _>>()?,
            }
        }
    })
}
fn from_tags(tags: BTreeMap<String, WireValue>) -> Result<HDict, TypedPayloadError> {
    let mut dict = HDict::new();
    for (tag, value) in tags {
        dict.set(tag, from_wire(value)?);
    }
    Ok(dict)
}
fn from_wire(value: WireValue) -> Result<Kind, TypedPayloadError> {
    Ok(match value {
        WireValue::Null {} => Kind::Null,
        WireValue::None {} => Kind::None,
        WireValue::Marker {} => Kind::Marker,
        WireValue::Na {} => Kind::NA,
        WireValue::Remove {} => Kind::Remove,
        WireValue::Bool { value } => Kind::Bool(value),
        WireValue::Int { value } => Kind::Int(parse_int(&value)?),
        WireValue::Float { bits } => Kind::Float(Float::new(parse_bits(&bits)?)),
        WireValue::Number { bits, unit } => Kind::Number(Number::new(parse_bits(&bits)?, unit)),
        WireValue::Str { value } => Kind::Str(value),
        WireValue::Ref { value, display } => Kind::Ref(HRef::new(value, display)),
        WireValue::Uri { value } => Kind::Uri(Uri::new(value)),
        WireValue::Symbol { value } => Kind::Symbol(Symbol::new(value)),
        WireValue::Date { value } => {
            Kind::Date(value.parse().map_err(|_| invalid("invalid date"))?)
        }
        WireValue::Time { value } => {
            Kind::Time(value.parse().map_err(|_| invalid("invalid time"))?)
        }
        WireValue::DateTime {
            seconds,
            nanos,
            offset,
            timezone,
        } => {
            let offset =
                FixedOffset::east_opt(offset).ok_or_else(|| invalid("invalid datetime offset"))?;
            let dt = DateTime::from_timestamp(parse_int(&seconds)?, nanos)
                .ok_or_else(|| invalid("invalid datetime timestamp"))?;
            Kind::DateTime(HDateTime::new(dt.with_timezone(&offset), timezone))
        }
        WireValue::Coord { lat, lng } => {
            Kind::Coord(Coord::new(parse_bits(&lat)?, parse_bits(&lng)?))
        }
        WireValue::Xstr { name, value } => Kind::XStr(XStr::new(name, value)),
        WireValue::Buf { base64 } => {
            let bytes = STANDARD
                .decode(&base64)
                .map_err(|e| invalid(e.to_string()))?;
            if STANDARD.encode(&bytes) != base64 {
                return Err(invalid("noncanonical base64"));
            }
            Kind::Buf(bytes)
        }
        WireValue::Nominal {
            spec,
            catalog,
            revision,
            value,
        } => Kind::Nominal(
            NominalScalar::new(spec, catalog, revision, value)
                .map_err(|e| invalid(e.to_string()))?,
        ),
        WireValue::List { items } => {
            Kind::List(items.into_iter().map(from_wire).collect::<Result<_, _>>()?)
        }
        WireValue::Dict { tags } => Kind::Dict(Box::new(from_tags(tags)?)),
        WireValue::Grid { meta, cols, rows } => {
            let cols = cols
                .into_iter()
                .map(|c| Ok(HCol::with_meta(c.name, from_tags(c.meta)?)))
                .collect::<Result<Vec<_>, TypedPayloadError>>()?;
            // Column names are retained verbatim. Duplicate columns are rejected,
            // since selecting one could otherwise ambiguously discard metadata.
            let mut names = std::collections::HashSet::new();
            if cols.iter().any(|c| !names.insert(&c.name)) {
                return Err(invalid("duplicate grid column"));
            }
            Kind::Grid(Box::new(HGrid::from_parts(
                from_tags(meta)?,
                cols,
                rows.into_iter().map(from_tags).collect::<Result<_, _>>()?,
            )))
        }
    })
}

// Parse directly with a bounded visitor, before serde_json::Value can collapse
// duplicate object fields or allocate an unbounded recursive document.
fn bounded_json(bytes: &[u8], limits: PayloadLimits) -> Result<Value, TypedPayloadError> {
    if limits.max_bytes == 0 || limits.max_nodes == 0 || !(1..=64).contains(&limits.max_depth) {
        return Err(TypedPayloadError::InvalidLimits);
    }
    if bytes.len() > limits.max_bytes {
        return Err(TypedPayloadError::ByteLimit {
            limit: limits.max_bytes,
        });
    }
    let mut budget = Budget { limits, nodes: 0 };
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        budget: &mut budget,
        depth: 0,
    }
    .deserialize(&mut deserializer)
    .map_err(|e| invalid(e.to_string()))?;
    deserializer.end().map_err(|e| invalid(e.to_string()))?;
    Ok(value)
}
struct Budget {
    limits: PayloadLimits,
    nodes: usize,
}
struct Seed<'a> {
    budget: &'a mut Budget,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.depth > self.budget.limits.max_depth {
            return Err(D::Error::custom("JSON depth limit exceeded"));
        }
        if self.budget.nodes >= self.budget.limits.max_nodes {
            return Err(D::Error::custom("JSON node limit exceeded"));
        }
        self.budget.nodes += 1;
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON without duplicate fields")
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.into()))
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Seed {
            budget: self.budget,
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(A::Error::custom(format!("duplicate field {key:?}")));
            }
            let value = map.next_value_seed(Seed {
                budget: self.budget,
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
