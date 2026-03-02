//! Content negotiation — parse Accept header to pick codec, decode request body.

use haystack_core::codecs::{CodecError, codec_for};
use haystack_core::data::HGrid;

/// Default MIME type when no Accept header is provided or no supported type is found.
const DEFAULT_MIME: &str = "text/zinc";

/// MIME type for Haystack Binary Format (HBF).
const HBF_MIME: &str = "application/x-haystack-binary";

/// Supported MIME types in preference order.
const SUPPORTED: &[&str] = &[
    "text/zinc",
    "application/json",
    "text/trio",
    "application/json;v=3",
    HBF_MIME,
];

/// Parse an Accept header and return the best supported MIME type.
///
/// Returns `"text/zinc"` when the header is empty, `*/*`, or contains
/// no recognized MIME type.
pub fn parse_accept(accept_header: &str) -> &'static str {
    let accept = accept_header.trim();
    if accept.is_empty() || accept == "*/*" {
        return DEFAULT_MIME;
    }

    // Parse weighted entries: "text/zinc;q=0.9, application/json;q=1.0"
    let mut candidates: Vec<(&str, f32)> = Vec::new();

    for part in accept.split(',') {
        let part = part.trim();
        let mut segments = part.splitn(2, ';');
        let mime = segments.next().unwrap_or("").trim();

        // Check for q= parameter
        let quality = segments
            .next()
            .and_then(|params| {
                for param in params.split(';') {
                    let param = param.trim();
                    if let Some(q_val) = param.strip_prefix("q=") {
                        return q_val.trim().parse::<f32>().ok();
                    }
                }
                None
            })
            .unwrap_or(1.0);

        // Handle application/json;v=3 specially: need to check the original part
        if mime == "application/json" && part.contains("v=3") {
            candidates.push(("application/json;v=3", quality));
        } else if mime == "*/*" {
            candidates.push((DEFAULT_MIME, quality));
        } else {
            candidates.push((mime, quality));
        }
    }

    // Sort by quality descending, then pick the first supported
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (mime, _) in &candidates {
        for supported in SUPPORTED {
            if mime == supported {
                return supported;
            }
        }
    }

    DEFAULT_MIME
}

/// Decode a request body into an HGrid using the given Content-Type.
///
/// Falls back to `"text/zinc"` if the content type is not recognized.
pub fn decode_request_grid(body: &str, content_type: &str) -> Result<HGrid, CodecError> {
    let mime = normalize_content_type(content_type);
    let codec = codec_for(mime)
        .unwrap_or_else(|| codec_for(DEFAULT_MIME).expect("default codec must exist"));
    codec.decode_grid(body)
}

/// Encode an HGrid for the response using the best Accept type.
///
/// Returns `(body_bytes, content_type)`. Text codecs produce UTF-8 bytes;
/// HBF produces raw binary.
pub fn encode_response_grid(
    grid: &HGrid,
    accept: &str,
) -> Result<(Vec<u8>, &'static str), CodecError> {
    let mime = parse_accept(accept);

    if mime == HBF_MIME {
        let bytes = haystack_core::codecs::encode_grid_binary(grid).map_err(CodecError::Encode)?;
        return Ok((bytes, HBF_MIME));
    }

    let codec = codec_for(mime)
        .unwrap_or_else(|| codec_for(DEFAULT_MIME).expect("default codec must exist"));
    let body = codec.encode_grid(grid)?;
    // Return the static mime type string that matches what we used
    for supported in SUPPORTED {
        if *supported == mime {
            return Ok((body.into_bytes(), supported));
        }
    }
    Ok((body.into_bytes(), DEFAULT_MIME))
}

/// Normalize a Content-Type header to a bare MIME type for codec lookup.
fn normalize_content_type(content_type: &str) -> &str {
    let ct = content_type.trim();
    if ct.is_empty() {
        return DEFAULT_MIME;
    }
    // Handle HBF binary format
    if ct.starts_with(HBF_MIME) {
        return HBF_MIME;
    }
    // Handle "application/json; v=3" or "application/json;v=3"
    if ct.starts_with("application/json") && ct.contains("v=3") {
        return "application/json;v=3";
    }
    // Strip any parameters like charset
    let base = ct.split(';').next().unwrap_or(ct).trim();
    // Verify it is a supported type
    for supported in SUPPORTED {
        if base == *supported {
            return supported;
        }
    }
    DEFAULT_MIME
}

/// Decode a request body (as raw bytes) into an HGrid using the given Content-Type.
///
/// Supports both text-based codecs and the binary HBF codec. Falls back to
/// `"text/zinc"` if the content type is not recognized.
pub fn decode_request_grid_bytes(body: &[u8], content_type: &str) -> Result<HGrid, CodecError> {
    let ct = normalize_content_type(content_type);
    if ct == HBF_MIME {
        return haystack_core::codecs::decode_grid_binary(body).map_err(CodecError::Encode);
    }
    let text = std::str::from_utf8(body).map_err(|e| CodecError::Encode(e.to_string()))?;
    decode_request_grid(text, content_type)
}

