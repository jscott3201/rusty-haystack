//! Optional bounded PostgreSQL authority for the shared application services.
pub mod admin;
mod config;
pub(crate) mod history_value;
mod recovery;
mod refresh;
pub(crate) mod storage;
pub use crate::service::postgres_snapshot::PostgresSnapshot;
pub(crate) use crate::storage_value as value;
pub use config::{PostgresConfig, PostgresLimits, PostgresTransport};

/// Public failures deliberately omit driver messages, endpoints, SQL and
/// credential/certificate material. Connection errors have no public source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PostgresError {
    #[error("invalid PostgreSQL configuration")]
    Configuration,
    #[error("ambient PostgreSQL configuration is unsupported")]
    AmbientConfiguration,
    #[error("PostgreSQL connection or verified transport failed")]
    Connection,
    #[error("PostgreSQL dataset is unavailable")]
    Unavailable,
    #[error("PostgreSQL durability configuration is unsupported")]
    Durability,
    #[error("PostgreSQL schema or stored data is incompatible")]
    Incompatible,
    #[error("PostgreSQL recovery limit exceeded")]
    Limit,
    #[error("PostgreSQL prepared dependency changed")]
    Conflict,
    #[error("PostgreSQL operation deadline exceeded")]
    Deadline,
}

impl From<crate::storage_value::PayloadError> for PostgresError {
    fn from(error: crate::storage_value::PayloadError) -> Self {
        match error {
            crate::storage_value::PayloadError::Limit => Self::Limit,
            crate::storage_value::PayloadError::Incompatible => Self::Incompatible,
        }
    }
}

use crate::{
    ApplicationBuilder, ApplicationError, ApplicationResource, ReadyInfo, ResourceContext,
    ResourceFuture,
};
use haystack_core::xeto::catalog::{ActivatedCatalog, Catalog};
use std::sync::Arc;

/// Explicit independent authorities for the selected durable services.
pub struct PostgresPolicies {
    pub reads: Arc<dyn crate::ReadPolicy>,
    pub entities: Arc<dyn crate::MutationPolicy>,
    pub history: Arc<dyn crate::HistoryMutationPolicy>,
}
#[derive(Debug, Clone, Default)]
pub struct PostgresServiceLimits {
    pub reads: crate::ReadLimits,
    pub entities: crate::MutationLimits,
    pub history: crate::HistoryLimits,
    pub history_mutations: crate::HistoryMutationLimits,
    pub history_store: crate::HistoryStoreLimits,
}

struct DatabaseResource(Arc<storage::Backend>);
impl Drop for DatabaseResource {
    fn drop(&mut self) {
        // Dropping an unstarted builder still transfers cleanup to a live
        // owned task. Explicit application close awaits the resource directly.
        let backend = self.0.clone();
        self.0.runtime.spawn(async move {
            let _ = backend.close().await;
        });
    }
}

impl ApplicationResource for DatabaseResource {
    fn name(&self) -> &str {
        "postgres"
    }
    fn initialize(&mut self, context: ResourceContext) -> ResourceFuture<'_, ReadyInfo> {
        Box::pin(async move {
            self.0
                .health()
                .await
                .map_err(|e| ApplicationError::resource("postgres", e.to_string()))?;
            let backend = self.0.clone();
            let cancel = context.cancellation();
            context.spawn("postgres-refresh", async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep(backend.limits.refresh_interval) => {},
                    }
                    // Await the actual worker even when close is requested.
                    // A transient failure denies admissions but remains retryable.
                    let _ = backend.health().await;
                }
                Ok(())
            })?;
            Ok(ReadyInfo::default())
        })
    }
    fn rollback_start(&mut self) -> ResourceFuture<'_> {
        self.close()
    }
    fn close(&mut self) -> ResourceFuture<'_> {
        Box::pin(async move {
            self.0
                .close()
                .await
                .map_err(|e| ApplicationError::resource("postgres", e.to_string()))
        })
    }
}

