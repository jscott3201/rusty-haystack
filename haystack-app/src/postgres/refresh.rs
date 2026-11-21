//! One coherent database cut, published under the local writer/capture fences.
use super::{PostgresError, PostgresServiceLimits, recovery, storage::Backend};
use crate::{HisStore, history_store::HistoryStoreInner};
use haystack_core::{graph::SharedGraph, xeto::catalog::ActivatedCatalog};
use std::{
    sync::{Arc, Weak},
    time::Instant,
};

pub(crate) struct Projection {
    pub(crate) backend: Weak<Backend>,
    pub(crate) graph: SharedGraph,
    pub(crate) history: Weak<HistoryStoreInner>,
    pub(crate) catalog: Arc<ActivatedCatalog>,
    pub(crate) limits: PostgresServiceLimits,
    pub(crate) dataset: [u8; 16],
    pub(crate) cursor_key: [u8; 32],
}
pub(crate) struct Candidate {
    pub(crate) recovered: recovery::Recovered,
    pub(crate) history: HisStore,
    pub(crate) receipts: Vec<sqlx::postgres::PgRow>,
}
impl Backend {
    pub(crate) async fn capture(
        self: &Arc<Self>,
        catalog: Arc<ActivatedCatalog>,
        limits: &PostgresServiceLimits,
        deadline: Instant,
    ) -> Result<Candidate, PostgresError> {
        self.check()?;
        let mut tx = self.read_transaction(deadline).await?;
        let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            // READ COMMITTED obtains this row lock before reading any other
            // authority relation. All cooperating writers use FOR UPDATE.
            let recovered = recovery::read_graph(&mut tx, catalog, self.limits.recovery).await?;
            #[cfg(test)]
            self.refresh_barrier(super::storage::RefreshStage::AfterEntities)
                .await?;
            if recovered.generation != self.generation {
                return Err(PostgresError::Incompatible);
            }
            let receipts = self.receipt_rows_at(1, &mut tx).await?;
            let history = HisStore::from_postgres(
                self.clone(),
                &mut tx,
                recovered.history_authority,
                recovered.history_sequence,
                limits.history_store.clone(),
            )
            .await?;
            Ok(Candidate {
                recovered,
                history,
                receipts,
            })
        })
        .await
        .unwrap_or(Err(PostgresError::Deadline));
        let cleanup = tx.rollback().await;
        match result {
            Ok(candidate) => {
                cleanup?;
                Ok(candidate)
            }
            Err(error) => Err(error),
        }
    }
    /// Blocking worker only. This gate also protects local commit publication,
    /// preventing a delayed cut N from overwriting already published N+1.
    pub(crate) fn refresh(&self, deadline: Instant) -> Result<(), PostgresError> {
        let _writer = self.publication_guard(deadline)?;
        self.refresh_locked(deadline)
    }
    pub(crate) fn refresh_locked(&self, deadline: Instant) -> Result<(), PostgresError> {
        self.check()?;
        let projection = self.projection.get().ok_or(PostgresError::Unavailable)?;
        let backend = projection
            .backend
            .upgrade()
            .ok_or(PostgresError::Unavailable)?;
        // The capture bounds SQL work internally, then awaits rollback and
        // physical connection closure outside that deadline. Never cancel its
        // cleanup future and release an admission ahead of the actual resource.
        let candidate = self.runtime.block_on(backend.capture(
            projection.catalog.clone(),
            &projection.limits,
            deadline,
        ));
        #[cfg(test)]
        self.runtime
            .block_on(self.refresh_barrier(super::storage::RefreshStage::BeforePublication))?;
        let result = candidate.and_then(|candidate| self.publish_candidate(projection, candidate));
        if matches!(result, Err(PostgresError::Incompatible)) {
            self.seal();
        }
        result
    }
    fn publish_candidate(
        &self,
        projection: &Projection,
        candidate: Candidate,
    ) -> Result<(), PostgresError> {
        let history = projection
            .history
            .upgrade()
            .ok_or(PostgresError::Unavailable)?;
        let next = candidate.recovered;
        let expected = projection.graph.state();
        if next.dataset != projection.dataset
            || next.cursor_key != projection.cursor_key
            || next.history_authority != history.authority
            || next.graph.state().incarnation != expected.incarnation
            || next.graph.state().catalog_generation != expected.catalog_generation
            || next.graph.version() < expected.revision
        {
            return Err(PostgresError::Incompatible);
        }
        // Decode and validate receipt bounds/identities before publishing any
        // projection. The authoritative lookup still reads SQL on every call.
        let validation_graph = SharedGraph::from_restored(
            haystack_core::graph::EntityGraph::restore(
                next.graph
                    .image(self.limits.recovery)
                    .map_err(|_| PostgresError::Incompatible)?,
                projection.catalog.clone(),
                self.limits.recovery,
            )
            .map_err(|_| PostgresError::Incompatible)?,
        );
        let _ = crate::EntityMutationStore::from_postgres(
            validation_graph,
            next.dataset,
            projection
                .backend
                .upgrade()
                .ok_or(PostgresError::Unavailable)?,
            candidate.receipts,
            &projection.limits.entities,
        )?;
        let mut incoming = candidate.history.inner.state.lock();
        if incoming.sequence < history.state.lock().sequence {
            return Err(PostgresError::Incompatible);
        }
        projection
            .graph
            .publish_restored_with(expected, next.graph, self.limits.statement_timeout, || {
                let mut current = history.state.lock();
                for (id, point) in incoming.points.drain() {
                    if let Some(existing) = current.points.get(&id) {
                        // Retained sessions observe this same Arc and detect a
                        // changed generation rather than reading a stranded cache.
                        *existing.write() = Arc::try_unwrap(point)
                            .ok()
                            .expect("unshared recovery point")
                            .into_inner();
                    } else {
                        current.points.insert(id, point);
                    }
                }
                for (binding, record) in incoming.receipts.drain() {
                    current.receipts.insert(binding, record);
                }
                current.receipt_bytes = current.receipt_bytes.max(incoming.receipt_bytes);
                std::mem::swap(&mut current.changes, &mut incoming.changes);
                current.sequence = incoming.sequence;
            })
            .map_err(|_| PostgresError::Incompatible)
    }
}
