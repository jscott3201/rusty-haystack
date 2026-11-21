//! Storage envelopes are versioned separately from the canonical typed payload.
//! SQL numeric projections are checked against these authoritative values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadError {
    Limit,
    Incompatible,
}
use base64::{Engine as _, engine::general_purpose::STANDARD};
use haystack_core::{
    codecs::typed,
    data::HDict,
    graph::{CommitSpan, DiffOp, RecoveryChange},
    kinds::Kind,
};
use serde::{Deserialize, Serialize};

pub(crate) fn encode_dict(row: &HDict) -> Result<Vec<u8>, PayloadError> {
    typed::encode(&Kind::Dict(Box::new(row.clone()))).map_err(|_| PayloadError::Limit)
}
pub(crate) fn decode_dict(bytes: &[u8]) -> Result<HDict, PayloadError> {
    let Kind::Dict(row) = typed::decode(bytes).map_err(|_| PayloadError::Incompatible)? else {
        return Err(PayloadError::Incompatible);
    };
    if encode_dict(&row)? != bytes {
        return Err(PayloadError::Incompatible);
    }
    Ok(*row)
}
#[cfg(feature = "postgres")]
pub(crate) fn number(value: &str) -> Result<u64, PayloadError> {
    let n = value
        .parse::<u64>()
        .map_err(|_| PayloadError::Incompatible)?;
    if n.to_string() != value {
        return Err(PayloadError::Incompatible);
    }
    Ok(n)
}
pub(crate) fn fixed<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N], PayloadError> {
    bytes.try_into().map_err(|_| PayloadError::Incompatible)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    format: u8,
    revision: u64,
    first: u64,
    last: u64,
    timestamp: i64,
    operation: u8,
    id: String,
    old: Option<String>,
    new: Option<String>,
    changed: Option<String>,
    previous: Option<String>,
}
fn encoded(row: &Option<HDict>) -> Result<Option<String>, PayloadError> {
    row.as_ref()
        .map(|row| encode_dict(row).map(|bytes| STANDARD.encode(bytes)))
        .transpose()
}
fn decoded(row: Option<String>) -> Result<Option<HDict>, PayloadError> {
    row.map(|row| {
        STANDARD
            .decode(row)
            .map_err(|_| PayloadError::Incompatible)
            .and_then(|bytes| decode_dict(&bytes))
    })
    .transpose()
}
pub(crate) fn encode_change(change: &RecoveryChange) -> Result<Vec<u8>, PayloadError> {
    serde_json::to_vec(&Change {
        format: 1,
        revision: change.version,
        first: change.span.first,
        last: change.span.last,
        timestamp: change.timestamp,
        operation: match change.op {
            DiffOp::Add => 1,
            DiffOp::Update => 2,
            DiffOp::Remove => 3,
        },
        id: change.id.clone(),
        old: encoded(&change.old)?,
        new: encoded(&change.new)?,
        changed: encoded(&change.changed)?,
        previous: encoded(&change.previous)?,
    })
    .map_err(|_| PayloadError::Incompatible)
}
pub(crate) fn decode_change(bytes: &[u8]) -> Result<RecoveryChange, PayloadError> {
    let c: Change = serde_json::from_slice(bytes).map_err(|_| PayloadError::Incompatible)?;
    if c.format != 1 {
        return Err(PayloadError::Incompatible);
    }
    let change = RecoveryChange {
        version: c.revision,
        span: CommitSpan {
            first: c.first,
            last: c.last,
        },
        timestamp: c.timestamp,
        op: match c.operation {
            1 => DiffOp::Add,
            2 => DiffOp::Update,
            3 => DiffOp::Remove,
            _ => return Err(PayloadError::Incompatible),
        },
        id: c.id,
        old: decoded(c.old)?,
        new: decoded(c.new)?,
        changed: decoded(c.changed)?,
        previous: decoded(c.previous)?,
    };
    if encode_change(&change)? != bytes {
        return Err(PayloadError::Incompatible);
    }
    Ok(change)
}
