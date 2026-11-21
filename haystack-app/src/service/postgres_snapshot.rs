//! One bounded SQL snapshot, owned by the original application admission.
use super::*;
use crate::postgres::{
    storage::{Backend, OwnedTransaction},
    value,
};
use haystack_core::graph::{EntityGraph, GraphImage, GraphState};
use sqlx::Row;
use tokio::sync::{mpsc, oneshot};

struct Request {
    request: ReadRequest,
    response: oneshot::Sender<Result<ReadPage, ReadError>>,
}

/// A bounded `REPEATABLE READ READ ONLY` session. Every page, filter traversal
/// and reference display uses the same recovered SQL snapshot and the caller's
/// current read policy. Budgets accumulate across pages; the original deadline,
/// cancellation, application work lease and pool capacity remain owned until
/// the SQL transaction and physical connection have actually closed.
///
/// This session covers entities only. Ordinary history retains its explicitly
/// bounded live semantics. Sessions and page cursors do not survive restart.
#[must_use = "close the snapshot to await transaction cleanup"]
pub struct PostgresSnapshot {
    sender: Option<mpsc::Sender<Request>>,
    completion: Option<oneshot::Receiver<Result<(), ReadError>>>,
    cancel: CancellationToken,
    deadline: Instant,
    state: GraphState,
}
impl PostgresSnapshot {
    pub fn state(&self) -> GraphState {
        self.state
    }
    pub async fn read(&mut self, request: ReadRequest) -> Result<ReadPage, ReadError> {
        let mut abandoned = CancelOnDrop {
            token: self.cancel.clone(),
            armed: true,
        };
        let (response, result) = oneshot::channel();
        self.sender
            .as_ref()
            .ok_or(ReadError::Unavailable)?
            .try_send(Request { request, response })
            .map_err(|_| ReadError::Unavailable)?;
        let output = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(ReadError::Cancelled),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(self.deadline)) => Err(ReadError::Deadline),
            result = result => result.unwrap_or(Err(ReadError::Unavailable)),
        };
        if output.is_ok() {
            abandoned.armed = false;
        }
        output
    }
    /// Request closure and await actual rollback, physical connection release
    /// and application admission release. Dropping requests the same cleanup;
    /// the owning worker retains all resources until it finishes.
    pub async fn close(mut self) -> Result<(), ReadError> {
        self.sender.take();
        self.cancel.cancel();
        match self.completion.take() {
            Some(done) => done.await.unwrap_or(Err(ReadError::Unavailable)),
            None => Ok(()),
        }
    }
}
impl Drop for PostgresSnapshot {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl ReadService {
    /// Open an entity snapshot through this service's ordinary bounded admission.
    /// Available only when PostgreSQL is the selected application authority.
    pub async fn postgres_snapshot(
        &self,
        context: ReadContext,
    ) -> Result<PostgresSnapshot, ReadError> {
        self.begin(context).await?.postgres_snapshot().await
    }
    #[cfg(test)]
    pub(crate) fn postgres_backend(&self) -> Arc<Backend> {
        self.inner
            .backend
            .clone()
            .expect("selected PostgreSQL service")
    }
}
impl ReadAdmission {
    pub(super) async fn verify_postgres(mut self) -> Result<Self, ReadError> {
        let Some(backend) = self.inner.backend.clone() else {
            return Ok(self);
        };
        self.retain_work()?;
        let owned = self.budget.as_ref().expect("live admission").clone();
        let observer = owned.clone();
        let (send, receive) = oneshot::channel();
        self.runtime().spawn(async move {
            let result = backend
                .refresh_until(owned.deadline)
                .await
                .map_err(|_| ReadError::Unavailable);
            let _ = send.send(result);
            drop(owned);
        });
        tokio::select! {
            biased;
            _=observer.cancelled()=>return Err(observer.check().err().unwrap_or(ReadError::Cancelled)),
            _=tokio::time::sleep_until(tokio::time::Instant::from_std(observer.deadline))=>return Err(ReadError::Deadline),
            result=receive=>result.map_err(|_|ReadError::Unavailable)??,
        }
        observer.check()?;
        Ok(self)
    }
    pub async fn postgres_snapshot(self) -> Result<PostgresSnapshot, ReadError> {
        let inner = self.inner.clone();
        let backend = inner.backend.clone().ok_or(ReadError::Unavailable)?;
        let runtime = self.runtime();
        let (principal, mut budget) = self.into_session()?;
        budget.deadline = budget
            .deadline
            .min(Instant::now() + backend.limits.snapshot_timeout);
        let cancel = budget.cancel.clone();
        let deadline = budget.deadline;
        let mut abandoned = CancelOnDrop {
            token: cancel.clone(),
            armed: true,
        };
        let (sender, receiver) = mpsc::channel(1);
        let (ready, opened) = oneshot::channel();
        let (completion, done) = oneshot::channel();
        runtime.spawn_blocking(move || {
            let result = run(inner, backend, principal, &mut budget, receiver, ready);
            // A completion receipt observes actual lease release as well as SQL
            // cleanup; waking before this drop would race owner drain.
            drop(budget);
            let _ = completion.send(result);
        });
        let state = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ReadError::Cancelled),
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return Err(ReadError::Deadline),
            opened = opened => opened.map_err(|_|ReadError::Unavailable)??,
        };
        abandoned.armed = false;
        Ok(PostgresSnapshot {
            sender: Some(sender),
            completion: Some(done),
            cancel,
            deadline,
            state,
        })
    }
}
fn run(
    inner: Arc<Inner>,
    backend: Arc<Backend>,
    principal: Principal,
    budget: &mut Budget,
    mut receiver: mpsc::Receiver<Request>,
    ready: oneshot::Sender<Result<GraphState, ReadError>>,
) -> Result<(), ReadError> {
    let runtime = backend.runtime.clone();
    let mut tx = match runtime.block_on(async {
        #[cfg(test)]
        backend
            .snapshot_acquiring
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let acquired = tokio::time::timeout_at(
            tokio::time::Instant::from_std(budget.deadline),
            backend.pool.acquire(),
        )
        .await;
        #[cfg(test)]
        backend
            .snapshot_acquiring
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        let mut connection = acquired
            .map_err(|_| ReadError::Deadline)?
            .map_err(|_| ReadError::Unavailable)?;
        // Session gate precedes BEGIN: RR's first statement must not freeze
        // a snapshot while waiting behind an exclusive schema transition.
        connection.close_on_drop();
        if backend.schema_shared(&mut connection).await.is_err() {
            // The schema gate precedes OwnedTransaction, but its failure must
            // still retain this worker and admission through physical cleanup.
            backend.close_snapshot_setup(connection).await;
            return Err(ReadError::Unavailable);
        }
        OwnedTransaction::begin(connection, true, backend.limits.statement_timeout)
            .await
            .map_err(|_| ReadError::Unavailable)
    }) {
        Ok(tx) => tx,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            return Err(error);
        }
    };
    let deadline = budget.deadline;
    let graph = runtime
        .block_on(tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            recover(&mut tx, &inner, &backend, budget),
        ))
        .unwrap_or(Err(ReadError::Deadline));
    let result = match graph {
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            Err(error)
        }
        Ok(graph) => {
            if ready.send(Ok(graph.state())).is_err() {
                Ok(())
            } else {
                loop {
                    let request=runtime.block_on(async {
                        tokio::select! {
                            biased;
                            _=budget.cancelled()=>None,
                            _=tokio::time::sleep_until(tokio::time::Instant::from_std(budget.deadline))=>None,
                            request=receiver.recv()=>request,
                        }
                    });
                    let Some(request) = request else { break Ok(()) };
                    let result = budget
                        .check()
                        .and_then(|()| backend.check().map_err(|_| ReadError::Unavailable))
                        .and_then(|()| {
                            inner.execute_on_graph(
                                principal.clone(),
                                Input::Request(request.request),
                                budget,
                                &graph,
                            )
                        });
                    let failed = result.is_err();
                    if request.response.send(result).is_err() || failed {
                        break Ok(());
                    };
                }
            }
        }
    };
    let cleanup = runtime
        .block_on(tx.rollback())
        .map_err(|_| ReadError::Unavailable);
    cleanup.and(result)
}
async fn recover(
    tx: &mut OwnedTransaction,
    inner: &Inner,
    backend: &Backend,
    budget: &mut Budget,
) -> Result<SharedGraph, ReadError> {
    budget.check()?;
    let catalog = inner
        .graph
        .read(|graph| graph.activated_catalog().cloned())
        .ok_or(ReadError::Unavailable)?;
    let meta=sqlx::query("SELECT format,authority_state,CASE WHEN octet_length(incarnation)=16 THEN incarnation END AS incarnation,revision::text,catalog_generation::text,owner_generation,CASE WHEN octet_length(catalog)<=1048576 THEN catalog END AS catalog FROM metadata WHERE singleton=1")
        .fetch_one(&mut **tx).await.map_err(|_|ReadError::Unavailable)?;
    let get = |name| {
        meta.try_get::<String, _>(name)
            .map_err(|_| ReadError::Unavailable)
    };
    if meta
        .try_get::<i64, _>("owner_generation")
        .map_err(|_| ReadError::Unavailable)?
        != backend.generation
        || meta
            .try_get::<i32, _>("format")
            .map_err(|_| ReadError::Unavailable)?
            != 3
        || get("authority_state")? != "active"
        || get("catalog")? != catalog.selection_identity()
    {
        return Err(ReadError::Unavailable);
    };
    let state = GraphState {
        incarnation: value::fixed(
            meta.try_get("incarnation")
                .map_err(|_| ReadError::Unavailable)?,
        )
        .map_err(|_| ReadError::Unavailable)?,
        revision: value::number(&get("revision")?).map_err(|_| ReadError::Unavailable)?,
        catalog_generation: value::number(&get("catalog_generation")?)
            .map_err(|_| ReadError::Unavailable)?,
    };
    let counts=sqlx::query("SELECT count(*) AS rows,coalesce(sum(octet_length(id)+octet_length(payload)),0)::bigint AS bytes FROM entities")
        .fetch_one(&mut **tx).await.map_err(|_|ReadError::Unavailable)?;
    let count = |name| {
        counts
            .try_get::<i64, _>(name)
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(ReadError::Unavailable)
    };
    let rows = count("rows")?;
    let bytes = count("bytes")?;
    if rows > backend.limits.recovery.max_rows || bytes > backend.limits.recovery.max_bytes {
        return Err(ReadError::Budget(BudgetKind::Retained));
    };
    budget.charge(
        BudgetKind::Retained,
        bytes
            .saturating_mul(32)
            .saturating_add(rows.saturating_mul(4096)),
    )?;
    budget.charge(
        BudgetKind::Work,
        bytes
            .saturating_mul(4)
            .saturating_add(rows.saturating_mul(256)),
    )?;
    let rows = sqlx::query("SELECT id,payload FROM entities ORDER BY id")
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| ReadError::Unavailable)?;
    let rows = rows
        .into_iter()
        .map(|row| {
            budget.check()?;
            let id: String = row.try_get("id").map_err(|_| ReadError::Unavailable)?;
            let payload: Vec<u8> = row.try_get("payload").map_err(|_| ReadError::Unavailable)?;
            let dict = value::decode_dict(&payload).map_err(|_| ReadError::Unavailable)?;
            if dict.id().is_none_or(|reference| reference.val != id) {
                return Err(ReadError::Unavailable);
            };
            Ok(dict)
        })
        .collect::<Result<Vec<_>, ReadError>>()?;
    let graph = EntityGraph::restore(
        GraphImage {
            state,
            rows,
            floor: state.revision,
            changes: vec![],
        },
        catalog,
        backend.limits.recovery,
    )
    .map_err(|error| match error {
        haystack_core::graph::RecoveryError::Limit => ReadError::Budget(BudgetKind::Retained),
        _ => ReadError::Unavailable,
    })?;
    budget.check()?;
    Ok(SharedGraph::from_restored(graph))
}
