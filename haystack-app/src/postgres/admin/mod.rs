//! Explicit same-database maintenance. A control schema owns one durable
//! selector and operation journal; all authority transitions hold the existing
//! schema gates. Raw administrator SQL remains outside this managed contract.
mod backup;
mod operations;
use super::{PostgresConfig, PostgresError, PostgresLimits, storage, value};
pub use backup::{Backup, MAX_BACKUP_BYTES, Manifest};
use haystack_core::xeto::catalog::{ActivatedCatalog, Catalog};
use serde::Serialize;
use sqlx::{Connection, PgConnection, Row};
use std::sync::Arc;

/// Each ID is an idempotency key. Reusing it for a different request fails.
pub enum Command {
    Export {
        operation: String,
        source: String,
        target: String,
    },
    Import {
        operation: String,
        backup: Vec<u8>,
    },
    Restore {
        operation: String,
        target: String,
        backup: Vec<u8>,
    },
    Activate {
        operation: String,
    },
    Cancel {
        operation: String,
    },
    Status {
        operation: Option<String>,
    },
}
pub enum Outcome {
    Backup(Backup),
    Status(Status),
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Authority {
    pub schema: String,
    pub dataset: [u8; 16],
    pub incarnation: [u8; 16],
    pub epoch: i64,
}
#[derive(Debug, Serialize)]
pub struct Status {
    pub format: u32,
    pub selected: Authority,
    pub operation: Option<String>,
    pub phase: Option<String>,
    pub source: Option<String>,
    pub target: Option<String>,
    pub backup_sha256: Option<String>,
    pub source_revision: Option<u64>,
    pub source_history_sequence: Option<u64>,
    pub archive: Option<Manifest>,
}
/// Execute bounded maintenance against `config.schema`, the explicit control
/// schema. Cancellation detaches an owned worker that still closes its session;
/// uncertain commit acknowledgements must be resolved by Status/retry.
pub async fn execute(
    config: PostgresConfig,
    catalog: Catalog,
    command: Command,
) -> Result<Outcome, PostgresError> {
    tokio::spawn(async move {
        let catalog = Arc::new(
            ActivatedCatalog::new(catalog, None).map_err(|_| PostgresError::Incompatible)?,
        );
        let parsed = match &command {
            Command::Import { backup, .. } | Command::Restore { backup, .. } => {
                Some(Backup::parse(backup, &catalog, &config.limits)?)
            }
            _ => None,
        };
        let options = config.options()?;
        let mut connection = tokio::time::timeout(
            config.limits.acquire_timeout,
            PgConnection::connect_with(&options),
        )
        .await
        .map_err(|_| PostgresError::Deadline)?
        .map_err(|_| PostgresError::Connection)?;
        let result = operations::execute(&mut connection, &config, &catalog, command, parsed).await;
        let closed = connection
            .close()
            .await
            .map_err(|_| PostgresError::Connection);
        match result {
            Ok(result) => {
                closed?;
                Ok(result)
            }
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(|_| PostgresError::Unavailable)?
}
/// Read the committed selection. Application construction separately pins all
/// returned fields under the target gate, refusing selection/startup races.
pub async fn selected(config: PostgresConfig) -> Result<Authority, PostgresError> {
    match execute(
        config,
        Catalog::load_http_pinned().map_err(|_| PostgresError::Incompatible)?,
        Command::Status { operation: None },
    )
    .await?
    {
        Outcome::Status(status) => Ok(status.selected),
        Outcome::Backup(_) => Err(PostgresError::Incompatible),
    }
}
pub(super) fn identifier(name: &str) -> Result<(), PostgresError> {
    if name.is_empty()
        || name.len() > 48
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        || name == "public"
        || name.starts_with("pg_")
    {
        return Err(PostgresError::Configuration);
    }
    Ok(())
}
fn operation_id(id: &str) -> Result<(), PostgresError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(PostgresError::Configuration);
    }
    Ok(())
}
async fn path(connection: &mut PgConnection, schema: &str) -> Result<(), PostgresError> {
    identifier(schema)?;
    sqlx::query("SELECT set_config('search_path',$1,false)")
        .bind(format!("\"{schema}\",pg_catalog"))
        .execute(connection)
        .await
        .map_err(storage::read_error)?;
    Ok(())
}
async fn begin(connection: &mut PgConnection) -> Result<(), PostgresError> {
    sqlx::query("BEGIN ISOLATION LEVEL READ COMMITTED")
        .execute(connection)
        .await
        .map_err(storage::read_error)?;
    Ok(())
}
async fn commit(connection: &mut PgConnection) -> Result<(), PostgresError> {
    // An unavailable acknowledgement is deliberately not classified as rollback.
    sqlx::query("COMMIT")
        .execute(connection)
        .await
        .map_err(|_| PostgresError::Unavailable)?;
    Ok(())
}
async fn exists(connection: &mut PgConnection, schema: &str) -> Result<bool, PostgresError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname=$1)")
        .bind(schema)
        .fetch_one(connection)
        .await
        .map_err(storage::read_error)
}
async fn gates(connection: &mut PgConnection, names: &[&str]) -> Result<(), PostgresError> {
    let mut names = names.to_vec();
    names.sort_unstable();
    names.dedup();
    for name in names {
        identifier(name)?;
        sqlx::query("SELECT pg_advisory_lock(hashtextextended(current_database() || ':' || $1,0))")
            .bind(name)
            .execute(&mut *connection)
            .await
            .map_err(storage::read_error)?;
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Interruption {
    AfterQuiesce,
    DuringImport,
    BeforeSelection,
    AfterSelectionCommit,
}
#[cfg(test)]
static INTERRUPTIONS: std::sync::OnceLock<
    parking_lot::Mutex<std::collections::HashMap<String, Interruption>>,
> = std::sync::OnceLock::new();
#[cfg(test)]
pub(crate) fn interrupt_once(operation: &str, stage: Interruption) {
    INTERRUPTIONS
        .get_or_init(Default::default)
        .lock()
        .insert(operation.into(), stage);
}
#[cfg(test)]
async fn interrupt(
    c: &mut PgConnection,
    operation: &str,
    stage: Interruption,
) -> Result<(), PostgresError> {
    let should_interrupt = {
        let mut faults = INTERRUPTIONS.get_or_init(Default::default).lock();
        if faults.get(operation) == Some(&stage) {
            faults.remove(operation);
            true
        } else {
            false
        }
    };
    if should_interrupt {
        // Real PostgreSQL backend termination. At AfterSelectionCommit the
        // internal COMMIT was acknowledged; this models response loss before
        // delivery to the consumer, not an unobserved wire COMMIT acknowledgement.
        let _ = sqlx::query("SELECT pg_terminate_backend(pg_backend_pid())")
            .execute(c)
            .await;
        return Err(PostgresError::Unavailable);
    }
    Ok(())
}
