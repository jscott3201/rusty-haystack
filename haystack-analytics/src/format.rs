use crate::{Error, Result};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
    UInt32Array,
};
use arrow_schema::{DataType, Field, Schema};
use haystack_core::{
    codecs::{Codec, history::*, zinc::ZincCodec},
    kinds::{HDateTime, Kind, Number},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

pub const MANIFEST_NAME: &str = "manifest.json";
pub(crate) const TEMP_MANIFEST: &str = ".manifest.pending";
/// Largest `groups_per_part`; also the footer preflight's row-group list cap.
pub(crate) const MAX_GROUPS_PER_PART: usize = 16;
/// Smallest `batch_bytes`: one sample's fixed conservative charge in
/// `HistoryReadValidator::sample_bytes`, so every batch admits one sample.
pub(crate) const MIN_BATCH_BYTES: usize = 512;
/// Arrow memory of a one-row batch besides its value bytes: nine columns of
/// 64-byte-aligned buffers. Measured 3,393. The part floor reserves it on top
/// of `batch_bytes`, which already charges a row's value bytes, so a part can
/// always hold a one-row batch.
pub(crate) const ARROW_BATCH_BYTES: usize = 4096;
/// Smallest `part_bytes` for `batch_bytes` and `footer_bytes`: the writer
/// reserve of a one-row batch charged up to `batch_bytes`.
pub(crate) fn min_part_bytes(batch_bytes: usize, footer_bytes: usize) -> u64 {
    MIN_PART_BYTES.max(writer_reserve(
        batch_bytes.saturating_add(ARROW_BATCH_BYTES) as u64,
        footer_bytes,
    ))
}
/// Footer bytes besides the row groups: schema, column orders, `created_by`
/// and the Arrow schema key-value, whose base64 carries the timezone and
/// point unit. Measured 1,372 (a 1-group footer of 1,790 less one group) with
/// timezone `UTC` and no unit; the codec caps the unit at 128 bytes and zone
/// names are short, so the margin covers both.
pub(crate) const FOOTER_BASE_BYTES: usize = 2048;
/// Footer bytes per row group (nine column chunks). Measured 418 for small
/// parts and 434 at 400 rows; offsets and sizes are varints that widen as a
/// part grows, and the margin covers every integer at its 8 MiB width.
pub(crate) const FOOTER_GROUP_BYTES: usize = 768;
/// Manifest bytes besides the header and part entries: the JSON envelope and
/// `limits` at full-width values. Measured 374.
pub(crate) const MANIFEST_BASE_BYTES: usize = 512;
/// JSON-escaped `source_header` the manifest floor admits. Measured 2,956 for
/// the reference header (`tests/support`) and 3,332 for the largest one the
/// bounded-history codec admits (256-byte id, longest unit and zone name);
/// see `floors_hold_the_largest_header_and_a_full_part`.
pub(crate) const MANIFEST_HEADER_BYTES: usize = 4096;
/// Manifest bytes per part entry at full-width values. Measured 149 to 153.
pub(crate) const MANIFEST_PART_BYTES: usize = 256;
/// Smallest `footer_bytes` for `groups_per_part`: a full part's footer fits.
pub(crate) fn min_footer_bytes(groups_per_part: usize) -> usize {
    FOOTER_BASE_BYTES.saturating_add(FOOTER_GROUP_BYTES.saturating_mul(groups_per_part))
}
/// Smallest `manifest_bytes` for `parts`: the manifest of `parts` part
/// entries and a header within [`MANIFEST_HEADER_BYTES`] fits.
pub(crate) fn min_manifest_bytes(parts: usize) -> usize {
    (MANIFEST_BASE_BYTES + MANIFEST_HEADER_BYTES)
        .saturating_add(MANIFEST_PART_BYTES.saturating_mul(parts))
}
/// Absolute `part_bytes` floor; [`min_part_bytes`] exceeds it for any batch.
pub(crate) const MIN_PART_BYTES: u64 = 1024;
/// Encoded size of a column chunk, page or batch may reach this multiple of its
/// conservative source charge (PLAIN length prefixes, levels, alignment).
pub(crate) const ENCODING_EXPANSION: u64 = 2;
/// Fixed encoding slack added on top of [`ENCODING_EXPANSION`].
pub(crate) const ENCODING_SLACK_BYTES: u64 = 64 * 1024;
/// Largest recorded `source_header` text accepted on reopen.
pub(crate) const MAX_HEADER_BYTES: usize = 128 * 1024;
/// The recorded header is exactly the two-line Zinc envelope with no rows.
pub(crate) const HEADER_LINES: usize = 2;
/// Upper bound on the encoded bytes of one row group, and of any page in it,
/// for a batch charged at most `batch_bytes`.
pub(crate) fn encoded_ceiling(batch_bytes: u64) -> u64 {
    batch_bytes
        .saturating_mul(ENCODING_EXPANSION)
        .saturating_add(ENCODING_SLACK_BYTES)
}
/// Writer admission reserve: the encoded ceiling of `arrow_bytes` plus a full
/// footer. `Limits::validate` requires a part to fit one maximal batch.
pub(crate) fn writer_reserve(arrow_bytes: u64, footer_bytes: usize) -> u64 {
    encoded_ceiling(arrow_bytes).saturating_add(footer_bytes as u64)
}
/// Lowercase hex SHA-256, the manifest's hash spelling.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
/// Hard finite ceilings. Options may tighten these, never enlarge them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub batch_rows: usize,
    /// Conservative source charge (512 bytes/sample plus variable values).
    pub batch_bytes: usize,
    pub field_bytes: usize,
    pub groups_per_part: usize,
    pub part_bytes: u64,
    pub total_bytes: u64,
    pub parts: usize,
    pub manifest_bytes: usize,
    pub footer_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            batch_rows: 1024,
            batch_bytes: 1024 * 1024,
            field_bytes: 32 * 1024,
            groups_per_part: MAX_GROUPS_PER_PART,
            part_bytes: 8 * 1024 * 1024,
            total_bytes: 64 * 1024 * 1024,
            parts: 256,
            manifest_bytes: 256 * 1024,
            footer_bytes: 128 * 1024,
        }
    }
}
impl Limits {
    /// Every option within the defaults and above its minimum, and a part
    /// large enough for the writer reserve of a `batch_bytes` batch plus the
    /// fixed Arrow overhead of [`ARROW_BATCH_BYTES`]. The reserve check is
    /// necessary, not sufficient for multi-row batches: Arrow memory is only
    /// known per batch, and `Writer::write` still enforces it then.
    ///
    /// The footer and manifest floors are derived from `groups_per_part` and
    /// `parts`, so any validating `Limits` can publish at least an empty and a
    /// one-row capture whose header is within [`MANIFEST_HEADER_BYTES`].
    pub(crate) fn validate(&self) -> Result<()> {
        let max = Self::default();
        if self.batch_rows == 0
            || self.batch_rows > max.batch_rows
            || self.batch_bytes < MIN_BATCH_BYTES
            || self.batch_bytes > max.batch_bytes
            || self.field_bytes == 0
            || self.field_bytes > max.field_bytes
            || self.groups_per_part == 0
            || self.groups_per_part > max.groups_per_part
            || self.part_bytes > max.part_bytes
            || self.part_bytes < min_part_bytes(self.batch_bytes, self.footer_bytes)
            || self.total_bytes < self.part_bytes
            || self.total_bytes > max.total_bytes
            || self.parts == 0
            || self.parts > max.parts
            || self.manifest_bytes < min_manifest_bytes(self.parts)
            || self.manifest_bytes > max.manifest_bytes
            || self.footer_bytes < min_footer_bytes(self.groups_per_part)
            || self.footer_bytes > max.footer_bytes
        {
            return Err(Error::Options);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub ordinal: usize,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub rows: u64,
    pub groups: usize,
}
/// Versioned manifest. `source_header` is the existing lossless Zinc/H4 header
/// envelope: integer observations are decimal strings, timestamps retain their
/// original offset and zone. No URL or credentials are stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u32,
    pub schema_version: u32,
    pub export_id: String,
    pub consistency: String,
    pub source_header: String,
    pub limits: Limits,
    pub rows: u64,
    pub parts: Vec<Part>,
}
pub(crate) fn encode_header(header: &HistoryReadHeader) -> Result<String> {
    ZincCodec
        .encode_grid(&result_header_grid(header).map_err(|_| Error::Invalid)?)
        .map_err(|_| Error::Invalid)
}
pub(crate) fn decode_header(text: &str) -> Result<HistoryReadHeader> {
    if text.len() > MAX_HEADER_BYTES || text.lines().count() != HEADER_LINES {
        return Err(Error::Invalid);
    }
    header_preflight(text)?;
    // The preflight is the contract; containment is only a backstop for an
    // upstream parser defect the preflight did not anticipate.
    let grid = crate::contain(|| ZincCodec.decode_grid(text).map_err(|_| Error::Invalid))?;
    if !grid.rows.is_empty() {
        return Err(Error::Invalid);
    }
    let header = result_header_from_grid(&grid).map_err(|_| Error::Invalid)?;
    HistoryReadValidator::new(header.clone()).map_err(|_| Error::Invalid)?;
    header
        .validate_for_request(&HistoryReadRequest {
            id: header.metadata.id.clone(),
            range: header.metadata.requested_range.clone(),
        })
        .map_err(|_| Error::Invalid)?;
    if header.terminal != HistoryTerminal::Complete {
        return Err(Error::Invalid);
    }
    // Canonical form: the recorded text must be exactly what the writer
    // encodes for this header, so spellings the decoder normalizes (another
    // `ver`, duplicate tags, extra control-JSON keys, spacing) are rejected.
    if encode_header(&header)? != text {
        return Err(Error::Invalid);
    }
    Ok(header)
}
/// Lexical preflight of the recorded header before the general Zinc parser.
/// Outside the quoted typed-control string the profile has no containers and
/// only printable ASCII (Zinc's temporal readers advance fixed byte counts, so
/// a code point there must never reach them) plus the line separator. Inside
/// a quoted string, Unicode is allowed but raw control characters are not:
/// the encoder escapes them.
pub(crate) fn header_preflight(text: &str) -> Result<()> {
    let mut quoted = false;
    let mut escaped = false;
    for c in text.chars() {
        if quoted {
            if c.is_control() {
                return Err(Error::Invalid);
            } else if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
        } else if c != '\n'
            && (!(' '..='~').contains(&c)
                || matches!(c, '[' | ']' | '{' | '}' | '(' | ')' | '`' | '<' | '>'))
        {
            return Err(Error::Invalid);
        }
    }
    if quoted || escaped {
        return Err(Error::Invalid);
    }
    Ok(())
}
pub(crate) fn schema(header: &HistoryReadHeader) -> Arc<Schema> {
    let metadata = HashMap::from([
        ("haystack.schema".into(), "1".into()),
        (
            "haystack.kind".into(),
            format!("{:?}", header.metadata.schema.kind),
        ),
        (
            "haystack.timezone".into(),
            header.metadata.schema.timezone.clone(),
        ),
        (
            "haystack.point_unit".into(),
            header.metadata.schema.unit.clone().unwrap_or_default(),
        ),
    ]);
    Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("point_id", DataType::Utf8, false),
            Field::new("utc_seconds", DataType::Int64, false),
            Field::new("nanoseconds", DataType::UInt32, false),
            Field::new("offset_seconds", DataType::Int32, false),
            Field::new("present", DataType::Boolean, false),
            Field::new("bool_value", DataType::Boolean, true),
            Field::new("number_value", DataType::Float64, true),
            Field::new("string_value", DataType::Utf8, true),
            Field::new("sample_unit", DataType::Utf8, true),
        ],
        metadata,
    ))
}
pub(crate) fn batch(
    header: &HistoryReadHeader,
    samples: &[HistorySample],
    limits: &Limits,
) -> Result<RecordBatch> {
    if samples.is_empty() || samples.len() > limits.batch_rows {
        return Err(Error::Limit);
    }
    let mut bytes = 0;
    for sample in samples {
        bytes += HistoryReadValidator::sample_bytes(sample);
        if bytes > limits.batch_bytes as u64 {
            return Err(Error::Limit);
        }
        if !valid_value(&sample.val, &header.metadata.schema) {
            return Err(Error::Invalid);
        }
        let field = match &sample.val {
            Kind::Str(s) => s.len(),
            Kind::Number(n) => n.unit.as_ref().map_or(0, String::len),
            _ => 0,
        };
        if field > limits.field_bytes {
            return Err(Error::Limit);
        }
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            samples.iter().map(|_| header.metadata.id.as_str()),
        )),
        Arc::new(Int64Array::from_iter_values(
            samples.iter().map(|s| s.ts.dt.timestamp()),
        )),
        Arc::new(UInt32Array::from_iter_values(
            samples.iter().map(|s| s.ts.dt.timestamp_subsec_nanos()),
        )),
        Arc::new(Int32Array::from_iter_values(
            samples.iter().map(|s| s.ts.dt.offset().local_minus_utc()),
        )),
        Arc::new(BooleanArray::from(
            samples
                .iter()
                .map(|s| !matches!(s.val, Kind::NA))
                .collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            samples
                .iter()
                .map(|s| {
                    if let Kind::Bool(v) = s.val {
                        Some(v)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            samples
                .iter()
                .map(|s| {
                    if let Kind::Number(v) = &s.val {
                        Some(v.val)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from_iter(samples.iter().map(|s| {
            if let Kind::Str(v) = &s.val {
                Some(v.as_str())
            } else {
                None
            }
        }))),
        Arc::new(StringArray::from_iter(samples.iter().map(|s| {
            if let Kind::Number(v) = &s.val {
                v.unit.as_deref()
            } else {
                None
            }
        }))),
    ];
    Ok(RecordBatch::try_new(schema(header), cols)?)
}
fn column<T: 'static>(batch: &RecordBatch, i: usize) -> Result<&T> {
    batch
        .column(i)
        .as_any()
        .downcast_ref()
        .ok_or(Error::Invalid)
}
pub(crate) fn samples(
    header: &HistoryReadHeader,
    batch: &RecordBatch,
    limits: &Limits,
) -> Result<Vec<HistorySample>> {
    if batch.schema() != schema(header) || batch.num_rows() > limits.batch_rows {
        return Err(Error::Invalid);
    }
    let id = column::<StringArray>(batch, 0)?;
    let sec = column::<Int64Array>(batch, 1)?;
    let nano = column::<UInt32Array>(batch, 2)?;
    let offset = column::<Int32Array>(batch, 3)?;
    let present = column::<BooleanArray>(batch, 4)?;
    let boolean = column::<BooleanArray>(batch, 5)?;
    let number = column::<Float64Array>(batch, 6)?;
    let string = column::<StringArray>(batch, 7)?;
    let unit = column::<StringArray>(batch, 8)?;
    let mut result = Vec::with_capacity(batch.num_rows());
    let mut bytes = 0;
    for i in 0..batch.num_rows() {
        if batch.columns()[..5].iter().any(|a| a.is_null(i)) || id.value(i) != header.metadata.id {
            return Err(Error::Invalid);
        }
        let slots = [boolean.is_valid(i), number.is_valid(i), string.is_valid(i)];
        let val = if !present.value(i) {
            if slots.iter().any(|v| *v) || unit.is_valid(i) {
                return Err(Error::Invalid);
            }
            Kind::NA
        } else {
            match (header.metadata.schema.kind, slots) {
                (HistoryKind::Bool, [true, false, false]) if unit.is_null(i) => {
                    Kind::Bool(boolean.value(i))
                }
                (HistoryKind::Number, [false, true, false]) => {
                    if unit.is_valid(i) && unit.value(i).len() > limits.field_bytes {
                        return Err(Error::Limit);
                    }
                    Kind::Number(Number::new(
                        number.value(i),
                        unit.is_valid(i).then(|| unit.value(i).to_owned()),
                    ))
                }
                (HistoryKind::Str, [false, false, true]) if unit.is_null(i) => {
                    if string.value(i).len() > limits.field_bytes {
                        return Err(Error::Limit);
                    }
                    Kind::Str(string.value(i).to_owned())
                }
                _ => return Err(Error::Invalid),
            }
        };
        let dt =
            chrono::DateTime::from_timestamp(sec.value(i), nano.value(i)).ok_or(Error::Invalid)?;
        let tz = chrono::FixedOffset::east_opt(offset.value(i)).ok_or(Error::Invalid)?;
        let sample = HistorySample {
            ts: HDateTime::new(dt.with_timezone(&tz), &header.metadata.schema.timezone),
            val,
        };
        bytes += HistoryReadValidator::sample_bytes(&sample);
        if bytes > limits.batch_bytes as u64 {
            return Err(Error::Limit);
        }
        result.push(sample);
    }
    Ok(result)
}
