// JSON wire format codecs — v4 (application/json) and v3 (application/json;v=3).

pub mod v3;
pub mod v4;

pub use v3::Json3Codec;
pub use v4::Json4Codec;

use crate::codecs::CodecError;

/// Reject the lowercase date/time separator that RFC 3339 §5.6 permits.
///
/// `chrono::DateTime::parse_from_rfc3339` accepts `2024-06-30t12:00:00Z`, and Zinc
/// does not. Leaving the two disagreeing means a grid that round-trips through JSON
/// stops decoding as Zinc, which is the codec-divergence class issue #14 was about.
/// Both JSON codecs call this before parsing so all four codecs answer alike.
pub(crate) fn reject_lowercase_t_separator(s: &str) -> Result<(), CodecError> {
    if s.as_bytes().get(10) == Some(&b't') {
        return Err(CodecError::Parse {
            pos: 10,
            message: "invalid datetime: separator must be an uppercase 'T'; \
                      a lowercase 't' is not valid Haystack"
                .to_string(),
        });
    }
    Ok(())
}
