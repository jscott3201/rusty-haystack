//! HBF (Haystack Binary Format) — a compact binary codec for Haystack types.
//!
//! This module provides efficient binary serialization and deserialization for
//! [`Kind`], [`HDict`], and [`HGrid`]. The format uses LEB128 varints for
//! lengths, single-byte type tags, and little-endian numeric encoding.
//!
//! # Wire format
//!
//! Files start with a 4-byte magic `b"HBF\x01"` followed by 1 flags byte
//! (bit 0 = zstd compressed). After the header, the payload is a single
//! encoded value (grid, dict, or kind).
//!
//! # Example
//!
//! ```
//! use haystack_core::codecs::hbf;
//! use haystack_core::data::HGrid;
//!
//! let grid = HGrid::new();
//! let bytes = hbf::encode_grid(&grid).unwrap();
//! let decoded = hbf::decode_grid(&bytes).unwrap();
//! assert_eq!(grid, decoded);
//! ```

pub mod de;
pub mod error;
pub mod ser;
pub(crate) mod varint;

pub use error::HbfError;

use crate::data::{HDict, HGrid};
use crate::kinds::Kind;

/// Magic header bytes identifying an HBF stream.
const MAGIC: &[u8; 4] = b"HBF\x01";

/// Encode an [`HGrid`] to HBF bytes, including the file header.
pub fn encode_grid(grid: &HGrid) -> Result<Vec<u8>, HbfError> {
    let mut payload = Vec::new();
    ser::write_grid(&mut payload, grid);
    // Compress if payload is large enough to benefit
    let (flags, body) = if payload.len() >= 256 {
        let compressed = zstd::encode_all(payload.as_slice(), 3)
            .map_err(|e| HbfError::Message(format!("compression failed: {e}")))?;
        if compressed.len() < payload.len() {
            (0x01u8, compressed)
        } else {
            (0x00u8, payload)
        }
    } else {
        (0x00u8, payload)
    };
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.extend_from_slice(MAGIC);
    buf.push(flags);
    buf.extend_from_slice(&body);
    Ok(buf)
}

/// Encode an [`HGrid`] to HBF bytes using streaming format.
///
/// Streaming format uses `u64::MAX` as the row count sentinel, followed by
/// length-prefixed rows terminated by a zero-length sentinel.
pub fn encode_grid_streaming(grid: &HGrid) -> Result<Vec<u8>, HbfError> {
    let mut payload = Vec::new();
    ser::write_grid_header(&mut payload, grid);
    // Streaming sentinel: row count = u64::MAX
    varint::encode_varint(&mut payload, u64::MAX);
    for row in &grid.rows {
        ser::write_grid_row(&mut payload, row);
    }
    // End sentinel: zero-length row
    varint::encode_varint(&mut payload, 0);

    // Compress if beneficial
    let (flags, body) = if payload.len() >= 256 {
        let compressed = zstd::encode_all(payload.as_slice(), 3)
            .map_err(|e| HbfError::Message(format!("compression failed: {e}")))?;
        if compressed.len() < payload.len() {
            (0x01u8, compressed)
        } else {
            (0x00u8, payload)
        }
    } else {
        (0x00u8, payload)
    };
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.extend_from_slice(MAGIC);
    buf.push(flags);
    buf.extend_from_slice(&body);
    Ok(buf)
}

/// Decode an [`HGrid`] from HBF bytes (expects the file header).
pub fn decode_grid(bytes: &[u8]) -> Result<HGrid, HbfError> {
    if bytes.len() < 5 {
        return Err(HbfError::Message("HBF data too short".into()));
    }
    if &bytes[0..4] != b"HBF\x01" {
        return Err(HbfError::Message("invalid HBF magic header".into()));
    }
    let flags = bytes[4];
    const MAX_DECOMPRESSED: usize = 512 * 1024 * 1024; // 512 MB
    let payload = if flags & 0x01 != 0 {
        let decompressed = zstd::decode_all(&bytes[5..])
            .map_err(|e| HbfError::Message(format!("decompression failed: {e}")))?;
        if decompressed.len() > MAX_DECOMPRESSED {
            return Err(HbfError::Message(format!(
                "decompressed payload too large: {} bytes",
                decompressed.len()
            )));
        }
        decompressed
    } else {
        bytes[5..].to_vec()
    };
    let mut reader = de::Reader::new(&payload);
    reader.read_grid()
}

/// Encode a single [`Kind`] value to HBF bytes, including the file header.
pub fn encode_kind(kind: &Kind) -> Result<Vec<u8>, HbfError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.push(0x00);
    ser::write_kind(&mut buf, kind);
    Ok(buf)
}

