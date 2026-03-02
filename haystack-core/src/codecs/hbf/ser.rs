//! HBF binary serializer — encodes Haystack types directly to compact bytes.

use chrono::Datelike;

use super::error::HbfError;
use super::varint::encode_varint;
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::Kind;

// Type tags
pub(super) const TAG_NULL: u8 = 0x00;
pub(super) const TAG_MARKER: u8 = 0x01;
pub(super) const TAG_NA: u8 = 0x02;
pub(super) const TAG_REMOVE: u8 = 0x03;
pub(super) const TAG_BOOL: u8 = 0x04;
pub(super) const TAG_NUMBER: u8 = 0x05;
pub(super) const TAG_STR: u8 = 0x06;
pub(super) const TAG_REF: u8 = 0x07;
pub(super) const TAG_URI: u8 = 0x08;
pub(super) const TAG_SYMBOL: u8 = 0x09;
pub(super) const TAG_DATE: u8 = 0x0A;
pub(super) const TAG_TIME: u8 = 0x0B;
pub(super) const TAG_DATETIME: u8 = 0x0C;
pub(super) const TAG_COORD: u8 = 0x0D;
pub(super) const TAG_XSTR: u8 = 0x0E;
pub(super) const TAG_LIST: u8 = 0x0F;
pub(super) const TAG_DICT: u8 = 0x10;
pub(super) const TAG_GRID: u8 = 0x11;

/// Write a Kind value (with type tag prefix) into `buf`.
pub fn write_kind(buf: &mut Vec<u8>, kind: &Kind) {
    match kind {
        Kind::Null => buf.push(TAG_NULL),
        Kind::Marker => buf.push(TAG_MARKER),
        Kind::NA => buf.push(TAG_NA),
        Kind::Remove => buf.push(TAG_REMOVE),
        Kind::Bool(v) => {
            buf.push(TAG_BOOL);
            buf.push(u8::from(*v));
        }
        Kind::Number(n) => {
            buf.push(TAG_NUMBER);
            buf.extend_from_slice(&n.val.to_le_bytes());
            write_opt_str(buf, n.unit.as_deref());
        }
        Kind::Str(s) => {
            buf.push(TAG_STR);
            write_str(buf, s);
        }
        Kind::Ref(r) => {
            buf.push(TAG_REF);
            write_str(buf, &r.val);
            write_opt_str(buf, r.dis.as_deref());
        }
        Kind::Uri(u) => {
            buf.push(TAG_URI);
            write_str(buf, u.val());
        }
        Kind::Symbol(s) => {
            buf.push(TAG_SYMBOL);
            write_str(buf, s.val());
        }
        Kind::Date(d) => {
            buf.push(TAG_DATE);
            buf.extend_from_slice(&d.num_days_from_ce().to_le_bytes());
        }
        Kind::Time(t) => {
            buf.push(TAG_TIME);
            let secs = t.num_seconds_from_midnight();
            let nanos = t.nanosecond() % 1_000_000_000;
            buf.extend_from_slice(&secs.to_le_bytes());
            buf.extend_from_slice(&nanos.to_le_bytes());
        }
        Kind::DateTime(hdt) => {
            buf.push(TAG_DATETIME);
            buf.extend_from_slice(&hdt.dt.timestamp_millis().to_le_bytes());
            // Store offset seconds for exact reconstruction
            buf.extend_from_slice(&hdt.dt.offset().local_minus_utc().to_le_bytes());
            write_str(buf, &hdt.tz_name);
        }
        Kind::Coord(c) => {
            buf.push(TAG_COORD);
            buf.extend_from_slice(&c.lat.to_le_bytes());
            buf.extend_from_slice(&c.lng.to_le_bytes());
        }
        Kind::XStr(x) => {
            buf.push(TAG_XSTR);
            write_str(buf, &x.type_name);
            write_str(buf, &x.val);
        }
        Kind::List(items) => {
            buf.push(TAG_LIST);
            encode_varint(buf, items.len() as u64);
            for item in items {
                write_kind(buf, item);
            }
        }
        Kind::Dict(d) => {
            buf.push(TAG_DICT);
            write_dict(buf, d);
        }
        Kind::Grid(g) => {
            buf.push(TAG_GRID);
            write_grid(buf, g);
        }
    }
}

/// Write an HDict (without type tag prefix) into `buf`.
pub fn write_dict(buf: &mut Vec<u8>, dict: &HDict) {
    let count = dict.len();
    encode_varint(buf, count as u64);
    for (key, val) in dict.iter() {
        write_str(buf, key);
        write_kind(buf, val);
    }
}

/// Write an HGrid (without type tag prefix) into `buf`.
pub fn write_grid(buf: &mut Vec<u8>, grid: &HGrid) {
    // Meta
    write_dict(buf, &grid.meta);
    // Columns
    encode_varint(buf, grid.cols.len() as u64);
    for col in &grid.cols {
        write_col(buf, col);
    }
    // Rows
    encode_varint(buf, grid.rows.len() as u64);
    for row in &grid.rows {
        write_dict(buf, row);
    }
}

/// Write an HCol into `buf`.
pub fn write_col(buf: &mut Vec<u8>, col: &HCol) {
    write_str(buf, &col.name);
    write_dict(buf, &col.meta);
}

/// Write a length-prefixed UTF-8 string.
fn write_str(buf: &mut Vec<u8>, s: &str) {
    encode_varint(buf, s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
}

/// Write an optional string: 0x00 for None, 0x01 + string for Some.
fn write_opt_str(buf: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(s) => {
            buf.push(0x01);
            write_str(buf, s);
        }
        None => buf.push(0x00),
    }
}

/// Encode a single Kind to HBF bytes (no file header).
pub fn encode_kind_raw(kind: &Kind) -> Result<Vec<u8>, HbfError> {
    let mut buf = Vec::new();
    write_kind(&mut buf, kind);
    Ok(buf)
}

/// Encode an HDict to HBF bytes (no file header).
pub fn encode_dict_raw(dict: &HDict) -> Result<Vec<u8>, HbfError> {
    let mut buf = Vec::new();
    write_dict(&mut buf, dict);
    Ok(buf)
}

/// Encode an HGrid to HBF bytes (no file header).
pub fn encode_grid_raw(grid: &HGrid) -> Result<Vec<u8>, HbfError> {
    let mut buf = Vec::new();
    write_grid(&mut buf, grid);
    Ok(buf)
}

/// Write the grid header (meta + columns) for streaming.
/// Does NOT write the row count — the caller writes rows incrementally.
pub fn write_grid_header(buf: &mut Vec<u8>, grid: &HGrid) {
    write_dict(buf, &grid.meta);
    encode_varint(buf, grid.cols.len() as u64);
    for col in &grid.cols {
        write_col(buf, col);
    }
}

/// Write a single row for streaming. Each row is a length-prefixed dict.
pub fn write_grid_row(buf: &mut Vec<u8>, row: &HDict) {
    let mut row_buf = Vec::new();
    write_dict(&mut row_buf, row);
    encode_varint(buf, row_buf.len() as u64);
    buf.extend_from_slice(&row_buf);
}

use chrono::Timelike;
