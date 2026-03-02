//! Custom serde Serialize/Deserialize implementations for Haystack types.
//! Enabled with the `haystack-serde` feature.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, NaiveDate, NaiveTime};
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::data::{HCol, HDict, HGrid};
use crate::kinds::{Coord, HDateTime, HRef, Kind, Number, Symbol, Uri, XStr};

// ---------------------------------------------------------------------------
// Number
// ---------------------------------------------------------------------------

impl Serialize for Number {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Number", 2)?;
        s.serialize_field("val", &self.val)?;
        s.serialize_field("unit", &self.unit)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Val,
            Unit,
        }

        struct NumberVisitor;

        impl<'de> Visitor<'de> for NumberVisitor {
            type Value = Number;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Number struct with val and optional unit")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Number, A::Error> {
                let mut val: Option<f64> = None;
                let mut unit: Option<Option<String>> = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Val => {
                            if val.is_some() {
                                return Err(de::Error::duplicate_field("val"));
                            }
                            val = Some(map.next_value()?);
                        }
                        Field::Unit => {
                            if unit.is_some() {
                                return Err(de::Error::duplicate_field("unit"));
                            }
                            unit = Some(map.next_value()?);
                        }
                    }
                }
                let val = val.ok_or_else(|| de::Error::missing_field("val"))?;
                let unit = unit.unwrap_or(None);
                Ok(Number { val, unit })
            }
        }

        deserializer.deserialize_struct("Number", &["val", "unit"], NumberVisitor)
    }
}

// ---------------------------------------------------------------------------
// HRef
// ---------------------------------------------------------------------------

impl Serialize for HRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("HRef", 2)?;
        s.serialize_field("val", &self.val)?;
        s.serialize_field("dis", &self.dis)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for HRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Val,
            Dis,
        }

        struct HRefVisitor;

        impl<'de> Visitor<'de> for HRefVisitor {
            type Value = HRef;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an HRef struct with val and optional dis")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<HRef, A::Error> {
                let mut val: Option<String> = None;
                let mut dis: Option<Option<String>> = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Val => {
                            if val.is_some() {
                                return Err(de::Error::duplicate_field("val"));
                            }
                            val = Some(map.next_value()?);
                        }
                        Field::Dis => {
                            if dis.is_some() {
                                return Err(de::Error::duplicate_field("dis"));
                            }
                            dis = Some(map.next_value()?);
                        }
                    }
                }
                let val = val.ok_or_else(|| de::Error::missing_field("val"))?;
                let dis = dis.unwrap_or(None);
                Ok(HRef { val, dis })
            }
        }

        deserializer.deserialize_struct("HRef", &["val", "dis"], HRefVisitor)
    }
}

// ---------------------------------------------------------------------------
// Uri (newtype)
// ---------------------------------------------------------------------------

impl Serialize for Uri {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct("Uri", &self.0)
    }
}

impl<'de> Deserialize<'de> for Uri {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UriVisitor;

        impl<'de> Visitor<'de> for UriVisitor {
            type Value = Uri;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a URI string")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Uri, E> {
                Ok(Uri(v.to_owned()))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<Uri, E> {
                Ok(Uri(v))
            }

            fn visit_newtype_struct<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Uri, D::Error> {
                let s = String::deserialize(deserializer)?;
                Ok(Uri(s))
            }
        }

        deserializer.deserialize_newtype_struct("Uri", UriVisitor)
    }
}

// ---------------------------------------------------------------------------
// Symbol (newtype)
// ---------------------------------------------------------------------------

impl Serialize for Symbol {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct("Symbol", &self.0)
    }
}

impl<'de> Deserialize<'de> for Symbol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SymbolVisitor;

        impl<'de> Visitor<'de> for SymbolVisitor {
            type Value = Symbol;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Symbol string")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Symbol, E> {
                Ok(Symbol(v.to_owned()))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<Symbol, E> {
                Ok(Symbol(v))
            }

            fn visit_newtype_struct<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Symbol, D::Error> {
                let s = String::deserialize(deserializer)?;
                Ok(Symbol(s))
            }
        }

        deserializer.deserialize_newtype_struct("Symbol", SymbolVisitor)
    }
}

// ---------------------------------------------------------------------------
// Coord
// ---------------------------------------------------------------------------

