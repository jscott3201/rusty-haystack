//! Versioned entity mutation, receipt and retained-feed extension protocol.
//!
//! Each H4 request/response is exactly one `payload` STR cell containing typed-v1
//! bytes. This preserves rich values independently of the outer H4 codec. Public
//! revisions are canonical decimal strings. Outcomes after submission are data,
//! never error grids; transport uncertainty must not cause automatic resubmission.
use super::typed;
use crate::{
    data::{HCol, HDict, HGrid},
    graph::{CommitSpan, DiffOp, EntityOperation},
    kinds::Kind,
};
use typed::TypedPayloadError;

pub const MAX_PAYLOAD_BYTES: usize = 1_048_576;
/// Worst-case H4 escaping, bounded metadata and framing included.
pub const MAX_GRID_BYTES: usize = MAX_PAYLOAD_BYTES * 6 + 4096;
pub const MAX_OPERATIONS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OperationIdentity {
    pub operation_id: String,
    pub dataset: [u8; 16],
    pub incarnation: [u8; 16],
}
#[derive(Debug, Clone)]
pub struct EntityBatchRequest {
    pub identity: OperationIdentity,
    pub expected_revision: u64,
    pub operations: Vec<EntityOperation>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptQualification {
    EphemeralMemory,
    ProviderProtocol,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityReceipt {
    pub identity: OperationIdentity,
    pub before_revision: u64,
    pub after_revision: u64,
    pub span: Option<CommitSpan>,
    pub qualification: ReceiptQualification,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    Invalid,
    Forbidden,
    Conflict,
    Capacity,
    Limit,
    Cancelled,
    Deadline,
    Provider,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownCause {
    Missing,
    Pending,
    Provider,
    Transport,
    InvalidAcknowledgement,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationOutcome {
    Committed(EntityReceipt),
    /// The provider guarantees this submission attempt caused no effect.
    Rejected {
        identity: OperationIdentity,
        reason: RejectionReason,
    },
    /// No conclusion about effect. Reconcile explicitly; never replay automatically.
    Unknown {
        identity: OperationIdentity,
        cause: UnknownCause,
    },
}
impl MutationOutcome {
    pub fn identity(&self) -> &OperationIdentity {
        match self {
            Self::Committed(r) => &r.identity,
            Self::Rejected { identity, .. } | Self::Unknown { identity, .. } => identity,
        }
    }
}
#[derive(Debug, Clone)]
pub struct ChangesRequest {
    pub cursor: Option<String>,
    pub max_diffs: usize,
}
#[derive(Debug, Clone)]
pub struct EntityDiff {
    pub revision: u64,
    pub span: CommitSpan,
    pub id: String,
    pub operation: DiffOp,
    pub changed: HDict,
    pub previous: Option<HDict>,
}
#[derive(Debug, Clone)]
pub struct ChangesPage {
    pub dataset: [u8; 16],
    pub incarnation: [u8; 16],
    pub head: u64,
    pub floor: u64,
    pub position: u64,
    pub cursor: String,
    pub complete: bool,
    pub changes: Vec<EntityDiff>,
}

fn check_kind(v: &Kind) -> Result<(), TypedPayloadError> {
    crate::graph::size::ValueBudget::new(1_000_000, 8 * MAX_PAYLOAD_BYTES, 64)
        .value(v, 0)
        .map_err(|_| invalid())
}
fn invalid() -> TypedPayloadError {
    TypedPayloadError::Invalid("invalid entity-v1 envelope".into())
}
fn dict(entries: impl IntoIterator<Item = (&'static str, Kind)>) -> HDict {
    let mut d = HDict::new();
    for (k, v) in entries {
        d.set(k, v);
    }
    d
}
fn kd(d: HDict) -> Kind {
    Kind::Dict(Box::new(d))
}
fn text(s: impl Into<String>) -> Kind {
    Kind::Str(s.into())
}
fn uint(n: u64) -> Kind {
    text(n.to_string())
}
fn fields<'a>(v: &'a Kind, names: &[&str]) -> Result<&'a HDict, TypedPayloadError> {
    let Kind::Dict(d) = v else {
        return Err(invalid());
    };
    if d.len() != names.len() || d.tag_names().any(|n| !names.contains(&n)) {
        return Err(invalid());
    }
    Ok(d)
}
fn string<'a>(d: &'a HDict, k: &str) -> Result<&'a str, TypedPayloadError> {
    match d.get(k) {
        Some(Kind::Str(s)) => Ok(s),
        _ => Err(invalid()),
    }
}
fn number(d: &HDict, k: &str) -> Result<u64, TypedPayloadError> {
    let s = string(d, k)?;
    let n = s.parse::<u64>().map_err(|_| invalid())?;
    if n.to_string() != s {
        return Err(invalid());
    }
    Ok(n)
}
fn value<'a>(d: &'a HDict, k: &str) -> Result<&'a Kind, TypedPayloadError> {
    d.get(k).ok_or_else(invalid)
}
fn hex(id: &[u8; 16]) -> String {
    id.iter().map(|v| format!("{v:02x}")).collect()
}
fn parse_hex(s: &str) -> Result<[u8; 16], TypedPayloadError> {
    if s.len() != 32
        || !s
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(invalid());
    }
    let mut id = [0; 16];
    for (i, b) in id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(id)
}
fn identity_kind(i: &OperationIdentity) -> Result<Kind, TypedPayloadError> {
    if i.operation_id.is_empty()
        || i.operation_id.len() > 128
        || i.operation_id.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    Ok(kd(dict([
        ("operationId", text(&i.operation_id)),
        ("dataset", text(hex(&i.dataset))),
        ("incarnation", text(hex(&i.incarnation))),
    ])))
}
fn identity(v: &Kind) -> Result<OperationIdentity, TypedPayloadError> {
    let d = fields(v, &["operationId", "dataset", "incarnation"])?;
    let i = OperationIdentity {
        operation_id: string(d, "operationId")?.into(),
        dataset: parse_hex(string(d, "dataset")?)?,
        incarnation: parse_hex(string(d, "incarnation")?)?,
    };
    identity_kind(&i)?;
    Ok(i)
}
fn span_kind(s: Option<CommitSpan>) -> Kind {
    s.map_or(Kind::Null, |s| {
        kd(dict([("first", uint(s.first)), ("last", uint(s.last))]))
    })
}
fn span(v: &Kind) -> Result<Option<CommitSpan>, TypedPayloadError> {
    if matches!(v, Kind::Null) {
        return Ok(None);
    }
    let d = fields(v, &["first", "last"])?;
    let s = CommitSpan {
        first: number(d, "first")?,
        last: number(d, "last")?,
    };
    if s.first == 0 || s.first > s.last {
        return Err(invalid());
    }
    Ok(Some(s))
}
fn checked_row(v: &Kind) -> Result<HDict, TypedPayloadError> {
    match v {
        Kind::Dict(d) => Ok(d.as_ref().clone()),
        _ => Err(invalid()),
    }
}
fn envelope(kind: &str, body: Kind) -> Kind {
    kd(dict([
        ("schema", text("entity-v1")),
        ("type", text(kind)),
        ("body", body),
    ]))
}
fn body<'a>(v: &'a Kind, kind: &str) -> Result<&'a Kind, TypedPayloadError> {
    let d = fields(v, &["schema", "type", "body"])?;
    if string(d, "schema")? != "entity-v1" || string(d, "type")? != kind {
        return Err(invalid());
    }
    value(d, "body")
}

