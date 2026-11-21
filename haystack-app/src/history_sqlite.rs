//! Native SQLite persistence for the existing one-point history contract.
use super::*;
use crate::{
    HistoryStoreLimits, history_storage_value as value,
    history_store::{AuthorityState, HistoryStoreInner},
    sqlite::{
        SqliteError, SqliteLimits,
        recovery::{self, StoredReceipt},
        storage::{Backend, QUANTUM, Stop, sql_error},
    },
};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::{HashMap, HashSet, VecDeque};

const PAYLOAD_LIMIT: usize = 1024 * 1024;

fn blob<'a>(row: &'a Row<'_>, index: usize) -> Result<&'a [u8], SqliteError> {
    let bytes = row
        .get_ref(index)
        .map_err(sql_error)?
        .as_blob()
        .map_err(|_| SqliteError::Incompatible)?;
    if bytes.len() > PAYLOAD_LIMIT {
        return Err(SqliteError::Limit);
    }
    Ok(bytes)
}
fn text<'a>(row: &'a Row<'_>, index: usize, limit: usize) -> Result<&'a str, SqliteError> {
    let text = row
        .get_ref(index)
        .map_err(sql_error)?
        .as_str()
        .map_err(|_| SqliteError::Incompatible)?;
    if text.len() > limit {
        return Err(SqliteError::Limit);
    }
    Ok(text)
}
fn counter(row: &Row<'_>, index: usize) -> Result<u64, SqliteError> {
    Ok(u64::from_be_bytes(
        blob(row, index)?
            .try_into()
            .map_err(|_| SqliteError::Incompatible)?,
    ))
}

/// The same native accounting runs before recovery allocation and before every
/// COMMIT. Encoded bytes bound typed input; work also charges per-row overhead.
struct Counts {
    points: usize,
    samples: usize,
    changes: usize,
    receipts: usize,
    data_bytes: usize,
    receipt_bytes: usize,
}
impl Counts {
    fn read(connection: &Connection) -> Result<Self, SqliteError> {
        let values: [i64; 8] = connection.query_row(
            "SELECT
             (SELECT count(*) FROM history_points),
             (SELECT coalesce(sum(length(CAST(id AS BLOB))+24+coalesce(length(CAST(evicted AS BLOB)),0)),0) FROM history_points),
             (SELECT count(*) FROM history_samples),
             (SELECT coalesce(sum(length(CAST(point AS BLOB))+12+length(payload)),0) FROM history_samples),
             (SELECT count(*) FROM history_changes),
             (SELECT coalesce(sum(8+length(payload)),0) FROM history_changes),
             (SELECT count(*) FROM history_receipts),
             (SELECT coalesce(sum(length(binding)+length(request)+length(outcome)),0) FROM history_receipts)",
            [], |row| Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?]),
        ).map_err(sql_error)?;
        let n = |index| usize::try_from(values[index]).map_err(|_| SqliteError::Limit);
        Ok(Self {
            points: n(0)?,
            samples: n(2)?,
            changes: n(4)?,
            receipts: n(6)?,
            data_bytes: n(1)?
                .checked_add(n(3)?)
                .and_then(|bytes| bytes.checked_add(usize::try_from(values[5]).ok()?))
                .ok_or(SqliteError::Limit)?,
            receipt_bytes: n(7)?,
        })
    }
    fn validate(
        &self,
        native: &SqliteLimits,
        store: &HistoryStoreLimits,
    ) -> Result<(), SqliteError> {
        let work = self
            .data_bytes
            .checked_add(self.receipt_bytes)
            .and_then(|bytes| bytes.checked_mul(4))
            .and_then(|bytes| {
                let rows = self
                    .points
                    .checked_add(self.samples)?
                    .checked_add(self.changes)?
                    .checked_add(self.receipts)?;
                bytes.checked_add(rows.checked_mul(128)?)
            })
            .ok_or(SqliteError::Limit)?;
        if self.points > store.max_points
            || self.samples > native.max_history_samples
            || self.changes > store.change_capacity
            || self.receipts > store.receipt_capacity.min(native.max_receipts)
            || self.data_bytes > native.max_history_bytes
            || self.receipt_bytes > native.max_receipt_bytes
            || work > native.max_history_work
        {
            return Err(SqliteError::Limit);
        }
        Ok(())
    }
}
fn history(backend: &Backend) -> Result<Arc<HistoryStoreInner>, SqliteError> {
    backend
        .projection
        .get()
        .and_then(|projection| projection.history.upgrade())
        .ok_or(SqliteError::Unavailable)
}
fn record(stored: StoredReceipt, binding: &HistoryBinding) -> Result<HistoryRecord, SqliteError> {
    let record = storage::record(stored.request, stored.outcome, binding)?;
    validate_for_request(&record.outcome, &record.request)
        .map_err(|_| SqliteError::Incompatible)?;
    Ok(record)
}
fn stored(connection: &Connection, key: &[u8]) -> Result<Option<StoredReceipt>, SqliteError> {
    let mut statement = connection
        .prepare("SELECT request,outcome FROM history_receipts WHERE binding=?1")
        .map_err(sql_error)?;
    let mut rows = statement.query([key]).map_err(sql_error)?;
    rows.next()
        .map_err(sql_error)?
        .map(|row| {
            // Check both borrowed fields before either owned copy.
            let request = blob(row, 0)?;
            let outcome = blob(row, 1)?;
            Ok(StoredReceipt {
                request: request.to_vec(),
                outcome: outcome.to_vec(),
            })
        })
        .transpose()
}
fn retained_bytes(binding: &HistoryBinding, record: &HistoryRecord) -> Result<usize, SqliteError> {
    let principal = match &binding.principal {
        PrincipalKey::Anonymous => 0,
        PrincipalKey::Authenticated(subject) | PrincipalKey::Embedding(subject) => subject.len(),
    };
    Ok(record
        .request
        .source_bytes()
        .map_err(|_| SqliteError::Incompatible)?
        .saturating_add(record.canonical.len().saturating_mul(8))
        .saturating_add(principal.saturating_mul(2))
        .saturating_add(4096))
}

