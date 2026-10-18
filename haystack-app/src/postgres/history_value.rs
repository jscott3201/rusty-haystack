use super::{PostgresError, value};
use crate::{HisItem, HistoryChangeRecord, HistoryOperationIdentity, HistoryState};
use chrono::{DateTime, FixedOffset, SecondsFormat};
use haystack_core::{data::HDict, kinds::Kind};
use serde::{Deserialize, Serialize};

pub(crate) fn timestamp(ts: DateTime<FixedOffset>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Nanos, false)
}
pub(crate) fn parse_timestamp(ts: &str) -> Result<DateTime<FixedOffset>, PostgresError> {
    let value = DateTime::parse_from_rfc3339(ts).map_err(|_| PostgresError::Incompatible)?;
    if timestamp(value) != ts {
        return Err(PostgresError::Incompatible);
    };
    Ok(value)
}
pub(crate) fn encode_sample(item: &HisItem) -> Result<Vec<u8>, PostgresError> {
    let mut row = HDict::new();
    row.set("ts", Kind::Str(timestamp(item.ts)));
    row.set("val", item.val.clone());
    value::encode_dict(&row).map_err(Into::into)
}
pub(crate) fn decode_sample(bytes: &[u8]) -> Result<HisItem, PostgresError> {
    let row = value::decode_dict(bytes)?;
    let Some(Kind::Str(ts)) = row.get("ts") else {
        return Err(PostgresError::Incompatible);
    };
    if row.len() != 2 {
        return Err(PostgresError::Incompatible);
    };
    Ok(HisItem {
        ts: parse_timestamp(ts)?,
        val: row.get("val").ok_or(PostgresError::Incompatible)?.clone(),
    })
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    authority: [u8; 16],
    incarnation: [u8; 16],
    generation: u64,
}
impl From<HistoryState> for State {
    fn from(s: HistoryState) -> Self {
        Self {
            authority: s.authority,
            incarnation: s.incarnation,
            generation: s.generation,
        }
    }
}
impl From<State> for HistoryState {
    fn from(s: State) -> Self {
        Self {
            authority: s.authority,
            incarnation: s.incarnation,
            generation: s.generation,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    format: u8,
    sequence: u64,
    point: String,
    before: State,
    after: State,
    submitted: u64,
    unique: u64,
    retained: u64,
    evicted: u64,
    evicted_through: Option<String>,
    operation_id: Option<String>,
}
pub(crate) fn encode_change(c: &HistoryChangeRecord) -> Result<Vec<u8>, PostgresError> {
    if c.operation.as_ref().is_some_and(|id| {
        id.authority != c.before.authority
            || id.incarnation != c.before.incarnation
            || id.point != c.point
    }) {
        return Err(PostgresError::Incompatible);
    };
    serde_json::to_vec(&Change {
        format: 1,
        sequence: c.sequence,
        point: c.point.clone(),
        before: c.before.into(),
        after: c.after.into(),
        submitted: c.submitted_samples,
        unique: c.unique_samples,
        retained: c.retained_samples,
        evicted: c.evicted_samples,
        evicted_through: c.evicted_through.map(timestamp),
        operation_id: c.operation.as_ref().map(|id| id.operation_id.clone()),
    })
    .map_err(|_| PostgresError::Incompatible)
}
pub(crate) fn decode_change(bytes: &[u8]) -> Result<HistoryChangeRecord, PostgresError> {
    let c: Change = serde_json::from_slice(bytes).map_err(|_| PostgresError::Incompatible)?;
    if c.format != 1
        || c.point.is_empty()
        || c.point.len() > 256
        || c.before.authority != c.after.authority
        || c.before.incarnation != c.after.incarnation
        || c.before.generation.checked_add(1) != Some(c.after.generation)
        || c.sequence == 0
        || c.unique > c.submitted
        || c.retained.checked_add(c.evicted).is_none()
    {
        return Err(PostgresError::Incompatible);
    };
    let record = HistoryChangeRecord {
        sequence: c.sequence,
        point: c.point.clone(),
        operation: c.operation_id.map(|operation_id| HistoryOperationIdentity {
            authority: c.before.authority,
            incarnation: c.before.incarnation,
            point: c.point.clone(),
            operation_id,
        }),
        before: c.before.into(),
        after: c.after.into(),
        submitted_samples: c.submitted,
        unique_samples: c.unique,
        retained_samples: c.retained,
        evicted_samples: c.evicted,
        evicted_through: c
            .evicted_through
            .as_deref()
            .map(parse_timestamp)
            .transpose()?,
    };
    if encode_change(&record)? != bytes {
        return Err(PostgresError::Incompatible);
    };
    Ok(record)
}
