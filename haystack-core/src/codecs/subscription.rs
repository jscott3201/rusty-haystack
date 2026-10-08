//! Bounded state-subscription-v1 DTOs shared by native, H4 and WebSocket adapters.
//! The outer H4 grid has one typed-v1 `payload` STR cell. Payloads preserve native
//! values; this is an application extension, not a Haystack 5 wire format.
use super::{
    entity::{self, EntityWire},
    typed::TypedPayloadError,
};
use crate::{data::HDict, kinds::Kind};
use std::{collections::BTreeSet, sync::Arc};

pub const PROFILE: &str = "state-subscription-v1";
pub const WS_PROTOCOL: &str = "haystack.state-subscription.v1";
pub const MAX_IDS: usize = 1000;
pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_LEASE_MS: u64 = 3_600_000;
pub const MAX_PAYLOAD_BYTES: usize = entity::MAX_PAYLOAD_BYTES;
pub const MAX_WIRE_BYTES: usize = entity::MAX_GRID_BYTES;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubscriptionId {
    pub authority: [u8; 16],
    pub watch: [u8; 16],
    /// Caller-known creation key; a body identity never grants session authority.
    pub key: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionCreate {
    pub authority: [u8; 16],
    pub key: String,
    /// Original order and duplicates bind the creation intent before normalization.
    pub ids: Vec<String>,
    pub lease_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionRequest {
    Describe,
    Create(SubscriptionCreate),
    Poll {
        watch: SubscriptionId,
    },
    Resume {
        watch: SubscriptionId,
        scope_generation: u64,
        acknowledged: u64,
    },
    Acknowledge {
        watch: SubscriptionId,
        scope_generation: u64,
        token: [u8; 16],
        through: u64,
    },
    Renew {
        watch: SubscriptionId,
        lease_ms: u64,
    },
    Replace {
        watch: SubscriptionId,
        expected_scope_generation: u64,
        ids: Vec<String>,
    },
    Unsubscribe {
        watch: SubscriptionId,
    },
}
impl SubscriptionRequest {
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Describe => "watchInfo",
            Self::Create(_) | Self::Replace { .. } => "watchSub",
            Self::Poll { .. } | Self::Resume { .. } => "watchPoll",
            Self::Acknowledge { .. } => "watchAck",
            Self::Renew { .. } => "watchRenew",
            Self::Unsubscribe { .. } => "watchUnsub",
        }
    }
    pub fn watch(&self) -> Option<&SubscriptionId> {
        match self {
            Self::Describe | Self::Create(_) => None,
            Self::Poll { watch }
            | Self::Resume { watch, .. }
            | Self::Acknowledge { watch, .. }
            | Self::Renew { watch, .. }
            | Self::Replace { watch, .. }
            | Self::Unsubscribe { watch } => Some(watch),
        }
    }
    pub fn source_bytes(&self) -> Result<usize, TypedPayloadError> {
        let mut bytes = 512usize;
        if let Some(watch) = self.watch() {
            check_key(&watch.key)?;
            bytes += watch.key.len();
        }
        match self {
            Self::Create(create) => {
                check_key(&create.key)?;
                check_lease(create.lease_ms)?;
                bytes += create.key.len() + ids_bytes(&create.ids, false)?;
            }
            Self::Replace {
                ids,
                expected_scope_generation,
                ..
            } => {
                if *expected_scope_generation == 0 {
                    return Err(invalid());
                }
                bytes += ids_bytes(ids, true)?;
            }
            Self::Renew { lease_ms, .. } => check_lease(*lease_ms)?,
            Self::Resume {
                scope_generation, ..
            }
            | Self::Acknowledge {
                scope_generation, ..
            } if *scope_generation == 0 => return Err(invalid()),
            _ => {}
        }
        if bytes > 65_536 {
            return Err(invalid());
        }
        Ok(bytes)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionRejection {
    Invalid,
    Forbidden,
    Conflict,
    Capacity,
    Limit,
    Pending,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionResync {
    Authority,
    SessionExpired,
    Revoked,
    LeaseExpired,
    Gap,
    Incarnation,
    Catalog,
    Policy,
    ScopeChanged,
    Shutdown,
    Overflow,
    Unknown,
}
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionDelivery {
    pub watch: SubscriptionId,
    pub dataset: [u8; 16],
    pub incarnation: [u8; 16],
    pub catalog_generation: u64,
    pub scope_generation: u64,
    pub token: [u8; 16],
    pub from: u64,
    pub through: u64,
    pub initial: bool,
    /// Complete current records for changed watched entities, never partial tag patches.
    pub rows: Vec<HDict>,
    /// Only authorized identities that existed in the acknowledged projection.
    pub removed: Vec<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionOutcome {
    Authority {
        authority: [u8; 16],
        dataset: [u8; 16],
    },
    Delivery(Arc<SubscriptionDelivery>),
    Idle {
        watch: SubscriptionId,
        scope_generation: u64,
        acknowledged: u64,
    },
    Acknowledged {
        watch: SubscriptionId,
        scope_generation: u64,
        token: [u8; 16],
        through: u64,
    },
    Renewed {
        watch: SubscriptionId,
        lease_ms: u64,
    },
    Closed {
        watch: SubscriptionId,
    },
    Rejected(SubscriptionRejection),
    Resync(SubscriptionResync),
    /// The operation may have taken effect. Reconcile explicitly; never auto-replay.
    Unknown,
}
impl SubscriptionOutcome {
    pub fn watch(&self) -> Option<&SubscriptionId> {
        match self {
            Self::Delivery(delivery) => Some(&delivery.watch),
            Self::Idle { watch, .. }
            | Self::Acknowledged { watch, .. }
            | Self::Renewed { watch, .. }
            | Self::Closed { watch } => Some(watch),
            _ => None,
        }
    }
    /// Bound variable-sized data before any envelope clone or encoder allocation.
    pub fn source_bytes(&self) -> Result<usize, TypedPayloadError> {
        let mut bytes = 1024usize;
        if let Some(watch) = self.watch() {
            check_key(&watch.key)?;
            bytes += watch.key.len();
        }
        match self {
            Self::Delivery(delivery) => {
                if delivery.rows.len() > MAX_IDS
                    || delivery.removed.len() > MAX_IDS
                    || delivery.scope_generation == 0
                    || delivery.from > delivery.through
                    || (delivery.initial
                        && (delivery.from != delivery.through || !delivery.removed.is_empty()))
                {
                    return Err(invalid());
                }
                let mut budget =
                    crate::graph::size::ValueBudget::new(100_000, MAX_PAYLOAD_BYTES * 4, 48);
                let mut ids = BTreeSet::new();
                for row in &delivery.rows {
                    budget.dict(row, 1).map_err(|_| invalid())?;
                    let Some(Kind::Ref(id)) = row.get("id") else {
                        return Err(invalid());
                    };
                    check_id(&id.val)?;
                    if !ids.insert(id.val.as_str()) {
                        return Err(invalid());
                    }
                }
                for id in &delivery.removed {
                    check_id(id)?;
                    if !ids.insert(id) {
                        return Err(invalid());
                    }
                }
                bytes = bytes.saturating_add(budget.used_bytes()).saturating_add(
                    delivery
                        .removed
                        .iter()
                        .map(|id| id.len() + 64)
                        .sum::<usize>(),
                );
            }
            Self::Idle {
                scope_generation, ..
            }
            | Self::Acknowledged {
                scope_generation, ..
            } if *scope_generation == 0 => return Err(invalid()),
            Self::Renewed { lease_ms, .. } => check_lease(*lease_ms)?,
            _ => {}
        }
        Ok(bytes)
    }
}
fn invalid() -> TypedPayloadError {
    TypedPayloadError::Invalid("invalid state subscription envelope".into())
}
fn check_key(key: &str) -> Result<(), TypedPayloadError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn check_id(id: &str) -> Result<(), TypedPayloadError> {
    if id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control) {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn check_lease(lease: u64) -> Result<(), TypedPayloadError> {
    if lease == 0 || lease > MAX_LEASE_MS {
        Err(invalid())
    } else {
        Ok(())
    }
}
fn ids_bytes(ids: &[String], allow_empty: bool) -> Result<usize, TypedPayloadError> {
    if (!allow_empty && ids.is_empty()) || ids.len() > MAX_IDS {
        return Err(invalid());
    }
    let mut bytes = 0usize;
    for id in ids {
        check_id(id)?;
        bytes = bytes.saturating_add(id.len() + 64);
        if bytes > 65_536 {
            return Err(invalid());
        }
    }
    Ok(bytes)
}
fn text(value: impl Into<String>) -> Kind {
    Kind::Str(value.into())
}
fn uint(value: u64) -> Kind {
    text(value.to_string())
}
fn dict(entries: impl IntoIterator<Item = (&'static str, Kind)>) -> Kind {
    let mut d = HDict::new();
    for (key, value) in entries {
        d.set(key, value);
    }
    Kind::Dict(Box::new(d))
}
fn hex(value: &[u8; 16]) -> Kind {
    text(value.iter().map(|v| format!("{v:02x}")).collect::<String>())
}
fn parse_hex(value: &Kind) -> Result<[u8; 16], TypedPayloadError> {
    let Kind::Str(s) = value else {
        return Err(invalid());
    };
    if s.len() != 32
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    let mut out = [0; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(out)
}
fn fields<'a>(value: &'a Kind, names: &[&str]) -> Result<&'a HDict, TypedPayloadError> {
    let Kind::Dict(d) = value else {
        return Err(invalid());
    };
    if d.len() != names.len() || !names.iter().all(|n| d.has(n)) {
        return Err(invalid());
    }
    Ok(d)
}
fn field<'a>(d: &'a HDict, key: &str) -> Result<&'a Kind, TypedPayloadError> {
    d.get(key).ok_or_else(invalid)
}
fn string<'a>(d: &'a HDict, key: &str) -> Result<&'a str, TypedPayloadError> {
    let Kind::Str(s) = field(d, key)? else {
        return Err(invalid());
    };
    Ok(s)
}
fn number(d: &HDict, key: &str) -> Result<u64, TypedPayloadError> {
    let s = string(d, key)?;
    let n = s.parse::<u64>().map_err(|_| invalid())?;
    if n.to_string() != s {
        return Err(invalid());
    }
    Ok(n)
}
fn identity(id: &SubscriptionId) -> Kind {
    dict([
        ("authority", hex(&id.authority)),
        ("watch", hex(&id.watch)),
        ("key", text(&id.key)),
    ])
}
fn parse_identity(value: &Kind) -> Result<SubscriptionId, TypedPayloadError> {
    let d = fields(value, &["authority", "watch", "key"])?;
    let key = string(d, "key")?;
    check_key(key)?;
    Ok(SubscriptionId {
        authority: parse_hex(field(d, "authority")?)?,
        watch: parse_hex(field(d, "watch")?)?,
        key: key.into(),
    })
}
fn ids_kind(ids: &[String]) -> Kind {
    Kind::List(ids.iter().map(text).collect())
}
fn parse_ids(value: &Kind, allow_empty: bool) -> Result<Vec<String>, TypedPayloadError> {
    let Kind::List(ids) = value else {
        return Err(invalid());
    };
    if ids.len() > MAX_IDS || (!allow_empty && ids.is_empty()) {
        return Err(invalid());
    }
    let mut bytes = 0usize;
    ids.iter()
        .map(|value| {
            let Kind::Str(id) = value else {
                return Err(invalid());
            };
            check_id(id)?;
            bytes = bytes.saturating_add(id.len() + 64);
            if bytes > 65_536 {
                return Err(invalid());
            }
            Ok(id.clone())
        })
        .collect()
}
fn envelope(kind: &str, body: Kind) -> Kind {
    dict([
        ("profile", text(PROFILE)),
        ("type", text(kind)),
        ("body", body),
    ])
}
fn open(value: &Kind) -> Result<(&str, &Kind), TypedPayloadError> {
    let d = fields(value, &["profile", "type", "body"])?;
    if string(d, "profile")? != PROFILE {
        return Err(invalid());
    }
    Ok((string(d, "type")?, field(d, "body")?))
}
impl EntityWire for SubscriptionRequest {
    fn to_kind(&self) -> Result<Kind, TypedPayloadError> {
        self.source_bytes()?;
        let (kind, body) = match self {
            Self::Describe => ("describe", dict([])),
            Self::Create(c) => (
                "create",
                dict([
                    ("authority", hex(&c.authority)),
                    ("key", text(&c.key)),
                    ("ids", ids_kind(&c.ids)),
                    ("leaseMs", uint(c.lease_ms)),
                ]),
            ),
            Self::Poll { watch } => ("poll", dict([("watch", identity(watch))])),
            Self::Resume {
                watch,
                scope_generation,
                acknowledged,
            } => (
                "resume",
                dict([
                    ("watch", identity(watch)),
                    ("scopeGeneration", uint(*scope_generation)),
                    ("acknowledged", uint(*acknowledged)),
                ]),
            ),
            Self::Acknowledge {
                watch,
                scope_generation,
                token,
                through,
            } => (
                "acknowledge",
                dict([
                    ("watch", identity(watch)),
                    ("scopeGeneration", uint(*scope_generation)),
                    ("token", hex(token)),
                    ("through", uint(*through)),
                ]),
            ),
            Self::Renew { watch, lease_ms } => (
                "renew",
                dict([("watch", identity(watch)), ("leaseMs", uint(*lease_ms))]),
            ),
            Self::Replace {
                watch,
                expected_scope_generation,
                ids,
            } => (
                "replace",
                dict([
                    ("watch", identity(watch)),
                    ("scopeGeneration", uint(*expected_scope_generation)),
                    ("ids", ids_kind(ids)),
                ]),
            ),
            Self::Unsubscribe { watch } => ("unsubscribe", dict([("watch", identity(watch))])),
        };
        Ok(envelope(kind, body))
    }
    fn from_kind(value: &Kind) -> Result<Self, TypedPayloadError> {
        let (kind, body) = open(value)?;
        let value = match kind {
            "describe" => {
                fields(body, &[])?;
                Self::Describe
            }
            "create" => {
                let d = fields(body, &["authority", "key", "ids", "leaseMs"])?;
                let key = string(d, "key")?;
                check_key(key)?;
                Self::Create(SubscriptionCreate {
                    authority: parse_hex(field(d, "authority")?)?,
                    key: key.into(),
                    ids: parse_ids(field(d, "ids")?, false)?,
                    lease_ms: number(d, "leaseMs")?,
                })
            }
            "poll" | "unsubscribe" => {
                let d = fields(body, &["watch"])?;
                let watch = parse_identity(field(d, "watch")?)?;
                if kind == "poll" {
                    Self::Poll { watch }
                } else {
                    Self::Unsubscribe { watch }
                }
            }
            "resume" => {
                let d = fields(body, &["watch", "scopeGeneration", "acknowledged"])?;
                Self::Resume {
                    watch: parse_identity(field(d, "watch")?)?,
                    scope_generation: number(d, "scopeGeneration")?,
                    acknowledged: number(d, "acknowledged")?,
                }
            }
            "acknowledge" => {
                let d = fields(body, &["watch", "scopeGeneration", "token", "through"])?;
                Self::Acknowledge {
                    watch: parse_identity(field(d, "watch")?)?,
                    scope_generation: number(d, "scopeGeneration")?,
                    token: parse_hex(field(d, "token")?)?,
                    through: number(d, "through")?,
                }
            }
            "renew" => {
                let d = fields(body, &["watch", "leaseMs"])?;
                Self::Renew {
                    watch: parse_identity(field(d, "watch")?)?,
                    lease_ms: number(d, "leaseMs")?,
                }
            }
            "replace" => {
                let d = fields(body, &["watch", "scopeGeneration", "ids"])?;
                Self::Replace {
                    watch: parse_identity(field(d, "watch")?)?,
                    expected_scope_generation: number(d, "scopeGeneration")?,
                    ids: parse_ids(field(d, "ids")?, true)?,
                }
            }
            _ => return Err(invalid()),
        };
        value.source_bytes()?;
        Ok(value)
    }
}
fn rejection_name(value: SubscriptionRejection) -> &'static str {
    match value {
        SubscriptionRejection::Invalid => "invalid",
        SubscriptionRejection::Forbidden => "forbidden",
        SubscriptionRejection::Conflict => "conflict",
        SubscriptionRejection::Capacity => "capacity",
        SubscriptionRejection::Limit => "limit",
        SubscriptionRejection::Pending => "pending",
    }
}
fn rejection(value: &str) -> Result<SubscriptionRejection, TypedPayloadError> {
    Ok(match value {
        "invalid" => SubscriptionRejection::Invalid,
        "forbidden" => SubscriptionRejection::Forbidden,
        "conflict" => SubscriptionRejection::Conflict,
        "capacity" => SubscriptionRejection::Capacity,
        "limit" => SubscriptionRejection::Limit,
        "pending" => SubscriptionRejection::Pending,
        _ => return Err(invalid()),
    })
}
fn resync_name(value: SubscriptionResync) -> &'static str {
    match value {
        SubscriptionResync::Authority => "authority",
        SubscriptionResync::SessionExpired => "sessionExpired",
        SubscriptionResync::Revoked => "revoked",
        SubscriptionResync::LeaseExpired => "leaseExpired",
        SubscriptionResync::Gap => "gap",
        SubscriptionResync::Incarnation => "incarnation",
        SubscriptionResync::Catalog => "catalog",
        SubscriptionResync::Policy => "policy",
        SubscriptionResync::ScopeChanged => "scopeChanged",
        SubscriptionResync::Shutdown => "shutdown",
        SubscriptionResync::Overflow => "overflow",
        SubscriptionResync::Unknown => "unknown",
    }
}
fn resync(value: &str) -> Result<SubscriptionResync, TypedPayloadError> {
    Ok(match value {
        "authority" => SubscriptionResync::Authority,
        "sessionExpired" => SubscriptionResync::SessionExpired,
        "revoked" => SubscriptionResync::Revoked,
        "leaseExpired" => SubscriptionResync::LeaseExpired,
        "gap" => SubscriptionResync::Gap,
        "incarnation" => SubscriptionResync::Incarnation,
        "catalog" => SubscriptionResync::Catalog,
        "policy" => SubscriptionResync::Policy,
        "scopeChanged" => SubscriptionResync::ScopeChanged,
        "shutdown" => SubscriptionResync::Shutdown,
        "overflow" => SubscriptionResync::Overflow,
        "unknown" => SubscriptionResync::Unknown,
        _ => return Err(invalid()),
    })
}
impl EntityWire for SubscriptionOutcome {
    fn to_kind(&self) -> Result<Kind, TypedPayloadError> {
        self.source_bytes()?;
        let (kind, body) = match self {
            Self::Authority { authority, dataset } => (
                "authority",
                dict([("authority", hex(authority)), ("dataset", hex(dataset))]),
            ),
            Self::Delivery(v) => (
                "delivery",
                dict([
                    ("watch", identity(&v.watch)),
                    ("dataset", hex(&v.dataset)),
                    ("incarnation", hex(&v.incarnation)),
                    ("catalogGeneration", uint(v.catalog_generation)),
                    ("scopeGeneration", uint(v.scope_generation)),
                    ("token", hex(&v.token)),
                    ("from", uint(v.from)),
                    ("through", uint(v.through)),
                    ("initial", Kind::Bool(v.initial)),
                    (
                        "rows",
                        Kind::List(
                            v.rows
                                .iter()
                                .map(|row| Kind::Dict(Box::new(row.clone())))
                                .collect(),
                        ),
                    ),
                    ("removed", ids_kind(&v.removed)),
                ]),
            ),
            Self::Idle {
                watch,
                scope_generation,
                acknowledged,
            } => (
                "idle",
                dict([
                    ("watch", identity(watch)),
                    ("scopeGeneration", uint(*scope_generation)),
                    ("acknowledged", uint(*acknowledged)),
                ]),
            ),
            Self::Acknowledged {
                watch,
                scope_generation,
                token,
                through,
            } => (
                "acknowledged",
                dict([
                    ("watch", identity(watch)),
                    ("scopeGeneration", uint(*scope_generation)),
                    ("token", hex(token)),
                    ("through", uint(*through)),
                ]),
            ),
            Self::Renewed { watch, lease_ms } => (
                "renewed",
                dict([("watch", identity(watch)), ("leaseMs", uint(*lease_ms))]),
            ),
            Self::Closed { watch } => ("closed", dict([("watch", identity(watch))])),
            Self::Rejected(reason) => (
                "rejected",
                dict([("reason", text(rejection_name(*reason)))]),
            ),
            Self::Resync(reason) => ("resync", dict([("reason", text(resync_name(*reason)))])),
            Self::Unknown => ("unknown", dict([])),
        };
        Ok(envelope(kind, body))
    }
    fn from_kind(value: &Kind) -> Result<Self, TypedPayloadError> {
        let (kind, body) = open(value)?;
        let value = match kind {
            "authority" => {
                let d = fields(body, &["authority", "dataset"])?;
                Self::Authority {
                    authority: parse_hex(field(d, "authority")?)?,
                    dataset: parse_hex(field(d, "dataset")?)?,
                }
            }
            "delivery" => {
                let d = fields(
                    body,
                    &[
                        "watch",
                        "dataset",
                        "incarnation",
                        "catalogGeneration",
                        "scopeGeneration",
                        "token",
                        "from",
                        "through",
                        "initial",
                        "rows",
                        "removed",
                    ],
                )?;
                let Kind::List(rows) = field(d, "rows")? else {
                    return Err(invalid());
                };
                if rows.len() > MAX_IDS {
                    return Err(invalid());
                }
                let mut budget =
                    crate::graph::size::ValueBudget::new(100_000, MAX_PAYLOAD_BYTES * 4, 48);
                for row in rows {
                    budget.value(row, 1).map_err(|_| invalid())?;
                }
                let rows = rows
                    .iter()
                    .map(|row| match row {
                        Kind::Dict(row) => Ok((**row).clone()),
                        _ => Err(invalid()),
                    })
                    .collect::<Result<_, _>>()?;
                let Kind::Bool(initial) = field(d, "initial")? else {
                    return Err(invalid());
                };
                Self::Delivery(Arc::new(SubscriptionDelivery {
                    watch: parse_identity(field(d, "watch")?)?,
                    dataset: parse_hex(field(d, "dataset")?)?,
                    incarnation: parse_hex(field(d, "incarnation")?)?,
                    catalog_generation: number(d, "catalogGeneration")?,
                    scope_generation: number(d, "scopeGeneration")?,
                    token: parse_hex(field(d, "token")?)?,
                    from: number(d, "from")?,
                    through: number(d, "through")?,
                    initial: *initial,
                    rows,
                    removed: parse_ids(field(d, "removed")?, true)?,
                }))
            }
            "idle" => {
                let d = fields(body, &["watch", "scopeGeneration", "acknowledged"])?;
                Self::Idle {
                    watch: parse_identity(field(d, "watch")?)?,
                    scope_generation: number(d, "scopeGeneration")?,
                    acknowledged: number(d, "acknowledged")?,
                }
            }
            "acknowledged" => {
                let d = fields(body, &["watch", "scopeGeneration", "token", "through"])?;
                Self::Acknowledged {
                    watch: parse_identity(field(d, "watch")?)?,
                    scope_generation: number(d, "scopeGeneration")?,
                    token: parse_hex(field(d, "token")?)?,
                    through: number(d, "through")?,
                }
            }
            "renewed" => {
                let d = fields(body, &["watch", "leaseMs"])?;
                Self::Renewed {
                    watch: parse_identity(field(d, "watch")?)?,
                    lease_ms: number(d, "leaseMs")?,
                }
            }
            "closed" => {
                let d = fields(body, &["watch"])?;
                Self::Closed {
                    watch: parse_identity(field(d, "watch")?)?,
                }
            }
            "rejected" => {
                let d = fields(body, &["reason"])?;
                Self::Rejected(rejection(string(d, "reason")?)?)
            }
            "resync" => {
                let d = fields(body, &["reason"])?;
                Self::Resync(resync(string(d, "reason")?)?)
            }
            "unknown" => {
                fields(body, &[])?;
                Self::Unknown
            }
            _ => return Err(invalid()),
        };
        value.source_bytes()?;
        Ok(value)
    }
}
/// Validate correlation before any adapter reports an operation as successful.
pub fn validate_for_request(
    outcome: &SubscriptionOutcome,
    request: &SubscriptionRequest,
) -> Result<(), TypedPayloadError> {
    request.source_bytes()?;
    outcome.source_bytes()?;
    if let Some(actual) = outcome.watch() {
        if let Some(expected) = request.watch() {
            if actual != expected {
                return Err(invalid());
            }
        } else if let SubscriptionRequest::Create(create) = request {
            if actual.authority != create.authority || actual.key != create.key {
                return Err(invalid());
            }
        } else {
            return Err(invalid());
        }
    }
    let valid = match (request, outcome) {
        (
            _,
            SubscriptionOutcome::Rejected(_)
            | SubscriptionOutcome::Resync(_)
            | SubscriptionOutcome::Unknown,
        ) => true,
        (SubscriptionRequest::Describe, SubscriptionOutcome::Authority { .. }) => true,
        (
            SubscriptionRequest::Create(_),
            SubscriptionOutcome::Delivery(_)
            | SubscriptionOutcome::Closed { .. }
            | SubscriptionOutcome::Idle { .. },
        ) => true,
        (
            SubscriptionRequest::Poll { .. },
            SubscriptionOutcome::Delivery(_)
            | SubscriptionOutcome::Idle { .. }
            | SubscriptionOutcome::Closed { .. },
        ) => true,
        (
            SubscriptionRequest::Resume {
                scope_generation,
                acknowledged,
                ..
            },
            SubscriptionOutcome::Delivery(delivery),
        ) => delivery.scope_generation == *scope_generation && delivery.from == *acknowledged,
        (
            SubscriptionRequest::Resume {
                scope_generation,
                acknowledged,
                ..
            },
            SubscriptionOutcome::Idle {
                scope_generation: actual_scope,
                acknowledged: actual_ack,
                ..
            },
        ) => scope_generation == actual_scope && acknowledged == actual_ack,
        (
            SubscriptionRequest::Replace {
                expected_scope_generation,
                ..
            },
            SubscriptionOutcome::Delivery(delivery),
        ) => {
            delivery.initial
                && expected_scope_generation.checked_add(1) == Some(delivery.scope_generation)
        }
        (
            SubscriptionRequest::Resume { .. }
            | SubscriptionRequest::Replace { .. }
            | SubscriptionRequest::Acknowledge { .. }
            | SubscriptionRequest::Renew { .. },
            SubscriptionOutcome::Closed { .. },
        ) => true,
        (
            SubscriptionRequest::Acknowledge {
                scope_generation,
                token,
                through,
                ..
            },
            SubscriptionOutcome::Acknowledged {
                scope_generation: actual_scope,
                token: actual_token,
                through: actual_through,
                ..
            },
        ) => scope_generation == actual_scope && token == actual_token && through == actual_through,
        (
            SubscriptionRequest::Renew { lease_ms, .. },
            SubscriptionOutcome::Renewed {
                lease_ms: actual, ..
            },
        ) => lease_ms == actual,
        (SubscriptionRequest::Unsubscribe { .. }, SubscriptionOutcome::Closed { .. }) => true,
        _ => false,
    };
    if valid { Ok(()) } else { Err(invalid()) }
}
pub use entity::{decode, encode, from_grid, to_grid};
fn allowed(codec: &dyn super::Codec) -> Result<(), TypedPayloadError> {
    match codec.mime_type() {
        "text/zinc" | "application/json" | "application/json;v=3" => Ok(()),
        _ => Err(invalid()),
    }
}
pub fn encode_grid<T: EntityWire>(
    value: &T,
    codec: &dyn super::Codec,
) -> Result<Vec<u8>, TypedPayloadError> {
    allowed(codec)?;
    entity::encode_grid(value, codec)
}
pub fn decode_grid<T: EntityWire>(
    bytes: &[u8],
    codec: &dyn super::Codec,
) -> Result<T, TypedPayloadError> {
    allowed(codec)?;
    // Reuse the bounded outer scanner. Zinc additionally requires complete rows
    // and scalars, rather than the permissive legacy grid decoder.
    let value = entity::decode_grid(bytes, codec)?;
    if codec.mime_type() == "text/zinc" {
        let source = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        from_grid::<T>(&super::zinc::decode_grid_complete_rows(source).map_err(|_| invalid())?)?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        codecs::codec_for,
        kinds::{HRef, Number},
    };
    fn watch() -> SubscriptionId {
        SubscriptionId {
            authority: [1; 16],
            watch: [2; 16],
            key: "known-create".into(),
        }
    }
    fn delivery() -> SubscriptionOutcome {
        let mut row = HDict::new();
        row.set("id", Kind::Ref(HRef::new("point", Some("表示".into()))));
        row.set(
            "value",
            Kind::Number(Number::new(
                f64::from_bits(0x3fd5555555555555),
                Some("fahrenheit".into()),
            )),
        );
        row.set("missing", Kind::NA);
        SubscriptionOutcome::Delivery(Arc::new(SubscriptionDelivery {
            watch: watch(),
            dataset: [3; 16],
            incarnation: [4; 16],
            catalog_generation: 9,
            scope_generation: 2,
            token: [5; 16],
            from: 10,
            through: 12,
            initial: false,
            rows: vec![row],
            removed: vec!["deleted".into()],
        }))
    }
    #[test]
    fn native_values_and_fences_round_trip_in_all_three_h4_envelopes() {
        let outcome = delivery();
        for format in ["text/zinc", "application/json;v=3", "application/json"] {
            let codec = codec_for(format).unwrap();
            let bytes = encode_grid(&outcome, codec).unwrap();
            assert_eq!(
                decode_grid::<SubscriptionOutcome>(&bytes, codec).unwrap(),
                outcome
            );
            let mut trailing = bytes.clone();
            trailing.extend_from_slice(b"junk");
            assert!(decode_grid::<SubscriptionOutcome>(&trailing, codec).is_err());
        }
        assert!(encode_grid(&outcome, codec_for("text/trio").unwrap()).is_err());
    }
    #[test]
    fn independently_constructed_control_fields_require_canonical_decimals_and_exact_schema() {
        let request = SubscriptionRequest::Acknowledge {
            watch: watch(),
            scope_generation: 2,
            token: [5; 16],
            through: 12,
        };
        for value in ["012", "12.0", "1.2e1", "+12", "-1", "18446744073709551616"] {
            let Kind::Dict(mut outer) = request.to_kind().unwrap() else {
                panic!()
            };
            let Kind::Dict(mut body) = outer.get("body").unwrap().clone() else {
                panic!()
            };
            body.set("through", Kind::Str(value.into()));
            outer.set("body", Kind::Dict(body));
            assert!(SubscriptionRequest::from_kind(&Kind::Dict(outer)).is_err());
        }
        let Kind::Dict(mut outer) = request.to_kind().unwrap() else {
            panic!()
        };
        outer.set("session", Kind::Str("user".into()));
        assert!(SubscriptionRequest::from_kind(&Kind::Dict(outer)).is_err());
        let mut grid = to_grid(&request).unwrap();
        grid.meta.set("err", Kind::Marker);
        assert!(from_grid::<SubscriptionRequest>(&grid).is_err());
    }
    #[test]
    fn contradictory_scope_acknowledgement_and_delivery_metadata_are_rejected() {
        let outcome = delivery();
        let good = SubscriptionRequest::Resume {
            watch: watch(),
            scope_generation: 2,
            acknowledged: 10,
        };
        assert!(validate_for_request(&outcome, &good).is_ok());
        for request in [
            SubscriptionRequest::Resume {
                watch: watch(),
                scope_generation: 1,
                acknowledged: 10,
            },
            SubscriptionRequest::Resume {
                watch: watch(),
                scope_generation: 2,
                acknowledged: 11,
            },
            SubscriptionRequest::Replace {
                watch: watch(),
                expected_scope_generation: 1,
                ids: vec!["point".into()],
            },
        ] {
            assert!(validate_for_request(&outcome, &request).is_err());
        }
        let SubscriptionOutcome::Delivery(delivery) = outcome else {
            panic!()
        };
        for malformed in [
            SubscriptionDelivery {
                from: 13,
                ..(*delivery).clone()
            },
            SubscriptionDelivery {
                initial: true,
                ..(*delivery).clone()
            },
            SubscriptionDelivery {
                removed: vec!["point".into()],
                ..(*delivery).clone()
            },
            SubscriptionDelivery {
                rows: vec![delivery.rows[0].clone(), delivery.rows[0].clone()],
                ..(*delivery).clone()
            },
        ] {
            assert!(encode(&SubscriptionOutcome::Delivery(Arc::new(malformed))).is_err());
        }
        let request = SubscriptionRequest::Acknowledge {
            watch: watch(),
            scope_generation: 2,
            token: [5; 16],
            through: 12,
        };
        let wrong = SubscriptionOutcome::Acknowledged {
            watch: watch(),
            scope_generation: 2,
            token: [6; 16],
            through: 12,
        };
        assert!(validate_for_request(&wrong, &request).is_err());
    }
    #[test]
    fn source_shape_limits_precede_large_envelope_construction() {
        let oversized = SubscriptionRequest::Create(SubscriptionCreate {
            authority: [1; 16],
            key: "known".into(),
            ids: vec!["point".into(); MAX_IDS + 1],
            lease_ms: 1,
        });
        assert!(to_grid(&oversized).is_err());
        let SubscriptionOutcome::Delivery(mut value) = delivery() else {
            panic!()
        };
        Arc::make_mut(&mut value).rows[0].set(
            "tooDeep",
            Kind::List(vec![Kind::List(vec![Kind::Str(
                "x".repeat(MAX_PAYLOAD_BYTES * 4),
            )])]),
        );
        assert!(to_grid(&SubscriptionOutcome::Delivery(value)).is_err());
    }
}
