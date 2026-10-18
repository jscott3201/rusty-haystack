//! SQLite persistence for the admitted entity mutation contract.
use super::*;
use crate::sqlite::{
    SqliteError,
    recovery::{self, StoredReceipt},
    storage::{Backend, Stop, sql_error},
};
use rusqlite::{Connection, OptionalExtension, params};

fn record(stored: StoredReceipt, binding: &Binding) -> Result<Record, SqliteError> {
    let request: EntityBatchRequest =
        entity::decode(&stored.request).map_err(|_| SqliteError::Incompatible)?;
    let outcome: MutationOutcome =
        entity::decode(&stored.outcome).map_err(|_| SqliteError::Incompatible)?;
    if request.identity != binding.identity
        || outcome.identity() != &binding.identity
        || entity::encode(&request).map_err(|_| SqliteError::Incompatible)? != stored.request
        || entity::encode(&outcome).map_err(|_| SqliteError::Incompatible)? != stored.outcome
    {
        return Err(SqliteError::Incompatible);
    }
    Ok(Record {
        canonical: stored.request.into(),
        request: Arc::new(request),
        outcome,
    })
}
fn stored(connection: &Connection, key: &[u8]) -> Result<Option<StoredReceipt>, SqliteError> {
    connection
        .query_row(
            "SELECT request,outcome FROM receipts WHERE binding=?1",
            [key],
            |row| {
                Ok(StoredReceipt {
                    request: row.get(0)?,
                    outcome: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(sql_error)
}
impl EntityMutationStore {
    #[cfg(test)]
    pub(crate) fn sqlite_backend(&self) -> Arc<Backend> {
        match self.inner.backend.as_ref().expect("SQLite selection") {
            super::storage::Backend::Sqlite(backend) => backend.clone(),
            #[cfg(feature = "postgres")]
            _ => panic!("SQLite selection"),
        }
    }
    pub(crate) fn from_sqlite(
        graph: SharedGraph,
        dataset: [u8; 16],
        backend: Arc<Backend>,
        rows: Vec<(Vec<u8>, StoredReceipt)>,
        limits: &MutationLimits,
        stop: &Stop,
    ) -> Result<Self, SqliteError> {
        stop.check()?;
        let mut receipts = Receipts::default();
        for (key, stored) in rows {
            stop.check()?;
            let binding = Binding::from_storage_key(&key)?;
            let record = record(stored, &binding)?;
            stop.check()?;
            if binding.identity.dataset != dataset
                || binding.identity.incarnation != graph.state().incarnation
            {
                return Err(SqliteError::Incompatible);
            }
            if let MutationOutcome::Committed(receipt) = &record.outcome
                && (receipt.qualification != ReceiptQualification::SqliteDurable
                    || receipt.after_revision > graph.version())
            {
                return Err(SqliteError::Incompatible);
            }
            let principal_bytes = match &binding.principal {
                PrincipalKey::Anonymous => 0,
                PrincipalKey::Authenticated(s) | PrincipalKey::Embedding(s) => s.len(),
            };
            receipts.bytes = receipts
                .bytes
                .checked_add(
                    record
                        .canonical
                        .len()
                        .saturating_mul(32)
                        .saturating_add(principal_bytes.saturating_mul(2))
                        .saturating_add(8192),
                )
                .ok_or(SqliteError::Limit)?;
            if receipts.entries.len() >= limits.receipt_capacity
                || receipts.bytes > limits.receipt_bytes
            {
                return Err(SqliteError::Limit);
            }
            receipts.entries.insert(binding, record);
        }
        stop.check()?;
        Ok(Self {
            inner: Arc::new(Store {
                graph,
                dataset,
                native_writes: false,
                backend: Some(super::storage::Backend::Sqlite(backend)),
                receipts: Mutex::new(receipts),
            }),
        })
    }
}
pub(super) fn reserve(
    backend: &Backend,
    binding: &Binding,
    request: &[u8],
    pending: &MutationOutcome,
    budget: &Budget,
) -> Result<Option<Record>, SqliteError> {
    let key = binding.storage_key()?;
    let pending = entity::encode(pending).map_err(|_| SqliteError::Limit)?;
    backend.transaction(&Stop::from_budget(budget), |connection| {
        if let Some(found) = stored(connection, &key)? { return Ok(Some(found)); }
        let (count, bytes): (i64, i64) = connection.query_row("SELECT count(*),coalesce(sum(length(binding)+length(request)+length(outcome)),0) FROM receipts", [], |row| Ok((row.get(0)?, row.get(1)?))).map_err(sql_error)?;
        if usize::try_from(count).map_err(|_| SqliteError::Limit)? >= backend.limits.max_receipts
            || usize::try_from(bytes).map_err(|_| SqliteError::Limit)?.saturating_add(key.len()).saturating_add(request.len()).saturating_add(pending.len()) > backend.limits.max_receipt_bytes { return Err(SqliteError::Limit); }
        connection.execute("INSERT INTO receipts(binding,request,outcome) VALUES(?1,?2,?3)", params![key, request, pending]).map_err(sql_error)?;
        Ok(None)
    })?.map(|found| record(found, binding)).transpose()
}
pub(super) fn lookup(
    backend: &Backend,
    binding: &Binding,
    budget: &Budget,
) -> Result<Option<Record>, SqliteError> {
    let stop = Stop::from_budget(budget);
    let _publication = backend.publication_guard(&stop)?;
    let key = binding.storage_key()?;
    let found = backend
        .read(&stop, |connection| stored(connection, &key))?
        .map(|found| record(found, binding))
        .transpose()?;
    if let Some(Record {
        outcome: MutationOutcome::Committed(receipt),
        ..
    }) = &found
    {
        let projection = backend.projection.get().ok_or(SqliteError::Unavailable)?;
        if receipt.qualification != ReceiptQualification::SqliteDurable
            || receipt.after_revision > projection.graph.version()
        {
            backend.seal();
            return Err(SqliteError::Unavailable);
        }
    }
    Ok(found)
}
/// Only the owning publisher settles a pending identity, after a new write
/// serialization barrier proves that its previous transaction has terminated.
fn settle_locked(
    backend: &Backend,
    binding: &Binding,
    reason: RejectionReason,
    stop: &Stop,
) -> Result<Record, SqliteError> {
    let key = binding.storage_key()?;
    backend.transaction(stop, |connection| {
        let mut found = stored(connection, &key)?.ok_or(SqliteError::Incompatible)?;
        let previous = record(
            StoredReceipt {
                request: found.request.clone(),
                outcome: found.outcome.clone(),
            },
            binding,
        )?;
        if matches!(
            previous.outcome,
            MutationOutcome::Unknown {
                cause: UnknownCause::Pending,
                ..
            }
        ) {
            let rejected = entity::encode(&MutationOutcome::Rejected {
                identity: binding.identity.clone(),
                reason,
            })
            .map_err(|_| SqliteError::Limit)?;
            let bytes: i64 = connection.query_row(
                "SELECT coalesce(sum(length(binding)+length(request)+length(outcome)),0) FROM receipts",
                [], |row| row.get(0),
            ).map_err(sql_error)?;
            let after = usize::try_from(bytes).ok()
                .and_then(|bytes| bytes.checked_sub(found.outcome.len()))
                .and_then(|bytes| bytes.checked_add(rejected.len()))
                .ok_or(SqliteError::Limit)?;
            if after > backend.limits.max_receipt_bytes {
                // A durable Pending binding must remain recoverable under the
                // same cap even when the final rejection envelope is larger.
                return Ok(previous);
            }
            found.outcome = rejected;
            connection
                .execute(
                    "UPDATE receipts SET outcome=?2 WHERE binding=?1",
                    params![key, found.outcome],
                )
                .map_err(sql_error)?;
        }
        record(found, binding)
    })
}
impl PreparedMutation {
    pub(super) fn reject_sqlite(
        self,
        backend: &Backend,
        reason: RejectionReason,
    ) -> MutationOutcome {
        let stop = Stop::new(
            Instant::now() + backend.limits.cleanup_timeout,
            CancellationToken::new(),
        );
        let Ok(_publication) = backend.publication_guard(&stop) else {
            return self.unknown();
        };
        match settle_locked(backend, &self.binding, reason, &stop) {
            Ok(record) => {
                self.cache_outcome(&record.outcome);
                record.outcome
            }
            Err(_) => self.unknown(),
        }
    }
    pub(super) fn publish_sqlite(self, backend: &Backend) -> MutationOutcome {
        let policy_owner = self.policy.clone();
        let policy = loop {
            if let Some(reason) = self.stop() {
                return self.reject_sqlite(backend, reason);
            };
            if let Some(policy) = policy_owner.try_read_for(std::time::Duration::from_millis(5)) {
                break policy;
            }
        };
        let writer = loop {
            if let Some(reason) = self.stop() {
                return self.reject_sqlite(backend, reason);
            };
            if let Some(guard) = backend
                .writer
                .try_lock_for(std::time::Duration::from_millis(5))
            {
                break guard;
            }
        };
        if backend.check().is_err() {
            return self.unknown();
        };
        if !Arc::ptr_eq(&policy.generation, &self.generation) {
            drop(policy);
            drop(writer);
            return self.reject_sqlite(backend, RejectionReason::Conflict);
        }
        let expected = self.plan.state();
        let candidate = loop {
            if let Some(reason) = self.stop() {
                drop(policy);
                drop(writer);
                return self.reject_sqlite(backend, reason);
            };
            if let Some(candidate) =
                self.store
                    .graph
                    .read_for(std::time::Duration::from_millis(5), |graph| {
                        if graph.state() != expected {
                            return Err(RejectionReason::Conflict);
                        };
                        let image = graph
                            .image(backend.limits.recovery)
                            .map_err(|_| RejectionReason::Capacity)?;
                        let catalog = graph
                            .activated_catalog()
                            .cloned()
                            .ok_or(RejectionReason::Conflict)?;
                        haystack_core::graph::EntityGraph::restore(
                            image,
                            catalog,
                            backend.limits.recovery,
                        )
                        .map_err(|_| RejectionReason::Provider)
                    })
            {
                break candidate;
            }
        };

        let mut candidate = match candidate {
            Ok(candidate) => candidate,
            Err(reason) => {
                drop(policy);
                drop(writer);
                return self.reject_sqlite(backend, reason);
            }
        };
        let request = match self.store.receipts.lock().entries.get(&self.binding) {
            Some(record) => record.request.clone(),
            None => return self.unknown(),
        };
        let plan = match candidate.prepare_batch(
            expected.revision,
            &request.operations,
            self.batch_limits,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                drop(policy);
                drop(writer);
                return self.reject_sqlite(backend, batch_reason(error));
            }
        };
        if candidate.apply_prepared(plan).is_err() {
            drop(policy);
            drop(writer);
            return self.reject_sqlite(backend, RejectionReason::Capacity);
        }
        // Every clone, index build and bounded serialization occurs before COMMIT.
        let image = match candidate.image(backend.limits.recovery) {
            Ok(image) => image,
            Err(_) => {
                drop(policy);
                drop(writer);
                return self.reject_sqlite(backend, RejectionReason::Capacity);
            }
        };
        let rows = image
            .rows
            .iter()
            .map(|row| {
                Ok((
                    row.id().ok_or(SqliteError::Incompatible)?.val.clone(),
                    crate::storage_value::encode_dict(row)?,
                ))
            })
            .collect::<Result<Vec<_>, SqliteError>>();
        let changes = image
            .changes
            .iter()
            .map(|change| Ok((change.version, crate::storage_value::encode_change(change)?)))
            .collect::<Result<Vec<_>, SqliteError>>();
        let (rows, changes) = match (rows, changes) {
            (Ok(rows), Ok(changes)) => (rows, changes),
            _ => {
                drop(policy);
                drop(writer);
                return self.reject_sqlite(backend, RejectionReason::Limit);
            }
        };
        let encoded_bytes = rows
            .iter()
            .fold(0usize, |total, (id, payload)| {
                total.saturating_add(id.len()).saturating_add(payload.len())
            })
            .saturating_add(changes.iter().fold(0usize, |total, (_, payload)| {
                total.saturating_add(payload.len())
            }));
        if encoded_bytes > backend.limits.recovery.max_bytes {
            drop(policy);
            drop(writer);
            return self.reject_sqlite(backend, RejectionReason::Limit);
        }
        let outcome = MutationOutcome::Committed(self.receipt.clone());
        let mut stop = Stop::new(self.deadline, self.cancel.clone());
        stop.owner_cancel = self.owner_cancel.clone();
        stop.owner_sealed = self.owner_sealed.clone();
        #[cfg(test)]
        let fault = backend.fault.lock().take();
        let result = (|| {
            let key = self.binding.storage_key()?;
            let encoded = entity::encode(&outcome).map_err(|_| SqliteError::Limit)?;
            let result = backend.transaction(&stop, |connection| {
                let metadata = recovery::metadata(connection)?;
                if metadata.state != expected || metadata.dataset != self.binding.identity.dataset
                    || recovery::schema_version(connection)? != backend.projection.get().ok_or(SqliteError::Unavailable)?.schema_version {
                    return Err(SqliteError::Conflict);
                }
                let previous = record(stored(connection, &key)?.ok_or(SqliteError::Incompatible)?, &self.binding)?;
                if !matches!(previous.outcome, MutationOutcome::Unknown { cause: UnknownCause::Pending, .. }) { return Ok(previous.outcome); }
                let bytes: i64 = connection.query_row("SELECT coalesce(sum(length(binding)+length(request)+length(outcome)),0) FROM receipts", [], |row| row.get(0)).map_err(sql_error)?;
                if usize::try_from(bytes).map_err(|_| SqliteError::Limit)?.saturating_add(encoded.len()) > backend.limits.max_receipt_bytes { return Err(SqliteError::Limit); }
                connection.execute("DELETE FROM entities", []).map_err(sql_error)?;
                for (id,payload) in &rows {
                    connection.execute("INSERT INTO entities(id,payload) VALUES(?1,?2)",params![id,payload]).map_err(sql_error)?;
                }
                connection.execute("DELETE FROM entity_changes", []).map_err(sql_error)?;
                for (revision,payload) in &changes {
                    connection.execute("INSERT INTO entity_changes(revision,payload) VALUES(?1,?2)",params![revision.to_be_bytes().as_slice(),payload]).map_err(sql_error)?;
                }
                connection.execute("UPDATE metadata SET revision=?1,floor=?2 WHERE singleton=1",params![image.state.revision.to_be_bytes().as_slice(),image.floor.to_be_bytes().as_slice()]).map_err(sql_error)?;
                connection.execute("UPDATE receipts SET outcome=?2 WHERE binding=?1",params![key,encoded]).map_err(sql_error)?;
                #[cfg(test)]
                crate::sqlite::qualification::before_commit(fault.as_ref(),connection)?;
                Ok(outcome.clone())
            })?;
            #[cfg(test)]
            crate::sqlite::qualification::after_commit(fault.as_ref(), backend)?;
            Ok::<_, SqliteError>(result)
        })();
        let resolved = match result {
            Ok(result) => result,
            Err(error) => {
                let cleanup = Stop::new(
                    Instant::now() + backend.limits.cleanup_timeout,
                    CancellationToken::new(),
                );
                let reason = self.stop().unwrap_or(match error {
                    SqliteError::Conflict => RejectionReason::Conflict,
                    SqliteError::Limit => RejectionReason::Limit,
                    _ => RejectionReason::Provider,
                });
                match settle_locked(backend, &self.binding, reason, &cleanup) {
                    Ok(record) => record.outcome,
                    Err(_) => return self.unknown(),
                }
            }
        };
        if resolved != outcome {
            self.cache_outcome(&resolved);
            return resolved;
        }
        // Durable proof precedes publication. Until publication completes the
        // writer gate blocks every read admission and receipt observation.
        drop(policy);
        if self
            .store
            .graph
            .publish_restored(expected, candidate, backend.limits.cleanup_timeout)
            .is_err()
        {
            backend.seal();
            return self.unknown();
        }
        self.cache_outcome(&outcome);
        drop(writer);
        outcome
    }
}
struct DurableProvider;
impl MutationProvider for DurableProvider {
    fn qualification(&self) -> ReceiptQualification {
        ReceiptQualification::SqliteDurable
    }
    fn commit(&self, prepared: PreparedMutation) -> MutationOutcome {
        prepared.publish()
    }
}
impl MutationService {
    pub(crate) fn sqlite(
        reads: ReadService,
        store: EntityMutationStore,
        policy: Arc<dyn MutationPolicy>,
        limits: MutationLimits,
        cursor_key: [u8; 32],
    ) -> Result<Self, ReadError> {
        let mut service =
            Self::with_provider(reads, store, policy, limits, Arc::new(DurableProvider))?;
        let inner = Arc::get_mut(&mut service.inner).ok_or(ReadError::Unavailable)?;
        inner.cursor_key = cursor_key;
        inner.started = None;
        Ok(service)
    }
}
