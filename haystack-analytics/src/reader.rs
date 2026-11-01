use crate::{Error, Result, contain, format::*, writer::part_name};
use bytes::Bytes;
use haystack_core::codecs::history::{
    HistoryReadHeader, HistoryReadValidator, HistorySample, MAX_ROWS,
};
use parquet::{
    arrow::arrow_reader::{
        ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReader,
        ParquetRecordBatchReaderBuilder,
    },
    basic::{Compression, Encoding, Type as Physical},
};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

/// Compact Thrift preflight: deepest struct/collection nesting.
const COMPACT_MAX_DEPTH: usize = 12;
/// Compact Thrift preflight: total fields and collection elements per message.
const COMPACT_MAX_NODES: usize = 16_384;
/// Highest Thrift field id admitted; every Parquet footer/page struct is lower.
/// Ids are tracked per struct so a duplicate field (which a preflight and the
/// upstream decoder could resolve differently) is rejected.
const COMPACT_MAX_FIELD_ID: i16 = 63;
/// Elements in any list or set other than the two footer lists below.
const COMPACT_MAX_LIST: u64 = 1024;
/// Entries in any map.
const COMPACT_MAX_MAP: u64 = 128;
/// Footer `schema` list: the root plus nine flat fields.
const FOOTER_MAX_SCHEMA_ELEMENTS: u64 = 1 + COLUMNS.len() as u64;
/// Bytes of one page header examined by the page preflight.
const MAX_PAGE_HEADER_BYTES: usize = 4096;
/// `PAR1` + footer length + `PAR1`: the smallest structurally possible file.
pub(crate) const PARQUET_MIN_FILE_BYTES: u64 = 12;
const PARQUET_MAGIC: &[u8; 4] = b"PAR1";
/// Thrift enum values in a page header for this profile.
const PAGE_TYPE_DATA_PAGE: i64 = 0;
const ENCODING_PLAIN: i64 = 0;
const ENCODING_RLE: i64 = 3;
/// Physical type and optionality of each column of `format::schema`. A
/// required column has no level section; an optional one has a single
/// definition level (bit width one) and no repetition levels.
const COLUMNS: [(Physical, bool); 9] = [
    (Physical::BYTE_ARRAY, false),
    (Physical::INT64, false),
    (Physical::INT32, false),
    (Physical::INT32, false),
    (Physical::BOOLEAN, false),
    (Physical::BOOLEAN, true),
    (Physical::DOUBLE, true),
    (Physical::BYTE_ARRAY, true),
    (Physical::BYTE_ARRAY, true),
];