/// Recover one bounded dataset and transfer its database lifetime to the shared
/// application. Its handle remains NotReady until `ApplicationBuilder::start`
/// has initialized every selected resource and listener. No caller receives the
/// mutable graph projection. Catalog replacement is unavailable in this profile.
pub async fn application(
    config: PostgresConfig,
    catalog: Catalog,
    policies: PostgresPolicies,
    limits: PostgresServiceLimits,
) -> Result<ApplicationBuilder, PostgresError> {
    // Cancellation detaches this bounded construction worker instead of
    // dropping a driver future with queued cleanup. No readiness handle has
    // escaped yet; a failed delivery awaits database closure in this worker.
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        match construct(config, catalog, policies, limits, None).await {
            Ok((builder, backend)) => {
                if let Err(undelivered) = send.send(Ok(builder)) {
                    let _ = backend.close().await;
                    drop(undelivered);
                }
            }
            Err(error) => {
                let _ = send.send(Err(error));
            }
        }
    });
    receive.await.map_err(|_| PostgresError::Unavailable)?
}
/// Resolve and pin the committed authority named by a control-schema config.
/// There is no fallback to a literal schema or a stale previous selection.
pub async fn selected_application(
    mut config: PostgresConfig,
    catalog: Catalog,
    policies: PostgresPolicies,
    limits: PostgresServiceLimits,
) -> Result<ApplicationBuilder, PostgresError> {
    let expected = admin::selected(config.clone()).await?;
    config.schema = expected.schema.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        match construct(config, catalog, policies, limits, Some(expected)).await {
            Ok((builder, backend)) => {
                if let Err(undelivered) = send.send(Ok(builder)) {
                    let _ = backend.close().await;
                    drop(undelivered);
                }
            }
            Err(error) => {
                let _ = send.send(Err(error));
            }
        }
    });
    receive.await.map_err(|_| PostgresError::Unavailable)?
}
async fn construct(
    config: PostgresConfig,
    catalog: Catalog,
    policies: PostgresPolicies,
    limits: PostgresServiceLimits,
    expected: Option<admin::Authority>,
) -> Result<(ApplicationBuilder, Arc<storage::Backend>), PostgresError> {
    let catalog =
        Arc::new(ActivatedCatalog::new(catalog, None).map_err(|_| PostgresError::Incompatible)?);
    let mut opened = storage::Opened::connect(config).await?;
    let recovered = match opened.recover_entities(catalog.clone()).await {
        Ok(recovered) => recovered,
        Err(error) => {
            use sqlx::Connection;
            let _ = tokio::time::timeout(
                opened.config.limits.statement_timeout,
                opened.connection.close(),
            )
            .await;
            return Err(error);
        }
    };
    if expected.as_ref().is_some_and(|pin| {
        pin.schema != opened.config.schema
            || pin.dataset != recovered.dataset
            || pin.incarnation != recovered.graph.state().incarnation
            || pin.epoch != recovered.generation
    }) {
        use sqlx::Connection;
        let _ = opened.connection.close().await;
        return Err(PostgresError::Conflict);
    }
    let backend = opened.finish(recovered.generation).await?;
    let result = async {
        let candidate = backend
            .capture(
                catalog.clone(),
                &limits,
                std::time::Instant::now() + backend.limits.transaction_timeout,
            )
            .await?;
        let recovered = candidate.recovered;
        let graph = haystack_core::graph::SharedGraph::from_restored(recovered.graph);
        let store = crate::EntityMutationStore::from_postgres(
            graph.clone(),
            recovered.dataset,
            backend.clone(),
            candidate.receipts,
            &limits.entities,
        )?;
        let history_store = candidate.history;
        backend
            .projection
            .set(refresh::Projection {
                backend: Arc::downgrade(&backend),
                graph: graph.clone(),
                history: Arc::downgrade(&history_store.inner),
                catalog,
                limits: limits.clone(),
                dataset: recovered.dataset,
                cursor_key: recovered.cursor_key,
            })
            .map_err(|_| PostgresError::Configuration)?;
        let mut builder = ApplicationBuilder::new(graph, policies.reads, limits.reads)
            .map_err(|_| PostgresError::Configuration)?;
        builder
            .configure_postgres(backend.clone())
            .map_err(|_| PostgresError::Configuration)?;
        let mutations = crate::MutationService::postgres(
            builder.handle().read_service(),
            store,
            policies.entities,
            limits.entities,
            recovered.cursor_key,
        )
        .map_err(|_| PostgresError::Configuration)?;
        let history = crate::HistoryService::new(
            builder.handle().read_service(),
            Arc::new(history_store),
            limits.history,
        )
        .map_err(|_| PostgresError::Configuration)?;
        let history_mutations = crate::HistoryMutationService::new(
            history.clone(),
            policies.history,
            limits.history_mutations,
        )
        .map_err(|_| PostgresError::Configuration)?;
        builder
            .entity_mutations(mutations)
            .and_then(|builder| builder.borrowed_history(history))
            .and_then(|builder| builder.history_mutations(history_mutations))
            .map(|builder| builder.owned_resource(DatabaseResource(backend.clone())))
            .map_err(|_| PostgresError::Configuration)
    }
    .await;
    if result.is_err() {
        let _ = backend.close().await;
    }
    result.map(|builder| (builder, backend))
}

#[cfg(test)]
mod qualification;
