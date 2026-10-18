//! Durable selection for the same admitted entity mutation contract.
use super::*;
use crate::postgres::{PostgresError, storage::Backend};
use sqlx::Row;

impl EntityMutationStore {
    pub(crate) fn from_postgres(
        graph: SharedGraph,
        dataset: [u8; 16],
        backend: Arc<Backend>,
        rows: Vec<sqlx::postgres::PgRow>,
        limits: &MutationLimits,
    ) -> Result<Self, PostgresError> {
        let mut receipts = Receipts::default();
        for row in rows {
            let binding = Binding::from_storage_key(
                &row.try_get::<Vec<u8>, _>("binding")
                    .map_err(|_| PostgresError::Incompatible)?,
            )?;
            let canonical: Vec<u8> = row
                .try_get("request")
                .map_err(|_| PostgresError::Incompatible)?;
            let outcome: Vec<u8> = row
                .try_get("outcome")
                .map_err(|_| PostgresError::Incompatible)?;
            let request: EntityBatchRequest =
                entity::decode(&canonical).map_err(|_| PostgresError::Incompatible)?;
            let outcome: MutationOutcome =
                entity::decode(&outcome).map_err(|_| PostgresError::Incompatible)?;
            if binding.identity != request.identity
                || outcome.identity() != &request.identity
                || request.identity.dataset != dataset
                || entity::encode(&request).map_err(|_| PostgresError::Incompatible)? != canonical
            {
                return Err(PostgresError::Incompatible);
            }
            if let MutationOutcome::Committed(receipt) = &outcome
                && (receipt.qualification != ReceiptQualification::PostgresDurable
                    || receipt.after_revision > graph.version())
            {
                return Err(PostgresError::Incompatible);
            }
            let principal_bytes = match &binding.principal {
                PrincipalKey::Anonymous => 0,
                PrincipalKey::Authenticated(s) | PrincipalKey::Embedding(s) => s.len(),
            };
            receipts.bytes = receipts
                .bytes
                .checked_add(
                    canonical
                        .len()
                        .saturating_mul(32)
                        .saturating_add(principal_bytes.saturating_mul(2))
                        .saturating_add(8192),
                )
                .ok_or(PostgresError::Limit)?;
            if receipts.entries.len() >= limits.receipt_capacity
                || receipts.bytes > limits.receipt_bytes
            {
                return Err(PostgresError::Limit);
            };
            receipts.entries.insert(
                binding,
                Record {
                    canonical: canonical.into(),
                    request: Arc::new(request),
                    outcome,
                },
            );
        }
        Ok(Self {
            inner: Arc::new(Store {
                graph,
                dataset,
                native_writes: false,
                backend: Some(super::storage::Backend::Postgres(backend)),
                receipts: Mutex::new(receipts),
            }),
        })
    }
}

impl Backend {
    #[cfg(test)]
    pub(crate) async fn receipt_rows(
        &self,
        domain: i16,
    ) -> Result<Vec<sqlx::postgres::PgRow>, PostgresError> {
        let mut tx = self
            .transaction(Instant::now() + self.limits.transaction_timeout)
            .await?;
        let result = self.receipt_rows_at(domain, &mut tx).await;
        tx.rollback().await?;
        result
    }
    pub(crate) async fn receipt_rows_at(
        &self,
        domain: i16,
        connection: &mut sqlx::PgConnection,
    ) -> Result<Vec<sqlx::postgres::PgRow>, PostgresError> {
        let counts = sqlx::query("SELECT count(*) AS n, coalesce(sum(octet_length(binding)+octet_length(request)+octet_length(outcome)),0)::bigint AS bytes FROM receipts WHERE domain=$1")
            .bind(domain).fetch_one(&mut *connection).await.map_err(crate::postgres::storage::read_error)?;
        if counts
            .try_get::<i64, _>("n")
            .map_err(|_| PostgresError::Incompatible)?
            > self.limits.max_receipts as i64
            || counts
                .try_get::<i64, _>("bytes")
                .map_err(|_| PostgresError::Incompatible)?
                > self.limits.max_receipt_bytes as i64
        {
            return Err(PostgresError::Limit);
        };
        sqlx::query("SELECT binding,request,outcome FROM receipts WHERE domain=$1 ORDER BY binding")
            .bind(domain)
            .fetch_all(&mut *connection)
            .await
            .map_err(crate::postgres::storage::read_error)
    }
}

