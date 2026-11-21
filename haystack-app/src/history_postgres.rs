use super::storage::{decode_outcome, encode_outcome};
use super::*;
use crate::{
    HistoryStoreLimits,
    history_store::{AuthorityState, HistoryStoreInner},
    postgres::{
        PostgresError, history_value,
        storage::{Backend, StoredReceipt},
        value,
    },
};
use parking_lot::Mutex;
use sqlx::Row;
use std::collections::{HashMap, VecDeque};

fn record(stored: StoredReceipt, binding: &HistoryBinding) -> Result<HistoryRecord, PostgresError> {
    storage::record(stored.request, stored.outcome, binding).map_err(Into::into)
}
pub(super) fn lookup(
    backend: &Backend,
    binding: &HistoryBinding,
    deadline: Instant,
) -> Result<Option<HistoryRecord>, PostgresError> {
    let _publication = backend.publication_guard(deadline)?;
    let key = binding.storage_key()?;
    let record = backend
        .runtime
        .block_on(backend.lookup_receipt(2, &key))?
        .map(|stored| record(stored, binding))
        .transpose()?;
    if record
        .as_ref()
        .is_some_and(|record| matches!(record.outcome, HistoryWriteOutcome::Committed(_)))
    {
        backend.refresh_locked(deadline)?;
    }
    Ok(record)
}
pub(super) fn reserve(
    backend: &Backend,
    binding: &HistoryBinding,
    canonical: &[u8],
    outcome: &HistoryWriteOutcome,
    deadline: Instant,
) -> Result<Option<HistoryRecord>, PostgresError> {
    let key = binding.storage_key()?;
    let outcome = encode_outcome(outcome)?;
    backend
        .runtime
        .block_on(backend.reserve_receipt(2, &key, canonical, &outcome, deadline))?
        .map(|stored| record(stored, binding))
        .transpose()
}
pub(super) fn settle(
    backend: &Backend,
    binding: &HistoryBinding,
    reason: HistoryWriteRejection,
    deadline: Instant,
) -> Result<Option<HistoryRecord>, PostgresError> {
    let key = binding.storage_key()?;
    let pending = encode_outcome(&HistoryWriteOutcome::Unknown {
        identity: binding.identity.clone(),
        cause: HistoryWriteUnknown::Pending,
    })?;
    let rejected = encode_outcome(&HistoryWriteOutcome::Rejected {
        identity: binding.identity.clone(),
        reason,
    })?;
    backend
        .runtime
        .block_on(backend.settle_receipt(2, &key, &pending, &rejected, deadline))?
        .map(|stored| record(stored, binding))
        .transpose()
}
impl HisStore {
    pub(crate) async fn from_postgres(
        backend: Arc<Backend>,
        connection: &mut sqlx::PgConnection,
        authority: [u8; 16],
        sequence: u64,
        limits: HistoryStoreLimits,
    ) -> Result<Self, PostgresError> {
        // Reuse ordinary finite-limit validation and the preallocated ledger.
        let validated =
            Self::with_limits(limits.clone()).map_err(|_| PostgresError::Configuration)?;
        let counts=sqlx::query("SELECT (SELECT count(*) FROM history_points) AS points, (SELECT coalesce(sum(octet_length(id)+coalesce(octet_length(evicted),0)),0)::bigint FROM history_points) AS point_bytes, (SELECT count(*) FROM history_samples) AS samples, (SELECT coalesce(sum(octet_length(payload)+octet_length(point)),0)::bigint FROM history_samples) AS bytes, (SELECT count(*) FROM history_changes) AS changes, (SELECT coalesce(sum(octet_length(payload)),0)::bigint FROM history_changes) AS change_bytes")
            .fetch_one(&mut *connection).await.map_err(crate::postgres::storage::read_error)?;
        let count = |name| {
            counts
                .try_get::<i64, _>(name)
                .map_err(|_| PostgresError::Incompatible)
                .and_then(|n| usize::try_from(n).map_err(|_| PostgresError::Limit))
        };
        if count("points")? > limits.max_points
            || count("samples")? > backend.limits.max_history_samples
            || count("changes")? > limits.change_capacity
            || count("bytes")?
                .saturating_add(count("point_bytes")?)
                .saturating_add(count("change_bytes")?)
                > backend.limits.recovery.max_bytes
        {
            return Err(PostgresError::Limit);
        };
        let point_rows = sqlx::query(
            "SELECT id,incarnation,generation::text,evicted FROM history_points ORDER BY id",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(crate::postgres::storage::read_error)?;
        let mut points = HashMap::new();
        for row in point_rows {
            let id: String = row.try_get("id").map_err(|_| PostgresError::Incompatible)?;
            if id.is_empty() || id.len() > 256 {
                return Err(PostgresError::Incompatible);
            };
            let evicted: Option<Vec<u8>> = row
                .try_get("evicted")
                .map_err(|_| PostgresError::Incompatible)?;
            let evicted = evicted
                .map(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|_| PostgresError::Incompatible)
                        .and_then(|s| history_value::parse_timestamp(&s).map_err(Into::into))
                })
                .transpose()?;
            let series = Series {
                incarnation: value::fixed(
                    row.try_get("incarnation")
                        .map_err(|_| PostgresError::Incompatible)?,
                )?,
                generation: value::number(
                    &row.try_get::<String, _>("generation")
                        .map_err(|_| PostgresError::Incompatible)?,
                )?,
                items: vec![],
                evicted,
            };
            if series.incarnation == [0; 16] {
                return Err(PostgresError::Incompatible);
            };
            points.insert(id, Arc::new(RwLock::new(series)));
        }
        let samples = sqlx::query(
            "SELECT point,seconds,nanos,payload FROM history_samples ORDER BY point,seconds,nanos",
        )
        .fetch_all(&mut *connection)
        .await
        .map_err(crate::postgres::storage::read_error)?;
        for row in samples {
            let id: String = row
                .try_get("point")
                .map_err(|_| PostgresError::Incompatible)?;
            let item = history_value::decode_sample(
                &row.try_get::<Vec<u8>, _>("payload")
                    .map_err(|_| PostgresError::Incompatible)?,
            )?;
            if item.ts.timestamp()
                != row
                    .try_get::<i64, _>("seconds")
                    .map_err(|_| PostgresError::Incompatible)?
                || item.ts.timestamp_subsec_nanos() as i32
                    != row
                        .try_get::<i32, _>("nanos")
                        .map_err(|_| PostgresError::Incompatible)?
            {
                return Err(PostgresError::Incompatible);
            };
            let mut series = points.get(&id).ok_or(PostgresError::Incompatible)?.write();
            if series.items.len() >= limits.max_items_per_point
                || series
                    .items
                    .last()
                    .is_some_and(|previous| previous.ts >= item.ts)
            {
                return Err(PostgresError::Incompatible);
            };
            series.items.push(item);
        }
        let mut changes = VecDeque::new();
        changes
            .try_reserve_exact(limits.change_capacity)
            .map_err(|_| PostgresError::Limit)?;
        let change_rows =
            sqlx::query("SELECT sequence::text,payload FROM history_changes ORDER BY sequence")
                .fetch_all(&mut *connection)
                .await
                .map_err(crate::postgres::storage::read_error)?;
        let mut previous = None;
        for row in change_rows {
            let change = history_value::decode_change(
                &row.try_get::<Vec<u8>, _>("payload")
                    .map_err(|_| PostgresError::Incompatible)?,
            )?;
            if change.sequence
                != value::number(
                    &row.try_get::<String, _>("sequence")
                        .map_err(|_| PostgresError::Incompatible)?,
                )?
                || change.before.authority != authority
                || previous.is_some_and(|n: u64| n.checked_add(1) != Some(change.sequence))
            {
                return Err(PostgresError::Incompatible);
            };
            previous = Some(change.sequence);
            changes.push_back(change);
        }
        if previous.unwrap_or(0) != sequence {
            return Err(PostgresError::Incompatible);
        };
        let mut receipts = HashMap::new();
        let mut receipt_bytes = 0usize;
        for row in backend.receipt_rows_at(2, connection).await? {
            let binding = HistoryBinding::from_storage_key(
                &row.try_get::<Vec<u8>, _>("binding")
                    .map_err(|_| PostgresError::Incompatible)?,
            )?;
            let record = record(
                StoredReceipt {
                    request: row
                        .try_get("request")
                        .map_err(|_| PostgresError::Incompatible)?,
                    outcome: row
                        .try_get("outcome")
                        .map_err(|_| PostgresError::Incompatible)?,
                },
                &binding,
            )?;
            if binding.identity.authority != authority {
                return Err(PostgresError::Incompatible);
            };
            if let HistoryWriteOutcome::Committed(receipt) = &record.outcome
                && (receipt.qualification != HistoryReceiptQualification::PostgresDurable
                    || receipt.change_sequence > sequence)
            {
                return Err(PostgresError::Incompatible);
            }
            let principal_bytes = match &binding.principal {
                PrincipalKey::Anonymous => 0,
                PrincipalKey::Authenticated(s) | PrincipalKey::Embedding(s) => s.len(),
            };
            receipt_bytes = receipt_bytes
                .saturating_add(
                    record
                        .request
                        .source_bytes()
                        .map_err(|_| PostgresError::Incompatible)?,
                )
                .saturating_add(record.canonical.len().saturating_mul(8))
                .saturating_add(principal_bytes.saturating_mul(2))
                .saturating_add(4096);
            if receipts.len() >= limits.receipt_capacity || receipt_bytes > limits.receipt_bytes {
                return Err(PostgresError::Limit);
            };
            receipts.insert(binding, record);
        }
        drop(validated);
        Ok(Self {
            inner: Arc::new(HistoryStoreInner {
                authority,
                backend: Some(storage::Backend::Postgres(backend)),
                limits,
                state: Mutex::new(AuthorityState {
                    points,
                    receipts,
                    receipt_bytes,
                    changes,
                    sequence,
                }),
            }),
        })
    }
}

