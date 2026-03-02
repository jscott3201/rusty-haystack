// Zinc wire format codec — the primary text-based wire format for Haystack.

mod encoder;
mod parser;

pub use encoder::{
    encode_grid, encode_grid_header, encode_grid_row, encode_meta, encode_scalar, escape_str,
};
pub use parser::{ZincParser, decode_grid, decode_scalar};

use super::{Codec, CodecError};
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::Kind;

/// Zinc wire format codec.
pub struct ZincCodec;

impl Codec for ZincCodec {
    fn mime_type(&self) -> &str {
        "text/zinc"
    }

    fn encode_grid(&self, grid: &HGrid) -> Result<String, CodecError> {
        encode_grid(grid)
    }

    fn decode_grid(&self, input: &str) -> Result<HGrid, CodecError> {
        decode_grid(input)
    }

    fn encode_scalar(&self, val: &Kind) -> Result<String, CodecError> {
        encode_scalar(val)
    }

    fn decode_scalar(&self, input: &str) -> Result<Kind, CodecError> {
        decode_scalar(input)
    }

    fn encode_grid_header(&self, grid: &HGrid) -> Result<Vec<u8>, CodecError> {
        encoder::encode_grid_header(grid).map(|s| s.into_bytes())
    }

    fn encode_grid_row(&self, cols: &[HCol], row: &HDict) -> Result<Vec<u8>, CodecError> {
        encoder::encode_grid_row(cols, row).map(|s| s.into_bytes())
    }
}