impl HisStore {
    pub(crate) fn from_sqlite(
        backend: Arc<Backend>,
        authority: [u8; 16],
        sequence: u64,
        limits: HistoryStoreLimits,
        stop: &Stop,
    ) -> Result<Self, SqliteError> {
        stop.check()?;
        let validated =
            Self::with_limits(limits.clone()).map_err(|_| SqliteError::Configuration)?;
        if authority == [0; 16] {
            return Err(SqliteError::Incompatible);
        }
        let selected = storage::Backend::Sqlite(backend.clone());
        let state = backend.read(stop, |connection| {
            let meta = recovery::metadata(connection)?;
            if meta.history_authority != authority || meta.history_sequence != sequence {
                return Err(SqliteError::Incompatible);
            }
            let counts = Counts::read(connection)?;
            counts.validate(&backend.limits, &limits)?;
            let mut points: HashMap<String, Arc<RwLock<Series>>> = HashMap::new();
            points.try_reserve(counts.points).map_err(|_| SqliteError::Limit)?;
            {
                let mut statement = connection.prepare(
                    "SELECT id,incarnation,generation,evicted FROM history_points ORDER BY id",
                ).map_err(sql_error)?;
                let mut rows = statement.query([]).map_err(sql_error)?;
                while let Some(row) = rows.next().map_err(sql_error)? {
                    stop.check()?;
                    let id = text(row, 0, 256)?;
                    let incarnation: [u8; 16] = blob(row, 1)?.try_into().map_err(|_| SqliteError::Incompatible)?;
                    let generation = counter(row, 2)?;
                    if id.is_empty() || incarnation != selected.point_incarnation(authority, id)
                        || generation == 0 || generation > sequence
                    {
                        return Err(SqliteError::Incompatible);
                    }
                    let evicted = if matches!(row.get_ref(3).map_err(sql_error)?, rusqlite::types::ValueRef::Null) {
                        None
                    } else {
                        Some(value::parse_timestamp(text(row, 3, 64)?)?)
                    };
                    points.insert(id.to_owned(), Arc::new(RwLock::new(Series {
                        incarnation, generation, items: Vec::new(), evicted,
                    })));
                }
            }
            {
                let mut statement = connection.prepare(
                    "SELECT point,seconds,nanos,payload FROM history_samples ORDER BY point,seconds,nanos",
                ).map_err(sql_error)?;
                let mut rows = statement.query([]).map_err(sql_error)?;
                while let Some(row) = rows.next().map_err(sql_error)? {
                    stop.check()?;
                    let id = text(row, 0, 256)?;
                    let point = points.get(id).ok_or(SqliteError::Incompatible)?;
                    let mut series = point.write();
                    if series.items.len() >= limits.max_items_per_point {
                        return Err(SqliteError::Limit);
                    }
                    let item = value::decode_sample(blob(row, 3)?)?;
                    stop.check()?;
                    if item.ts.timestamp() != row.get::<_, i64>(1).map_err(sql_error)?
                        || i64::from(item.ts.timestamp_subsec_nanos()) != row.get::<_, i64>(2).map_err(sql_error)?
                        || crate::history_store::sample_bytes(&item.val).is_none()
                        || !haystack_core::codecs::shared::h4_datetime_representable(&item.ts)
                        || series.items.last().is_some_and(|previous| previous.ts >= item.ts)
                    {
                        return Err(SqliteError::Incompatible);
                    }
                    series.items.try_reserve(1).map_err(|_| SqliteError::Limit)?;
                    series.items.push(item);
                }
            }
            let mut changes: VecDeque<HistoryChangeRecord> = VecDeque::new();
            changes.try_reserve_exact(limits.change_capacity).map_err(|_| SqliteError::Limit)?;
            let mut last: HashMap<String, usize> = HashMap::new();
            last.try_reserve(counts.points).map_err(|_| SqliteError::Limit)?;
            {
                let mut statement = connection.prepare(
                    "SELECT sequence,payload FROM history_changes ORDER BY sequence",
                ).map_err(sql_error)?;
                let mut rows = statement.query([]).map_err(sql_error)?;
                while let Some(row) = rows.next().map_err(sql_error)? {
                    stop.check()?;
                    let position = counter(row, 0)?;
                    let change = value::decode_change(blob(row, 1)?)?;
                    stop.check()?;
                    if change.sequence != position || change.before.authority != authority
                        || change.operation.is_none()
                        || change.operation.as_ref().is_some_and(|identity| validate_identity(identity).is_err())
                        || changes.back().is_some_and(|previous| previous.sequence.checked_add(1) != Some(position))
                    {
                        return Err(SqliteError::Incompatible);
                    }
                    let point = points.get(&change.point).ok_or(SqliteError::Incompatible)?.read();
                    if change.after.incarnation != point.incarnation || change.after.generation > point.generation {
                        return Err(SqliteError::Incompatible);
                    }
                    if let Some(previous) = last.get(&change.point)
                        && changes[*previous].after != change.before
                    {
                        return Err(SqliteError::Incompatible);
                    }
                    last.insert(change.point.clone(), changes.len());
                    changes.push_back(change);
                }
            }
            if changes.back().map_or(0, |change| change.sequence) != sequence {
                return Err(SqliteError::Incompatible);
            }
            for (id, index) in last {
                stop.check()?;
                let change = &changes[index];
                let series = points.get(&id).ok_or(SqliteError::Incompatible)?.read();
                if change.after.generation != series.generation
                    || change.retained_samples != series.items.len() as u64
                    || change.evicted_through.map(value::timestamp) != series.evicted.map(value::timestamp)
                {
                    return Err(SqliteError::Incompatible);
                }
            }
            let mut receipts = HashMap::new();
            receipts.try_reserve(counts.receipts).map_err(|_| SqliteError::Limit)?;
            let mut proven_changes = Vec::new();
            proven_changes.try_reserve_exact(changes.len()).map_err(|_| SqliteError::Limit)?;
            proven_changes.resize(changes.len(), false);
            let mut proven_tips = HashSet::new();
            proven_tips.try_reserve(counts.points).map_err(|_| SqliteError::Limit)?;
            let mut receipt_bytes = 0usize;
            {
                let mut statement = connection.prepare(
                    "SELECT binding,request,outcome FROM history_receipts ORDER BY binding",
                ).map_err(sql_error)?;
                let mut rows = statement.query([]).map_err(sql_error)?;
                while let Some(row) = rows.next().map_err(sql_error)? {
                    stop.check()?;
                    let key = blob(row, 0)?;
                    let request = blob(row, 1)?;
                    let outcome = blob(row, 2)?;
                    let binding = HistoryBinding::from_storage_key(key)?;
                    let record = record(StoredReceipt {
                        request: request.to_vec(), outcome: outcome.to_vec(),
                    }, &binding)?;
                    stop.check()?;
                    if binding.identity.authority != authority
                        || binding.identity.incarnation != selected.point_incarnation(authority, &binding.identity.point)
                    {
                        return Err(SqliteError::Incompatible);
                    }
                    if let HistoryWriteOutcome::Committed(receipt) = &record.outcome {
                        let point = points.get(&binding.identity.point).ok_or(SqliteError::Incompatible)?.read();
                        if receipt.qualification != HistoryReceiptQualification::SqliteDurable
                            || receipt.change_sequence > sequence
                            || receipt.after_generation > point.generation
                            || (receipt.after_generation == point.generation
                                && receipt.retained_samples != point.items.len() as u64)
                        {
                            return Err(SqliteError::Incompatible);
                        }
                        if receipt.after_generation == point.generation
                            && !proven_tips.insert(binding.identity.point.clone())
                        {
                            return Err(SqliteError::Incompatible);
                        }
                        if let Some(first) = changes.front()
                            && receipt.change_sequence >= first.sequence
                        {
                            let index = usize::try_from(receipt.change_sequence - first.sequence)
                                .map_err(|_| SqliteError::Incompatible)?;
                            let change = changes.get(index).ok_or(SqliteError::Incompatible)?;
                            if change.operation.as_ref() != Some(&receipt.identity)
                                || change.before.generation != receipt.before_generation
                                || change.after.generation != receipt.after_generation
                                || change.submitted_samples != receipt.submitted_samples
                                || change.unique_samples != receipt.unique_samples
                                || change.retained_samples != receipt.retained_samples
                                || change.evicted_samples != receipt.evicted_samples
                                || proven_changes[index]
                            {
                                return Err(SqliteError::Incompatible);
                            }
                            proven_changes[index] = true;
                        }
                    }
                    receipt_bytes = receipt_bytes.checked_add(retained_bytes(&binding, &record)?).ok_or(SqliteError::Limit)?;
                    if receipt_bytes > limits.receipt_bytes {
                        return Err(SqliteError::Limit);
                    }
                    receipts.insert(binding, record);
                }
            }
            // Every persisted point was created by a committed write. Its tip
            // still needs durable receipt proof after its change leaves the
            // bounded ledger. Older receipts cannot prove a fabricated tip.
            if proven_changes.iter().any(|proven| !proven) || proven_tips.len() != points.len() {
                return Err(SqliteError::Incompatible);
            }
            stop.check()?;
            Ok(AuthorityState { points, receipts, receipt_bytes, changes, sequence })
        })?;
        drop(validated);
        stop.check()?;
        Ok(Self {
            inner: Arc::new(HistoryStoreInner {
                authority,
                backend: Some(selected),
                limits,
                state: Mutex::new(state),
            }),
        })
    }
}

