//! Haystack wire format codecs for serialization and deserialization.
//!
//! Provides the [`Codec`] trait and five built-in implementations:
//!
//! | MIME Type | Module | Description |
//! |----------|--------|-------------|
//! | `text/zinc` | [`zinc`] | Zinc — the default Haystack text format (fastest encode/decode) |
//! | `text/trio` | [`trio`] | Trio — tag-oriented format for defining entities and defs |
//! | `application/json` | [`json`] (v4) | Haystack JSON v4 — standard JSON encoding |
//! | `application/json;v=3` | [`json`] (v3) | Haystack JSON v3 — legacy JSON encoding |
//! | `text/csv` | [`csv`] | CSV — comma-separated values for spreadsheet interop |
//!
//! Additional output-only codecs:
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`rdf`] | RDF serialization in Turtle and JSON-LD formats |
//!
//! Use [`codec_for()`] to look up a codec by MIME type:
//!
//! ```rust
//! use haystack_core::codecs::codec_for;
//!
//! let zinc = codec_for("text/zinc").unwrap();
//! let grid = zinc.decode_grid("ver:\"3.0\"\nempty\n").unwrap();
//! let encoded = zinc.encode_grid(&grid).unwrap();
//! ```
//!
//! The [`shared`] submodule provides common encoding/decoding helper functions
//! used by multiple codec implementations.

#[cfg(feature = "arrow")]
pub mod arrow_ipc;
pub mod csv;
#[cfg(feature = "haystack-serde")]
pub mod hbf;
pub mod json;
pub mod rdf;
pub mod shared;
pub mod trio;
pub mod zinc;

use crate::data::{HCol, HDict, HGrid};
use crate::kinds::Kind;

/// MIME type for Haystack Binary Format.
pub const HBF_MIME: &str = "application/x-haystack-binary";

/// Errors that can occur during encoding or decoding.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("parse error at position {pos}: {message}")]
    Parse { pos: usize, message: String },
    #[error("encoding error: {0}")]
    Encode(String),
    #[error("unsupported kind for this codec")]
    UnsupportedKind,
}

/// Trait for Haystack wire format codecs.
pub trait Codec: Send + Sync {
    /// The MIME type for this codec (e.g. `"text/zinc"`).
    fn mime_type(&self) -> &str;

    /// Encode an HGrid to a string.
    fn encode_grid(&self, grid: &HGrid) -> Result<String, CodecError>;

    /// Decode a string to an HGrid.
    fn decode_grid(&self, input: &str) -> Result<HGrid, CodecError>;

    /// Encode a single scalar Kind value to a string.
    fn encode_scalar(&self, val: &Kind) -> Result<String, CodecError>;

    /// Decode a string to a single scalar Kind value.
    fn decode_scalar(&self, input: &str) -> Result<Kind, CodecError>;

    /// Encode the grid header (version line + meta + column definitions).
    ///
    /// For Zinc: `ver:"3.0" meta\ncol1,col2,col3\n`
    /// Default implementation returns the full encoded grid (no streaming benefit).
    fn encode_grid_header(&self, grid: &HGrid) -> Result<Vec<u8>, CodecError> {
        self.encode_grid(grid).map(|s| s.into_bytes())
    }

    /// Encode a single grid row given the column definitions.
    ///
    /// For Zinc: `val1,val2,val3\n`
    /// Default implementation returns an empty vec (header contained everything).
    fn encode_grid_row(&self, _cols: &[HCol], _row: &HDict) -> Result<Vec<u8>, CodecError> {
        Ok(Vec::new())
    }
}

static ZINC: zinc::ZincCodec = zinc::ZincCodec;
static TRIO: trio::TrioCodec = trio::TrioCodec;
static JSON4: json::Json4Codec = json::Json4Codec;
static JSON3: json::Json3Codec = json::Json3Codec;
static CSV: csv::CsvCodec = csv::CsvCodec;

/// Look up a codec by MIME type.
///
/// Returns a static codec reference for the given MIME type, or `None` if
/// the MIME type is not supported.
pub fn codec_for(mime_type: &str) -> Option<&'static dyn Codec> {
    match mime_type {
        "text/zinc" => Some(&ZINC),
        "text/trio" => Some(&TRIO),
        "application/json" => Some(&JSON4),
        "application/json;v=3" => Some(&JSON3),
        "text/csv" => Some(&CSV),
        _ => None,
    }
}

/// Encode an HGrid to binary using HBF (Haystack Binary Format).
///
/// Returns `Some(bytes)` when the `haystack-serde` feature is enabled,
/// or `None` otherwise. The MIME type for HBF is `application/x-haystack-binary`.
#[cfg(feature = "haystack-serde")]
pub fn encode_grid_binary(grid: &HGrid) -> Result<Vec<u8>, String> {
    hbf::encode_grid(grid).map_err(|e| e.to_string())
}

/// Decode an HGrid from binary HBF (Haystack Binary Format) bytes.
///
/// Returns `Ok(grid)` when the `haystack-serde` feature is enabled.
/// The MIME type for HBF is `application/x-haystack-binary`.
#[cfg(feature = "haystack-serde")]
pub fn decode_grid_binary(bytes: &[u8]) -> Result<HGrid, String> {
    hbf::decode_grid(bytes).map_err(|e| e.to_string())
}