impl PreparedHistoryMutation {
    pub(super) fn reject_postgres(
        self,
        backend: &Backend,
        reason: HistoryWriteRejection,
    ) -> HistoryWriteOutcome {
        match settle(
            backend,
            &self.binding,
            reason,
            Instant::now() + backend.limits.statement_timeout,
        ) {
            Ok(Some(record)) => {
                if let Some(cached) = self
                    .store
                    .inner
                    .state
                    .lock()
                    .receipts
                    .get_mut(&self.binding)
                {
                    cached.outcome = record.outcome.clone();
                }
                record.outcome
            }
            _ => self.unknown(),
        }
    }
    pub(super) fn publish_postgres(mut self, backend: &Backend) -> HistoryWriteOutcome {
        let policy_owner = self.policy.clone();
        let policy = loop {
            if let Some(reason) = self.stopped() {
                return self.reject_postgres(backend, reason);
            };
            if let Some(guard) = policy_owner.try_read_for(Duration::from_millis(5)) {
                break guard;
            }
        };
        let writer = loop {
            if let Some(reason) = self.stopped() {
                return self.reject_postgres(backend, reason);
            };
            if let Some(guard) = backend.writer.try_lock_for(Duration::from_millis(5)) {
                break guard;
            }
        };
        if backend.check().is_err() {
            return self.unknown();
        };
        if !Arc::ptr_eq(&policy.generation, &self.generation)
            || self.graph.state() != self.plan.as_ref().expect("unconsumed plan").graph
        {
            drop(policy);
            drop(writer);
            return self.reject_postgres(backend, HistoryWriteRejection::Conflict);
        }
        let store = self.store.clone();
        let state = loop {
            if let Some(reason) = self.stopped() {
                drop(policy);
                drop(writer);
                return self.reject_postgres(backend, reason);
            };
            if let Some(guard) = store.inner.state.try_lock_for(Duration::from_millis(5)) {
                break guard;
            }
        };
        let point = self.plan.as_ref().expect("unconsumed plan").point.clone();
        let series = loop {
            if let Some(reason) = self.stopped() {
                drop(state);
                drop(policy);
                drop(writer);
                return self.reject_postgres(backend, reason);
            };
            if let Some(guard) = point.try_write_for(Duration::from_millis(5)) {
                break guard;
            }
        };
        let mut plan = self.plan.take().expect("unconsumed plan");
        if store.series_state(&series) != plan.change.before {
            drop(series);
            drop(state);
            drop(policy);
            drop(writer);
            return self.reject_postgres(backend, HistoryWriteRejection::Conflict);
        }
        let Some(sequence) = state.sequence.checked_add(1) else {
            drop(series);
            drop(state);
            drop(policy);
            drop(writer);
            return self.reject_postgres(backend, HistoryWriteRejection::Limit);
        };
        plan.change.sequence = sequence;
        plan.receipt.change_sequence = sequence;
        let committed = HistoryWriteOutcome::Committed(plan.receipt.clone());
        let before_sequence = state.sequence;
        drop(series);
        drop(state);
        // Preparation and publication are excluded by the writer gate. Never
        // hold a point or history-capture lock while awaiting SQL or a pool slot.
        let result=backend.runtime.block_on(async {
            let encoded=encode_outcome(&committed)?;
            let change=history_value::encode_change(&plan.change)?;
            let mut bytes=change.len();
            let samples=plan.next.items.iter().map(|item| {
                let encoded=history_value::encode_sample(item)?;
                bytes=bytes.saturating_add(encoded.len());
                if bytes>backend.limits.recovery.max_bytes {return Err(PostgresError::Limit)};
                Ok((item.ts.timestamp(),item.ts.timestamp_subsec_nanos() as i32,encoded))
            }).collect::<Result<Vec<_>,PostgresError>>()?;
            if samples.len()>backend.limits.max_history_samples {return Err(PostgresError::Limit)};
            let key=self.binding.storage_key()?;
            let mut tx=backend.transaction(self.deadline).await?;
            let prepared=tokio::time::timeout_at(tokio::time::Instant::from_std(self.deadline),async {
            let current=sqlx::query("SELECT history_sequence::text,revision::text,incarnation,catalog_generation::text FROM metadata WHERE singleton=1")
                .fetch_one(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            let get = |name| current.try_get::<String,_>(name).map_err(|_|PostgresError::Incompatible);
            if value::number(&get("history_sequence")?)? != before_sequence
                || value::number(&get("revision")?)? != plan.graph.revision
                || value::number(&get("catalog_generation")?)? != plan.graph.catalog_generation
                || current.try_get::<Vec<u8>,_>("incarnation").map_err(|_|PostgresError::Incompatible)? != plan.graph.incarnation
            {return Err(PostgresError::Conflict)};
            let stored:Vec<u8>=sqlx::query_scalar("SELECT outcome FROM receipts WHERE domain=2 AND binding=$1")
                .bind(&key).fetch_one(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            let stored=decode_outcome(&stored)?;
            if !matches!(stored,HistoryWriteOutcome::Unknown {cause:HistoryWriteUnknown::Pending,..}) {
                return Ok(Some(stored));
            }
            let other:i64=sqlx::query_scalar("SELECT count(*) FROM history_samples WHERE point<>$1").bind(&plan.change.point).fetch_one(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            if usize::try_from(other).map_err(|_|PostgresError::Limit)?.saturating_add(samples.len())>backend.limits.max_history_samples {return Err(PostgresError::Limit)};
            let retained:i64=sqlx::query_scalar("SELECT (SELECT coalesce(sum(octet_length(point)+octet_length(payload)),0)::bigint FROM history_samples WHERE point<>$1) + (SELECT coalesce(sum(octet_length(id)+coalesce(octet_length(evicted),0)),0)::bigint FROM history_points WHERE id<>$1) + (SELECT coalesce(sum(octet_length(payload)),0)::bigint FROM history_changes WHERE sequence>$2::text::numeric)")
                .bind(&plan.change.point).bind(sequence.saturating_sub(store.inner.limits.change_capacity as u64).to_string())
                .fetch_one(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            let retained=usize::try_from(retained).map_err(|_|PostgresError::Limit)?;
            let next_bytes=samples.iter().fold(change.len(),|n,(_,_,payload)|n.saturating_add(plan.change.point.len()).saturating_add(payload.len()))
                .saturating_add(plan.change.point.len()).saturating_add(plan.next.evicted.map_or(0,|ts|history_value::timestamp(ts).len()));
            if retained.saturating_add(next_bytes)>backend.limits.recovery.max_bytes {return Err(PostgresError::Limit)};
            sqlx::query("INSERT INTO history_points(id,incarnation,generation,evicted) VALUES($1,$2,$3::text::numeric,$4) ON CONFLICT(id) DO UPDATE SET incarnation=excluded.incarnation,generation=excluded.generation,evicted=excluded.evicted")
                .bind(&plan.change.point).bind(plan.next.incarnation.as_slice()).bind(plan.next.generation.to_string())
                .bind(plan.next.evicted.map(|ts|history_value::timestamp(ts).into_bytes())).execute(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            sqlx::query("DELETE FROM history_samples WHERE point=$1").bind(&plan.change.point).execute(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            for (seconds,nanos,payload) in &samples {
                sqlx::query("INSERT INTO history_samples(point,seconds,nanos,payload) VALUES($1,$2,$3,$4)").bind(&plan.change.point).bind(seconds).bind(nanos).bind(payload).execute(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            }
            sqlx::query("INSERT INTO history_changes(sequence,payload) VALUES($1::text::numeric,$2)").bind(sequence.to_string()).bind(change).execute(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            sqlx::query("DELETE FROM history_changes WHERE sequence<=$1::text::numeric").bind(sequence.saturating_sub(store.inner.limits.change_capacity as u64).to_string()).execute(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            sqlx::query("UPDATE metadata SET history_sequence=$1::text::numeric WHERE singleton=1").bind(sequence.to_string()).execute(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            sqlx::query("UPDATE receipts SET outcome=$2 WHERE domain=2 AND binding=$1").bind(key).bind(encoded).execute(&mut *tx).await.map_err(|_|PostgresError::Unavailable)?;
            if self.stopped().is_some() {return Err(PostgresError::Deadline)};
            Ok(None)
            }).await.unwrap_or(Err(PostgresError::Deadline));
            match prepared {
                Err(error)=>{tx.rollback().await.map_err(|_|PostgresError::Unavailable)?;return Err(error)},
                Ok(Some(stored))=>{tx.rollback().await.map_err(|_|PostgresError::Unavailable)?;return Ok(stored)},
                Ok(None)=>{}
            }
            backend.commit_mutation(tx,self._lease.clone()).await?;
            Ok(committed.clone())
        });
        let resolved = match result {
            Ok(outcome) => outcome,
            Err(error) => match settle(
                backend,
                &self.binding,
                if error == PostgresError::Conflict {
                    HistoryWriteRejection::Conflict
                } else {
                    HistoryWriteRejection::Provider
                },
                Instant::now() + backend.limits.statement_timeout,
            ) {
                Ok(Some(record)) => record.outcome,
                _ => {
                    return self.unknown();
                }
            },
        };
        let mut state = store.inner.state.lock();
        let mut series = point.write();
        if resolved == committed {
            *series = plan.next;
            store.append_change(&mut state, plan.change);
        }
        if let Some(record) = state.receipts.get_mut(&self.binding) {
            record.outcome = resolved.clone();
        } else {
            backend.seal();
            return self.unknown();
        };
        drop(series);
        drop(state);
        drop(policy);
        drop(writer);
        resolved
    }
}
