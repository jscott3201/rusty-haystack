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
/// Smallest `manifest_bytes`; an empty manifest with a header fits easily.
pub(crate) const MIN_MANIFEST_BYTES: usize = 1024;
/// Smallest `footer_bytes`. Real footers are larger; this only rejects zero.
pub(crate) const MIN_FOOTER_BYTES: usize = 128;
/// Smallest `part_bytes` before the writer-reserve check below applies.
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
    /// large enough for the writer reserve of one full `batch_bytes` batch.
    /// The reserve check is necessary, not sufficient: Arrow memory is only
    /// known per batch, and `Writer::write` still enforces it then.
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
            || self.part_bytes < MIN_PART_BYTES
            || self.part_bytes > max.part_bytes
            || self.part_bytes < writer_reserve(self.batch_bytes as u64, self.footer_bytes)
            || self.total_bytes < self.part_bytes
            || self.total_bytes > max.total_bytes
            || self.parts == 0
            || self.parts > max.parts
            || self.manifest_bytes < MIN_MANIFEST_BYTES
            || self.manifest_bytes > max.manifest_bytes
            || self.footer_bytes < MIN_FOOTER_BYTES
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
