// Haystack codecs — wire format serialization / deserialization.

pub mod csv;
pub mod json;
pub mod rdf;
pub mod shared;
pub mod trio;
pub mod zinc;

use crate::data::HGrid;
use crate::kinds::Kind;

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