/// Encode a grid as a streaming byte iterator: yields header chunk then row chunks.
///
/// Returns `(header_bytes, row_batches, content_type)`.
/// Rows are batched into groups of ~500 to balance streaming granularity against
/// allocation overhead. For codecs without streaming support (or HBF), the header
/// contains the full response and `row_batches` is empty.
pub fn encode_response_streaming(
    grid: &HGrid,
    accept: &str,
) -> Result<(Vec<u8>, Vec<Vec<u8>>, &'static str), CodecError> {
    let mime = parse_accept(accept);

    // HBF: full binary encode (zstd compression needs the full payload)
    if mime == HBF_MIME {
        let bytes = haystack_core::codecs::encode_grid_binary(grid).map_err(CodecError::Encode)?;
        return Ok((bytes, Vec::new(), HBF_MIME));
    }

    let codec = codec_for(mime)
        .unwrap_or_else(|| codec_for(DEFAULT_MIME).expect("default codec must exist"));

    let header = codec.encode_grid_header(grid)?;

    // Batch rows into chunks of 500 to avoid per-row allocation overhead
    const BATCH_SIZE: usize = 500;
    let mut batches = Vec::with_capacity(grid.rows.len() / BATCH_SIZE + 1);
    for chunk in grid.rows.chunks(BATCH_SIZE) {
        let mut buf = Vec::new();
        for row in chunk {
            buf.extend_from_slice(&codec.encode_grid_row(&grid.cols, row)?);
        }
        batches.push(buf);
    }

    let ct = SUPPORTED
        .iter()
        .find(|&&s| s == mime)
        .copied()
        .unwrap_or(DEFAULT_MIME);
    Ok((header, batches, ct))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accept_empty() {
        assert_eq!(parse_accept(""), "text/zinc");
    }

    #[test]
    fn parse_accept_wildcard() {
        assert_eq!(parse_accept("*/*"), "text/zinc");
    }

    #[test]
    fn parse_accept_json() {
        assert_eq!(parse_accept("application/json"), "application/json");
    }

    #[test]
    fn parse_accept_zinc() {
        assert_eq!(parse_accept("text/zinc"), "text/zinc");
    }

    #[test]
    fn parse_accept_trio() {
        assert_eq!(parse_accept("text/trio"), "text/trio");
    }

    #[test]
    fn parse_accept_json_v3() {
        assert_eq!(parse_accept("application/json;v=3"), "application/json;v=3");
    }

    #[test]
    fn parse_accept_unsupported_falls_back() {
        assert_eq!(parse_accept("text/html"), "text/zinc");
    }

    #[test]
    fn parse_accept_multiple_with_quality() {
        assert_eq!(
            parse_accept("text/zinc;q=0.5, application/json;q=1.0"),
            "application/json"
        );
    }

    #[test]
    fn normalize_content_type_empty() {
        assert_eq!(normalize_content_type(""), "text/zinc");
    }

    #[test]
    fn normalize_content_type_json_v3() {
        assert_eq!(
            normalize_content_type("application/json; v=3"),
            "application/json;v=3"
        );
    }

    #[test]
    fn normalize_content_type_with_charset() {
        assert_eq!(
            normalize_content_type("text/zinc; charset=utf-8"),
            "text/zinc"
        );
    }

    #[test]
    fn decode_request_grid_empty_zinc() {
        // Empty zinc grid: "ver:\"3.0\"\nempty\n"
        let result = decode_request_grid("ver:\"3.0\"\nempty\n", "text/zinc");
        assert!(result.is_ok());
    }

    #[test]
    fn encode_response_grid_default() {
        let grid = HGrid::new();
        let result = encode_response_grid(&grid, "");
        assert!(result.is_ok());
        let (_, content_type) = result.unwrap();
        assert_eq!(content_type, "text/zinc");
    }

    #[test]
    fn parse_accept_hbf() {
        assert_eq!(parse_accept(HBF_MIME), HBF_MIME);
    }

    #[test]
    fn normalize_content_type_hbf() {
        assert_eq!(normalize_content_type(HBF_MIME), HBF_MIME);
    }

    #[test]
    fn encode_decode_hbf_round_trip() {
        let grid = HGrid::new();
        let (bytes, ct) = encode_response_grid(&grid, HBF_MIME).unwrap();
        assert_eq!(ct, HBF_MIME);
        let decoded = decode_request_grid_bytes(&bytes, HBF_MIME).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn decode_request_grid_bytes_text_fallback() {
        let result = decode_request_grid_bytes(b"ver:\"3.0\"\nempty\n", "text/zinc");
        assert!(result.is_ok());
    }

    #[test]
    fn streaming_zinc_matches_full_encode() {
        use haystack_core::data::{HCol, HDict};
        use haystack_core::kinds::{HRef, Kind};

        let mut rows = Vec::new();
        for i in 0..5 {
            let mut d = HDict::new();
            d.set(
                String::from("id"),
                Kind::Ref(HRef::from_val(&format!("r{i}"))),
            );
            d.set(String::from("dis"), Kind::Str(format!("Row {i}")));
            rows.push(d);
        }
        let cols = vec![
            HCol::new(String::from("id")),
            HCol::new(String::from("dis")),
        ];
        let grid = HGrid::from_parts(HDict::new(), cols, rows);

        // Full encode
        let (full_bytes, ct) = encode_response_grid(&grid, "text/zinc").unwrap();
        assert_eq!(ct, "text/zinc");

        // Streaming encode
        let (header, row_chunks, ct2) = encode_response_streaming(&grid, "text/zinc").unwrap();
        assert_eq!(ct2, "text/zinc");

        let mut streamed = header;
        for chunk in row_chunks {
            streamed.extend_from_slice(&chunk);
        }
        assert_eq!(full_bytes, streamed);
    }
}