fn record(
    stored: crate::postgres::storage::StoredReceipt,
    binding: &Binding,
) -> Result<Record, PostgresError> {
    let request: EntityBatchRequest =
        entity::decode(&stored.request).map_err(|_| PostgresError::Incompatible)?;
    let outcome: MutationOutcome =
        entity::decode(&stored.outcome).map_err(|_| PostgresError::Incompatible)?;
    if request.identity != binding.identity
        || outcome.identity() != &binding.identity
        || entity::encode(&request).map_err(|_| PostgresError::Incompatible)? != stored.request
    {
        return Err(PostgresError::Incompatible);
    };
    Ok(Record {
        canonical: stored.request.into(),
        request: Arc::new(request),
        outcome,
    })
}
pub(super) fn reserve(
    backend: &Backend,
    binding: &Binding,
    request: &[u8],
    pending: &MutationOutcome,
    deadline: Instant,
) -> Result<Option<Record>, PostgresError> {
    let key = binding.storage_key()?;
    let pending = entity::encode(pending).map_err(|_| PostgresError::Incompatible)?;
    #[cfg(test)]
    if let Some(seed) = backend.reservation_seed.lock().take() {
        // Model a prior committed reservation whose acknowledgment was lost
        // before memory-cache insertion. This boundary is reached only after
        // the initial SQL absence lookup and the in-memory cache miss.
        assert!(
            backend
                .runtime
                .block_on(
                    backend.reserve_receipt(1, &key, &seed.request, &seed.outcome, deadline,)
                )?
                .is_none()
        );
    }
    backend
        .runtime
        .block_on(backend.reserve_receipt(1, &key, request, &pending, deadline))?
        .map(|stored| record(stored, binding))
        .transpose()
}
pub(super) fn lookup(
    backend: &Backend,
    binding: &Binding,
    deadline: Instant,
) -> Result<Option<Record>, PostgresError> {
    let _publication = backend.publication_guard(deadline)?;
    let key = binding.storage_key()?;
    let record = backend
        .runtime
        .block_on(backend.lookup_receipt(1, &key))?
        .map(|stored| record(stored, binding))
        .transpose()?;
    if record
        .as_ref()
        .is_some_and(|record| matches!(record.outcome, MutationOutcome::Committed(_)))
    {
        backend.refresh_locked(deadline)?;
    }
    Ok(record)
}
pub(super) fn settle(
    backend: &Backend,
    binding: &Binding,
    reason: RejectionReason,
    deadline: Instant,
) -> Result<Option<Record>, PostgresError> {
    let key = binding.storage_key()?;
    let pending = entity::encode(&MutationOutcome::Unknown {
        identity: binding.identity.clone(),
        cause: UnknownCause::Pending,
    })
    .map_err(|_| PostgresError::Incompatible)?;
    let rejected = entity::encode(&MutationOutcome::Rejected {
        identity: binding.identity.clone(),
        reason,
    })
    .map_err(|_| PostgresError::Incompatible)?;
    backend
        .runtime
        .block_on(backend.settle_receipt(1, &key, &pending, &rejected, deadline))?
        .map(|stored| record(stored, binding))
        .transpose()
}
impl PreparedMutation {
    pub(super) fn reject_postgres(
        self,
        backend: &Backend,
        reason: RejectionReason,
    ) -> MutationOutcome {
        // Rejection persistence is itself bounded. Failure retains uncertainty;
        // it does not free/rebind the operation identity for replay.
        let deadline = Instant::now() + backend.limits.statement_timeout;
        match settle(backend, &self.binding, reason, deadline) {
            Ok(Some(record)) => {
                self.cache_outcome(&record.outcome);
                record.outcome
            }
            _ => self.unknown(),
        }
    }
    pub(super) fn publish_postgres(self, backend: &Backend) -> MutationOutcome {
        let policy_owner = self.policy.clone();
        let policy = loop {
            if let Some(reason) = self.stop() {
                return self.reject_postgres(backend, reason);
            };
            if let Some(policy) = policy_owner.try_read_for(std::time::Duration::from_millis(5)) {
                break policy;
            }
        };
        let writer = loop {
            if let Some(reason) = self.stop() {
                return self.reject_postgres(backend, reason);
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
            return self.reject_postgres(backend, RejectionReason::Conflict);
        }
        let expected = self.plan.state();
        let candidate = loop {
            if let Some(reason) = self.stop() {
                drop(policy);
                drop(writer);
                return self.reject_postgres(backend, reason);
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
                return self.reject_postgres(backend, reason);
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
                return self.reject_postgres(backend, batch_reason(error));
            }
        };
        if candidate.apply_prepared(plan).is_err() {
            drop(policy);
            drop(writer);
            return self.reject_postgres(backend, RejectionReason::Capacity);
        }
        // Every clone, index build and bounded serialization occurs before COMMIT.
        let image = match candidate.image(backend.limits.recovery) {
            Ok(image) => image,
            Err(_) => {
                drop(policy);
                drop(writer);
                return self.reject_postgres(backend, RejectionReason::Capacity);
            }
        };
        let rows = image
            .rows
            .iter()
            .map(|row| {
                Ok((
                    row.id().ok_or(PostgresError::Incompatible)?.val.clone(),
                    crate::postgres::value::encode_dict(row)?,
                ))
            })
            .collect::<Result<Vec<_>, PostgresError>>();
        let changes = image
            .changes
            .iter()
            .map(|change| {
                Ok((
                    change.version,
                    crate::postgres::value::encode_change(change)?,
                ))
            })
            .collect::<Result<Vec<_>, PostgresError>>();
        let (rows, changes) = match (rows, changes) {
            (Ok(rows), Ok(changes)) => (rows, changes),
            _ => {
                drop(policy);
                drop(writer);
                return self.reject_postgres(backend, RejectionReason::Limit);
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
            return self.reject_postgres(backend, RejectionReason::Limit);
        }
        let outcome = MutationOutcome::Committed(self.receipt.clone());
        let result = backend.runtime.block_on(async {
            let key = self.binding.storage_key()?;
            let mut tx = backend.transaction(self.deadline).await?;
            let prepared = tokio::time::timeout_at(tokio::time::Instant::from_std(self.deadline), async {
            let state = sqlx::query("SELECT revision::text,incarnation,catalog_generation::text FROM metadata WHERE singleton=1")
                .fetch_one(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
            if crate::postgres::value::number(&state.try_get::<String,_>("revision").map_err(|_| PostgresError::Incompatible)?)? != expected.revision
                || state.try_get::<Vec<u8>,_>("incarnation").map_err(|_| PostgresError::Incompatible)? != expected.incarnation
                || crate::postgres::value::number(&state.try_get::<String,_>("catalog_generation").map_err(|_| PostgresError::Incompatible)?)? != expected.catalog_generation
            {return Err(PostgresError::Conflict)};
            let stored: Vec<u8> = sqlx::query_scalar("SELECT outcome FROM receipts WHERE domain=1 AND binding=$1")
                .bind(&key).fetch_one(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
            let stored: MutationOutcome = entity::decode(&stored).map_err(|_| PostgresError::Incompatible)?;
            if !matches!(stored,MutationOutcome::Unknown {cause:UnknownCause::Pending,..}) {
                return Ok(Some(stored));
            }
            // A bounded full projection replacement is deliberately simple for
            // this first profile; SQL IDs remain useful lossless query keys.
            sqlx::query("DELETE FROM entities").execute(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
            for (id,payload) in &rows {
                sqlx::query("INSERT INTO entities(id,payload) VALUES($1,$2)").bind(id).bind(payload).execute(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
            }
            sqlx::query("DELETE FROM entity_changes").execute(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
            for (revision,payload) in &changes {
                sqlx::query("INSERT INTO entity_changes(revision,payload) VALUES($1::text::numeric,$2)").bind(revision.to_string()).bind(payload).execute(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
            }
            sqlx::query("UPDATE metadata SET revision=$1::text::numeric,floor=$2::text::numeric WHERE singleton=1")
                .bind(image.state.revision.to_string()).bind(image.floor.to_string()).execute(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
            let encoded = entity::encode(&outcome).map_err(|_| PostgresError::Limit)?;
            sqlx::query("UPDATE receipts SET outcome=$2 WHERE domain=1 AND binding=$1")
                .bind(&key).bind(encoded).execute(&mut *tx).await.map_err(|_| PostgresError::Unavailable)?;
            if self.stop().is_some() {
                return Err(PostgresError::Deadline);
            }
            Ok(None)
            }).await.unwrap_or(Err(PostgresError::Deadline));
            match prepared {
                Err(error) => { tx.rollback().await.map_err(|_| PostgresError::Unavailable)?; return Err(error); },
                Ok(Some(stored)) => { tx.rollback().await.map_err(|_| PostgresError::Unavailable)?; return Ok(stored); },
                Ok(None) => {}
            }
            // After this call begins, any error is uncertain until a fresh
            // serialization barrier and receipt read settle its actual effect.
            backend.commit_mutation(tx,self._lease.clone()).await?;
            Ok(outcome.clone())
        });
        let resolved = match result {
            Ok(result) => result,
            Err(error) => match settle(
                backend,
                &self.binding,
                if error == PostgresError::Conflict {
                    RejectionReason::Conflict
                } else {
                    RejectionReason::Provider
                },
                Instant::now() + backend.limits.statement_timeout,
            ) {
                Ok(Some(record)) => record.outcome,
                _ => {
                    return self.unknown();
                }
            },
        };
        if resolved != outcome {
            drop(policy);
            drop(writer);
            self.cache_outcome(&resolved);
            return resolved;
        }
        // The SQL authority now proves commit. Publication failure cannot be
        // reported as Rejected or permit further stale projection service.
        self.cache_outcome(&outcome);
        drop(policy);
        if self
            .store
            .graph
            .publish_restored(expected, candidate, backend.limits.statement_timeout)
            .is_err()
        {
            backend.seal();
            return self.unknown();
        }
        drop(writer);
        outcome
    }
}

struct DurableProvider;
impl MutationProvider for DurableProvider {
    fn qualification(&self) -> ReceiptQualification {
        ReceiptQualification::PostgresDurable
    }
    fn commit(&self, prepared: PreparedMutation) -> MutationOutcome {
        prepared.publish()
    }
}
impl MutationService {
    pub(crate) fn postgres(
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