impl Serialize for Coord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Coord", 2)?;
        s.serialize_field("lat", &self.lat)?;
        s.serialize_field("lng", &self.lng)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for Coord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Lat,
            Lng,
        }

        struct CoordVisitor;

        impl<'de> Visitor<'de> for CoordVisitor {
            type Value = Coord;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Coord with lat and lng")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Coord, A::Error> {
                let mut lat: Option<f64> = None;
                let mut lng: Option<f64> = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Lat => {
                            if lat.is_some() {
                                return Err(de::Error::duplicate_field("lat"));
                            }
                            lat = Some(map.next_value()?);
                        }
                        Field::Lng => {
                            if lng.is_some() {
                                return Err(de::Error::duplicate_field("lng"));
                            }
                            lng = Some(map.next_value()?);
                        }
                    }
                }
                let lat = lat.ok_or_else(|| de::Error::missing_field("lat"))?;
                let lng = lng.ok_or_else(|| de::Error::missing_field("lng"))?;
                Ok(Coord { lat, lng })
            }
        }

        deserializer.deserialize_struct("Coord", &["lat", "lng"], CoordVisitor)
    }
}

// ---------------------------------------------------------------------------
// XStr
// ---------------------------------------------------------------------------

impl Serialize for XStr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("XStr", 2)?;
        s.serialize_field("type_name", &self.type_name)?;
        s.serialize_field("val", &self.val)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for XStr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            TypeName,
            Val,
        }

        struct XStrVisitor;

        impl<'de> Visitor<'de> for XStrVisitor {
            type Value = XStr;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an XStr with type_name and val")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<XStr, A::Error> {
                let mut type_name: Option<String> = None;
                let mut val: Option<String> = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::TypeName => {
                            if type_name.is_some() {
                                return Err(de::Error::duplicate_field("type_name"));
                            }
                            type_name = Some(map.next_value()?);
                        }
                        Field::Val => {
                            if val.is_some() {
                                return Err(de::Error::duplicate_field("val"));
                            }
                            val = Some(map.next_value()?);
                        }
                    }
                }
                let type_name = type_name.ok_or_else(|| de::Error::missing_field("type_name"))?;
                let val = val.ok_or_else(|| de::Error::missing_field("val"))?;
                Ok(XStr { type_name, val })
            }
        }

        deserializer.deserialize_struct("XStr", &["type_name", "val"], XStrVisitor)
    }
}

// ---------------------------------------------------------------------------
// HDateTime
// ---------------------------------------------------------------------------

impl Serialize for HDateTime {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("HDateTime", 2)?;
        s.serialize_field("dt", &self.dt.to_rfc3339())?;
        s.serialize_field("tz", &self.tz_name)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for HDateTime {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Dt,
            Tz,
        }

        struct HDateTimeVisitor;

        impl<'de> Visitor<'de> for HDateTimeVisitor {
            type Value = HDateTime;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an HDateTime with dt (RFC3339) and tz")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<HDateTime, A::Error> {
                let mut dt: Option<String> = None;
                let mut tz: Option<String> = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Dt => {
                            if dt.is_some() {
                                return Err(de::Error::duplicate_field("dt"));
                            }
                            dt = Some(map.next_value()?);
                        }
                        Field::Tz => {
                            if tz.is_some() {
                                return Err(de::Error::duplicate_field("tz"));
                            }
                            tz = Some(map.next_value()?);
                        }
                    }
                }
                let dt_str = dt.ok_or_else(|| de::Error::missing_field("dt"))?;
                let tz_name = tz.ok_or_else(|| de::Error::missing_field("tz"))?;
                let dt = DateTime::parse_from_rfc3339(&dt_str)
                    .map_err(|e| de::Error::custom(format!("invalid RFC3339 datetime: {e}")))?;
                Ok(HDateTime { dt, tz_name })
            }
        }

        deserializer.deserialize_struct("HDateTime", &["dt", "tz"], HDateTimeVisitor)
    }
}

// ---------------------------------------------------------------------------
// HDict (serialized as a map of String → Kind)
// ---------------------------------------------------------------------------