pub(super) fn lookup(
    backend: &Backend,
    binding: &HistoryBinding,
    budget: &Budget,
) -> Result<Option<HistoryRecord>, SqliteError> {
    let stop = Stop::from_budget(budget);
    let _publication = backend.publication_guard(&stop)?;
    let key = binding.storage_key()?;
    let found = backend
        .read(&stop, |connection| stored(connection, &key))?
        .map(|stored| record(stored, binding))
        .transpose()?;
    if let Some(HistoryRecord {
        outcome: HistoryWriteOutcome::Committed(receipt),
        ..
    }) = &found
    {
        let history = history(backend)?;
        let sequence = loop {
            stop.check()?;
            if let Some(state) = history.state.try_lock_for(QUANTUM) {
                break state.sequence;
            }
        };
        if receipt.qualification != HistoryReceiptQualification::SqliteDurable
            || binding.identity.authority != history.authority
            || receipt.change_sequence > sequence
        {
            backend.seal();
            return Err(SqliteError::Unavailable);
        }
    }
    Ok(found)
}
pub(super) fn reserve(
    backend: &Backend,
    binding: &HistoryBinding,
    canonical: &[u8],
    outcome: &HistoryWriteOutcome,
    budget: &Budget,
) -> Result<Option<HistoryRecord>, SqliteError> {
    let key = binding.storage_key()?;
    let encoded = storage::encode_outcome(outcome)?;
    if key.len() > PAYLOAD_LIMIT || canonical.len() > PAYLOAD_LIMIT || encoded.len() > PAYLOAD_LIMIT
    {
        return Err(SqliteError::Limit);
    }
    let history = history(backend)?;
    backend
        .transaction(&Stop::from_budget(budget), |connection| {
            if let Some(found) = stored(connection, &key)? {
                return Ok(Some(found));
            }
            connection
                .execute(
                    "INSERT INTO history_receipts VALUES(?1,?2,?3)",
                    params![key, canonical, encoded],
                )
                .map_err(sql_error)?;
            Counts::read(connection)?.validate(&backend.limits, &history.limits)?;
            Ok(None)
        })?
        .map(|stored| record(stored, binding))
        .transpose()
}
fn settle_locked(
    backend: &Backend,
    binding: &HistoryBinding,
    reason: HistoryWriteRejection,
    stop: &Stop,
) -> Result<HistoryRecord, SqliteError> {
    let key = binding.storage_key()?;
    let history = history(backend)?;
    backend.transaction(stop, |connection| {
        let mut stored = stored(connection, &key)?.ok_or(SqliteError::Incompatible)?;
        let previous = record(
            StoredReceipt {
                request: stored.request.clone(),
                outcome: stored.outcome.clone(),
            },
            binding,
        )?;
        if matches!(
            previous.outcome,
            HistoryWriteOutcome::Unknown {
                cause: HistoryWriteUnknown::Pending,
                ..
            }
        ) {
            let rejected = storage::encode_outcome(&HistoryWriteOutcome::Rejected {
                identity: binding.identity.clone(),
                reason,
            })?;
            let mut counts = Counts::read(connection)?;
            counts.validate(&backend.limits, &history.limits)?;
            counts.receipt_bytes = counts
                .receipt_bytes
                .checked_sub(stored.outcome.len())
                .and_then(|bytes| bytes.checked_add(rejected.len()))
                .ok_or(SqliteError::Limit)?;
            if rejected.len() > PAYLOAD_LIMIT
                || counts.validate(&backend.limits, &history.limits).is_err()
            {
                // Retaining Pending preserves the durable intent and same-config
                // reopen when the final rejection envelope requires more bytes.
                return Ok(previous);
            }
            stored.outcome = rejected;
            connection
                .execute(
                    "UPDATE history_receipts SET outcome=?2 WHERE binding=?1",
                    params![key, stored.outcome],
                )
                .map_err(sql_error)?;
        }
        record(stored, binding)
    })
}