/// Decode a single [`Kind`] value from HBF bytes (expects the file header).
pub fn decode_kind(bytes: &[u8]) -> Result<Kind, HbfError> {
    if bytes.len() < 5 || &bytes[..4] != MAGIC {
        return Err(HbfError::InvalidMagic);
    }
    let mut reader = de::Reader::new(&bytes[5..]);
    reader.read_kind()
}

/// Encode an [`HDict`] to HBF bytes, including the file header.
pub fn encode_dict(dict: &HDict) -> Result<Vec<u8>, HbfError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.push(0x00);
    ser::write_dict(&mut buf, dict);
    Ok(buf)
}

/// Decode an [`HDict`] from HBF bytes (expects the file header).
pub fn decode_dict(bytes: &[u8]) -> Result<HDict, HbfError> {
    if bytes.len() < 5 || &bytes[..4] != MAGIC {
        return Err(HbfError::InvalidMagic);
    }
    let mut reader = de::Reader::new(&bytes[5..]);
    reader.read_dict()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{HCol, HGrid};
    use crate::kinds::*;
    use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};

    // ── Kind round-trip tests ────────────────────────────────────────

    #[test]
    fn roundtrip_null() {
        let k = Kind::Null;
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_marker() {
        let k = Kind::Marker;
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_na() {
        let k = Kind::NA;
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_remove() {
        let k = Kind::Remove;
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_bool() {
        for v in [true, false] {
            let k = Kind::Bool(v);
            let bytes = encode_kind(&k).unwrap();
            assert_eq!(decode_kind(&bytes).unwrap(), k);
        }
    }

    #[test]
    fn roundtrip_number_unitless() {
        let k = Kind::Number(Number::unitless(42.0));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_number_with_unit() {
        let k = Kind::Number(Number::new(72.5, Some("°F".into())));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
        // Compact: header(5) + tag(1) + f64(8) + opt_flag(1) + varint_len(1) + unit_bytes(2) = 18
        assert!(
            bytes.len() < 25,
            "expected compact encoding, got {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn roundtrip_str() {
        let k = Kind::Str("hello world".into());
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_ref_no_dis() {
        let k = Kind::Ref(HRef::from_val("site-1"));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_ref_with_dis() {
        let k = Kind::Ref(HRef::new("site-1", Some("Main Site".into())));
        let bytes = encode_kind(&k).unwrap();
        let decoded = decode_kind(&bytes).unwrap();
        // HRef equality ignores dis, so check both fields
        if let Kind::Ref(r) = &decoded {
            assert_eq!(r.val, "site-1");
            assert_eq!(r.dis.as_deref(), Some("Main Site"));
        } else {
            panic!("expected Ref, got {decoded:?}");
        }
    }

    #[test]
    fn roundtrip_uri() {
        let k = Kind::Uri(Uri::new("http://example.com/api"));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_symbol() {
        let k = Kind::Symbol(Symbol::new("hot-water"));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_date() {
        let k = Kind::Date(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_time() {
        let k = Kind::Time(NaiveTime::from_hms_nano_opt(14, 30, 0, 123_456_789).unwrap());
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_datetime() {
        let offset = FixedOffset::west_opt(5 * 3600).unwrap();
        let dt = offset.with_ymd_and_hms(2024, 1, 1, 8, 12, 5).unwrap();
        let k = Kind::DateTime(HDateTime::new(dt, "New_York"));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_coord() {
        let k = Kind::Coord(Coord::new(37.5458266, -77.4491888));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_xstr() {
        let k = Kind::XStr(XStr::new("Color", "red"));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_list() {
        let k = Kind::List(vec![
            Kind::Number(Number::unitless(1.0)),
            Kind::Str("two".into()),
            Kind::Bool(true),
        ]);
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_dict_kind() {
        let mut d = HDict::new();
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str("Main".into()));
        let k = Kind::Dict(Box::new(d.clone()));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    #[test]
    fn roundtrip_grid_kind() {
        let mut meta = HDict::new();
        meta.set("ver", Kind::Str("3.0".into()));
        let grid = HGrid::from_parts(
            meta.clone(),
            vec![HCol::new("name")],
            vec![{
                let mut r = HDict::new();
                r.set("name", Kind::Str("test".into()));
                r
            }],
        );
        let k = Kind::Grid(Box::new(grid));
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    // ── Dict round-trip tests ────────────────────────────────────────

    #[test]
    fn roundtrip_empty_dict() {
        let d = HDict::new();
        let bytes = encode_dict(&d).unwrap();
        let decoded = decode_dict(&bytes).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn roundtrip_dict_with_tags() {
        let mut d = HDict::new();
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str("Building A".into()));
        d.set(
            "area",
            Kind::Number(Number::new(35000.0, Some("ft²".into()))),
        );
        let bytes = encode_dict(&d).unwrap();
        let decoded = decode_dict(&bytes).unwrap();
        assert_eq!(d, decoded);
    }

    // ── Grid round-trip tests ────────────────────────────────────────

    #[test]
    fn roundtrip_empty_grid() {
        let grid = HGrid::new();
        let bytes = encode_grid(&grid).unwrap();
        let decoded = decode_grid(&bytes).unwrap();
        assert_eq!(grid, decoded);
    }

    #[test]
    fn roundtrip_grid_with_data() {
        let mut meta = HDict::new();
        meta.set("ver", Kind::Str("3.0".into()));

        let mut col_meta = HDict::new();
        col_meta.set("dis", Kind::Str("Display Name".into()));

        let cols = vec![
            HCol::new("id"),
            HCol::with_meta("dis", col_meta),
            HCol::new("area"),
        ];

        let mut row1 = HDict::new();
        row1.set("id", Kind::Ref(HRef::from_val("site-1")));
        row1.set("dis", Kind::Str("Main Campus".into()));
        row1.set(
            "area",
            Kind::Number(Number::new(50000.0, Some("ft²".into()))),
        );

        let mut row2 = HDict::new();
        row2.set("id", Kind::Ref(HRef::from_val("site-2")));
        row2.set("dis", Kind::Str("Annex".into()));
        row2.set(
            "area",
            Kind::Number(Number::new(12000.0, Some("ft²".into()))),
        );

        let grid = HGrid::from_parts(meta, cols, vec![row1, row2]);
        let bytes = encode_grid(&grid).unwrap();
        let decoded = decode_grid(&bytes).unwrap();
        assert_eq!(grid, decoded);
    }

    // ── Nested structures ────────────────────────────────────────────

    #[test]
    fn roundtrip_nested_dict_in_list() {
        let mut inner = HDict::new();
        inner.set("x", Kind::Number(Number::unitless(1.0)));
        let k = Kind::List(vec![
            Kind::Dict(Box::new(inner)),
            Kind::List(vec![Kind::Marker, Kind::NA]),
        ]);
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }

    // ── Compactness ──────────────────────────────────────────────────

    #[test]
    fn number_compact_size() {
        let k = Kind::Number(Number::new(72.5, Some("°F".into())));
        let bytes = encode_kind(&k).unwrap();
        // header(5) + tag(1) + f64(8) + opt(1) + len(1) + "°F"(3 UTF-8 bytes) = 19
        assert_eq!(bytes.len(), 19);
    }

    #[test]
    fn marker_compact_size() {
        let bytes = encode_kind(&Kind::Marker).unwrap();
        // header(5) + tag(1) = 6
        assert_eq!(bytes.len(), 6);
    }

    // ── Error cases ──────────────────────────────────────────────────

    #[test]
    fn error_invalid_magic() {
        let bytes = b"NOPE\x00\x01";
        assert!(decode_kind(bytes).is_err());
    }

    #[test]
    fn error_truncated_header() {
        let bytes = b"HBF";
        assert!(decode_kind(bytes).is_err());
    }

    #[test]
    fn error_truncated_payload() {
        // Valid header but payload is truncated mid-number
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"HBF\x01\x00");
        bytes.push(0x05); // TAG_NUMBER
        bytes.extend_from_slice(&[0x00, 0x00]); // only 2 of 8 f64 bytes
        assert!(decode_kind(&bytes).is_err());
    }

    #[test]
    fn error_invalid_tag() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"HBF\x01\x00");
        bytes.push(0xFF); // unknown tag
        let err = decode_kind(&bytes).unwrap_err();
        assert!(matches!(err, HbfError::InvalidTag(0xFF)));
    }

    // ── Special float values ─────────────────────────────────────────

    #[test]
    fn roundtrip_number_special_values() {
        for val in [f64::INFINITY, f64::NEG_INFINITY] {
            let k = Kind::Number(Number::unitless(val));
            let bytes = encode_kind(&k).unwrap();
            assert_eq!(decode_kind(&bytes).unwrap(), k);
        }
        // NaN needs special handling since NaN != NaN
        let k = Kind::Number(Number::unitless(f64::NAN));
        let bytes = encode_kind(&k).unwrap();
        if let Kind::Number(n) = decode_kind(&bytes).unwrap() {
            assert!(n.val.is_nan());
        } else {
            panic!("expected Number");
        }
    }

    #[test]
    fn roundtrip_empty_string() {
        let k = Kind::Str(String::new());
        let bytes = encode_kind(&k).unwrap();
        assert_eq!(decode_kind(&bytes).unwrap(), k);
    }
}
