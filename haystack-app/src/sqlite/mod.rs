//! Optional local-file SQLite entity authority. History is not selected.
mod config;
pub(crate) mod recovery;
pub(crate) mod storage;
pub use config::{SqliteConfig, SqliteLimits};

use crate::{
    ApplicationBuilder, ApplicationError, ApplicationResource, ReadyInfo, ResourceContext,
    ResourceFuture,
};
use haystack_core::{
    graph::SharedGraph,
    xeto::catalog::{ActivatedCatalog, Catalog},
};
use std::{sync::Arc, time::Instant};

/// Deliberately omits driver errors, SQL, and filesystem paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SqliteError {
    #[error("invalid SQLite configuration or unsupported file")]
    Configuration,
    #[error("SQLite file already has an application owner")]
    AlreadyOwned,
    #[error("SQLite file or connection is unavailable")]
    Unavailable,
    #[error("SQLite durability configuration is unsupported")]
    Durability,
    #[error("SQLite schema or stored data is incompatible")]
    Incompatible,
    #[error("SQLite recovery or retention limit exceeded")]
    Limit,
    #[error("SQLite prepared dependency changed")]
    Conflict,
    #[error("SQLite operation deadline exceeded")]
    Deadline,
    #[error("SQLite operation was cancelled")]
    Cancelled,
}
impl From<crate::storage_value::PayloadError> for SqliteError {
    fn from(error: crate::storage_value::PayloadError) -> Self {
        match error {
            crate::storage_value::PayloadError::Limit => Self::Limit,
            crate::storage_value::PayloadError::Incompatible => Self::Incompatible,
        }
    }
}

pub struct SqlitePolicies {
    pub reads: Arc<dyn crate::ReadPolicy>,
    pub entities: Arc<dyn crate::MutationPolicy>,
}
#[derive(Debug, Clone, Default)]
pub struct SqliteServiceLimits {
    pub reads: crate::ReadLimits,
    pub entities: crate::MutationLimits,
}

struct DatabaseResource(Arc<storage::Backend>);
impl Drop for DatabaseResource {
    fn drop(&mut self) {
        let backend = self.0.clone();
        self.0
            .runtime
            .spawn_blocking(move || backend.close_blocking());
    }
}
impl ApplicationResource for DatabaseResource {
    fn name(&self) -> &str {
        "sqlite"
    }
    fn initialize(&mut self, context: ResourceContext) -> ResourceFuture<'_, ReadyInfo> {
        Box::pin(async move {
            let backend = self.0.clone();
            let stop = storage::Stop::new(context.startup_deadline(), context.cancellation());
            let cancel = stop.cancel.clone().drop_guard();
            self.0
                .runtime
                .spawn_blocking(move || backend.verify(&stop))
                .await
                .map_err(|_| ApplicationError::resource("sqlite", "initialization worker failed"))?
                .map_err(|error| ApplicationError::resource("sqlite", error.to_string()))?;
            cancel.disarm();
            Ok(ReadyInfo::default())
        })
    }
    fn rollback_start(&mut self) -> ResourceFuture<'_> {
        self.close()
    }
    fn close(&mut self) -> ResourceFuture<'_> {
        Box::pin(async move {
            let backend = self.0.clone();
            self.0
                .runtime
                .spawn_blocking(move || backend.close_blocking())
                .await
                .map_err(|_| ApplicationError::resource("sqlite", "cleanup worker failed"))?
                .map_err(|error| ApplicationError::resource("sqlite", error.to_string()))
        })
    }
}

/// Recover a bounded entity authority and transfer its connection and owner lock
/// to an application. Readiness still requires `ApplicationBuilder::start`.
/// No mutable graph or implicit history provider escapes this constructor.
pub async fn application(
    config: SqliteConfig,
    catalog: Catalog,
    policies: SqlitePolicies,
    limits: SqliteServiceLimits,
) -> Result<ApplicationBuilder, SqliteError> {
    config.validate()?;
    let stop = storage::Stop::new(
        Instant::now() + config.limits.operation_timeout,
        crate::CancellationToken::new(),
    );
    let worker_stop = stop.clone();
    let runtime = tokio::runtime::Handle::current();
    let worker_runtime = runtime.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    runtime.spawn_blocking(move || {
        let result = construct(
            config,
            catalog,
            policies,
            limits,
            worker_runtime,
            &worker_stop,
        );
        match result {
            Ok((builder, backend)) => {
                if let Err(error) = worker_stop.check() {
                    let _ = backend.close_blocking();
                    drop(builder);
                    let _ = send.send(Err(error));
                } else if let Err(undelivered) = send.send(Ok((builder, backend.clone()))) {
                    let _ = backend.close_blocking();
                    drop(undelivered);
                }
            }
            Err(error) => {
                let _ = send.send(Err(error));
            }
        }
    });
    let (builder, backend) = receive.await.map_err(|_| SqliteError::Unavailable)??;
    // Delivery may itself be delayed after the worker has completed. Keep the
    // original construction deadline and physically close an expired result.
    if let Err(error) = stop.check() {
        let _ = runtime
            .spawn_blocking(move || backend.close_blocking())
            .await;
        drop(builder);
        return Err(error);
    }
    Ok(builder)
}
fn construct(
    config: SqliteConfig,
    catalog: Catalog,
    policies: SqlitePolicies,
    limits: SqliteServiceLimits,
    runtime: tokio::runtime::Handle,
    stop: &storage::Stop,
) -> Result<(ApplicationBuilder, Arc<storage::Backend>), SqliteError> {
    #[cfg(test)]
    let recovery_end = config.recovery_end.clone();
    stop.check()?;
    let catalog =
        Arc::new(ActivatedCatalog::new(catalog, None).map_err(|_| SqliteError::Incompatible)?);
    stop.check()?;
    let backend = storage::Backend::open(config, runtime, stop)?;
    let result = (|| {
        let recovered = backend.initialize_and_recover(catalog.clone(), stop)?;
        stop.check()?;
        let graph = SharedGraph::from_restored(recovered.graph);
        let store = crate::EntityMutationStore::from_sqlite(
            graph.clone(),
            recovered.dataset,
            backend.clone(),
            recovered.receipts,
            &limits.entities,
            stop,
        )?;
        stop.check()?;
        backend
            .projection
            .set(storage::Projection {
                graph: graph.clone(),
                catalog,
                dataset: recovered.dataset,
                schema_version: recovered.schema_version,
            })
            .map_err(|_| SqliteError::Configuration)?;
        let mut builder = ApplicationBuilder::new(graph, policies.reads, limits.reads)
            .map_err(|_| SqliteError::Configuration)?;
        stop.check()?;
        builder
            .configure_sqlite(backend.clone())
            .map_err(|_| SqliteError::Configuration)?;
        let mutations = crate::MutationService::sqlite(
            builder.handle().read_service(),
            store,
            policies.entities,
            limits.entities,
            recovered.cursor_key,
        )
        .map_err(|_| SqliteError::Configuration)?;
        stop.check()?;
        let builder = builder
            .entity_mutations(mutations)
            .map(|builder| builder.owned_resource(DatabaseResource(backend.clone())))
            .map_err(|_| SqliteError::Configuration)?;
        #[cfg(test)]
        if let Some(gate) = recovery_end {
            gate.hold_at(stop.deadline);
        }
        stop.check()?;
        Ok(builder)
    })();
    match result {
        Ok(builder) => Ok((builder, backend)),
        Err(error) => {
            let _ = backend.close_blocking();
            Err(error)
        }
    }
}

#[cfg(test)]
pub(crate) mod qualification;