/// Lazy bounded reader of one committed dataset.
///
/// `open` verifies every part's hash and physical layout before returning, so
/// corruption is rejected before any row. Reading then holds one part in
/// memory at a time (at most the recorded `part_bytes`): each part is opened
/// once, its exact bytes are hashed against the manifest again, guarded and
/// decoded from that same buffer, so a part replaced after `open` fails with
/// `Error::Invalid` instead of being decoded unchecked. Only consuming through
/// `next_batch() == Ok(None)` establishes semantic completion.
///
/// Both `open` and `next_batch` do blocking file I/O and CPU-bound decoding;
/// from async code, run them on `tokio::task::spawn_blocking`.
pub struct Dataset {
    dir: PathBuf,
    manifest: Manifest,
    manifest_sha256: String,
    header: HistoryReadHeader,
    validator: HistoryReadValidator,
    part: usize,
    group: usize,
    reader: Option<ParquetRecordBatchReader>,
    current: Option<(Bytes, ArrowReaderMetadata)>,
    complete: bool,
    failed: bool,
}
impl Dataset {
    pub fn open(directory: impl Into<PathBuf>) -> Result<Self> {
        let dir = directory.into();
        let max = Limits::default();
        let bytes = read_regular(&dir.join(MANIFEST_NAME), max.manifest_bytes as u64, false)?;
        let manifest_sha256 = sha256_hex(&bytes);
        let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
        manifest.limits.validate().map_err(|_| Error::Invalid)?;
        if manifest.manifest_version != 1
            || manifest.schema_version != 1
            || manifest.consistency != "completedGenerationCheckedLiveCapture"
            || uuid::Uuid::parse_str(&manifest.export_id).is_err()
            || manifest.parts.len() > manifest.limits.parts
            || bytes.len() > manifest.limits.manifest_bytes
        {
            return Err(Error::Invalid);
        }
        if manifest.rows > MAX_ROWS as u64 {
            return Err(Error::Limit);
        }
        let header = decode_header(&manifest.source_header)?;
        if header.returned_count != manifest.rows {
            return Err(Error::Invalid);
        }
        let mut rows = 0u64;
        let mut bytes = 0u64;
        for (i, part) in manifest.parts.iter().enumerate() {
            if part.ordinal != i
                || part.name != part_name(i)
                || part.bytes < PARQUET_MIN_FILE_BYTES
                || part.bytes > manifest.limits.part_bytes
                || part.groups == 0
                || part.groups > manifest.limits.groups_per_part
                || part.rows == 0
                || part.rows > (part.groups * manifest.limits.batch_rows) as u64
                || part.sha256.len() != 64
                || !part
                    .sha256
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            {
                return Err(Error::Invalid);
            }
            rows = rows.checked_add(part.rows).ok_or(Error::Limit)?;
            bytes = bytes.checked_add(part.bytes).ok_or(Error::Limit)?;
            if bytes > manifest.limits.total_bytes || rows > MAX_ROWS as u64 {
                return Err(Error::Limit);
            }
            let data = load_part(&dir, part)?;
            checked_metadata(&data, part, &header, &manifest.limits)?;
        }
        if rows != manifest.rows {
            return Err(Error::Invalid);
        }
        let validator = HistoryReadValidator::new(header.clone()).map_err(|_| Error::Invalid)?;
        Ok(Self {
            dir,
            manifest,
            manifest_sha256,
            header,
            validator,
            part: 0,
            group: 0,
            reader: None,
            current: None,
            complete: false,
            failed: false,
        })
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    /// Lowercase hex SHA-256 of the exact manifest bytes this reader parsed:
    /// a stable identity for the dataset (the manifest pins every part hash).
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
    pub fn header(&self) -> &HistoryReadHeader {
        &self.header
    }
    pub fn semantically_complete(&self) -> bool {
        self.complete
    }
    pub fn next_batch(&mut self) -> Result<Option<Vec<HistorySample>>> {
        if self.failed {
            return Err(Error::Invalid);
        }
        let result = self.next_inner();
        if result.is_err() {
            self.failed = true;
            self.reader = None;
            self.current = None;
        }
        result
    }
    fn next_inner(&mut self) -> Result<Option<Vec<HistorySample>>> {
        if self.complete {
            return Ok(None);
        }
        loop {
            if let Some(reader) = &mut self.reader {
                // Guarded input; containment is only the backstop.
                if let Some(batch) = contain(|| Ok(reader.next()))? {
                    let batch = batch?;
                    let batch = arrow_array::RecordBatch::try_new(
                        schema(&self.header),
                        batch.columns().to_vec(),
                    )?;
                    let samples = samples(&self.header, &batch, &self.manifest.limits)?;
                    for sample in &samples {
                        self.validator.accept(sample).map_err(|_| Error::Invalid)?;
                    }
                    return Ok(Some(samples));
                }
                self.reader = None;
                self.group += 1;
            }
            if self.part == self.manifest.parts.len() {
                self.validator.finish().map_err(|_| Error::Invalid)?;
                self.complete = true;
                return Ok(None);
            }
            let part = &self.manifest.parts[self.part];
            if self.group == part.groups {
                self.part += 1;
                self.group = 0;
                self.current = None;
                continue;
            }
            if self.current.is_none() {
                // Re-hash the exact bytes that will be decoded.
                let data = load_part(&self.dir, part)?;
                let metadata = checked_metadata(&data, part, &self.header, &self.manifest.limits)?;
                self.current = Some((data, metadata));
            }
            let (data, metadata) = self.current.as_ref().ok_or(Error::Invalid)?;
            let builder =
                ParquetRecordBatchReaderBuilder::new_with_metadata(data.clone(), metadata.clone())
                    .with_row_groups(vec![self.group])
                    .with_batch_size(self.manifest.limits.batch_rows);
            self.reader = Some(contain(|| Ok(builder.build()?))?);
        }
    }
}
/// Open `path` once and read it whole. The path must name a regular file (not
/// a symlink), and type and length are checked on the opened handle, so the
/// returned bytes are exactly what was hashed or parsed. A file longer than
/// `max` is `Error::Limit`, or `Error::Invalid` when `exact` requires `max`.
pub(crate) fn read_regular(path: &Path, max: u64, exact: bool) -> Result<Vec<u8>> {
    let link = fs::symlink_metadata(path)?;
    if !link.file_type().is_file() {
        return Err(Error::Invalid);
    }
    let file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || !same_file(&link, &meta) || (exact && meta.len() != max) {
        return Err(Error::Invalid);
    }
    if meta.len() > max {
        return Err(Error::Limit);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    // A length that changed while reading is a concurrent modification.
    if bytes.len() as u64 != meta.len() {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}
#[cfg(unix)]
fn same_file(link: &fs::Metadata, handle: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    link.dev() == handle.dev() && link.ino() == handle.ino()
}
#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}
fn load_part(dir: &Path, part: &Part) -> Result<Bytes> {
    let bytes = read_regular(&dir.join(&part.name), part.bytes, true)?;
    if sha256_hex(&bytes) != part.sha256 {
        return Err(Error::Invalid);
    }
    Ok(Bytes::from(bytes))
}
/// Preflight Compact Thrift before upstream allocation. Containers, nesting,
/// total fields, field ids and byte lengths are bounded, including unknown
/// fields. The top-level schema is restricted to the root plus nine flat
/// fields. This is a framing guard; upstream still performs full validation.
struct Compact<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: usize,
    page: Option<Vec<(i16, i64)>>,
}
impl<'a> Compact<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            nodes: 0,
            page: None,
        }
    }
    fn byte(&mut self) -> Result<u8> {
        let n = *self.bytes.get(self.pos).ok_or(Error::Invalid)?;
        self.pos += 1;
        Ok(n)
    }
    fn varint(&mut self) -> Result<u64> {
        let mut n = 0;
        for shift in (0..70).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                return Err(Error::Invalid);
            }
            n |= ((b & 127) as u64) << shift;
            if b & 128 == 0 {
                return Ok(n);
            }
        }
        Err(Error::Invalid)
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.pos = self
            .pos
            .checked_add(n)
            .filter(|p| *p <= self.bytes.len())
            .ok_or(Error::Invalid)?;
        Ok(())
    }
    fn node(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > COMPACT_MAX_NODES {
            return Err(Error::Limit);
        }
        Ok(())
    }
    fn structure(&mut self, depth: usize, footer: bool) -> Result<Vec<(i16, i64)>> {
        if depth > COMPACT_MAX_DEPTH {
            return Err(Error::Limit);
        }
        let mut last = 0i16;
        let mut seen = 0u64;
        let mut integers = vec![];
        loop {
            let field = self.byte()?;
            if field == 0 {
                return Ok(integers);
            }
            last = if field >> 4 == 0 {
                let n = self.varint()?;
                i16::try_from(((n >> 1) as i64) ^ -((n & 1) as i64)).map_err(|_| Error::Invalid)?
            } else {
                last.checked_add((field >> 4) as i16)
                    .ok_or(Error::Invalid)?
            };
            if !(1..=COMPACT_MAX_FIELD_ID).contains(&last) || seen & (1 << last) != 0 {
                return Err(Error::Invalid);
            }
            seen |= 1 << last;
            let ty = field & 15;
            self.node()?;
            if matches!(ty, 4..=6) {
                let n = self.varint()?;
                integers.push((last, ((n >> 1) as i64) ^ -((n & 1) as i64)));
            } else if ty == 12 && depth == 0 && last == 5 && !footer {
                self.page = Some(self.structure(depth + 1, false)?);
            } else if ty == 9 && footer && depth == 0 && last == 2 {
                self.list(depth, FOOTER_MAX_SCHEMA_ELEMENTS)?;
            } else if ty == 9 && footer && depth == 0 && last == 4 {
                self.list(depth, MAX_GROUPS_PER_PART as u64)?;
            } else {
                self.value(ty, depth, false)?;
            }
        }
    }
    fn list(&mut self, depth: usize, max: u64) -> Result<()> {
        let h = self.byte()?;
        let count = if h >> 4 == 15 {
            self.varint()?
        } else {
            (h >> 4) as u64
        };
        if count > max {
            return Err(Error::Limit);
        }
        for _ in 0..count {
            self.node()?;
            self.value(h & 15, depth + 1, true)?;
        }
        Ok(())
    }
    fn value(&mut self, ty: u8, depth: usize, collection: bool) -> Result<()> {
        if depth > COMPACT_MAX_DEPTH {
            return Err(Error::Limit);
        }
        match ty {
            1 | 2 => {
                if collection {
                    self.byte()?;
                }
            }
            3 => self.skip(1)?,
            4..=6 => {
                self.varint()?;
            }
            7 => self.skip(8)?,
            8 => {
                let n = self.varint()?;
                if n > self.bytes.len() as u64 {
                    return Err(Error::Limit);
                }
                self.skip(n as usize)?;
            }
            9 | 10 => self.list(depth, COMPACT_MAX_LIST)?,
            11 => {
                let n = self.varint()?;
                if n > COMPACT_MAX_MAP {
                    return Err(Error::Limit);
                }
                if n != 0 {
                    let h = self.byte()?;
                    for _ in 0..n {
                        self.value(h >> 4, depth + 1, true)?;
                        self.value(h & 15, depth + 1, true)?;
                    }
                }
            }
            12 => {
                self.structure(depth + 1, false)?;
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}
/// Checks magic, footer length and the footer's Compact framing over the
/// exact part bytes. Returns the footer's start offset.
pub(crate) fn footer_guard(bytes: &[u8], limits: &Limits) -> Result<u64> {
    let len = bytes.len() as u64;
    if len < PARQUET_MIN_FILE_BYTES || len > limits.part_bytes {
        return Err(Error::Limit);
    }
    let tail = &bytes[bytes.len() - 8..];
    if &bytes[..4] != PARQUET_MAGIC || &tail[4..] != PARQUET_MAGIC {
        return Err(Error::Invalid);
    }
    let size = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as u64;
    if size > limits.footer_bytes as u64 || size > len - PARQUET_MIN_FILE_BYTES {
        return Err(Error::Limit);
    }
    let start = len - 8 - size;
    let footer = &bytes[start as usize..bytes.len() - 8];
    let mut parser = Compact::new(footer);
    parser.structure(0, true)?;
    if parser.pos != footer.len() {
        return Err(Error::Invalid);
    }
    Ok(start)
}
/// Footer and layout validation of one part's exact bytes, before any decode.
/// Every column-chunk offset and size is checked as a signed integer here, so
/// upstream helpers that assert on them (`ColumnChunkMetaData::byte_range`)
/// never see a negative value.
pub(crate) fn checked_metadata(
    bytes: &Bytes,
    part: &Part,
    header: &HistoryReadHeader,
    limits: &Limits,
) -> Result<ArrowReaderMetadata> {
    let footer_start = footer_guard(bytes, limits)?;
    let metadata = contain(|| {
        Ok(ArrowReaderMetadata::load(
            bytes,
            ArrowReaderOptions::new().with_skip_arrow_metadata(true),
        )?)
    })?;
    let file = metadata.metadata();
    if metadata.schema().fields() != schema(header).fields()
        || file.num_row_groups() != part.groups
        || file.file_metadata().num_rows() != part.rows as i64
    {
        return Err(Error::Invalid);
    }
    let group_ceiling = encoded_ceiling(limits.batch_bytes as u64);
    let mut next_offset = PARQUET_MAGIC.len() as u64;
    for group in file.row_groups() {
        if group.num_rows() <= 0
            || group.num_rows() > limits.batch_rows as i64
            || group.num_columns() != COLUMNS.len()
        {
            return Err(Error::Invalid);
        }
        let mut group_bytes = 0u64;
        for (col, (physical, optional)) in group.columns().iter().zip(COLUMNS) {
            // Reject a dictionary page before deriving the chunk start.
            if col.dictionary_page_offset().is_some()
                || col.file_path().is_some()
                || col.statistics().is_some()
                || col.bloom_filter_offset().is_some()
                || col.column_index_offset().is_some()
                || col.offset_index_offset().is_some()
            {
                return Err(Error::Invalid);
            }
            let offset = u64::try_from(col.data_page_offset()).map_err(|_| Error::Invalid)?;
            let len = u64::try_from(col.compressed_size()).map_err(|_| Error::Invalid)?;
            let end = offset.checked_add(len).ok_or(Error::Invalid)?;
            let descriptor = col.column_descr();
            if offset != next_offset
                || len == 0
                || end > footer_start
                || col.compression() != Compression::UNCOMPRESSED
                || col.num_values() != group.num_rows()
                || col.uncompressed_size() != col.compressed_size()
                || col.column_type() != physical
                || descriptor.max_rep_level() != 0
                || descriptor.max_def_level() != i16::from(optional)
                || col
                    .encodings()
                    .any(|e| !matches!(e, Encoding::PLAIN | Encoding::RLE))
            {
                return Err(Error::Invalid);
            }
            group_bytes = group_bytes.checked_add(len).ok_or(Error::Limit)?;
            if group_bytes > group_ceiling {
                return Err(Error::Limit);
            }
            page_guard(
                &bytes[offset as usize..end as usize],
                group.num_rows() as u64,
                (physical, optional),
                limits,
            )?;
            next_offset = end;
        }
    }
    // This profile emits no page indexes or unrelated chunks: the last
    // chunk ends exactly where the footer begins.
    if next_offset != footer_start {
        return Err(Error::Invalid);
    }
    Ok(metadata)
}
/// Validates every page of one column chunk before upstream decodes it: the
/// page header framing and sizes, then the page body itself. For an optional
/// column the body is a 4-byte length prefix and an RLE/bit-packed hybrid
/// definition-level run list (bit width one) that must parse, stay inside its
/// prefix and cover exactly `num_values`; then the PLAIN payload must be
/// exactly the size its physical type implies for the non-null count.
fn page_guard(chunk: &[u8], rows: u64, column: (Physical, bool), limits: &Limits) -> Result<()> {
    let ceiling = encoded_ceiling(limits.batch_bytes as u64);
    let mut pos = 0;
    let mut values = 0;
    let mut pages = 0;
    while pos < chunk.len() {
        pages += 1;
        if pages > limits.batch_rows {
            return Err(Error::Limit);
        }
        let rest = &chunk[pos..];
        let mut parser = Compact::new(&rest[..rest.len().min(MAX_PAGE_HEADER_BYTES)]);
        let fields = parser.structure(0, false)?;
        let get = |fields: &[(i16, i64)], id| {
            fields
                .iter()
                .find(|(k, _)| *k == id)
                .map(|(_, v)| *v)
                .ok_or(Error::Invalid)
        };
        let kind = get(&fields, 1)?;
        let raw = get(&fields, 2)?;
        let compressed = get(&fields, 3)?;
        if kind != PAGE_TYPE_DATA_PAGE || raw < 0 || raw != compressed {
            return Err(Error::Invalid);
        }
        let raw = raw as u64;
        if raw > ceiling {
            return Err(Error::Limit);
        }
        if raw > (rest.len() - parser.pos) as u64 {
            return Err(Error::Invalid);
        }
        let data = parser.page.as_deref().ok_or(Error::Invalid)?;
        let count = get(data, 1)?;
        if count <= 0
            || count as u64 > rows
            || get(data, 2)? != ENCODING_PLAIN
            || get(data, 3)? != ENCODING_RLE
            || get(data, 4)? != ENCODING_RLE
        {
            return Err(Error::Invalid);
        }
        let body = &rest[parser.pos..parser.pos + raw as usize];
        page_body(body, count as usize, column)?;
        values += count as u64;
        pos += parser.pos + raw as usize;
    }
    if values != rows {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn page_body(body: &[u8], count: usize, (physical, optional): (Physical, bool)) -> Result<()> {
    let (present, payload) = if optional {
        let prefix = body.get(..4).ok_or(Error::Invalid)?;
        let len = u32::from_le_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) as usize;
        let end = len.checked_add(4).ok_or(Error::Invalid)?;
        let levels = body.get(4..end).ok_or(Error::Invalid)?;
        (definition_levels(levels, count)?, &body[end..])
    } else {
        (count, body)
    };
    let width = match physical {
        Physical::BOOLEAN => {
            return (payload.len() == present.div_ceil(8))
                .then_some(())
                .ok_or(Error::Invalid);
        }
        Physical::INT32 => 4,
        Physical::INT64 | Physical::DOUBLE => 8,
        Physical::BYTE_ARRAY => {
            let mut pos = 0usize;
            for _ in 0..present {
                let prefix = payload.get(pos..pos + 4).ok_or(Error::Invalid)?;
                let len = u32::from_le_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]);
                pos = (pos + 4)
                    .checked_add(len as usize)
                    .filter(|end| *end <= payload.len())
                    .ok_or(Error::Invalid)?;
            }
            return (pos == payload.len()).then_some(()).ok_or(Error::Invalid);
        }
        _ => return Err(Error::Invalid),
    };
    (Some(payload.len()) == present.checked_mul(width))
        .then_some(())
        .ok_or(Error::Invalid)
}
/// Parses a bit-width-one RLE/bit-packed hybrid level run list that must
/// cover exactly `count` levels and consume exactly `levels`. Returns how many
/// levels are 1 (non-null). Only a final bit-packed run may pad, by fewer than
/// eight levels.
fn definition_levels(levels: &[u8], count: usize) -> Result<usize> {
    let mut parser = Compact::new(levels);
    let mut produced = 0usize;
    let mut present = 0usize;
    while produced < count {
        let header = parser.varint()?;
        let left = count - produced;
        let run = usize::try_from(header >> 1).map_err(|_| Error::Invalid)?;
        if run == 0 {
            return Err(Error::Invalid);
        }
        if header & 1 == 0 {
            let value = parser.byte()?;
            if value > 1 || run > left {
                return Err(Error::Invalid);
            }
            produced += run;
            present += run * value as usize;
        } else {
            // `run` groups of eight one-bit levels, one byte per group.
            let start = parser.pos;
            parser.skip(run)?;
            let taken = run.checked_mul(8).ok_or(Error::Invalid)?.min(left);
            if run * 8 - taken >= 8 {
                return Err(Error::Invalid);
            }
            let packed = &levels[start..parser.pos];
            present += (0..taken)
                .filter(|bit| packed[bit / 8] >> (bit % 8) & 1 == 1)
                .count();
            produced += taken;
        }
    }
    if parser.pos != levels.len() {
        return Err(Error::Invalid);
    }
    Ok(present)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn compact(bytes: &[u8], footer: bool) -> Result<Vec<(i16, i64)>> {
        Compact::new(bytes).structure(0, footer)
    }
    #[test]
    fn compact_preflight_rejects_allocation_and_recursion_bombs() {
        // Independent Compact Thrift bytes, not output from our writer:
        // field 2 list<struct> claiming u32::MAX schema entries.
        let huge_schema = [0x29, 0xfc, 0xff, 0xff, 0xff, 0xff, 0x0f];
        let huge_groups = [0x49, 0xfc, 0x80, 0x80, 0x80, 0x01];
        let mut deep = vec![0x1c; 15];
        deep.extend([0; 16]);
        let long_binary = [0x18, 0xff, 0xff, 0xff, 0xff, 0x0f];
        let huge_map = [0x1b, 0x81, 0x01, 0x55];
        for (name, bytes) in [
            ("schema", &huge_schema[..]),
            ("groups", &huge_groups[..]),
            ("deep", &deep),
            ("binary", &long_binary[..]),
            ("map", &huge_map[..]),
        ] {
            assert_eq!(compact(bytes, true).unwrap_err(), Error::Limit, "{name}");
        }
        for n in 0..huge_schema.len() {
            assert_eq!(
                compact(&huge_schema[..n], true).unwrap_err(),
                Error::Invalid,
                "{n}"
            );
        }
        // A repeated field id, id zero and an id above the ceiling are each
        // framing errors: preflight and upstream could disagree on them.
        for bytes in [
            &[0x15, 0, 0x05, 0x02, 0, 0][..],
            &[0x05, 0x00, 0, 0][..],
            &[0x05, 0x80, 0x01, 0, 0][..],
        ] {
            assert_eq!(compact(bytes, false).unwrap_err(), Error::Invalid);
        }
        // Node ceiling: a flat struct cannot carry more than the node budget.
        let mut wide = vec![0x19, 0xfc];
        let mut n = COMPACT_MAX_LIST as usize;
        while n > 0 {
            wide.push(0x80 | (n & 0x7f) as u8);
            n >>= 7;
        }
        *wide.last_mut().unwrap() &= 0x7f;
        assert_eq!(compact(&wide, false).unwrap_err(), Error::Invalid);
    }
    /// DATA_PAGE header: PLAIN values, RLE/RLE levels, `raw` payload bytes,
    /// `count` values, followed by `body`.
    fn page(raw: u8, count: u8, body: &[u8]) -> Vec<u8> {
        let mut page = vec![
            0x15,
            0,
            0x15,
            raw * 2,
            0x15,
            raw * 2,
            0x2c,
            0x15,
            count * 2,
            0x15,
            0,
            0x15,
            6,
            0x15,
            6,
            0,
            0,
        ];
        page.extend(body);
        page
    }
    const INT32: (Physical, bool) = (Physical::INT32, false);
    const OPTIONAL_INT32: (Physical, bool) = (Physical::INT32, true);
    #[test]
    fn page_preflight_rejects_size_count_and_encoding_before_decode() {
        // Independent bytes: one required INT32 value.
        let valid = page(4, 1, &[7, 0, 0, 0]);
        let limits = Limits::default();
        page_guard(&valid, 1, INT32, &limits).unwrap();
        for (index, value) in [(1, 4), (3, 2), (5, 2), (8, 126), (10, 10), (12, 0), (14, 0)] {
            let mut bad = valid.clone();
            bad[index] = value;
            assert_eq!(
                page_guard(&bad, 1, INT32, &limits).unwrap_err(),
                Error::Invalid,
                "index {index}"
            );
        }
        for n in 0..valid.len() {
            assert_eq!(
                page_guard(&valid[..n], 1, INT32, &limits).unwrap_err(),
                Error::Invalid,
                "{n}"
            );
        }
        // Wrong payload size for the physical type and count.
        assert_eq!(
            page_guard(&page(3, 1, &[7, 0, 0]), 1, INT32, &limits).unwrap_err(),
            Error::Invalid
        );
        // A page claiming more than the encoded ceiling is a limit, checked
        // before its body is examined.
        let mut huge = vec![
            0x15, 0, 0x16, 0xfe, 0xff, 0xff, 0x7f, 0x16, 0xfe, 0xff, 0xff, 0x7f,
        ];
        huge.extend([0x1c, 0x15, 2, 0x15, 0, 0x15, 6, 0x15, 6, 0, 0]);
        assert_eq!(
            page_guard(&huge, 1, INT32, &limits).unwrap_err(),
            Error::Limit
        );
    }
    #[test]
    fn page_preflight_validates_definition_levels_and_plain_payloads() {
        let limits = Limits::default();
        // Three levels as one RLE run of 1, then three INT32 values.
        let mut body = vec![2, 0, 0, 0, 6, 1];
        body.extend([0; 12]);
        page_guard(&page(18, 3, &body), 3, OPTIONAL_INT32, &limits).unwrap();
        // One bit-packed group 0b101 (two present, padded to eight).
        let mut packed = vec![2, 0, 0, 0, 3, 0b101];
        packed.extend([0; 8]);
        page_guard(&page(14, 3, &packed), 3, OPTIONAL_INT32, &limits).unwrap();
        let cases: [(&str, Vec<u8>); 9] = [
            ("prefix beyond body", vec![0xff, 0, 0, 0, 6, 1]),
            ("missing prefix", vec![2, 0]),
            ("rle run beyond count", vec![2, 0, 0, 0, 8, 1]),
            ("rle run of zero", vec![2, 0, 0, 0, 0, 1]),
            ("level above max", vec![2, 0, 0, 0, 6, 2]),
            ("packed groups beyond prefix", vec![2, 0, 0, 0, 0x7f, 0]),
            ("packed overshoot by a group", vec![3, 0, 0, 0, 5, 0, 0]),
            ("levels short of count", vec![2, 0, 0, 0, 4, 1]),
            ("trailing level bytes", vec![3, 0, 0, 0, 6, 1, 0]),
        ];
        for (name, mut levels) in cases {
            levels.extend([0; 12]);
            assert_eq!(
                page_body(&levels, 3, OPTIONAL_INT32).unwrap_err(),
                Error::Invalid,
                "{name}"
            );
        }
        // PLAIN payload sizes per physical type.
        assert!(page_body(&[0b1], 1, (Physical::BOOLEAN, false)).is_ok());
        assert!(page_body(&[0; 2], 9, (Physical::BOOLEAN, false)).is_ok());
        assert!(page_body(&[0; 8], 1, (Physical::DOUBLE, false)).is_ok());
        assert!(page_body(&[1, 0, 0, 0, b'x'], 1, (Physical::BYTE_ARRAY, false)).is_ok());
        for (name, body, count, column) in [
            ("bool short", &[][..], 1, (Physical::BOOLEAN, false)),
            ("bool long", &[0, 0][..], 8, (Physical::BOOLEAN, false)),
            ("double short", &[0; 7][..], 1, (Physical::DOUBLE, false)),
            ("int64 long", &[0; 9][..], 1, (Physical::INT64, false)),
            (
                "bytes overrun",
                &[2, 0, 0, 0, b'x'][..],
                1,
                (Physical::BYTE_ARRAY, false),
            ),
            (
                "bytes trailing",
                &[0, 0, 0, 0, b'x'][..],
                1,
                (Physical::BYTE_ARRAY, false),
            ),
            (
                "bytes huge",
                &[0xff, 0xff, 0xff, 0xff][..],
                1,
                (Physical::BYTE_ARRAY, false),
            ),
            ("foreign type", &[0; 4][..], 1, (Physical::FLOAT, false)),
        ] {
            assert_eq!(
                page_body(body, count, column).unwrap_err(),
                Error::Invalid,
                "{name}"
            );
        }
    }
    #[test]
    fn oversized_footer_is_rejected_before_reading_it() {
        let mut bytes = b"PAR1".to_vec();
        bytes.extend(u32::MAX.to_le_bytes());
        bytes.extend(b"PAR1");
        assert_eq!(
            footer_guard(&bytes, &Limits::default()).unwrap_err(),
            Error::Limit
        );
        assert_eq!(
            footer_guard(b"PAR1PAR1", &Limits::default()).unwrap_err(),
            Error::Limit
        );
        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert_eq!(
            footer_guard(&bad_magic, &Limits::default()).unwrap_err(),
            Error::Invalid
        );
    }
}