/// Shared strict schema. Implementations validate source bounds before cloning
/// source-sized values into the typed envelope.
pub trait EntityWire: Sized {
    fn to_kind(&self) -> Result<Kind, TypedPayloadError>;
    fn from_kind(value: &Kind) -> Result<Self, TypedPayloadError>;
}
pub fn encode<T: EntityWire>(value: &T) -> Result<Vec<u8>, TypedPayloadError> {
    typed::encode(&value.to_kind()?)
}
pub fn decode<T: EntityWire>(bytes: &[u8]) -> Result<T, TypedPayloadError> {
    T::from_kind(&typed::decode(bytes)?)
}
pub fn to_grid<T: EntityWire>(value: &T) -> Result<HGrid, TypedPayloadError> {
    let payload = String::from_utf8(encode(value)?).map_err(|_| invalid())?;
    Ok(HGrid {
        meta: HDict::new(),
        cols: vec![HCol::new("payload")],
        rows: vec![dict([("payload", text(payload))])],
    })
}
pub fn from_grid<T: EntityWire>(grid: &HGrid) -> Result<T, TypedPayloadError> {
    if !grid.meta.is_empty()
        || grid.cols.len() != 1
        || grid.cols[0].name != "payload"
        || !grid.cols[0].meta.is_empty()
        || grid.rows.len() != 1
        || grid.rows[0].len() != 1
    {
        return Err(invalid());
    }
    decode(string(&grid.rows[0], "payload")?.as_bytes())
}
fn measure_rows<'a>(rows: impl IntoIterator<Item = &'a HDict>) -> Result<(), TypedPayloadError> {
    let mut b = crate::graph::size::ValueBudget::new(100_000, 4 * MAX_PAYLOAD_BYTES, 48);
    for row in rows {
        b.dict(row, 1).map_err(|_| invalid())?;
    }
    Ok(())
}
impl EntityBatchRequest {
    /// Conservative source ownership charge, checked before envelope cloning.
    pub fn source_bytes(&self) -> Result<usize, TypedPayloadError> {
        if self.operations.is_empty() || self.operations.len() > MAX_OPERATIONS {
            return Err(invalid());
        }
        identity_kind(&self.identity)?;
        let mut budget = crate::graph::size::ValueBudget::new(100_000, 4 * MAX_PAYLOAD_BYTES, 48);
        for operation in &self.operations {
            let id = operation.target().map_err(|_| invalid())?;
            if id.is_empty() || id.len() > 1024 {
                return Err(invalid());
            }
            budget
                .charge(id.len().saturating_add(1), id.len().saturating_add(1024))
                .map_err(|_| invalid())?;
            match operation {
                EntityOperation::Add(row) => budget.dict(row, 1),
                EntityOperation::Patch { changes, .. } => budget.dict(changes, 1),
                EntityOperation::Remove { .. } => Ok(()),
            }
            .map_err(|_| invalid())?;
        }
        Ok(budget.used_bytes())
    }
}
impl EntityWire for EntityBatchRequest {
    fn to_kind(&self) -> Result<Kind, TypedPayloadError> {
        self.source_bytes()?;
        let mut ops = Vec::new();
        for op in &self.operations {
            let id = op.target().map_err(|_| invalid())?;
            if id.is_empty() || id.len() > 1024 {
                return Err(invalid());
            }
            ops.push(kd(match op {
                EntityOperation::Add(row) => {
                    dict([("operation", text("add")), ("row", kd(row.clone()))])
                }
                EntityOperation::Patch { id, changes } => dict([
                    ("operation", text("patch")),
                    ("id", text(id)),
                    ("changes", kd(changes.clone())),
                ]),
                EntityOperation::Remove { id } => {
                    dict([("operation", text("remove")), ("id", text(id))])
                }
            }));
        }
        Ok(envelope(
            "batch",
            kd(dict([
                ("identity", identity_kind(&self.identity)?),
                ("expectedRevision", uint(self.expected_revision)),
                ("operations", Kind::List(ops)),
            ])),
        ))
    }
    fn from_kind(v: &Kind) -> Result<Self, TypedPayloadError> {
        check_kind(v)?;
        let d = fields(
            body(v, "batch")?,
            &["identity", "expectedRevision", "operations"],
        )?;
        let Kind::List(ops) = value(d, "operations")? else {
            return Err(invalid());
        };
        if ops.is_empty() || ops.len() > MAX_OPERATIONS {
            return Err(invalid());
        }
        let mut operations = Vec::new();
        for op in ops {
            let Kind::Dict(raw) = op else {
                return Err(invalid());
            };
            operations.push(match string(raw, "operation")? {
                "add" => {
                    let d = fields(op, &["operation", "row"])?;
                    EntityOperation::Add(checked_row(value(d, "row")?)?)
                }
                "patch" => {
                    let d = fields(op, &["operation", "id", "changes"])?;
                    EntityOperation::Patch {
                        id: string(d, "id")?.into(),
                        changes: checked_row(value(d, "changes")?)?,
                    }
                }
                "remove" => {
                    let d = fields(op, &["operation", "id"])?;
                    EntityOperation::Remove {
                        id: string(d, "id")?.into(),
                    }
                }
                _ => return Err(invalid()),
            });
        }
        let r = Self {
            identity: identity(value(d, "identity")?)?,
            expected_revision: number(d, "expectedRevision")?,
            operations,
        };
        r.to_kind()?;
        Ok(r)
    }
}
impl EntityWire for OperationIdentity {
    fn to_kind(&self) -> Result<Kind, TypedPayloadError> {
        Ok(envelope("receiptRequest", identity_kind(self)?))
    }
    fn from_kind(v: &Kind) -> Result<Self, TypedPayloadError> {
        check_kind(v)?;
        identity(body(v, "receiptRequest")?)
    }
}
fn reason_str(r: RejectionReason) -> &'static str {
    match r {
        RejectionReason::Invalid => "invalid",
        RejectionReason::Forbidden => "forbidden",
        RejectionReason::Conflict => "conflict",
        RejectionReason::Capacity => "capacity",
        RejectionReason::Limit => "limit",
        RejectionReason::Cancelled => "cancelled",
        RejectionReason::Deadline => "deadline",
        RejectionReason::Provider => "provider",
    }
}
fn parse_reason(s: &str) -> Result<RejectionReason, TypedPayloadError> {
    Ok(match s {
        "invalid" => RejectionReason::Invalid,
        "forbidden" => RejectionReason::Forbidden,
        "conflict" => RejectionReason::Conflict,
        "capacity" => RejectionReason::Capacity,
        "limit" => RejectionReason::Limit,
        "cancelled" => RejectionReason::Cancelled,
        "deadline" => RejectionReason::Deadline,
        "provider" => RejectionReason::Provider,
        _ => return Err(invalid()),
    })
}
fn cause_str(c: UnknownCause) -> &'static str {
    match c {
        UnknownCause::Missing => "missing",
        UnknownCause::Pending => "pending",
        UnknownCause::Provider => "provider",
        UnknownCause::Transport => "transport",
        UnknownCause::InvalidAcknowledgement => "invalidAcknowledgement",
    }
}
fn parse_cause(s: &str) -> Result<UnknownCause, TypedPayloadError> {
    Ok(match s {
        "missing" => UnknownCause::Missing,
        "pending" => UnknownCause::Pending,
        "provider" => UnknownCause::Provider,
        "transport" => UnknownCause::Transport,
        "invalidAcknowledgement" => UnknownCause::InvalidAcknowledgement,
        _ => return Err(invalid()),
    })
}
impl EntityWire for MutationOutcome {
    fn to_kind(&self) -> Result<Kind, TypedPayloadError> {
        let mut d = dict([("identity", identity_kind(self.identity())?)]);
        match self {
            Self::Committed(r) => {
                validate_receipt(r)?;
                d.set("outcome", text("committed"));
                d.set("beforeRevision", uint(r.before_revision));
                d.set("afterRevision", uint(r.after_revision));
                d.set("span", span_kind(r.span));
                d.set(
                    "qualification",
                    text(match r.qualification {
                        ReceiptQualification::EphemeralMemory => "ephemeralMemory",
                        ReceiptQualification::ProviderProtocol => "providerProtocol",
                    }),
                );
            }
            Self::Rejected { reason, .. } => {
                d.set("outcome", text("rejected"));
                d.set("reason", text(reason_str(*reason)));
            }
            Self::Unknown { cause, .. } => {
                d.set("outcome", text("unknown"));
                d.set("cause", text(cause_str(*cause)));
            }
        }
        Ok(envelope("outcome", kd(d)))
    }
    fn from_kind(v: &Kind) -> Result<Self, TypedPayloadError> {
        check_kind(v)?;
        let b = body(v, "outcome")?;
        let Kind::Dict(d) = b else {
            return Err(invalid());
        };
        let id = identity(value(d, "identity")?)?;
        Ok(match string(d, "outcome")? {
            "committed" => {
                fields(
                    b,
                    &[
                        "identity",
                        "outcome",
                        "beforeRevision",
                        "afterRevision",
                        "span",
                        "qualification",
                    ],
                )?;
                let r = EntityReceipt {
                    identity: id,
                    before_revision: number(d, "beforeRevision")?,
                    after_revision: number(d, "afterRevision")?,
                    span: span(value(d, "span")?)?,
                    qualification: match string(d, "qualification")? {
                        "ephemeralMemory" => ReceiptQualification::EphemeralMemory,
                        "providerProtocol" => ReceiptQualification::ProviderProtocol,
                        _ => return Err(invalid()),
                    },
                };
                validate_receipt(&r)?;
                Self::Committed(r)
            }
            "rejected" => {
                fields(b, &["identity", "outcome", "reason"])?;
                Self::Rejected {
                    identity: id,
                    reason: parse_reason(string(d, "reason")?)?,
                }
            }
            "unknown" => {
                fields(b, &["identity", "outcome", "cause"])?;
                Self::Unknown {
                    identity: id,
                    cause: parse_cause(string(d, "cause")?)?,
                }
            }
            _ => return Err(invalid()),
        })
    }
}
fn validate_receipt(r: &EntityReceipt) -> Result<(), TypedPayloadError> {
    if r.after_revision < r.before_revision
        || match r.span {
            None => r.after_revision != r.before_revision,
            Some(s) => {
                r.before_revision.checked_add(1) != Some(s.first)
                    || s.last != r.after_revision
                    || s.first > s.last
            }
        }
    {
        return Err(invalid());
    }
    Ok(())
}
impl EntityWire for ChangesRequest {
    fn to_kind(&self) -> Result<Kind, TypedPayloadError> {
        if self.max_diffs == 0
            || self.max_diffs > MAX_OPERATIONS
            || self.cursor.as_ref().is_some_and(|s| s.len() > 1024)
        {
            return Err(invalid());
        }
        Ok(envelope(
            "changesRequest",
            kd(dict([
                ("cursor", self.cursor.as_ref().map_or(Kind::Null, text)),
                ("maxDiffs", uint(self.max_diffs as u64)),
            ])),
        ))
    }
    fn from_kind(v: &Kind) -> Result<Self, TypedPayloadError> {
        check_kind(v)?;
        let d = fields(body(v, "changesRequest")?, &["cursor", "maxDiffs"])?;
        let r = Self {
            cursor: match value(d, "cursor")? {
                Kind::Null => None,
                Kind::Str(s) => Some(s.clone()),
                _ => return Err(invalid()),
            },
            max_diffs: usize::try_from(number(d, "maxDiffs")?).map_err(|_| invalid())?,
        };
        r.to_kind()?;
        Ok(r)
    }
}
impl EntityWire for ChangesPage {
    fn to_kind(&self) -> Result<Kind, TypedPayloadError> {
        if self.changes.len() > MAX_OPERATIONS
            || self
                .changes
                .iter()
                .any(|c| c.id.is_empty() || c.id.len() > 1024)
            || self.cursor.is_empty()
            || self.cursor.len() > 1024
            || self.floor > self.position
            || self.position > self.head
        {
            return Err(invalid());
        }
        measure_rows(
            self.changes
                .iter()
                .flat_map(|c| std::iter::once(&c.changed).chain(c.previous.iter())),
        )?;
        let changes = self
            .changes
            .iter()
            .map(|c| {
                kd(dict([
                    ("revision", uint(c.revision)),
                    ("span", span_kind(Some(c.span))),
                    ("id", text(&c.id)),
                    (
                        "operation",
                        text(match c.operation {
                            DiffOp::Add => "add",
                            DiffOp::Update => "patch",
                            DiffOp::Remove => "remove",
                        }),
                    ),
                    ("changed", kd(c.changed.clone())),
                    (
                        "previous",
                        c.previous.as_ref().map_or(Kind::Null, |d| kd(d.clone())),
                    ),
                ]))
            })
            .collect();
        Ok(envelope(
            "changesPage",
            kd(dict([
                ("dataset", text(hex(&self.dataset))),
                ("incarnation", text(hex(&self.incarnation))),
                ("head", uint(self.head)),
                ("floor", uint(self.floor)),
                ("position", uint(self.position)),
                ("cursor", text(&self.cursor)),
                ("complete", Kind::Bool(self.complete)),
                ("changes", Kind::List(changes)),
            ])),
        ))
    }
    fn from_kind(v: &Kind) -> Result<Self, TypedPayloadError> {
        check_kind(v)?;
        let d = fields(
            body(v, "changesPage")?,
            &[
                "dataset",
                "incarnation",
                "head",
                "floor",
                "position",
                "cursor",
                "complete",
                "changes",
            ],
        )?;
        let Kind::List(items) = value(d, "changes")? else {
            return Err(invalid());
        };
        if items.len() > MAX_OPERATIONS {
            return Err(invalid());
        }
        let mut changes = Vec::new();
        let mut last = 0;
        let mut previous_span: Option<CommitSpan> = None;
        let floor = number(d, "floor")?;
        let position = number(d, "position")?;
        for item in items {
            let c = fields(
                item,
                &["revision", "span", "id", "operation", "changed", "previous"],
            )?;
            let s = span(value(c, "span")?)?.ok_or_else(invalid)?;
            let revision = number(c, "revision")?;
            if revision < s.first
                || revision > s.last
                || revision <= last
                || s.first <= floor
                || s.last > position
                || previous_span.is_some_and(|old| old != s && s.first <= old.last)
            {
                return Err(invalid());
            }
            last = revision;
            previous_span = Some(s);
            changes.push(EntityDiff {
                revision,
                span: s,
                id: string(c, "id")?.into(),
                operation: match string(c, "operation")? {
                    "add" => DiffOp::Add,
                    "patch" => DiffOp::Update,
                    "remove" => DiffOp::Remove,
                    _ => return Err(invalid()),
                },
                changed: checked_row(value(c, "changed")?)?,
                previous: match value(c, "previous")? {
                    Kind::Null => None,
                    v => Some(checked_row(v)?),
                },
            });
        }
        let r = Self {
            dataset: parse_hex(string(d, "dataset")?)?,
            incarnation: parse_hex(string(d, "incarnation")?)?,
            head: number(d, "head")?,
            floor: number(d, "floor")?,
            position: number(d, "position")?,
            cursor: string(d, "cursor")?.into(),
            complete: match value(d, "complete")? {
                Kind::Bool(v) => *v,
                _ => return Err(invalid()),
            },
            changes,
        };
        r.to_kind()?;
        if last > r.position || r.complete != (r.position == r.head) {
            return Err(invalid());
        }
        Ok(r)
    }
}