impl Serialize for HDict {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.iter().count()))?;
        for (k, v) in self.iter() {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for HDict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HDictVisitor;

        impl<'de> Visitor<'de> for HDictVisitor {
            type Value = HDict;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map of String → Kind")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<HDict, A::Error> {
                let mut dict = HDict::new();
                while let Some((key, val)) = map.next_entry::<String, Kind>()? {
                    dict.set(key, val);
                }
                Ok(dict)
            }
        }

        deserializer.deserialize_map(HDictVisitor)
    }
}

// ---------------------------------------------------------------------------
// HCol
// ---------------------------------------------------------------------------

impl Serialize for HCol {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("HCol", 2)?;
        s.serialize_field("name", &self.name)?;
        s.serialize_field("meta", &self.meta)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for HCol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Name,
            Meta,
        }

        struct HColVisitor;

        impl<'de> Visitor<'de> for HColVisitor {
            type Value = HCol;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an HCol with name and meta")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<HCol, A::Error> {
                let mut name: Option<String> = None;
                let mut meta: Option<HDict> = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Name => {
                            if name.is_some() {
                                return Err(de::Error::duplicate_field("name"));
                            }
                            name = Some(map.next_value()?);
                        }
                        Field::Meta => {
                            if meta.is_some() {
                                return Err(de::Error::duplicate_field("meta"));
                            }
                            meta = Some(map.next_value()?);
                        }
                    }
                }
                let name = name.ok_or_else(|| de::Error::missing_field("name"))?;
                let meta = meta.unwrap_or_default();
                Ok(HCol { name, meta })
            }
        }

        deserializer.deserialize_struct("HCol", &["name", "meta"], HColVisitor)
    }
}

// ---------------------------------------------------------------------------
// HGrid
// ---------------------------------------------------------------------------

impl Serialize for HGrid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("HGrid", 3)?;
        s.serialize_field("meta", &self.meta)?;
        s.serialize_field("cols", &self.cols)?;
        s.serialize_field("rows", &self.rows)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for HGrid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Meta,
            Cols,
            Rows,
        }

        struct HGridVisitor;

        impl<'de> Visitor<'de> for HGridVisitor {
            type Value = HGrid;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an HGrid with meta, cols, and rows")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<HGrid, A::Error> {
                let mut meta: Option<HDict> = None;
                let mut cols: Option<Vec<HCol>> = None;
                let mut rows: Option<Vec<HDict>> = None;
                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Meta => {
                            if meta.is_some() {
                                return Err(de::Error::duplicate_field("meta"));
                            }
                            meta = Some(map.next_value()?);
                        }
                        Field::Cols => {
                            if cols.is_some() {
                                return Err(de::Error::duplicate_field("cols"));
                            }
                            cols = Some(map.next_value()?);
                        }
                        Field::Rows => {
                            if rows.is_some() {
                                return Err(de::Error::duplicate_field("rows"));
                            }
                            rows = Some(map.next_value()?);
                        }
                    }
                }
                let meta = meta.unwrap_or_default();
                let cols = cols.ok_or_else(|| de::Error::missing_field("cols"))?;
                let rows = rows.ok_or_else(|| de::Error::missing_field("rows"))?;
                Ok(HGrid { meta, cols, rows })
            }
        }

        deserializer.deserialize_struct("HGrid", &["meta", "cols", "rows"], HGridVisitor)
    }
}

// ---------------------------------------------------------------------------
// Kind (externally tagged: { "type": "...", ... })
// ---------------------------------------------------------------------------

