//! Single-point history-write-v1. Original ordered typed intent is bound before
//! upsert/retention. History identities and receipts never share the entity domain.
//! H4 carries a versioned STR control plus ordinary ts/val cells; it does not
//! define these project-specific receipts or any durability guarantee.
use super::{
    Codec,
    history::HistorySample,
    typed::{self, TypedPayloadError},
};
use crate::{
    data::{HCol, HDict, HGrid},
    kinds::{HRef, Kind},
};

pub const PROFILE: &str = "history-write-v1";
pub const MAX_SAMPLES: usize = 1024;
pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_GRID_BYTES: usize = 6 * MAX_SOURCE_BYTES + 16 * 1024;
pub const MAX_RECEIPT_BYTES: usize = 32 * 1024;
const CONTROL: &str = "historyWrite";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HistoryOperationIdentity {
    pub authority: [u8; 16],
    pub point: String,
    pub incarnation: [u8; 16],
    pub operation_id: String,
}
#[derive(Debug, Clone)]
pub struct HistoryWriteRequest {
    pub identity: HistoryOperationIdentity,
    pub expected_generation: u64,
    pub samples: Vec<HistorySample>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryReceiptQualification {
    EphemeralMemory,
    /// A surviving in-process authority and provider protocol fixture only.
    ProviderProtocol,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryWriteReceipt {
    pub identity: HistoryOperationIdentity,
    pub before_generation: u64,
    pub after_generation: u64,
    /// One required record in this authority's bounded history change ledger.
    pub change_sequence: u64,
    pub submitted_samples: u64,
    pub unique_samples: u64,
    pub retained_samples: u64,
    pub evicted_samples: u64,
    pub qualification: HistoryReceiptQualification,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryWriteRejection {
    Invalid,
    EmptyBatch,
    Forbidden,
    Conflict,
    Unsupported,
    Capacity,
    Limit,
    Cancelled,
    Deadline,
    Provider,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryWriteUnknown {
    Missing,
    Pending,
    Provider,
    Transport,
    InvalidAcknowledgement,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryWriteOutcome {
    Committed(HistoryWriteReceipt),
    /// This submission attempt caused no sample/generation/change effect.
    Rejected {
        identity: HistoryOperationIdentity,
        reason: HistoryWriteRejection,
    },
    /// No conclusion about effect. Lookup explicitly; never replay automatically.
    Unknown {
        identity: HistoryOperationIdentity,
        cause: HistoryWriteUnknown,
    },
}
impl HistoryWriteOutcome {
    pub fn identity(&self) -> &HistoryOperationIdentity {
        match self {
            Self::Committed(receipt) => &receipt.identity,
            Self::Rejected { identity, .. } | Self::Unknown { identity, .. } => identity,
        }
    }
}
fn invalid() -> TypedPayloadError {
    TypedPayloadError::Invalid("invalid history-write-v1 envelope".into())
}
fn dict(entries: impl IntoIterator<Item = (&'static str, Kind)>) -> HDict {
    let mut result = HDict::new();
    for (key, value) in entries {
        result.set(key, value);
    }
    result
}
fn kd(value: HDict) -> Kind {
    Kind::Dict(Box::new(value))
}
fn text(value: impl Into<String>) -> Kind {
    Kind::Str(value.into())
}
fn uint(value: u64) -> Kind {
    text(value.to_string())
}
fn hex(value: &[u8; 16]) -> Kind {
    text(value.iter().map(|b| format!("{b:02x}")).collect::<String>())
}
fn fields(value: &HDict, names: &[&str]) -> Result<(), TypedPayloadError> {
    if value.len() == names.len() && names.iter().all(|name| value.has(name)) {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn string<'a>(value: &'a HDict, key: &str) -> Result<&'a str, TypedPayloadError> {
    match value.get(key) {
        Some(Kind::Str(value)) => Ok(value),
        _ => Err(invalid()),
    }
}
fn number(value: &HDict, key: &str) -> Result<u64, TypedPayloadError> {
    let source = string(value, key)?;
    let value: u64 = source.parse().map_err(|_| invalid())?;
    if value.to_string() == source {
        Ok(value)
    } else {
        Err(invalid())
    }
}
fn parse_hex(value: &HDict, key: &str) -> Result<[u8; 16], TypedPayloadError> {
    let source = string(value, key)?;
    if source.len() != 32
        || !source
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    let mut result = [0; 16];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&source[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(result)
}
pub fn validate_identity(identity: &HistoryOperationIdentity) -> Result<(), TypedPayloadError> {
    if identity.point.is_empty()
        || identity.point.len() > 256
        || identity.operation_id.is_empty()
        || identity.operation_id.len() > 128
        || identity.point.chars().any(char::is_control)
        || identity.operation_id.chars().any(char::is_control)
    {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn identity_kind(identity: &HistoryOperationIdentity) -> Kind {
    kd(dict([
        ("authority", hex(&identity.authority)),
        ("point", text(&identity.point)),
        ("incarnation", hex(&identity.incarnation)),
        ("operationId", text(&identity.operation_id)),
    ]))
}
fn identity(control: &HDict) -> Result<HistoryOperationIdentity, TypedPayloadError> {
    let Some(Kind::Dict(value)) = control.get("identity") else {
        return Err(invalid());
    };
    fields(value, &["authority", "point", "incarnation", "operationId"])?;
    let result = HistoryOperationIdentity {
        authority: parse_hex(value, "authority")?,
        point: string(value, "point")?.into(),
        incarnation: parse_hex(value, "incarnation")?,
        operation_id: string(value, "operationId")?.into(),
    };
    validate_identity(&result)?;
    Ok(result)
}
impl HistoryWriteRequest {
    /// Bound the original source before any clone or projection. Unsupported
    /// typed values can be rejected by admission without erasing their identity.
    pub fn source_bytes(&self) -> Result<usize, TypedPayloadError> {
        validate_identity(&self.identity)?;
        if self.samples.len() > MAX_SAMPLES {
            return Err(invalid());
        }
        let mut budget = crate::graph::size::ValueBudget::new(4_000_000, MAX_SOURCE_BYTES, 16);
        budget
            .charge(
                1,
                1024 + self.identity.point.len() + self.identity.operation_id.len(),
            )
            .map_err(|_| invalid())?;
        for sample in &self.samples {
            if sample.ts.tz_name.len() > 128 {
                return Err(invalid());
            }
            budget
                .charge(1, sample.ts.tz_name.len().saturating_add(128))
                .map_err(|_| invalid())?;
            budget.value(&sample.val, 0).map_err(|_| invalid())?;
        }
        Ok(budget.used_bytes())
    }
}
/// Preserve all original rows, timezone spelling/offset, Number bits and units.
/// This is never derived from the normalized/deduplicated stored series.
pub fn canonical_request(request: &HistoryWriteRequest) -> Result<Vec<u8>, TypedPayloadError> {
    request.source_bytes()?;
    let rows = request
        .samples
        .iter()
        .map(|sample| {
            kd(dict([
                ("ts", Kind::DateTime(sample.ts.clone())),
                ("val", sample.val.clone()),
            ]))
        })
        .collect();
    typed::encode(&kd(dict([
        ("profile", text(PROFILE)),
        ("identity", identity_kind(&request.identity)),
        ("expectedGeneration", uint(request.expected_generation)),
        ("samples", Kind::List(rows)),
    ])))
}
fn control(value: HDict) -> Result<Kind, TypedPayloadError> {
    let bytes = typed::encode(&kd(value))?;
    if bytes.len() > MAX_RECEIPT_BYTES / 6 {
        return Err(invalid());
    }
    Ok(text(String::from_utf8(bytes).map_err(|_| invalid())?))
}
fn parse_control(grid: &HGrid, kind: &str) -> Result<HDict, TypedPayloadError> {
    let source = string(&grid.meta, CONTROL)?;
    let value = typed::decode_with_limits(
        source.as_bytes(),
        typed::PayloadLimits {
            max_bytes: MAX_RECEIPT_BYTES / 6,
            max_depth: 12,
            max_nodes: 256,
        },
    )?;
    let Kind::Dict(value) = value else {
        return Err(invalid());
    };
    if string(&value, "profile")? != PROFILE || string(&value, "type")? != kind {
        return Err(invalid());
    }
    Ok(*value)
}
fn wire_value(value: &Kind) -> bool {
    match value {
        Kind::Bool(_) | Kind::Str(_) | Kind::NA => true,
        Kind::Number(value) => {
            value
                .unit
                .as_ref()
                .is_none_or(|unit| unit.len() <= 128 && crate::kinds::unit_for(unit).is_some())
                && (!value.val.is_nan()
                    || (value.unit.is_none() && value.val.to_bits() == f64::NAN.to_bits()))
        }
        _ => false,
    }
}
pub fn request_grid(request: &HistoryWriteRequest) -> Result<HGrid, TypedPayloadError> {
    request.source_bytes()?;
    if request.samples.iter().any(|sample| {
        !wire_value(&sample.val)
            || !super::shared::h4_datetime_representable(&sample.ts.dt)
            || sample.ts.tz_name == "Rel"
            || sample.ts.tz_name.contains('/')
            || crate::kinds::tz_for(&sample.ts.tz_name).is_none()
    }) {
        return Err(invalid());
    }
    Ok(HGrid::from_parts(
        dict([
            ("id", Kind::Ref(HRef::from_val(&request.identity.point))),
            (
                CONTROL,
                control(dict([
                    ("profile", text(PROFILE)),
                    ("type", text("submit")),
                    ("identity", identity_kind(&request.identity)),
                    ("expectedGeneration", uint(request.expected_generation)),
                ]))?,
            ),
        ]),
        vec![HCol::new("ts"), HCol::new("val")],
        request
            .samples
            .iter()
            .map(|sample| {
                dict([
                    ("ts", Kind::DateTime(sample.ts.clone())),
                    ("val", sample.val.clone()),
                ])
            })
            .collect(),
    ))
}
pub fn request_from_grid(grid: &HGrid) -> Result<HistoryWriteRequest, TypedPayloadError> {
    fields(&grid.meta, &["id", CONTROL])?;
    if grid.cols.len() != 2
        || grid.cols[0].name != "ts"
        || grid.cols[1].name != "val"
        || grid.cols.iter().any(|column| !column.meta.is_empty())
        || grid.rows.len() > MAX_SAMPLES
    {
        return Err(invalid());
    }
    let control = parse_control(grid, "submit")?;
    fields(
        &control,
        &["profile", "type", "identity", "expectedGeneration"],
    )?;
    let identity = identity(&control)?;
    let Some(Kind::Ref(id)) = grid.meta.get("id") else {
        return Err(invalid());
    };
    if id.val != identity.point || id.dis.is_some() {
        return Err(invalid());
    }
    // Check the entire source before the first row/value clone.
    let mut size = crate::graph::size::ValueBudget::new(4_000_000, MAX_SOURCE_BYTES, 16);
    for row in &grid.rows {
        size.dict(row, 0).map_err(|_| invalid())?;
    }
    let mut samples = Vec::with_capacity(grid.rows.len());
    for row in &grid.rows {
        fields(row, &["ts", "val"])?;
        let Some(Kind::DateTime(ts)) = row.get("ts") else {
            return Err(invalid());
        };
        if ts.tz_name.len() > 128 {
            return Err(invalid());
        }
        samples.push(HistorySample {
            ts: ts.clone(),
            val: row.get("val").ok_or_else(invalid)?.clone(),
        });
    }
    let result = HistoryWriteRequest {
        identity,
        expected_generation: number(&control, "expectedGeneration")?,
        samples,
    };
    result.source_bytes()?;
    Ok(result)
}
fn empty_grid(value: HDict) -> Result<HGrid, TypedPayloadError> {
    Ok(HGrid::from_parts(
        dict([(CONTROL, control(value)?)]),
        vec![],
        vec![],
    ))
}
fn check_empty(grid: &HGrid, allow_error: bool) -> Result<(), TypedPayloadError> {
    if !grid.rows.is_empty()
        || !grid.cols.is_empty()
        || grid
            .meta
            .tag_names()
            .any(|name| name != CONTROL && !(allow_error && name == "err"))
        || (grid.meta.has("err") && !matches!(grid.meta.get("err"), Some(Kind::Marker)))
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn lookup_grid(identity: &HistoryOperationIdentity) -> Result<HGrid, TypedPayloadError> {
    validate_identity(identity)?;
    empty_grid(dict([
        ("profile", text(PROFILE)),
        ("type", text("lookup")),
        ("identity", identity_kind(identity)),
    ]))
}
pub fn lookup_from_grid(grid: &HGrid) -> Result<HistoryOperationIdentity, TypedPayloadError> {
    check_empty(grid, false)?;
    let control = parse_control(grid, "lookup")?;
    fields(&control, &["profile", "type", "identity"])?;
    identity(&control)
}
fn rejection_name(reason: HistoryWriteRejection) -> &'static str {
    match reason {
        HistoryWriteRejection::Invalid => "invalid",
        HistoryWriteRejection::EmptyBatch => "emptyBatch",
        HistoryWriteRejection::Forbidden => "forbidden",
        HistoryWriteRejection::Conflict => "conflict",
        HistoryWriteRejection::Unsupported => "unsupported",
        HistoryWriteRejection::Capacity => "capacity",
        HistoryWriteRejection::Limit => "limit",
        HistoryWriteRejection::Cancelled => "cancelled",
        HistoryWriteRejection::Deadline => "deadline",
        HistoryWriteRejection::Provider => "provider",
    }
}
fn unknown_name(cause: HistoryWriteUnknown) -> &'static str {
    match cause {
        HistoryWriteUnknown::Missing => "missing",
        HistoryWriteUnknown::Pending => "pending",
        HistoryWriteUnknown::Provider => "provider",
        HistoryWriteUnknown::Transport => "transport",
        HistoryWriteUnknown::InvalidAcknowledgement => "invalidAcknowledgement",
    }
}
pub fn outcome_grid(outcome: &HistoryWriteOutcome) -> Result<HGrid, TypedPayloadError> {
    validate_identity(outcome.identity())?;
    let mut control = dict([
        ("profile", text(PROFILE)),
        ("type", text("outcome")),
        ("identity", identity_kind(outcome.identity())),
    ]);
    match outcome {
        HistoryWriteOutcome::Committed(receipt) => {
            validate_receipt(receipt)?;
            control.set("outcome", text("committed"));
            for (name, value) in [
                ("beforeGeneration", receipt.before_generation),
                ("afterGeneration", receipt.after_generation),
                ("changeSequence", receipt.change_sequence),
                ("submittedSamples", receipt.submitted_samples),
                ("uniqueSamples", receipt.unique_samples),
                ("retainedSamples", receipt.retained_samples),
                ("evictedSamples", receipt.evicted_samples),
            ] {
                control.set(name, uint(value));
            }
            control.set(
                "qualification",
                text(match receipt.qualification {
                    HistoryReceiptQualification::EphemeralMemory => "ephemeralMemory",
                    HistoryReceiptQualification::ProviderProtocol => "providerProtocol",
                }),
            );
        }
        HistoryWriteOutcome::Rejected { reason, .. } => {
            control.set("outcome", text("rejected"));
            control.set("reason", text(rejection_name(*reason)));
        }
        HistoryWriteOutcome::Unknown { cause, .. } => {
            control.set("outcome", text("unknown"));
            control.set("cause", text(unknown_name(*cause)));
        }
    }
    empty_grid(control)
}
pub fn outcome_from_grid(grid: &HGrid) -> Result<HistoryWriteOutcome, TypedPayloadError> {
    check_empty(grid, true)?;
    let control = parse_control(grid, "outcome")?;
    let identity = identity(&control)?;
    Ok(match string(&control, "outcome")? {
        "committed" => {
            fields(
                &control,
                &[
                    "profile",
                    "type",
                    "identity",
                    "outcome",
                    "beforeGeneration",
                    "afterGeneration",
                    "changeSequence",
                    "submittedSamples",
                    "uniqueSamples",
                    "retainedSamples",
                    "evictedSamples",
                    "qualification",
                ],
            )?;
            let receipt = HistoryWriteReceipt {
                identity,
                before_generation: number(&control, "beforeGeneration")?,
                after_generation: number(&control, "afterGeneration")?,
                change_sequence: number(&control, "changeSequence")?,
                submitted_samples: number(&control, "submittedSamples")?,
                unique_samples: number(&control, "uniqueSamples")?,
                retained_samples: number(&control, "retainedSamples")?,
                evicted_samples: number(&control, "evictedSamples")?,
                qualification: match string(&control, "qualification")? {
                    "ephemeralMemory" => HistoryReceiptQualification::EphemeralMemory,
                    "providerProtocol" => HistoryReceiptQualification::ProviderProtocol,
                    _ => return Err(invalid()),
                },
            };
            validate_receipt(&receipt)?;
            HistoryWriteOutcome::Committed(receipt)
        }
        "rejected" => {
            fields(
                &control,
                &["profile", "type", "identity", "outcome", "reason"],
            )?;
            let reason = match string(&control, "reason")? {
                "invalid" => HistoryWriteRejection::Invalid,
                "emptyBatch" => HistoryWriteRejection::EmptyBatch,
                "forbidden" => HistoryWriteRejection::Forbidden,
                "conflict" => HistoryWriteRejection::Conflict,
                "unsupported" => HistoryWriteRejection::Unsupported,
                "capacity" => HistoryWriteRejection::Capacity,
                "limit" => HistoryWriteRejection::Limit,
                "cancelled" => HistoryWriteRejection::Cancelled,
                "deadline" => HistoryWriteRejection::Deadline,
                "provider" => HistoryWriteRejection::Provider,
                _ => return Err(invalid()),
            };
            HistoryWriteOutcome::Rejected { identity, reason }
        }
        "unknown" => {
            fields(
                &control,
                &["profile", "type", "identity", "outcome", "cause"],
            )?;
            let cause = match string(&control, "cause")? {
                "missing" => HistoryWriteUnknown::Missing,
                "pending" => HistoryWriteUnknown::Pending,
                "provider" => HistoryWriteUnknown::Provider,
                "transport" => HistoryWriteUnknown::Transport,
                "invalidAcknowledgement" => HistoryWriteUnknown::InvalidAcknowledgement,
                _ => return Err(invalid()),
            };
            HistoryWriteOutcome::Unknown { identity, cause }
        }
        _ => return Err(invalid()),
    })
}
fn validate_receipt(receipt: &HistoryWriteReceipt) -> Result<(), TypedPayloadError> {
    if receipt.before_generation.checked_add(1) != Some(receipt.after_generation)
        || receipt.change_sequence == 0
        || receipt.submitted_samples == 0
        || receipt.submitted_samples > MAX_SAMPLES as u64
        || receipt.unique_samples == 0
        || receipt.unique_samples > receipt.submitted_samples
        || receipt.retained_samples == 0
        || receipt.retained_samples > 1_000_000
        || receipt.evicted_samples > 1_000_000 + MAX_SAMPLES as u64
    {
        Err(invalid())
    } else {
        Ok(())
    }
}
/// Verify committed accounting against the exact submission, including original
/// duplicate timestamps. A plausible unrelated receipt never establishes success.
pub fn validate_for_request(
    outcome: &HistoryWriteOutcome,
    request: &HistoryWriteRequest,
) -> Result<(), TypedPayloadError> {
    request.source_bytes()?;
    if outcome.identity() != &request.identity {
        return Err(invalid());
    }
    if let HistoryWriteOutcome::Committed(receipt) = outcome {
        validate_receipt(receipt)?;
        let mut instants = request
            .samples
            .iter()
            .map(|sample| sample.ts.dt)
            .collect::<Vec<_>>();
        instants.sort_unstable();
        instants.dedup();
        if receipt.before_generation != request.expected_generation
            || receipt.submitted_samples != request.samples.len() as u64
            || receipt.unique_samples != instants.len() as u64
        {
            return Err(invalid());
        }
    }
    Ok(())
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
fn preflight(bytes: &[u8], limit: usize) -> Result<&str, TypedPayloadError> {
    if bytes.len() > limit {
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
            if syntax > 512 * 1024 {
                return Err(invalid());
            }
            match byte {
                b'"' => {
                    quoted = true;
                    strings += 1;
                    if strings > MAX_SAMPLES * 16 + 128 {
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
fn decode_grid(bytes: &[u8], codec: &dyn Codec, limit: usize) -> Result<HGrid, TypedPayloadError> {
    allowed(codec)?;
    let source = preflight(bytes, limit)?;
    if codec.mime_type() == "text/zinc" {
        super::zinc::decode_grid_complete_rows(source).map_err(|_| invalid())
    } else {
        codec.decode_grid(source).map_err(|_| invalid())
    }
}
fn encode_grid(grid: HGrid, codec: &dyn Codec, limit: usize) -> Result<Vec<u8>, TypedPayloadError> {
    allowed(codec)?;
    let bytes = codec
        .encode_grid(&grid)
        .map_err(|_| invalid())?
        .into_bytes();
    preflight(&bytes, limit)?;
    Ok(bytes)
}
pub fn encode_request(
    request: &HistoryWriteRequest,
    codec: &dyn Codec,
) -> Result<Vec<u8>, TypedPayloadError> {
    encode_grid(request_grid(request)?, codec, MAX_GRID_BYTES)
}
pub fn decode_request(
    bytes: &[u8],
    codec: &dyn Codec,
) -> Result<HistoryWriteRequest, TypedPayloadError> {
    request_from_grid(&decode_grid(bytes, codec, MAX_GRID_BYTES)?)
}
pub fn encode_lookup(
    identity: &HistoryOperationIdentity,
    codec: &dyn Codec,
) -> Result<Vec<u8>, TypedPayloadError> {
    encode_grid(lookup_grid(identity)?, codec, MAX_RECEIPT_BYTES)
}
pub fn decode_lookup(
    bytes: &[u8],
    codec: &dyn Codec,
) -> Result<HistoryOperationIdentity, TypedPayloadError> {
    lookup_from_grid(&decode_grid(bytes, codec, MAX_RECEIPT_BYTES)?)
}
pub fn encode_outcome(
    outcome: &HistoryWriteOutcome,
    codec: &dyn Codec,
) -> Result<Vec<u8>, TypedPayloadError> {
    encode_grid(outcome_grid(outcome)?, codec, MAX_RECEIPT_BYTES)
}
pub fn decode_outcome(
    bytes: &[u8],
    codec: &dyn Codec,
) -> Result<HistoryWriteOutcome, TypedPayloadError> {
    outcome_from_grid(&decode_grid(bytes, codec, MAX_RECEIPT_BYTES)?)
}
