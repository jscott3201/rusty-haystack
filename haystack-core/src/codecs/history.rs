//! Single-point bounded history contract shared by embedded and HTTP callers.
//! Completeness covers retained records in a half-open range, never records
//! already evicted by retention. Entity and history states are observations,
//! not an atomic cross-store snapshot.
use crate::{
    graph::GraphState,
    kinds::{HDateTime, Kind},
};

pub const PROFILE: &str = "bounded-history-v1";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryReadRequest {
    pub id: String,
    pub range: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    Bool,
    Number,
    Str,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistorySchema {
    pub kind: HistoryKind,
    pub unit: Option<String>,
    /// Exact admitted Haystack city spelling; not an IANA path.
    pub timezone: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryCapabilities {
    pub live_generation_checked: bool,
    pub snapshots: bool,
    pub demand_driven: bool,
    pub cooperative_cancellation: bool,
    pub native_values: bool,
}
impl HistoryCapabilities {
    pub const BOUNDED_LIVE: Self = Self {
        live_generation_checked: true,
        snapshots: false,
        demand_driven: true,
        cooperative_cancellation: true,
        native_values: false,
    };
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryState {
    /// Provider dataset/authority, independent of the entity graph authority.
    pub authority: [u8; 16],
    pub incarnation: [u8; 16],
    pub generation: u64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryCoverage {
    pub retained_start: Option<HDateTime>,
    pub retained_end: Option<HDateTime>,
    pub retained_count: u64,
    /// Largest timestamp evicted by retention. Reads do not reconstruct it.
    pub evicted_through: Option<HDateTime>,
}
/// Selected finite application/collector bounds. Byte and work counts use a
/// conservative source-size accounting, not compression or payload length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryBounds {
    pub batch_rows: u64,
    pub batch_bytes: u64,
    pub total_rows: u64,
    pub total_bytes: u64,
    pub total_work: u64,
    pub response_bytes: u64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryMetadata {
    pub id: String,
    pub requested_range: String,
    pub evaluated_at: HDateTime,
    pub start: HDateTime,
    pub end: HDateTime,
    pub schema: HistorySchema,
    pub capabilities: HistoryCapabilities,
    pub bounds: HistoryBounds,
    pub graph: GraphState,
    /// Opaque authorized policy observation; no policy scope key is exposed.
    pub policy_observation: [u8; 16],
    pub history: HistoryState,
    pub coverage: HistoryCoverage,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryReason {
    Rows,
    Bytes,
    Work,
    Deadline,
    Cancelled,
    OwnerStopped,
    GenerationChanged,
    MetadataChanged,
    PolicyChanged,
    UnsupportedValue,
    Provider,
    InvalidProvider,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryTerminal {
    Complete,
    Limited(HistoryReason),
    Interrupted(HistoryReason),
    Failed(HistoryReason),
}
#[derive(Debug, Clone, PartialEq)]
pub struct HistorySample {
    pub ts: HDateTime,
    pub val: Kind,
}
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryBatch {
    pub samples: Vec<HistorySample>,
    /// None means another pull is required; an empty nonterminal batch is invalid.
    pub terminal: Option<HistoryTerminal>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryReadResult {
    pub metadata: HistoryMetadata,
    pub samples: Vec<HistorySample>,
    pub terminal: HistoryTerminal,
}

use super::{
    Codec,
    typed::{self, TypedPayloadError},
};
use crate::data::{HCol, HDict, HGrid};
use crate::kinds::{HRef, unit_for};
pub const MAX_REQUEST_BYTES: usize = 32 * 1024;
pub const MAX_GRID_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CONTROL_BYTES: usize = 16 * 1024;
pub const MAX_ROWS: usize = 10_000;
const CONTROL: &str = "history";
fn invalid() -> TypedPayloadError {
    TypedPayloadError::Invalid("invalid bounded-history-v1 envelope".into())
}
fn dict(values: impl IntoIterator<Item = (&'static str, Kind)>) -> HDict {
    let mut result = HDict::new();
    for (name, value) in values {
        result.set(name, value);
    }
    result
}
fn text(value: impl Into<String>) -> Kind {
    Kind::Str(value.into())
}
fn uint(value: u64) -> Kind {
    text(value.to_string())
}
fn hex(id: &[u8; 16]) -> Kind {
    text(
        id.iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
    )
}
fn string<'a>(dict: &'a HDict, name: &str) -> Result<&'a str, TypedPayloadError> {
    match dict.get(name) {
        Some(Kind::Str(value)) => Ok(value),
        _ => Err(invalid()),
    }
}
fn number(dict: &HDict, name: &str) -> Result<u64, TypedPayloadError> {
    let string = string(dict, name)?;
    let value = string.parse::<u64>().map_err(|_| invalid())?;
    if value.to_string() == string {
        Ok(value)
    } else {
        Err(invalid())
    }
}
fn identity(dict: &HDict, name: &str) -> Result<[u8; 16], TypedPayloadError> {
    let string = string(dict, name)?;
    if string.len() != 32
        || !string
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid());
    }
    let mut id = [0; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&string[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(id)
}
fn fields(dict: &HDict, names: &[&str]) -> Result<(), TypedPayloadError> {
    if dict.len() == names.len() && names.iter().all(|name| dict.has(name)) {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn date(dict: &HDict, name: &str) -> Result<HDateTime, TypedPayloadError> {
    match dict.get(name) {
        Some(Kind::DateTime(value)) => Ok(value.clone()),
        _ => Err(invalid()),
    }
}
fn optional_date(dict: &HDict, name: &str) -> Result<Option<HDateTime>, TypedPayloadError> {
    match dict.get(name) {
        Some(Kind::Null) => Ok(None),
        Some(Kind::DateTime(value)) => Ok(Some(value.clone())),
        _ => Err(invalid()),
    }
}
fn date_kind(value: &Option<HDateTime>) -> Kind {
    value
        .as_ref()
        .map_or(Kind::Null, |value| Kind::DateTime(value.clone()))
}
fn control(dict: HDict) -> Result<Kind, TypedPayloadError> {
    let bytes = typed::encode(&Kind::Dict(Box::new(dict)))?;
    if bytes.len() > MAX_CONTROL_BYTES {
        return Err(invalid());
    }
    Ok(text(String::from_utf8(bytes).map_err(|_| invalid())?))
}
fn parse_control(grid: &HGrid) -> Result<HDict, TypedPayloadError> {
    let source = string(&grid.meta, CONTROL)?;
    let value = typed::decode_with_limits(
        source.as_bytes(),
        typed::PayloadLimits {
            max_bytes: MAX_CONTROL_BYTES,
            max_depth: 12,
            max_nodes: 512,
        },
    )?;
    match value {
        Kind::Dict(dict) => Ok(*dict),
        _ => Err(invalid()),
    }
}
fn kind_name(kind: HistoryKind) -> &'static str {
    match kind {
        HistoryKind::Bool => "Bool",
        HistoryKind::Number => "Number",
        HistoryKind::Str => "Str",
    }
}
fn reason_name(reason: HistoryReason) -> &'static str {
    match reason {
        HistoryReason::Rows => "rows",
        HistoryReason::Bytes => "bytes",
        HistoryReason::Work => "work",
        HistoryReason::Deadline => "deadline",
        HistoryReason::Cancelled => "cancelled",
        HistoryReason::OwnerStopped => "ownerStopped",
        HistoryReason::GenerationChanged => "generationChanged",
        HistoryReason::MetadataChanged => "metadataChanged",
        HistoryReason::PolicyChanged => "policyChanged",
        HistoryReason::UnsupportedValue => "unsupportedValue",
        HistoryReason::Provider => "provider",
        HistoryReason::InvalidProvider => "invalidProvider",
    }
}
fn parse_reason(value: &str) -> Result<HistoryReason, TypedPayloadError> {
    match value {
        "rows" => Ok(HistoryReason::Rows),
        "bytes" => Ok(HistoryReason::Bytes),
        "work" => Ok(HistoryReason::Work),
        "deadline" => Ok(HistoryReason::Deadline),
        "cancelled" => Ok(HistoryReason::Cancelled),
        "ownerStopped" => Ok(HistoryReason::OwnerStopped),
        "generationChanged" => Ok(HistoryReason::GenerationChanged),
        "metadataChanged" => Ok(HistoryReason::MetadataChanged),
        "policyChanged" => Ok(HistoryReason::PolicyChanged),
        "unsupportedValue" => Ok(HistoryReason::UnsupportedValue),
        "provider" => Ok(HistoryReason::Provider),
        "invalidProvider" => Ok(HistoryReason::InvalidProvider),
        _ => Err(invalid()),
    }
}
pub fn valid_terminal(terminal: HistoryTerminal) -> bool {
    matches!(
        terminal,
        HistoryTerminal::Complete
            | HistoryTerminal::Limited(
                HistoryReason::Rows | HistoryReason::Bytes | HistoryReason::Work
            )
            | HistoryTerminal::Interrupted(
                HistoryReason::Deadline
                    | HistoryReason::Cancelled
                    | HistoryReason::OwnerStopped
                    | HistoryReason::GenerationChanged
                    | HistoryReason::MetadataChanged
                    | HistoryReason::PolicyChanged
            )
            | HistoryTerminal::Failed(
                HistoryReason::UnsupportedValue
                    | HistoryReason::Provider
                    | HistoryReason::InvalidProvider
            )
    )
}
fn terminal_parts(terminal: HistoryTerminal) -> (&'static str, &'static str) {
    match terminal {
        HistoryTerminal::Complete => ("complete", "none"),
        HistoryTerminal::Limited(reason) => ("limited", reason_name(reason)),
        HistoryTerminal::Interrupted(reason) => ("interrupted", reason_name(reason)),
        HistoryTerminal::Failed(reason) => ("failed", reason_name(reason)),
    }
}
fn parse_terminal(control: &HDict) -> Result<HistoryTerminal, TypedPayloadError> {
    let status = string(control, "terminal")?;
    let reason = string(control, "reason")?;
    let result = match status {
        "complete" if reason == "none" => HistoryTerminal::Complete,
        "limited" => HistoryTerminal::Limited(parse_reason(reason)?),
        "interrupted" => HistoryTerminal::Interrupted(parse_reason(reason)?),
        "failed" => HistoryTerminal::Failed(parse_reason(reason)?),
        _ => return Err(invalid()),
    };
    if valid_terminal(result) {
        Ok(result)
    } else {
        Err(invalid())
    }
}
fn allowed(codec: &dyn Codec) -> Result<(), TypedPayloadError> {
    if matches!(
        codec.mime_type(),
        "text/zinc" | "application/json" | "application/json;v=3"
    ) {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn preflight(bytes: &[u8], request: bool) -> Result<&str, TypedPayloadError> {
    if bytes.len()
        > if request {
            MAX_REQUEST_BYTES
        } else {
            MAX_GRID_BYTES
        }
    {
        return Err(invalid());
    }
    let (mut quoted, mut escape, mut depth, mut syntax, mut strings) =
        (false, false, 0usize, 0usize, 0usize);
    for byte in bytes {
        if quoted {
            if escape {
                escape = false;
            } else if *byte == b'\\' {
                escape = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            syntax += 1;
            if syntax > if request { 4096 } else { 2 * 1024 * 1024 } {
                return Err(invalid());
            }
            match byte {
                b'"' => {
                    quoted = true;
                    strings += 1;
                    if strings > if request { 64 } else { MAX_ROWS * 16 + 128 } {
                        return Err(invalid());
                    }
                }
                b'[' | b'{' => {
                    depth += 1;
                    if depth > 8 {
                        return Err(invalid());
                    }
                }
                b']' | b'}' => {
                    depth = depth.checked_sub(1).ok_or_else(invalid)?;
                }
                _ => {}
            }
        }
    }
    if quoted || depth != 0 {
        return Err(invalid());
    }
    std::str::from_utf8(bytes).map_err(|_| invalid())
}
/// Explicit profile marker, with ordinary id/range H4 cells.
pub fn request_grid(request: &HistoryReadRequest) -> Result<HGrid, TypedPayloadError> {
    if request.id.is_empty()
        || request.id.len() > 256
        || request.range.is_empty()
        || request.range.len() > 1024
    {
        return Err(invalid());
    }
    Ok(HGrid::from_parts(
        dict([(
            CONTROL,
            control(dict([
                ("profile", text(PROFILE)),
                ("type", text("request")),
            ]))?,
        )]),
        vec![HCol::new("id"), HCol::new("range")],
        vec![dict([
            ("id", Kind::Ref(HRef::from_val(&request.id))),
            ("range", text(&request.range)),
        ])],
    ))
}
pub fn request_from_grid(grid: &HGrid) -> Result<HistoryReadRequest, TypedPayloadError> {
    fields(&grid.meta, &[CONTROL])?;
    if grid.cols.len() != 2
        || grid.cols[0].name != "id"
        || grid.cols[1].name != "range"
        || grid.cols.iter().any(|col| !col.meta.is_empty())
        || grid.rows.len() != 1
    {
        return Err(invalid());
    }
    let control = parse_control(grid)?;
    fields(&control, &["profile", "type"])?;
    if string(&control, "profile")? != PROFILE || string(&control, "type")? != "request" {
        return Err(invalid());
    }
    let row = &grid.rows[0];
    fields(row, &["id", "range"])?;
    let Some(Kind::Ref(id)) = row.get("id") else {
        return Err(invalid());
    };
    if id.dis.is_some() {
        return Err(invalid());
    }
    let range = string(row, "range")?;
    if id.val.is_empty() || id.val.len() > 256 || range.is_empty() || range.len() > 1024 {
        return Err(invalid());
    }
    Ok(HistoryReadRequest {
        id: id.val.clone(),
        range: range.into(),
    })
}
/// Explicit legacy request adapter. It does not authorize scoped result semantics.
pub fn decode_legacy_request(
    bytes: &[u8],
    codec: &dyn Codec,
) -> Result<HistoryReadRequest, TypedPayloadError> {
    allowed(codec)?;
    let mut grid = codec
        .decode_grid(preflight(bytes, true)?)
        .map_err(|_| invalid())?;
    if !grid.meta.is_empty() {
        return Err(invalid());
    }
    grid.meta.set(
        CONTROL,
        control(dict([
            ("profile", text(PROFILE)),
            ("type", text("request")),
        ]))?,
    );
    request_from_grid(&grid)
}
pub fn decode_request(
    bytes: &[u8],
    codec: &dyn Codec,
) -> Result<HistoryReadRequest, TypedPayloadError> {
    allowed(codec)?;
    request_from_grid(
        &codec
            .decode_grid(preflight(bytes, true)?)
            .map_err(|_| invalid())?,
    )
}
pub fn encode_request(
    request: &HistoryReadRequest,
    codec: &dyn Codec,
) -> Result<Vec<u8>, TypedPayloadError> {
    allowed(codec)?;
    let bytes = codec
        .encode_grid(&request_grid(request)?)
        .map_err(|_| invalid())?
        .into_bytes();
    preflight(&bytes, true)?;
    Ok(bytes)
}
const RESPONSE_FIELDS: &[&str] = &[
    "profile",
    "type",
    "requestedRange",
    "evaluatedAt",
    "kind",
    "unit",
    "timezone",
    "graphIncarnation",
    "graphRevision",
    "catalogGeneration",
    "policyObservation",
    "historyAuthority",
    "historyIncarnation",
    "historyGeneration",
    "retainedStart",
    "retainedEnd",
    "retainedCount",
    "evictedThrough",
    "mode",
    "atomicMetadata",
    "nativeValues",
    "terminal",
    "reason",
    "returnedCount",
    "completeBasis",
    "batchRows",
    "batchBytes",
    "totalRows",
    "totalBytes",
    "totalWork",
    "responseBytes",
];
pub fn result_grid(result: &HistoryReadResult) -> Result<HGrid, TypedPayloadError> {
    if result.samples.len() > MAX_ROWS {
        return Err(invalid());
    }
    let metadata = &result.metadata;
    let coverage = &metadata.coverage;
    if [&metadata.start, &metadata.end, &metadata.evaluated_at]
        .into_iter()
        .chain(coverage.retained_start.iter())
        .chain(coverage.retained_end.iter())
        .chain(coverage.evicted_through.iter())
        .chain(result.samples.iter().map(|sample| &sample.ts))
        .any(|time| !super::shared::h4_datetime_representable(&time.dt))
    {
        return Err(invalid());
    }
    validate_result(result)?;
    let (terminal, reason) = terminal_parts(result.terminal);
    let control = control(dict([
        ("profile", text(PROFILE)),
        ("type", text("result")),
        ("requestedRange", text(&metadata.requested_range)),
        ("evaluatedAt", Kind::DateTime(metadata.evaluated_at.clone())),
        ("kind", text(kind_name(metadata.schema.kind))),
        (
            "unit",
            metadata.schema.unit.as_ref().map_or(Kind::Null, text),
        ),
        ("timezone", text(&metadata.schema.timezone)),
        ("graphIncarnation", hex(&metadata.graph.incarnation)),
        ("graphRevision", uint(metadata.graph.revision)),
        ("catalogGeneration", uint(metadata.graph.catalog_generation)),
        ("policyObservation", hex(&metadata.policy_observation)),
        ("historyAuthority", hex(&metadata.history.authority)),
        ("historyIncarnation", hex(&metadata.history.incarnation)),
        ("historyGeneration", uint(metadata.history.generation)),
        (
            "retainedStart",
            date_kind(&metadata.coverage.retained_start),
        ),
        ("retainedEnd", date_kind(&metadata.coverage.retained_end)),
        ("retainedCount", uint(metadata.coverage.retained_count)),
        (
            "evictedThrough",
            date_kind(&metadata.coverage.evicted_through),
        ),
        ("batchRows", uint(metadata.bounds.batch_rows)),
        ("batchBytes", uint(metadata.bounds.batch_bytes)),
        ("totalRows", uint(metadata.bounds.total_rows)),
        ("totalBytes", uint(metadata.bounds.total_bytes)),
        ("totalWork", uint(metadata.bounds.total_work)),
        ("responseBytes", uint(metadata.bounds.response_bytes)),
        ("mode", text("generationCheckedLive")),
        ("atomicMetadata", Kind::Bool(false)),
        ("nativeValues", Kind::Bool(false)),
        ("terminal", text(terminal)),
        ("reason", text(reason)),
        ("returnedCount", uint(result.samples.len() as u64)),
        ("completeBasis", text("retainedRecords")),
    ]))?;
    let rows = result
        .samples
        .iter()
        .map(|sample| {
            dict([
                ("ts", Kind::DateTime(sample.ts.clone())),
                ("val", sample.val.clone()),
            ])
        })
        .collect();
    Ok(HGrid::from_parts(
        dict([
            (CONTROL, control),
            ("id", Kind::Ref(HRef::from_val(&metadata.id))),
            ("hisStart", Kind::DateTime(metadata.start.clone())),
            ("hisEnd", Kind::DateTime(metadata.end.clone())),
        ]),
        vec![HCol::new("ts"), HCol::new("val")],
        rows,
    ))
}
pub fn result_from_grid(grid: &HGrid) -> Result<HistoryReadResult, TypedPayloadError> {
    fields(&grid.meta, &[CONTROL, "id", "hisStart", "hisEnd"])?;
    if grid.cols.len() != 2
        || grid.cols[0].name != "ts"
        || grid.cols[1].name != "val"
        || grid.cols.iter().any(|col| !col.meta.is_empty())
        || grid.rows.len() > MAX_ROWS
    {
        return Err(invalid());
    }
    let control = parse_control(grid)?;
    fields(&control, RESPONSE_FIELDS)?;
    if string(&control, "profile")? != PROFILE
        || string(&control, "type")? != "result"
        || string(&control, "mode")? != "generationCheckedLive"
        || control.get("atomicMetadata") != Some(&Kind::Bool(false))
        || control.get("nativeValues") != Some(&Kind::Bool(false))
        || string(&control, "completeBasis")? != "retainedRecords"
        || number(&control, "returnedCount")? != grid.rows.len() as u64
    {
        return Err(invalid());
    }
    let Some(Kind::Ref(id)) = grid.meta.get("id") else {
        return Err(invalid());
    };
    if id.dis.is_some() || id.val.len() > 256 {
        return Err(invalid());
    }
    let schema = HistorySchema {
        kind: match string(&control, "kind")? {
            "Bool" => HistoryKind::Bool,
            "Number" => HistoryKind::Number,
            "Str" => HistoryKind::Str,
            _ => return Err(invalid()),
        },
        unit: match control.get("unit") {
            Some(Kind::Null) => None,
            Some(Kind::Str(unit)) if unit.len() <= 128 => Some(unit.clone()),
            _ => return Err(invalid()),
        },
        timezone: string(&control, "timezone")?.into(),
    };
    let metadata = HistoryMetadata {
        id: id.val.clone(),
        requested_range: string(&control, "requestedRange")?.into(),
        evaluated_at: date(&control, "evaluatedAt")?,
        start: date(&grid.meta, "hisStart")?,
        end: date(&grid.meta, "hisEnd")?,
        schema,
        capabilities: HistoryCapabilities::BOUNDED_LIVE,
        bounds: HistoryBounds {
            batch_rows: number(&control, "batchRows")?,
            batch_bytes: number(&control, "batchBytes")?,
            total_rows: number(&control, "totalRows")?,
            total_bytes: number(&control, "totalBytes")?,
            total_work: number(&control, "totalWork")?,
            response_bytes: number(&control, "responseBytes")?,
        },
        graph: GraphState {
            incarnation: identity(&control, "graphIncarnation")?,
            revision: number(&control, "graphRevision")?,
            catalog_generation: number(&control, "catalogGeneration")?,
        },
        policy_observation: identity(&control, "policyObservation")?,
        history: HistoryState {
            authority: identity(&control, "historyAuthority")?,
            incarnation: identity(&control, "historyIncarnation")?,
            generation: number(&control, "historyGeneration")?,
        },
        coverage: HistoryCoverage {
            retained_start: optional_date(&control, "retainedStart")?,
            retained_end: optional_date(&control, "retainedEnd")?,
            retained_count: number(&control, "retainedCount")?,
            evicted_through: optional_date(&control, "evictedThrough")?,
        },
    };
    // Source-sized, bounded admission before cloning any untrusted row values.
    let mut budget = crate::graph::size::ValueBudget::new(MAX_ROWS * 8, MAX_GRID_BYTES, 8);
    let mut source_bytes = 0u64;
    for row in &grid.rows {
        fields(row, &["ts", "val"])?;
        let size = match row.get("val") {
            Some(Kind::Bool(_) | Kind::NA) => 512,
            Some(Kind::Str(value)) => 512u64.saturating_add(value.len() as u64),
            Some(Kind::Number(value)) => {
                512u64.saturating_add(value.unit.as_ref().map_or(0, |unit| unit.len() as u64))
            }
            _ => return Err(invalid()),
        };
        source_bytes = source_bytes.saturating_add(size);
        if source_bytes > metadata.bounds.total_bytes || source_bytes > metadata.bounds.total_work {
            return Err(invalid());
        }
        for (_, value) in row.iter() {
            budget.value(value, 0).map_err(|_| invalid())?;
        }
    }
    let mut samples = Vec::with_capacity(grid.rows.len());
    for row in &grid.rows {
        samples.push(HistorySample {
            ts: date(row, "ts")?,
            val: row.get("val").ok_or_else(invalid)?.clone(),
        });
    }
    let result = HistoryReadResult {
        metadata,
        samples,
        terminal: parse_terminal(&control)?,
    };
    validate_result(&result)?;
    Ok(result)
}
pub fn encode_result(
    result: &HistoryReadResult,
    codec: &dyn Codec,
) -> Result<Vec<u8>, TypedPayloadError> {
    allowed(codec)?;
    let bytes = codec
        .encode_grid(&result_grid(result)?)
        .map_err(|_| invalid())?
        .into_bytes();
    preflight(&bytes, false)?;
    if bytes.len() as u64 > result.metadata.bounds.response_bytes {
        return Err(invalid());
    }
    Ok(bytes)
}
pub fn decode_result(
    bytes: &[u8],
    codec: &dyn Codec,
) -> Result<HistoryReadResult, TypedPayloadError> {
    allowed(codec)?;
    let text = preflight(bytes, false)?;
    let grid = if codec.mime_type() == "text/zinc" {
        super::zinc::decode_grid_complete_rows(text)
    } else {
        codec.decode_grid(text)
    }
    .map_err(|_| invalid())?;
    let result = result_from_grid(&grid)?;
    if bytes.len() as u64 > result.metadata.bounds.response_bytes {
        return Err(invalid());
    }
    Ok(result)
}
fn validate_time(time: &HDateTime, zone: &str) -> Result<(), TypedPayloadError> {
    if time.tz_name != zone {
        return Err(invalid());
    }
    #[cfg(feature = "chrono-tz")]
    if crate::kinds::offset_at(zone, time.dt) != Some(*time.dt.offset()) {
        return Err(invalid());
    }
    Ok(())
}
pub fn validate_result(result: &HistoryReadResult) -> Result<(), TypedPayloadError> {
    let m = &result.metadata;
    let c = &m.coverage;
    let zone = &m.schema.timezone;
    let bounds = m.bounds;
    if bounds.batch_rows == 0
        || bounds.batch_rows > bounds.total_rows
        || bounds.total_rows > MAX_ROWS as u64
        || bounds.batch_bytes == 0
        || bounds.batch_bytes > bounds.total_bytes
        || bounds.total_bytes > MAX_GRID_BYTES as u64
        || bounds.total_work == 0
        || bounds.total_work > 256 * 1024 * 1024
        || bounds.response_bytes == 0
        || bounds.response_bytes > MAX_GRID_BYTES as u64
        || result.samples.len() as u64 > bounds.total_rows
    {
        return Err(invalid());
    }
    if m.id.is_empty()
        || m.id.len() > 256
        || m.requested_range.is_empty()
        || m.requested_range.len() > 1024
        || zone.len() > 128
        || zone == "Rel"
        || zone.contains('/')
        || crate::kinds::tz_for(zone).is_none()
        || m.start.dt > m.end.dt
        || m.capabilities != HistoryCapabilities::BOUNDED_LIVE
        || !valid_terminal(result.terminal)
        || result.samples.len() > MAX_ROWS
        || result.samples.len() as u64 > c.retained_count
        || (c.retained_count == 0) != (c.retained_start.is_none() && c.retained_end.is_none())
        || c.retained_start.is_some() != c.retained_end.is_some()
    {
        return Err(invalid());
    }
    if (m.schema.kind == HistoryKind::Number && m.schema.unit.is_none())
        || m.schema.unit.as_ref().is_some_and(|unit| {
            unit.len() > 128 || m.schema.kind != HistoryKind::Number || unit_for(unit).is_none()
        })
    {
        return Err(invalid());
    }
    for time in [&m.start, &m.end, &m.evaluated_at]
        .into_iter()
        .chain(c.retained_start.iter())
        .chain(c.retained_end.iter())
        .chain(c.evicted_through.iter())
    {
        validate_time(time, zone)?;
    }
    if let (Some(start), Some(end)) = (&c.retained_start, &c.retained_end) {
        if start.dt > end.dt
            || (c.retained_count == 1 && start.dt != end.dt)
            || c.evicted_through
                .as_ref()
                .is_some_and(|through| through.dt >= start.dt)
        {
            return Err(invalid());
        }
        if result.terminal == HistoryTerminal::Complete {
            let includes_first = m.start.dt <= start.dt && start.dt < m.end.dt;
            let includes_last = m.start.dt <= end.dt && end.dt < m.end.dt;
            if (includes_first
                && result.samples.first().map(|sample| sample.ts.dt) != Some(start.dt))
                || (includes_last
                    && result.samples.last().map(|sample| sample.ts.dt) != Some(end.dt))
                || (includes_first
                    && includes_last
                    && result.samples.len() as u64 != c.retained_count)
            {
                return Err(invalid());
            }
        }
    }
    let mut budget = crate::graph::size::ValueBudget::new(MAX_ROWS * 8, MAX_GRID_BYTES, 8);
    let mut previous = None;
    let mut admitted_bytes = 0u64;
    for sample in &result.samples {
        validate_time(&sample.ts, zone)?;
        if sample.ts.dt < m.start.dt
            || sample.ts.dt >= m.end.dt
            || previous.is_some_and(|previous| sample.ts.dt <= previous)
            || c.retained_start
                .as_ref()
                .is_none_or(|start| sample.ts.dt < start.dt)
            || c.retained_end
                .as_ref()
                .is_none_or(|end| sample.ts.dt > end.dt)
        {
            return Err(invalid());
        }
        previous = Some(sample.ts.dt);
        budget.value(&sample.val, 0).map_err(|_| invalid())?;
        if !valid_value(&sample.val, &m.schema) {
            return Err(invalid());
        }
        admitted_bytes = admitted_bytes.saturating_add(
            512 + match &sample.val {
                Kind::Str(value) => value.len() as u64,
                Kind::Number(value) => value.unit.as_ref().map_or(0, |unit| unit.len() as u64),
                _ => 0,
            },
        );
        if admitted_bytes > bounds.total_bytes || admitted_bytes > bounds.total_work {
            return Err(invalid());
        }
    }
    Ok(())
}
/// Bind a response to this exact requested point/range. Never infer success or
/// replay a call on a missing, contradictory or foreign envelope.
pub fn validate_for_request(
    result: &HistoryReadResult,
    request: &HistoryReadRequest,
) -> Result<(), TypedPayloadError> {
    validate_result(result)?;
    if result.metadata.id != request.id || result.metadata.requested_range != request.range {
        return Err(invalid());
    }
    validate_range(result)
}
fn validate_range(result: &HistoryReadResult) -> Result<(), TypedPayloadError> {
    use chrono::{NaiveDate, NaiveTime};
    let m = &result.metadata;
    let boundary =
        |source: &str, actual: &HDateTime, inclusive_date: bool| -> Result<(), TypedPayloadError> {
            if let Ok(date) = NaiveDate::parse_from_str(source.trim(), "%Y-%m-%d") {
                let date = if inclusive_date {
                    date.succ_opt().ok_or_else(invalid)?
                } else {
                    date
                };
                if actual.dt.date_naive() != date || actual.dt.time() != NaiveTime::MIN {
                    return Err(invalid());
                }
                validate_calendar_midnight(actual)?;
            } else {
                let Kind::DateTime(expected) = super::zinc::ZincCodec
                    .decode_scalar(source.trim())
                    .map_err(|_| invalid())?
                else {
                    return Err(invalid());
                };
                if expected.tz_name == "Rel"
                    || expected.tz_name.contains('/')
                    || crate::kinds::tz_for(&expected.tz_name).is_none()
                {
                    return Err(invalid());
                }
                #[cfg(feature = "chrono-tz")]
                if crate::kinds::offset_at(&expected.tz_name, expected.dt)
                    != Some(*expected.dt.offset())
                {
                    return Err(invalid());
                }
                if expected.dt != actual.dt {
                    return Err(invalid());
                }
            }
            Ok(())
        };
    match m.requested_range.trim() {
        "today" | "yesterday" => {
            validate_calendar_midnight(&m.start)?;
            validate_calendar_midnight(&m.end)?;
            let date = m.evaluated_at.dt.date_naive();
            let date = if m.requested_range.trim() == "yesterday" {
                date.pred_opt().ok_or_else(invalid)?
            } else {
                date
            };
            if m.start.dt.date_naive() != date
                || m.end.dt.date_naive() != date.succ_opt().ok_or_else(invalid)?
                || m.start.dt.time() != NaiveTime::MIN
                || m.end.dt.time() != NaiveTime::MIN
            {
                return Err(invalid());
            }
        }
        range => {
            if let Some((start, end)) = range.split_once(',') {
                boundary(start, &m.start, false)?;
                boundary(end, &m.end, true)?;
            } else {
                boundary(range, &m.start, false)?;
                boundary(range, &m.end, true)?;
            }
        }
    }
    Ok(())
}

// Calendar boundaries require a unique local midnight. Explicit DateTime
// boundaries deliberately keep their instant-based cross-zone validation.
fn validate_calendar_midnight(actual: &HDateTime) -> Result<(), TypedPayloadError> {
    if actual.dt.time() != chrono::NaiveTime::MIN {
        return Err(invalid());
    }
    #[cfg(feature = "chrono-tz")]
    if crate::kinds::resolve_local_offset(&actual.tz_name, actual.dt.naive_local())
        != Some(chrono::LocalResult::Single(*actual.dt.offset()))
    {
        return Err(invalid());
    }
    Ok(())
}

/// Shared strict H4 history value admission against an already validated schema.
pub fn valid_value(value: &Kind, schema: &HistorySchema) -> bool {
    match (value, schema.kind) {
        (Kind::NA, _) => true,
        (Kind::Bool(_), HistoryKind::Bool) | (Kind::Str(_), HistoryKind::Str) => true,
        (Kind::Number(number), HistoryKind::Number) => match &number.unit {
            None => !number.val.is_nan() || number.val.to_bits() == f64::NAN.to_bits(),
            Some(unit) => {
                !number.val.is_nan()
                    && schema
                        .unit
                        .as_ref()
                        .and_then(|unit| unit_for(unit))
                        .zip(unit_for(unit))
                        .is_some_and(|(point, sample)| point.name == sample.name)
            }
        },
        _ => false,
    }
}