impl PreparedHistoryMutation {
    fn sqlite_stop(&self) -> Stop {
        let mut stop = Stop::new(self.deadline, self.cancel.clone());
        stop.owner_cancel = self.owner_cancel.clone();
        stop.owner_sealed = self.owner_sealed.clone();
        stop.session = self.session.clone();
        stop
    }
    fn cache_sqlite(&self, outcome: &HistoryWriteOutcome, stop: &Stop) -> Result<(), SqliteError> {
        loop {
            stop.check()?;
            if let Some(mut state) = self.store.inner.state.try_lock_for(QUANTUM) {
                state
                    .receipts
                    .get_mut(&self.binding)
                    .ok_or(SqliteError::Incompatible)?
                    .outcome = outcome.clone();
                return Ok(());
            }
        }
    }
    pub(super) fn reject_sqlite(
        self,
        backend: &Backend,
        reason: HistoryWriteRejection,
    ) -> HistoryWriteOutcome {
        let cleanup = Stop::new(
            Instant::now() + backend.limits.cleanup_timeout,
            CancellationToken::new(),
        );
        let Ok(_publication) = backend.publication_guard(&cleanup) else {
            return self.unknown();
        };
        match settle_locked(backend, &self.binding, reason, &cleanup) {
            Ok(record) if self.cache_sqlite(&record.outcome, &cleanup).is_ok() => record.outcome,
            _ => {
                backend.seal();
                self.unknown()
            }
        }
    }
    pub(super) fn publish_sqlite(mut self, backend: &Backend) -> HistoryWriteOutcome {
        let stop = self.sqlite_stop();
        let policy_owner = self.policy.clone();
        let policy = loop {
            if let Some(reason) = self.stopped() {
                return self.reject_sqlite(backend, reason);
            }
            if let Some(policy) = policy_owner.try_read_for(QUANTUM) {
                break policy;
            }
        };
        let writer = match backend.publication_guard(&stop) {
            Ok(writer) => writer,
            Err(_) => {
                drop(policy);
                let reason = self.stopped().unwrap_or(HistoryWriteRejection::Provider);
                return self.reject_sqlite(backend, reason);
            }
        };
        if !Arc::ptr_eq(&policy.generation, &self.generation) {
            drop(writer);
            drop(policy);
            return self.reject_sqlite(backend, HistoryWriteRejection::Conflict);
        }
        let store = self.store.clone();
        let point = self.plan.as_ref().expect("unconsumed plan").point.clone();
        let capture = (|| {
            loop {
                stop.check()?;
                let Some(state) = store.inner.state.try_lock_for(QUANTUM) else {
                    continue;
                };
                let Some(series) = point.try_read() else {
                    drop(state);
                    continue;
                };
                let plan = self.plan.as_ref().expect("unconsumed plan");
                if store.series_state(&series) != plan.change.before {
                    return Err(SqliteError::Conflict);
                }
                let sequence = state.sequence.checked_add(1).ok_or(SqliteError::Limit)?;
                let pending = state
                    .receipts
                    .get(&self.binding)
                    .ok_or(SqliteError::Incompatible)?;
                if !matches!(
                    pending.outcome,
                    HistoryWriteOutcome::Unknown {
                        cause: HistoryWriteUnknown::Pending,
                        ..
                    }
                ) {
                    return Err(SqliteError::Conflict);
                }
                return Ok(sequence);
            }
        })();
        let sequence = match capture {
            Ok(sequence) => sequence,
            Err(error) => {
                drop(writer);
                drop(policy);
                let reason = self.stopped().unwrap_or(match error {
                    SqliteError::Conflict => HistoryWriteRejection::Conflict,
                    SqliteError::Limit => HistoryWriteRejection::Limit,
                    _ => HistoryWriteRejection::Provider,
                });
                return self.reject_sqlite(backend, reason);
            }
        };
        let mut plan = self.plan.take().expect("one publication");
        plan.change.sequence = sequence;
        plan.receipt.change_sequence = sequence;
        let committed = HistoryWriteOutcome::Committed(plan.receipt.clone());
        #[cfg(test)]
        let fault = backend.fault.lock().take();
        let result = (|| {
            stop.check()?;
            // The entity writer uses this same gate. Reading graph state here
            // never waits for the writer while holding a graph guard.
            let graph_state = loop {
                stop.check()?;
                if let Some(state) = self.graph.read_for(QUANTUM, |graph| graph.state()) {
                    break state;
                }
            };
            if graph_state != plan.graph {
                return Err(SqliteError::Conflict);
            }
            let key = self.binding.storage_key()?;
            let encoded = storage::encode_outcome(&committed)?;
            let change = value::encode_change(&plan.change)?;
            if plan.next.items.len() > backend.limits.max_history_samples
                || encoded.len() > PAYLOAD_LIMIT
                || change.len() > PAYLOAD_LIMIT
            {
                return Err(SqliteError::Limit);
            }
            let mut samples = Vec::new();
            samples
                .try_reserve_exact(plan.next.items.len())
                .map_err(|_| SqliteError::Limit)?;
            let mut bytes = change.len();
            for item in &plan.next.items {
                stop.check()?;
                let payload = value::encode_sample(item)?;
                bytes = bytes.checked_add(payload.len()).ok_or(SqliteError::Limit)?;
                if payload.len() > PAYLOAD_LIMIT || bytes > backend.limits.max_history_bytes {
                    return Err(SqliteError::Limit);
                }
                samples.push((
                    item.ts.timestamp(),
                    i64::from(item.ts.timestamp_subsec_nanos()),
                    payload,
                ));
            }
            let evicted = plan.next.evicted.map(value::timestamp);
            let result = backend.transaction(&stop, |connection| {
                let meta = recovery::metadata(connection)?;
                if meta.state != plan.graph || meta.history_authority != store.authority()
                    || meta.history_sequence.checked_add(1) != Some(sequence)
                    || recovery::schema_version(connection)? != backend.projection.get().ok_or(SqliteError::Unavailable)?.schema_version
                {
                    return Err(SqliteError::Conflict);
                }
                let found = stored(connection, &key)?.ok_or(SqliteError::Incompatible)?;
                let previous = record(found, &self.binding)?;
                if !matches!(previous.outcome, HistoryWriteOutcome::Unknown { cause: HistoryWriteUnknown::Pending, .. }) {
                    return Ok(previous.outcome);
                }
                let existing: Option<(Vec<u8>, Vec<u8>)> = connection.query_row(
                    "SELECT incarnation,generation FROM history_points WHERE id=?1",
                    [&plan.change.point], |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional().map_err(sql_error)?;
                match existing {
                    Some((incarnation, generation)) => {
                        if incarnation != plan.change.before.incarnation
                            || recovery::revision(generation)? != plan.change.before.generation {
                            return Err(SqliteError::Conflict);
                        }
                    }
                    None if plan.change.before.generation == 0 => {}
                    _ => return Err(SqliteError::Conflict),
                }
                connection.execute(
                    "INSERT INTO history_points VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET incarnation=excluded.incarnation,generation=excluded.generation,evicted=excluded.evicted",
                    params![plan.change.point, plan.next.incarnation.as_slice(), plan.next.generation.to_be_bytes().as_slice(), evicted],
                ).map_err(sql_error)?;
                connection.execute("DELETE FROM history_samples WHERE point=?1", [&plan.change.point]).map_err(sql_error)?;
                for (seconds, nanos, payload) in &samples {
                    stop.check()?;
                    connection.execute("INSERT INTO history_samples VALUES(?1,?2,?3,?4)",
                        params![plan.change.point, seconds, nanos, payload]).map_err(sql_error)?;
                }
                connection.execute("INSERT INTO history_changes VALUES(?1,?2)",
                    params![sequence.to_be_bytes().as_slice(), change]).map_err(sql_error)?;
                let floor = sequence.saturating_sub(store.inner.limits.change_capacity as u64);
                connection.execute("DELETE FROM history_changes WHERE sequence<=?1",
                    [floor.to_be_bytes().as_slice()]).map_err(sql_error)?;
                connection.execute("UPDATE metadata SET history_sequence=?1 WHERE singleton=1",
                    [sequence.to_be_bytes().as_slice()]).map_err(sql_error)?;
                connection.execute("UPDATE history_receipts SET outcome=?2 WHERE binding=?1",
                    params![key, encoded]).map_err(sql_error)?;
                Counts::read(connection)?.validate(&backend.limits, &store.inner.limits)?;
                #[cfg(test)]
                crate::sqlite::qualification::before_commit(fault.as_ref(), connection)?;
                Ok(committed.clone())
            })?;
            #[cfg(test)]
            crate::sqlite::qualification::after_commit(fault.as_ref(), backend)?;
            Ok::<_, SqliteError>(result)
        })();
        let cleanup = Stop::new(
            Instant::now() + backend.limits.cleanup_timeout,
            CancellationToken::new(),
        );
        let resolved = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let reason = self.stopped().unwrap_or(match error {
                    SqliteError::Conflict => HistoryWriteRejection::Conflict,
                    SqliteError::Limit => HistoryWriteRejection::Limit,
                    _ => HistoryWriteRejection::Provider,
                });
                match settle_locked(backend, &self.binding, reason, &cleanup) {
                    Ok(record) => record.outcome,
                    Err(_) => {
                        backend.seal();
                        return self.unknown();
                    }
                }
            }
        };
        drop(policy);
        if resolved != committed {
            if matches!(resolved, HistoryWriteOutcome::Committed(_))
                || self.cache_sqlite(&resolved, &cleanup).is_err()
            {
                backend.seal();
                return self.unknown();
            }
            return resolved;
        }
        // A successful native COMMIT is not exposed until the bounded projection,
        // ledger and receipt publish together. No authority/point lock spanned SQL.
        loop {
            if cleanup.check().is_err() {
                backend.seal();
                return self.unknown();
            }
            let Some(mut state) = store.inner.state.try_lock_for(QUANTUM) else {
                continue;
            };
            let Some(mut series) = point.try_write() else {
                drop(state);
                continue;
            };
            if state.sequence.checked_add(1) != Some(sequence)
                || store.series_state(&series) != plan.change.before
                || !state.receipts.contains_key(&self.binding)
            {
                backend.seal();
                return self.unknown();
            }
            *series = plan.next;
            store.append_change(&mut state, plan.change);
            state
                .receipts
                .get_mut(&self.binding)
                .expect("reserved receipt")
                .outcome = resolved.clone();
            break;
        }
        drop(writer);
        resolved
    }
}
