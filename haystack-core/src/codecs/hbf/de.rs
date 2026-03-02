//! HBF binary deserializer — decodes Haystack types from compact bytes.

use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};

use super::error::HbfError;
use super::ser;
use super::varint::decode_varint;
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::{Coord, HDateTime, HRef, Kind, Number, Symbol, Uri, XStr};

/// Maximum recursion depth for nested structures.
const MAX_DEPTH: u32 = 100;
/// Maximum number of columns in a grid.
const MAX_COLS: usize = 10_000;
/// Maximum number of rows in a grid.
const MAX_ROWS: usize = 10_000_000;
/// Maximum items in a list/dict.
const MAX_COLLECTION: usize = 1_000_000;

/// A reader that tracks position within a byte slice.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    depth: u32,
}

impl<'a> Reader<'a> {
    /// Create a new reader over the given byte slice.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            depth: 0,
        }
    }

    /// Read a single byte, advancing the position.
    fn read_u8(&mut self) -> Result<u8, HbfError> {
        if self.pos >= self.data.len() {
            return Err(HbfError::Eof);
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// Read exactly `n` bytes as a slice, advancing the position.
    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], HbfError> {
        if self.pos + n > self.data.len() {
            return Err(HbfError::Eof);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Read a little-endian f64.
    fn read_f64(&mut self) -> Result<f64, HbfError> {
        let bytes = self.read_bytes(8)?;
        Ok(f64::from_le_bytes(bytes.try_into().map_err(|_| {
            HbfError::Message("invalid byte slice length".into())
        })?))
    }

    /// Read a little-endian i32.
    fn read_i32(&mut self) -> Result<i32, HbfError> {
        let bytes = self.read_bytes(4)?;
        Ok(i32::from_le_bytes(bytes.try_into().map_err(|_| {
            HbfError::Message("invalid byte slice length".into())
        })?))
    }

    /// Read a little-endian u32.
    fn read_u32(&mut self) -> Result<u32, HbfError> {
        let bytes = self.read_bytes(4)?;
        Ok(u32::from_le_bytes(bytes.try_into().map_err(|_| {
            HbfError::Message("invalid byte slice length".into())
        })?))
    }

    /// Read a little-endian i64.
    fn read_i64(&mut self) -> Result<i64, HbfError> {
        let bytes = self.read_bytes(8)?;
        Ok(i64::from_le_bytes(bytes.try_into().map_err(|_| {
            HbfError::Message("invalid byte slice length".into())
        })?))
    }

    /// Read a varint-length-prefixed UTF-8 string.
    fn read_str(&mut self) -> Result<String, HbfError> {
        let len = decode_varint(self.data, &mut self.pos)? as usize;
        if self.pos + len > self.data.len() {
            return Err(HbfError::Eof);
        }
        let s = std::str::from_utf8(&self.data[self.pos..self.pos + len])?;
        self.pos += len;
        Ok(s.to_string())
    }

    /// Read an optional string: 0x00 → None, 0x01 → Some(string).
    fn read_opt_str(&mut self) -> Result<Option<String>, HbfError> {
        let flag = self.read_u8()?;
        match flag {
            0x00 => Ok(None),
            0x01 => Ok(Some(self.read_str()?)),
            other => Err(HbfError::Message(format!(
                "invalid optional flag: 0x{other:02x}"
            ))),
        }
    }

    /// Read a tagged Kind value.
    pub fn read_kind(&mut self) -> Result<Kind, HbfError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(HbfError::Message("maximum nesting depth exceeded".into()));
        }
        let tag = self.read_u8()?;
        let kind = match tag {
            ser::TAG_NULL => Ok(Kind::Null),
            ser::TAG_MARKER => Ok(Kind::Marker),
            ser::TAG_NA => Ok(Kind::NA),
            ser::TAG_REMOVE => Ok(Kind::Remove),
            ser::TAG_BOOL => {
                let b = self.read_u8()?;
                Ok(Kind::Bool(b != 0))
            }
            ser::TAG_NUMBER => {
                let val = self.read_f64()?;
                let unit = self.read_opt_str()?;
                Ok(Kind::Number(Number::new(val, unit)))
            }
            ser::TAG_STR => {
                let s = self.read_str()?;
                Ok(Kind::Str(s))
            }
            ser::TAG_REF => {
                let val = self.read_str()?;
                let dis = self.read_opt_str()?;
                Ok(Kind::Ref(HRef::new(val, dis)))
            }
            ser::TAG_URI => {
                let s = self.read_str()?;
                Ok(Kind::Uri(Uri::new(s)))
            }
            ser::TAG_SYMBOL => {
                let s = self.read_str()?;
                Ok(Kind::Symbol(Symbol::new(s)))
            }
            ser::TAG_DATE => {
                let days = self.read_i32()?;
                let d = NaiveDate::from_num_days_from_ce_opt(days)
                    .ok_or_else(|| HbfError::Message(format!("invalid date days: {days}")))?;
                Ok(Kind::Date(d))
            }
            ser::TAG_TIME => {
                let secs = self.read_u32()?;
                let nanos = self.read_u32()?;
                let t = NaiveTime::from_num_seconds_from_midnight_opt(secs, nanos)
                    .ok_or_else(|| HbfError::Message("invalid time".into()))?;
                Ok(Kind::Time(t))
            }
            ser::TAG_DATETIME => {
                let millis = self.read_i64()?;
                let offset_secs = self.read_i32()?;
                let tz_name = self.read_str()?;
                let offset = FixedOffset::east_opt(offset_secs)
                    .ok_or_else(|| HbfError::Message("invalid tz offset".into()))?;
                let dt = offset
                    .timestamp_millis_opt(millis)
                    .single()
                    .ok_or_else(|| HbfError::Message("invalid datetime millis".into()))?;
                Ok(Kind::DateTime(HDateTime::new(dt, tz_name)))
            }
            ser::TAG_COORD => {
                let lat = self.read_f64()?;
                let lng = self.read_f64()?;
                Ok(Kind::Coord(Coord::new(lat, lng)))
            }
            ser::TAG_XSTR => {
                let type_name = self.read_str()?;
                let val = self.read_str()?;
                Ok(Kind::XStr(XStr::new(type_name, val)))
            }
            ser::TAG_LIST => {
                let count = decode_varint(self.data, &mut self.pos)? as usize;
                if count > MAX_COLLECTION {
                    return Err(HbfError::Message(format!("list too large: {count}")));
                }
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.read_kind()?);
                }
                Ok(Kind::List(items))
            }
            ser::TAG_DICT => {
                let dict = self.read_dict()?;
                Ok(Kind::Dict(Box::new(dict)))
            }
            ser::TAG_GRID => {
                let grid = self.read_grid()?;
                Ok(Kind::Grid(Box::new(grid)))
            }
            _ => Err(HbfError::InvalidTag(tag)),
        };
        self.depth -= 1;
        kind
    }

    /// Read an HDict (no type tag prefix expected).
    pub fn read_dict(&mut self) -> Result<HDict, HbfError> {
        let count = decode_varint(self.data, &mut self.pos)? as usize;
        if count > MAX_COLLECTION {
            return Err(HbfError::Message(format!("dict too large: {count}")));
        }
        let mut dict = HDict::new();
        for _ in 0..count {
            let key = self.read_str()?;
            let val = self.read_kind()?;
            dict.set(key, val);
        }
        Ok(dict)
    }

    /// Read an HGrid (no type tag prefix expected).
    pub fn read_grid(&mut self) -> Result<HGrid, HbfError> {
        // Meta
        let meta = self.read_dict()?;
        // Columns
        let col_count = decode_varint(self.data, &mut self.pos)? as usize;
        if col_count > MAX_COLS {
            return Err(HbfError::Message(format!("too many columns: {col_count}")));
        }
        let mut cols = Vec::with_capacity(col_count);
        for _ in 0..col_count {
            cols.push(self.read_col()?);
        }
        // Rows
        let row_count = decode_varint(self.data, &mut self.pos)? as usize;
        if row_count > MAX_ROWS {
            return Err(HbfError::Message(format!("too many rows: {row_count}")));
        }
        let mut rows = Vec::with_capacity(row_count);
        for _ in 0..row_count {
            rows.push(self.read_dict()?);
        }
        Ok(HGrid::from_parts(meta, cols, rows))
    }

    /// Read an HCol.
    fn read_col(&mut self) -> Result<HCol, HbfError> {
        let name = self.read_str()?;
        let meta = self.read_dict()?;
        Ok(HCol::with_meta(name, meta))
    }
}

/// Decode a single Kind from HBF bytes (no file header expected).
pub fn decode_kind_raw(bytes: &[u8]) -> Result<Kind, HbfError> {
    let mut reader = Reader::new(bytes);
    reader.read_kind()
}

/// Decode an HDict from HBF bytes (no file header expected).
pub fn decode_dict_raw(bytes: &[u8]) -> Result<HDict, HbfError> {
    let mut reader = Reader::new(bytes);
    reader.read_dict()
}

/// Decode an HGrid from HBF bytes (no file header expected).
pub fn decode_grid_raw(bytes: &[u8]) -> Result<HGrid, HbfError> {
    let mut reader = Reader::new(bytes);
    reader.read_grid()
}