/// Validate the small outer grid grammar before invoking a general legacy
/// decoder. Quoted payload bytes may be large; nesting, unquoted syntax and the
/// number of strings remain small. The payload itself has typed-v1 limits.
pub fn decode_grid<T: EntityWire>(
    bytes: &[u8],
    codec: &dyn super::Codec,
) -> Result<T, TypedPayloadError> {
    if bytes.len() > MAX_GRID_BYTES {
        return Err(invalid());
    }
    let source = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let (mut quoted, mut escape, mut syntax, mut strings, mut depth) =
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
            if syntax > 8192 {
                return Err(invalid());
            }
            match byte {
                b'"' => {
                    quoted = true;
                    strings += 1;
                    if strings > 64 {
                        return Err(invalid());
                    }
                }
                b'[' | b'{' => {
                    depth += 1;
                    if depth > 8 {
                        return Err(invalid());
                    }
                }
                b']' | b'}' => depth = depth.checked_sub(1).ok_or_else(invalid)?,
                _ => {}
            }
        }
    }
    if quoted || depth != 0 {
        return Err(invalid());
    }
    let grid = codec.decode_grid(source).map_err(|_| invalid())?;
    from_grid(&grid)
}
pub fn encode_grid<T: EntityWire>(
    value: &T,
    codec: &dyn super::Codec,
) -> Result<Vec<u8>, TypedPayloadError> {
    let bytes = codec
        .encode_grid(&to_grid(value)?)
        .map_err(|_| invalid())?
        .into_bytes();
    if bytes.len() > MAX_GRID_BYTES {
        return Err(invalid());
    }
    Ok(bytes)
}