impl Serialize for Kind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Kind::Null => {
                let mut s = serializer.serialize_struct("Kind", 1)?;
                s.serialize_field("type", "null")?;
                s.end()
            }
            Kind::Marker => {
                let mut s = serializer.serialize_struct("Kind", 1)?;
                s.serialize_field("type", "marker")?;
                s.end()
            }
            Kind::NA => {
                let mut s = serializer.serialize_struct("Kind", 1)?;
                s.serialize_field("type", "na")?;
                s.end()
            }
            Kind::Remove => {
                let mut s = serializer.serialize_struct("Kind", 1)?;
                s.serialize_field("type", "remove")?;
                s.end()
            }
            Kind::Bool(v) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "bool")?;
                s.serialize_field("val", v)?;
                s.end()
            }
            Kind::Number(n) => {
                let len = if n.unit.is_some() { 3 } else { 2 };
                let mut s = serializer.serialize_struct("Kind", len)?;
                s.serialize_field("type", "num")?;
                s.serialize_field("val", &n.val)?;
                if let Some(ref u) = n.unit {
                    s.serialize_field("unit", u)?;
                }
                s.end()
            }
            Kind::Str(v) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "str")?;
                s.serialize_field("val", v)?;
                s.end()
            }
            Kind::Ref(r) => {
                let len = if r.dis.is_some() { 3 } else { 2 };
                let mut s = serializer.serialize_struct("Kind", len)?;
                s.serialize_field("type", "ref")?;
                s.serialize_field("val", &r.val)?;
                if let Some(ref d) = r.dis {
                    s.serialize_field("dis", d)?;
                }
                s.end()
            }
            Kind::Uri(u) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "uri")?;
                s.serialize_field("val", &u.0)?;
                s.end()
            }
            Kind::Symbol(sym) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "symbol")?;
                s.serialize_field("val", &sym.0)?;
                s.end()
            }
            Kind::Date(d) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "date")?;
                s.serialize_field("val", &d.format("%Y-%m-%d").to_string())?;
                s.end()
            }
            Kind::Time(t) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "time")?;
                s.serialize_field("val", &t.format("%H:%M:%S").to_string())?;
                s.end()
            }
            Kind::DateTime(hdt) => {
                let mut s = serializer.serialize_struct("Kind", 3)?;
                s.serialize_field("type", "dateTime")?;
                s.serialize_field("val", &hdt.dt.to_rfc3339())?;
                s.serialize_field("tz", &hdt.tz_name)?;
                s.end()
            }
            Kind::Coord(c) => {
                let mut s = serializer.serialize_struct("Kind", 3)?;
                s.serialize_field("type", "coord")?;
                s.serialize_field("lat", &c.lat)?;
                s.serialize_field("lng", &c.lng)?;
                s.end()
            }
            Kind::XStr(x) => {
                let mut s = serializer.serialize_struct("Kind", 3)?;
                s.serialize_field("type", "xstr")?;
                s.serialize_field("type_name", &x.type_name)?;
                s.serialize_field("val", &x.val)?;
                s.end()
            }
            Kind::List(list) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "list")?;
                s.serialize_field("val", list)?;
                s.end()
            }
            Kind::Dict(dict) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "dict")?;
                s.serialize_field("val", dict.as_ref())?;
                s.end()
            }
            Kind::Grid(grid) => {
                let mut s = serializer.serialize_struct("Kind", 2)?;
                s.serialize_field("type", "grid")?;
                s.serialize_field("val", grid.as_ref())?;
                s.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Kind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KindVisitor;

        impl<'de> Visitor<'de> for KindVisitor {
            type Value = Kind;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Kind object with a \"type\" field")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Kind, A::Error> {
                // Collect all fields into a temporary map, since "type" may not come first.
                let mut fields: HashMap<String, serde_json::Value> = HashMap::new();
                while let Some((key, val)) = map.next_entry::<String, serde_json::Value>()? {
                    fields.insert(key, val);
                }

                let type_str = fields
                    .get("type")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| de::Error::missing_field("type"))?;

                match type_str {
                    "null" => Ok(Kind::Null),
                    "marker" => Ok(Kind::Marker),
                    "na" => Ok(Kind::NA),
                    "remove" => Ok(Kind::Remove),
                    "bool" => {
                        let v = fields
                            .get("val")
                            .and_then(|v| v.as_bool())
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        Ok(Kind::Bool(v))
                    }
                    "num" => {
                        let val = fields
                            .get("val")
                            .and_then(|v| v.as_f64())
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        let unit = fields
                            .get("unit")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_owned());
                        Ok(Kind::Number(Number { val, unit }))
                    }
                    "str" => {
                        let v = fields
                            .get("val")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        Ok(Kind::Str(v.to_owned()))
                    }
                    "ref" => {
                        let val = fields
                            .get("val")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("val"))?
                            .to_owned();
                        let dis = fields
                            .get("dis")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_owned());
                        Ok(Kind::Ref(HRef { val, dis }))
                    }
                    "uri" => {
                        let v = fields
                            .get("val")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        Ok(Kind::Uri(Uri(v.to_owned())))
                    }
                    "symbol" => {
                        let v = fields
                            .get("val")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        Ok(Kind::Symbol(Symbol(v.to_owned())))
                    }
                    "date" => {
                        let v = fields
                            .get("val")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        let date = NaiveDate::parse_from_str(v, "%Y-%m-%d")
                            .map_err(|e| de::Error::custom(format!("invalid date: {e}")))?;
                        Ok(Kind::Date(date))
                    }
                    "time" => {
                        let v = fields
                            .get("val")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        let time = NaiveTime::parse_from_str(v, "%H:%M:%S")
                            .or_else(|_| NaiveTime::parse_from_str(v, "%H:%M:%S%.f"))
                            .map_err(|e| de::Error::custom(format!("invalid time: {e}")))?;
                        Ok(Kind::Time(time))
                    }
                    "dateTime" => {
                        let dt_str = fields
                            .get("val")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        let tz = fields
                            .get("tz")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("tz"))?;
                        let dt = DateTime::parse_from_rfc3339(dt_str).map_err(|e| {
                            de::Error::custom(format!("invalid RFC3339 datetime: {e}"))
                        })?;
                        Ok(Kind::DateTime(HDateTime {
                            dt,
                            tz_name: tz.to_owned(),
                        }))
                    }
                    "coord" => {
                        let lat = fields
                            .get("lat")
                            .and_then(|v| v.as_f64())
                            .ok_or_else(|| de::Error::missing_field("lat"))?;
                        let lng = fields
                            .get("lng")
                            .and_then(|v| v.as_f64())
                            .ok_or_else(|| de::Error::missing_field("lng"))?;
                        Ok(Kind::Coord(Coord { lat, lng }))
                    }
                    "xstr" => {
                        let type_name = fields
                            .get("type_name")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("type_name"))?
                            .to_owned();
                        let val = fields
                            .get("val")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::missing_field("val"))?
                            .to_owned();
                        Ok(Kind::XStr(XStr { type_name, val }))
                    }
                    "list" => {
                        let arr = fields
                            .remove("val")
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        let list: Vec<Kind> = serde_json::from_value(arr)
                            .map_err(|e| de::Error::custom(format!("invalid list: {e}")))?;
                        Ok(Kind::List(list))
                    }
                    "dict" => {
                        let obj = fields
                            .remove("val")
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        let dict: HDict = serde_json::from_value(obj)
                            .map_err(|e| de::Error::custom(format!("invalid dict: {e}")))?;
                        Ok(Kind::Dict(Box::new(dict)))
                    }
                    "grid" => {
                        let obj = fields
                            .remove("val")
                            .ok_or_else(|| de::Error::missing_field("val"))?;
                        let grid: HGrid = serde_json::from_value(obj)
                            .map_err(|e| de::Error::custom(format!("invalid grid: {e}")))?;
                        Ok(Kind::Grid(Box::new(grid)))
                    }
                    other => Err(de::Error::unknown_variant(
                        other,
                        &[
                            "null", "marker", "na", "remove", "bool", "num", "str", "ref", "uri",
                            "symbol", "date", "time", "dateTime", "coord", "xstr", "list", "dict",
                            "grid",
                        ],
                    )),
                }
            }
        }

        deserializer.deserialize_map(KindVisitor)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + fmt::Debug + PartialEq>(val: &T) {
        let json = serde_json::to_string(val).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, val, "round-trip failed for json: {json}");
    }

    // --- scalar types ---

    #[test]
    fn number_with_unit() {
        let n = Number {
            val: 72.5,
            unit: Some("°F".into()),
        };
        round_trip(&n);
    }

    #[test]
    fn number_unitless() {
        let n = Number {
            val: 42.0,
            unit: None,
        };
        round_trip(&n);
    }

    #[test]
    fn href_with_dis() {
        let r = HRef {
            val: "abc-123".into(),
            dis: Some("My Thing".into()),
        };
        round_trip(&r);
    }

    #[test]
    fn href_without_dis() {
        let r = HRef {
            val: "abc-123".into(),
            dis: None,
        };
        round_trip(&r);
    }

    #[test]
    fn uri_round_trip() {
        let u = Uri("https://example.com".into());
        round_trip(&u);
    }

    #[test]
    fn symbol_round_trip() {
        let s = Symbol("hot-water".into());
        round_trip(&s);
    }

    #[test]
    fn coord_round_trip() {
        let c = Coord {
            lat: 37.5,
            lng: -122.4,
        };
        round_trip(&c);
    }

    #[test]
    fn xstr_round_trip() {
        let x = XStr {
            type_name: "Foo".into(),
            val: "bar".into(),
        };
        round_trip(&x);
    }

    #[test]
    fn hdatetime_round_trip() {
        let dt = DateTime::parse_from_rfc3339("2024-01-15T14:30:00-05:00").unwrap();
        let hdt = HDateTime {
            dt,
            tz_name: "New_York".into(),
        };
        round_trip(&hdt);
    }

    // --- collection types ---

    #[test]
    fn hdict_empty() {
        let d = HDict::new();
        round_trip(&d);
    }

    #[test]
    fn hdict_with_tags() {
        let mut d = HDict::new();
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str("Main".into()));
        d.set(
            "area",
            Kind::Number(Number {
                val: 5000.0,
                unit: Some("ft²".into()),
            }),
        );
        let json = serde_json::to_string(&d).unwrap();
        let back: HDict = serde_json::from_str(&json).unwrap();
        assert_eq!(back.get("site"), d.get("site"));
        assert_eq!(back.get("dis"), d.get("dis"));
        assert_eq!(back.get("area"), d.get("area"));
    }

    #[test]
    fn hcol_round_trip() {
        let col = HCol {
            name: "temp".into(),
            meta: HDict::new(),
        };
        round_trip(&col);
    }

    #[test]
    fn hgrid_round_trip() {
        let mut meta = HDict::new();
        meta.set("ver", Kind::Str("3.0".into()));
        let cols = vec![
            HCol {
                name: "id".into(),
                meta: HDict::new(),
            },
            HCol {
                name: "dis".into(),
                meta: HDict::new(),
            },
        ];
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::from_val("site-1")));
        row.set("dis", Kind::Str("Main Campus".into()));
        let grid = HGrid {
            meta,
            cols,
            rows: vec![row],
        };
        let json = serde_json::to_string(&grid).unwrap();
        let back: HGrid = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cols.len(), 2);
        assert_eq!(back.rows.len(), 1);
        assert_eq!(
            back.rows[0].get("dis"),
            Some(&Kind::Str("Main Campus".into()))
        );
    }

    // --- Kind variants ---

    #[test]
    fn kind_null() {
        round_trip(&Kind::Null);
    }

    #[test]
    fn kind_marker() {
        round_trip(&Kind::Marker);
    }

    #[test]
    fn kind_na() {
        round_trip(&Kind::NA);
    }

    #[test]
    fn kind_remove() {
        round_trip(&Kind::Remove);
    }

    #[test]
    fn kind_bool() {
        round_trip(&Kind::Bool(true));
        round_trip(&Kind::Bool(false));
    }

    #[test]
    fn kind_number_with_unit() {
        let k = Kind::Number(Number {
            val: 72.5,
            unit: Some("°F".into()),
        });
        round_trip(&k);
    }

    #[test]
    fn kind_number_unitless() {
        let k = Kind::Number(Number {
            val: 42.0,
            unit: None,
        });
        round_trip(&k);
    }

    #[test]
    fn kind_str() {
        round_trip(&Kind::Str("hello world".into()));
    }

    #[test]
    fn kind_ref_with_dis() {
        let k = Kind::Ref(HRef {
            val: "abc-123".into(),
            dis: Some("My Thing".into()),
        });
        round_trip(&k);
    }

    #[test]
    fn kind_ref_without_dis() {
        let k = Kind::Ref(HRef {
            val: "abc-123".into(),
            dis: None,
        });
        round_trip(&k);
    }

    #[test]
    fn kind_uri() {
        round_trip(&Kind::Uri(Uri("https://example.com".into())));
    }

    #[test]
    fn kind_symbol() {
        round_trip(&Kind::Symbol(Symbol("hot".into())));
    }

    #[test]
    fn kind_date() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        round_trip(&Kind::Date(d));
    }

    #[test]
    fn kind_time() {
        let t = NaiveTime::from_hms_opt(14, 30, 0).unwrap();
        round_trip(&Kind::Time(t));
    }

    #[test]
    fn kind_datetime() {
        let dt = DateTime::parse_from_rfc3339("2024-01-15T14:30:00-05:00").unwrap();
        let k = Kind::DateTime(HDateTime {
            dt,
            tz_name: "New_York".into(),
        });
        round_trip(&k);
    }

    #[test]
    fn kind_coord() {
        let k = Kind::Coord(Coord {
            lat: 37.5,
            lng: -122.4,
        });
        round_trip(&k);
    }

    #[test]
    fn kind_xstr() {
        let k = Kind::XStr(XStr {
            type_name: "Foo".into(),
            val: "bar".into(),
        });
        round_trip(&k);
    }

    #[test]
    fn kind_list() {
        let k = Kind::List(vec![
            Kind::Str("a".into()),
            Kind::Number(Number::unitless(1.0)),
            Kind::Marker,
        ]);
        round_trip(&k);
    }

    #[test]
    fn kind_dict() {
        let mut d = HDict::new();
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str("Campus".into()));
        let k = Kind::Dict(Box::new(d));
        let json = serde_json::to_string(&k).unwrap();
        let back: Kind = serde_json::from_str(&json).unwrap();
        if let Kind::Dict(bd) = &back {
            assert_eq!(bd.get("site"), Some(&Kind::Marker));
            assert_eq!(bd.get("dis"), Some(&Kind::Str("Campus".into())));
        } else {
            panic!("expected Kind::Dict");
        }
    }

    #[test]
    fn kind_grid() {
        let cols = vec![HCol {
            name: "val".into(),
            meta: HDict::new(),
        }];
        let mut row = HDict::new();
        row.set("val", Kind::Number(Number::unitless(99.0)));
        let grid = HGrid {
            meta: HDict::new(),
            cols,
            rows: vec![row],
        };
        let k = Kind::Grid(Box::new(grid));
        let json = serde_json::to_string(&k).unwrap();
        let back: Kind = serde_json::from_str(&json).unwrap();
        if let Kind::Grid(bg) = &back {
            assert_eq!(bg.rows.len(), 1);
        } else {
            panic!("expected Kind::Grid");
        }
    }

    // --- nested structures ---

    #[test]
    fn nested_dict_in_grid_in_dict() {
        let mut inner_dict = HDict::new();
        inner_dict.set(
            "temp",
            Kind::Number(Number {
                val: 72.0,
                unit: Some("°F".into()),
            }),
        );

        let cols = vec![HCol {
            name: "data".into(),
            meta: HDict::new(),
        }];
        let mut row = HDict::new();
        row.set("data", Kind::Dict(Box::new(inner_dict)));
        let grid = HGrid {
            meta: HDict::new(),
            cols,
            rows: vec![row],
        };

        let mut outer = HDict::new();
        outer.set("grid", Kind::Grid(Box::new(grid)));
        outer.set("name", Kind::Str("nested test".into()));

        let json = serde_json::to_string(&outer).unwrap();
        let back: HDict = serde_json::from_str(&json).unwrap();
        assert!(back.get("grid").is_some());
        assert_eq!(back.get("name"), Some(&Kind::Str("nested test".into())));
    }

    // --- specific JSON shape tests ---

    #[test]
    fn kind_null_json_shape() {
        let json = serde_json::to_value(&Kind::Null).unwrap();
        assert_eq!(json, serde_json::json!({"type": "null"}));
    }

    #[test]
    fn kind_num_json_shape() {
        let k = Kind::Number(Number {
            val: 72.5,
            unit: Some("°F".into()),
        });
        let json = serde_json::to_value(&k).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "num", "val": 72.5, "unit": "°F"})
        );
    }

    #[test]
    fn kind_num_no_unit_json_shape() {
        let k = Kind::Number(Number {
            val: 42.0,
            unit: None,
        });
        let json = serde_json::to_value(&k).unwrap();
        assert_eq!(json, serde_json::json!({"type": "num", "val": 42.0}));
    }

    #[test]
    fn kind_ref_json_shape() {
        let k = Kind::Ref(HRef {
            val: "abc-123".into(),
            dis: Some("My Thing".into()),
        });
        let json = serde_json::to_value(&k).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "ref", "val": "abc-123", "dis": "My Thing"})
        );
    }
}
