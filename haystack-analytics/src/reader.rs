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
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Compact Thrift preflight: deepest struct/collection nesting. The typed
/// shapes below are shallower; this is an independent bound.
const COMPACT_MAX_DEPTH: usize = 12;
/// Compact Thrift preflight: total fields and list elements per message. The
/// typed shapes' count ranges already bound every admitted message well below
/// this (a 16-group footer is about 3,000 nodes); this is an independent bound.
const COMPACT_MAX_NODES: usize = 16_384;
/// Highest Thrift field id admitted; every Parquet footer/page struct is lower.
/// Ids are tracked per struct so a duplicate field (which a preflight and the
/// upstream decoder could resolve differently) is rejected.
const COMPACT_MAX_FIELD_ID: i16 = 63;
/// Elements of a column's `encodings` and `encoding_stats` lists. Upstream
/// shifts by each encoding value; this profile writes two (PLAIN, RLE).
const MAX_ENCODINGS: u64 = 8;
/// Entries of the footer `key_value_metadata` list (the profile writes one,
/// the Arrow schema).
const MAX_KEY_VALUES: u64 = 8;
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
///
/// On Unix the open itself refuses a symlink (`O_NOFOLLOW`) and cannot block
/// on a FIFO or device swapped in after the `lstat` (`O_NONBLOCK`); the
/// handle's own `fstat` must then be a regular file. Elsewhere the `lstat`
/// and the handle check remain, with the swap window between them.
pub(crate) fn read_regular(path: &Path, max: u64, exact: bool) -> Result<Vec<u8>> {
    let link = fs::symlink_metadata(path)?;
    if !link.file_type().is_file() {
        return Err(Error::Invalid);
    }
    let file = open_regular(path)?;
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
/// Opens `path` for reading without following a final symlink or blocking
/// on a FIFO (Unix), and requires the opened handle to be a regular file.
fn open_regular(path: &Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Err(Error::Invalid),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(Error::Invalid);
    }
    Ok(file)
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
/// Compact Thrift wire shape of a field or list element, as the upstream
/// decoder reads it.
///
/// Upstream (`parquet` 59.3) picks a decoder by field id alone and ignores
/// the declared wire type: a field 4 declared `binary` is still read as a
/// `list<RowGroup>` header, and an `i64`-typed field read as `i32` is
/// truncated. A generic skip-by-wire-type preflight would therefore bound a
/// different structure from the one upstream allocates for. The preflight
/// instead walks the exact typed shapes of this profile, requires every
/// field's declared wire type to match, range-checks narrow integers, and
/// rejects any field id outside the shape, so upstream sees only structures
/// whose every count and length was bounded here.
#[derive(Clone, Copy)]
enum Wire {
    /// Struct field only: the value is the wire type (1 true, 2 false).
    Bool,
    Byte,
    I16,
    I32,
    I64,
    Binary,
    /// Element shape and inclusive element-count range.
    List(&'static Wire, u64, u64),
    Struct(&'static [(i16, Wire)]),
}
impl Wire {
    fn code(self) -> u8 {
        match self {
            Wire::Bool => 1,
            Wire::Byte => 3,
            Wire::I16 => 4,
            Wire::I32 => 5,
            Wire::I64 => 6,
            Wire::Binary => 8,
            Wire::List(..) => 9,
            Wire::Struct(_) => 12,
        }
    }
}
type Shape = &'static [(i16, Wire)];
const N_COLUMNS: u64 = COLUMNS.len() as u64;
const EMPTY: Shape = &[];
/// `LogicalType` union: STRING (1) or INTEGER (10).
const LOGICAL_TYPE: Shape = &[
    (1, Wire::Struct(EMPTY)),
    (10, Wire::Struct(&[(1, Wire::Byte), (2, Wire::Bool)])),
];
/// `SchemaElement`: type, repetition_type, name, num_children,
/// converted_type, logicalType. `type_length`, `scale`, `precision` and
/// `field_id` are not part of the profile.
const SCHEMA_ELEMENT: Shape = &[
    (1, Wire::I32),
    (3, Wire::I32),
    (4, Wire::Binary),
    (5, Wire::I32),
    (6, Wire::I32),
    (10, Wire::Struct(LOGICAL_TYPE)),
];
/// `PageEncodingStats`: page_type, encoding, count.
const PAGE_ENCODING_STATS: Shape = &[(1, Wire::I32), (2, Wire::I32), (3, Wire::I32)];
/// `ColumnMetaData` as written: type, encodings, path_in_schema, codec,
/// num_values, total sizes, data_page_offset, encoding_stats. Statistics,
/// dictionary, index, bloom and size-statistics fields are rejected.
const COLUMN_META_DATA: Shape = &[
    (1, Wire::I32),
    (2, Wire::List(&Wire::I32, 1, MAX_ENCODINGS)),
    (3, Wire::List(&Wire::Binary, 1, 1)),
    (4, Wire::I32),
    (5, Wire::I64),
    (6, Wire::I64),
    (7, Wire::I64),
    (9, Wire::I64),
    (
        13,
        Wire::List(&Wire::Struct(PAGE_ENCODING_STATS), 0, MAX_ENCODINGS),
    ),
];
/// `ColumnChunk`: file_offset and meta_data only (no external file path,
/// page indexes or encryption).
const COLUMN_CHUNK: Shape = &[(2, Wire::I64), (3, Wire::Struct(COLUMN_META_DATA))];
/// `RowGroup`: exactly one chunk per column, sizes, rows, offsets, ordinal.
const ROW_GROUP: Shape = &[
    (
        1,
        Wire::List(&Wire::Struct(COLUMN_CHUNK), N_COLUMNS, N_COLUMNS),
    ),
    (2, Wire::I64),
    (3, Wire::I64),
    (5, Wire::I64),
    (6, Wire::I64),
    (7, Wire::I16),
];
const KEY_VALUE: Shape = &[(1, Wire::Binary), (2, Wire::Binary)];
/// `ColumnOrder` union: TYPE_ORDER (1).
const COLUMN_ORDER: Shape = &[(1, Wire::Struct(EMPTY))];
/// `FileMetaData`: exactly the root plus one element per column, at most
/// `MAX_GROUPS_PER_PART` row groups, one column order per column.
const FILE_META_DATA: Shape = &[
    (1, Wire::I32),
    (
        2,
        Wire::List(&Wire::Struct(SCHEMA_ELEMENT), N_COLUMNS + 1, N_COLUMNS + 1),
    ),
    (3, Wire::I64),
    (
        4,
        Wire::List(&Wire::Struct(ROW_GROUP), 1, MAX_GROUPS_PER_PART as u64),
    ),
    (5, Wire::List(&Wire::Struct(KEY_VALUE), 0, MAX_KEY_VALUES)),
    (6, Wire::Binary),
    (
        7,
        Wire::List(&Wire::Struct(COLUMN_ORDER), N_COLUMNS, N_COLUMNS),
    ),
];
/// `DataPageHeader`: num_values and the three encodings (no statistics).
const DATA_PAGE_HEADER: Shape = &[
    (1, Wire::I32),
    (2, Wire::I32),
    (3, Wire::I32),
    (4, Wire::I32),
];
/// `PageHeader`: type, sizes and a v1 data page header (no CRC, no other
/// page kinds).
const PAGE_HEADER: Shape = &[
    (1, Wire::I32),
    (2, Wire::I32),
    (3, Wire::I32),
    (5, Wire::Struct(DATA_PAGE_HEADER)),
];
/// A decoded preflight value: integers are kept for the profile checks,
/// binaries are only bounded and skipped.
#[cfg_attr(test, derive(Debug))]
enum Node {
    Int(i64),
    Bytes,
    List(Vec<Node>),
    Struct(Vec<(i16, Node)>),
}
fn field(fields: &[(i16, Node)], id: i16) -> Option<&Node> {
    fields.iter().find(|(k, _)| *k == id).map(|(_, v)| v)
}
fn int(fields: &[(i16, Node)], id: i16) -> Option<i64> {
    match field(fields, id)? {
        Node::Int(v) => Some(*v),
        _ => None,
    }
}
fn list(fields: &[(i16, Node)], id: i16) -> &[Node] {
    match field(fields, id) {
        Some(Node::List(items)) => items,
        _ => &[],
    }
}
fn structure_of(node: &Node) -> Result<&[(i16, Node)]> {
    match node {
        Node::Struct(fields) => Ok(fields),
        _ => Err(Error::Invalid),
    }
}
/// Typed Compact Thrift preflight, run before upstream decodes (and so before
/// it allocates for) a footer or page header. See [`Wire`].
struct Compact<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: usize,
}
impl<'a> Compact<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            nodes: 0,
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
    fn zigzag(&mut self) -> Result<i64> {
        let n = self.varint()?;
        Ok(((n >> 1) as i64) ^ -((n & 1) as i64))
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
    fn structure(&mut self, shape: Shape, depth: usize) -> Result<Vec<(i16, Node)>> {
        if depth > COMPACT_MAX_DEPTH {
            return Err(Error::Limit);
        }
        let mut last = 0i16;
        let mut seen = 0u64;
        let mut fields = vec![];
        loop {
            let header = self.byte()?;
            if header == 0 {
                return Ok(fields);
            }
            last = if header >> 4 == 0 {
                i16::try_from(self.zigzag()?).map_err(|_| Error::Invalid)?
            } else {
                last.checked_add((header >> 4) as i16)
                    .ok_or(Error::Invalid)?
            };
            if !(1..=COMPACT_MAX_FIELD_ID).contains(&last) || seen & (1 << last) != 0 {
                return Err(Error::Invalid);
            }
            seen |= 1 << last;
            self.node()?;
            let wire = shape
                .iter()
                .find(|(id, _)| *id == last)
                .map(|(_, wire)| *wire)
                .ok_or(Error::Invalid)?;
            let ty = header & 15;
            let value = match wire {
                Wire::Bool if matches!(ty, 1 | 2) => Node::Int(i64::from(ty == 1)),
                Wire::Bool => return Err(Error::Invalid),
                _ if ty != wire.code() => return Err(Error::Invalid),
                _ => self.value(wire, depth)?,
            };
            fields.push((last, value));
        }
    }
    fn value(&mut self, wire: Wire, depth: usize) -> Result<Node> {
        if depth > COMPACT_MAX_DEPTH {
            return Err(Error::Limit);
        }
        Ok(match wire {
            // A bool list element has its own byte encoding; the profile has
            // none, so only struct-field bools (handled by the caller) exist.
            Wire::Bool => return Err(Error::Invalid),
            Wire::Byte => Node::Int(i64::from(self.byte()? as i8)),
            Wire::I16 => Node::Int(
                i16::try_from(self.zigzag()?)
                    .map_err(|_| Error::Invalid)?
                    .into(),
            ),
            Wire::I32 => Node::Int(
                i32::try_from(self.zigzag()?)
                    .map_err(|_| Error::Invalid)?
                    .into(),
            ),
            Wire::I64 => Node::Int(self.zigzag()?),
            Wire::Binary => {
                let n = self.varint()?;
                if n > self.bytes.len() as u64 {
                    return Err(Error::Limit);
                }
                self.skip(n as usize)?;
                Node::Bytes
            }
            Wire::List(element, min, max) => {
                let h = self.byte()?;
                // Upstream reads a zero header byte as an empty list of any
                // element type.
                let count = match h >> 4 {
                    _ if h == 0 => 0,
                    15 => self.varint()?,
                    n => n as u64,
                };
                if count > max {
                    return Err(Error::Limit);
                }
                if count < min || (h != 0 && h & 15 != element.code()) {
                    return Err(Error::Invalid);
                }
                let mut items = vec![];
                for _ in 0..count {
                    self.node()?;
                    items.push(self.value(*element, depth + 1)?);
                }
                Node::List(items)
            }
            Wire::Struct(shape) => Node::Struct(self.structure(shape, depth + 1)?),
        })
    }
}
/// Parquet `Type` enum value of a physical type in this profile.
fn physical_code(physical: Physical) -> i64 {
    match physical {
        Physical::BOOLEAN => 0,
        Physical::INT32 => 1,
        Physical::INT64 => 2,
        Physical::DOUBLE => 5,
        Physical::BYTE_ARRAY => 6,
        _ => -1,
    }
}
/// Profile checks on the preflighted footer, for the integers upstream turns
/// into structure: the schema tree's `num_children` (upstream reserves that
/// many children before reading them), each leaf's physical type and
/// repetition, and each chunk's encodings (upstream shifts by each value).
fn footer_profile(footer: &[(i16, Node)]) -> Result<()> {
    for (i, element) in list(footer, 2).iter().enumerate() {
        let element = structure_of(element)?;
        let children = int(element, 5);
        let ok = if i == 0 {
            children == Some(N_COLUMNS as i64)
                && int(element, 1).is_none()
                && int(element, 3).is_none()
        } else {
            let (physical, optional) = COLUMNS[i - 1];
            matches!(children, None | Some(0))
                && int(element, 1) == Some(physical_code(physical))
                && int(element, 3) == Some(i64::from(optional))
        };
        if !ok {
            return Err(Error::Invalid);
        }
    }
    let plain_or_rle = |v: Option<i64>| matches!(v, Some(ENCODING_PLAIN | ENCODING_RLE));
    for group in list(footer, 4) {
        for chunk in list(structure_of(group)?, 1) {
            let Some(meta) = field(structure_of(chunk)?, 3) else {
                return Err(Error::Invalid);
            };
            let meta = structure_of(meta)?;
            for encoding in list(meta, 2) {
                if !matches!(encoding, Node::Int(ENCODING_PLAIN | ENCODING_RLE)) {
                    return Err(Error::Invalid);
                }
            }
            for stats in list(meta, 13) {
                let stats = structure_of(stats)?;
                if int(stats, 1) != Some(PAGE_TYPE_DATA_PAGE) || !plain_or_rle(int(stats, 2)) {
                    return Err(Error::Invalid);
                }
            }
        }
    }
    Ok(())
}
/// Checks magic, footer length and the footer's typed Compact framing and
/// profile over the exact part bytes. Returns the footer's start offset.
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
    let fields = parser.structure(FILE_META_DATA, 0)?;
    if parser.pos != footer.len() {
        return Err(Error::Invalid);
    }
    footer_profile(&fields)?;
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
        let fields = parser.structure(PAGE_HEADER, 0)?;
        let get = |fields: &[(i16, Node)], id| int(fields, id).ok_or(Error::Invalid);
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
        let data = structure_of(field(&fields, 5).ok_or(Error::Invalid)?)?;
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
    /// Independent Compact Thrift encoder for hand-built inputs: it shares no
    /// code with the preflight or with the Parquet writer.
    #[derive(Default)]
    struct Thrift(Vec<u8>);
    impl Thrift {
        fn uvar(&mut self, mut n: u64) -> &mut Self {
            while n >= 0x80 {
                self.0.push(n as u8 | 0x80);
                n >>= 7;
            }
            self.0.push(n as u8);
            self
        }
        fn int(&mut self, v: i64) -> &mut Self {
            self.uvar(((v << 1) ^ (v >> 63)) as u64)
        }
        /// Short-form field header: id delta and wire type.
        fn field(&mut self, delta: u8, ty: u8) -> &mut Self {
            self.0.push(delta << 4 | ty);
            self
        }
        /// Long-form list header: element type and count.
        fn list(&mut self, ty: u8, count: u64) -> &mut Self {
            self.0.push(0xf0 | ty);
            self.uvar(count)
        }
        fn bin(&mut self, bytes: &[u8]) -> &mut Self {
            self.uvar(bytes.len() as u64);
            self.0.extend(bytes);
            self
        }
        fn stop(&mut self) -> &mut Self {
            self.0.push(0);
            self
        }
        fn done(&mut self) -> Vec<u8> {
            std::mem::take(&mut self.0)
        }
    }
    fn footer(bytes: &[u8]) -> Result<()> {
        let mut parser = Compact::new(bytes);
        let fields = parser.structure(FILE_META_DATA, 0)?;
        footer_profile(&fields)
    }
    /// Footer whose only field is the schema list: a root with
    /// `root_children`, then the nine profile leaves, the first of which
    /// carries `leaf_children`.
    fn schema_footer(root_children: i64, leaf_children: Option<i64>) -> Vec<u8> {
        let mut t = Thrift::default();
        t.field(2, 9).list(12, N_COLUMNS + 1);
        t.field(4, 8)
            .bin(b"root")
            .field(1, 5)
            .int(root_children)
            .stop();
        for (i, (physical, optional)) in COLUMNS.into_iter().enumerate() {
            t.field(1, 5).int(physical_code(physical));
            t.field(2, 5).int(i64::from(optional));
            t.field(1, 8).bin(b"c");
            match leaf_children {
                Some(n) if i == 0 => t.field(1, 5).int(n),
                _ => &mut t,
            };
            t.stop();
        }
        t.stop();
        t.0
    }
    #[test]
    fn schema_num_children_is_bounded_before_upstream_reserves_for_it() {
        // Upstream `parquet_schema_from_array` reserves `num_children`
        // entries before reading any child; 0x7fffffff requested 16 GiB.
        footer(&schema_footer(9, None)).unwrap();
        footer(&schema_footer(9, Some(0))).unwrap();
        for (root, leaf) in [
            (0x7fff_ffff, None),
            (i32::MIN.into(), None),
            (-1, None),
            (0, None),
            (8, None),
            (10, None),
            (9, Some(0x7fff_ffff)),
            (9, Some(1)),
            (9, Some(-1)),
        ] {
            assert_eq!(
                footer(&schema_footer(root, leaf)).unwrap_err(),
                Error::Invalid,
                "root {root} leaf {leaf:?}"
            );
        }
        // Outside i32: upstream would truncate, so the preflight rejects it.
        assert_eq!(
            footer(&schema_footer(9 + (1 << 32), None)).unwrap_err(),
            Error::Invalid
        );
    }
    #[test]
    fn every_upstream_capacity_count_is_bounded_by_the_typed_preflight() {
        const HUGE: u64 = 0x7fff_ffff;
        let limit = |bytes: &[u8], name: &str| {
            assert_eq!(footer(bytes).unwrap_err(), Error::Limit, "{name}");
        };
        let invalid = |bytes: &[u8], name: &str| {
            assert_eq!(footer(bytes).unwrap_err(), Error::Invalid, "{name}");
        };
        // FileMetaData lists: schema (2), row_groups (4), key_value (5),
        // column_orders (7).
        limit(
            &Thrift::default().field(2, 9).list(12, HUGE).done(),
            "schema",
        );
        invalid(
            &Thrift::default().field(2, 9).list(12, 9).done(),
            "short schema",
        );
        limit(
            &Thrift::default().field(4, 9).list(12, HUGE).done(),
            "groups",
        );
        limit(
            &Thrift::default().field(4, 9).list(12, 17).done(),
            "groups+1",
        );
        limit(
            &Thrift::default().field(5, 9).list(12, HUGE).done(),
            "key_value",
        );
        limit(
            &Thrift::default().field(7, 9).list(12, HUGE).done(),
            "orders",
        );
        invalid(
            &Thrift::default().field(7, 9).list(12, 8).done(),
            "orders-1",
        );
        // created_by longer than the footer.
        limit(
            &Thrift::default().field(6, 8).uvar(HUGE).done(),
            "created_by",
        );
        // RowGroup.columns, then ColumnMetaData lists inside the first chunk.
        let group = |t: &mut Thrift| {
            t.field(4, 9).list(12, 1);
        };
        let mut t = Thrift::default();
        group(&mut t);
        limit(&t.field(1, 9).list(12, HUGE).done(), "columns");
        let mut t = Thrift::default();
        group(&mut t);
        limit(&t.field(1, 9).list(12, 10).done(), "columns+1");
        let chunk = |t: &mut Thrift| {
            group(t);
            t.field(1, 9).list(12, 9).field(3, 12);
        };
        let mut t = Thrift::default();
        chunk(&mut t);
        limit(&t.field(2, 9).list(5, HUGE).done(), "encodings");
        let mut t = Thrift::default();
        chunk(&mut t);
        limit(&t.field(3, 9).list(8, HUGE).done(), "path_in_schema");
        let mut t = Thrift::default();
        chunk(&mut t);
        invalid(&t.field(3, 9).list(8, 0).done(), "empty path_in_schema");
        let mut t = Thrift::default();
        chunk(&mut t);
        limit(&t.field(13, 9).list(12, HUGE).done(), "encoding_stats");
        // Fields upstream decodes by id but the profile never writes:
        // sorting_columns, statistics, size statistics, file_path, crc.
        let mut t = Thrift::default();
        group(&mut t);
        invalid(&t.field(4, 9).list(12, 0).done(), "sorting_columns");
        let mut t = Thrift::default();
        chunk(&mut t);
        invalid(&t.field(12, 12).stop().done(), "statistics");
        let mut t = Thrift::default();
        chunk(&mut t);
        invalid(&t.field(15, 12).stop().done(), "size_statistics");
        let mut t = Thrift::default();
        group(&mut t);
        invalid(
            &t.field(1, 9).list(12, 9).field(1, 8).bin(b"x").done(),
            "file_path",
        );
        assert_eq!(
            Compact::new(&[0x45, 0, 0])
                .structure(PAGE_HEADER, 0)
                .unwrap_err(),
            Error::Invalid,
            "crc"
        );
    }
    #[test]
    fn declared_wire_type_must_match_what_upstream_decodes() {
        // Upstream reads field 4 as list<RowGroup> whatever its declared type.
        // Declared binary, its bytes are a list header claiming i32::MAX
        // groups: a skip-by-type preflight would pass it to upstream.
        let mut inner = Thrift::default();
        inner.list(12, 0x7fff_ffff);
        let mut t = Thrift::default();
        t.field(4, 8).bin(&inner.0).stop();
        assert_eq!(footer(&t.0).unwrap_err(), Error::Invalid);
        // A bool-typed field has no payload, so upstream would read the next
        // bytes as num_children; an i64-typed one is truncated to i32.
        for ty in [1, 2, 3, 6, 8, 9, 12] {
            let mut t = Thrift::default();
            t.field(2, 9).list(12, 1).field(5, ty).int(9).stop().stop();
            assert_eq!(footer(&t.0).unwrap_err(), Error::Invalid, "type {ty}");
        }
        // List element type, set instead of list, and maps.
        for bytes in [
            Thrift::default().field(2, 9).list(5, 10).done(),
            Thrift::default().field(2, 10).list(12, 10).done(),
            Thrift::default().field(5, 11).uvar(0).done(),
        ] {
            assert_eq!(footer(&bytes).unwrap_err(), Error::Invalid);
        }
        // Unknown ids are rejected rather than skipped: upstream skips a
        // list<bool> without consuming its element bytes.
        let mut t = Thrift::default();
        t.field(8, 9).list(1, 3).0.extend([1, 1, 1]);
        assert_eq!(footer(&t.0).unwrap_err(), Error::Invalid);
    }
    #[test]
    fn compact_preflight_rejects_allocation_and_recursion_bombs() {
        // Independent Compact Thrift bytes, not output from our writer:
        // field 2 list<struct> claiming u32::MAX schema entries.
        let huge_schema = [0x29, 0xfc, 0xff, 0xff, 0xff, 0xff, 0x0f];
        let huge_groups = [0x49, 0xfc, 0x80, 0x80, 0x80, 0x01];
        let long_binary = [0x68, 0xff, 0xff, 0xff, 0xff, 0x0f];
        for (name, bytes) in [
            ("schema", &huge_schema[..]),
            ("groups", &huge_groups[..]),
            ("binary", &long_binary[..]),
        ] {
            assert_eq!(footer(bytes).unwrap_err(), Error::Limit, "{name}");
        }
        for n in 0..huge_schema.len() {
            assert_eq!(
                footer(&huge_schema[..n]).unwrap_err(),
                Error::Invalid,
                "{n}"
            );
        }
        // The typed shapes are at most seven deep; the depth ceiling is an
        // independent bound checked on entry.
        assert_eq!(
            Compact::new(&[0])
                .structure(EMPTY, COMPACT_MAX_DEPTH + 1)
                .unwrap_err(),
            Error::Limit
        );
        Compact::new(&[0])
            .structure(EMPTY, COMPACT_MAX_DEPTH)
            .unwrap();
        // A repeated field id, id zero and an id above the ceiling are each
        // framing errors: preflight and upstream could disagree on them.
        for bytes in [
            &[0x15, 0, 0x05, 0x02, 0, 0][..],
            &[0x05, 0x00, 0, 0][..],
            &[0x05, 0x80, 0x01, 0, 0][..],
        ] {
            assert_eq!(
                Compact::new(bytes).structure(PAGE_HEADER, 0).unwrap_err(),
                Error::Invalid
            );
        }
    }
    #[test]
    fn node_ceiling_bounds_a_well_formed_message() {
        // A test-only shape whose list admits any count, so only the node
        // ceiling can stop it. Every element is present and well formed: the
        // over-budget message fails on the ceiling, not on end of input.
        const WIDE: Shape = &[(1, Wire::List(&Wire::I32, 0, u64::MAX))];
        let message = |elements: usize| {
            let mut t = Thrift::default();
            t.field(1, 9).list(5, elements as u64);
            t.0.extend(std::iter::repeat_n(0, elements));
            t.stop();
            t.0
        };
        // One node for the field plus one per element.
        let fits = message(COMPACT_MAX_NODES - 1);
        let mut parser = Compact::new(&fits);
        parser.structure(WIDE, 0).unwrap();
        assert_eq!(parser.pos, fits.len());
        let over = message(COMPACT_MAX_NODES);
        assert_eq!(
            Compact::new(&over).structure(WIDE, 0).unwrap_err(),
            Error::Limit
        );
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
            0x15, 0, 0x15, 0xfe, 0xff, 0xff, 0xff, 0x0f, 0x15, 0xfe, 0xff, 0xff, 0xff, 0x0f,
        ];
        huge.extend([0x2c, 0x15, 2, 0x15, 0, 0x15, 6, 0x15, 6, 0, 0]);
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
    #[cfg(unix)]
    #[test]
    fn open_refuses_symlinks_and_fifos_swapped_in_after_lstat() {
        // `open_regular` is the step after the `lstat`; calling it directly
        // stands in for a path swapped between the two.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        open_regular(&file).unwrap();
        // A blocking open of a FIFO with no writer would hang here.
        for path in [&link, &fifo] {
            assert_eq!(open_regular(path).unwrap_err(), Error::Invalid, "{path:?}");
            assert_eq!(
                read_regular(path, 16, false).unwrap_err(),
                Error::Invalid,
                "{path:?}"
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
