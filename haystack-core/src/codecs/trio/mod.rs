// Trio wire format codec — record-per-entity text format used for
// definition files and entity data.

mod encoder;
mod parser;

pub use encoder::encode_grid;
pub use parser::decode_grid;

use super::{Codec, CodecError};
use crate::data::HGrid;
use crate::kinds::Kind;

/// Trio wire format codec.
pub struct TrioCodec;

impl Codec for TrioCodec {
    fn mime_type(&self) -> &str {
        "text/trio"
    }

    fn encode_grid(&self, grid: &HGrid) -> Result<String, CodecError> {
        encode_grid(grid)
    }

    fn decode_grid(&self, input: &str) -> Result<HGrid, CodecError> {
        decode_grid(input)
    }

    fn encode_scalar(&self, val: &Kind) -> Result<String, CodecError> {
        // Trio delegates scalar encoding to Zinc
        super::zinc::encode_scalar(val)
    }

    fn decode_scalar(&self, input: &str) -> Result<Kind, CodecError> {
        // Trio delegates scalar decoding to Zinc
        super::zinc::decode_scalar(input)
    }
}
