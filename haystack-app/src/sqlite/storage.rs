use super::{SqliteConfig, SqliteError, SqliteLimits};
use crate::CancellationToken;
use haystack_core::{graph::SharedGraph, xeto::catalog::ActivatedCatalog};
use parking_lot::{Mutex, MutexGuard};
use rusqlite::{Connection, ErrorCode, OpenFlags, config::DbConfig};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(crate) const QUANTUM: Duration = Duration::from_millis(5);

#[derive(Clone)]
pub(crate) struct Stop {
    pub deadline: Instant,
    pub cancel: CancellationToken,
    pub owner_cancel: Option<CancellationToken>,
    pub owner_sealed: Option<CancellationToken>,
    pub(crate) session: Option<crate::budget::SessionFence>,
}
impl Stop {
    pub(crate) fn new(deadline: Instant, cancel: CancellationToken) -> Self {
        Self {
            deadline,
            cancel,
            owner_cancel: None,
            owner_sealed: None,
            session: None,
        }
    }
    pub(crate) fn from_budget(budget: &crate::budget::Budget) -> Self {
        Self {
            deadline: budget.deadline,
            cancel: budget.cancel.clone(),
            owner_cancel: budget.owner_cancel.clone(),
            owner_sealed: budget.owner_sealed.clone(),
            session: budget.session.clone(),
        }
    }
    pub(crate) fn check(&self) -> Result<(), SqliteError> {
        if Instant::now() >= self.deadline {
            return Err(SqliteError::Deadline);
        }
        if self.cancel.is_cancelled()
            || self
                .owner_cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            || self
                .owner_sealed
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            || self
                .session
                .as_ref()
                .is_some_and(|session| session.check().is_err())
        {
            return Err(SqliteError::Cancelled);
        }
        Ok(())
    }
}
pub(crate) struct Projection {
    pub graph: SharedGraph,
    pub catalog: Arc<ActivatedCatalog>,
    pub dataset: [u8; 16],
    pub schema_version: i64,
    pub history: std::sync::Weak<crate::history_store::HistoryStoreInner>,
}
struct Database {
    connection: Connection,
    // Drop order releases the native connection before the owner handle.
    owner: File,
}
pub(crate) struct Backend {
    database: Mutex<Option<Database>>,
    pub(crate) writer: Mutex<()>,
    pub(crate) projection: OnceLock<Projection>,
    pub(crate) limits: SqliteLimits,
    pub(crate) runtime: tokio::runtime::Handle,
    sealed: AtomicBool,
    #[cfg(test)]
    pub(crate) fault: Mutex<Option<super::qualification::Fault>>,
    #[cfg(test)]
    pub(crate) publication_waits: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) native_busy_waits: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) history_open_waits: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) history_pull_waits: std::sync::atomic::AtomicUsize,
}
impl Drop for Backend {
    fn drop(&mut self) {
        let Some(Database { connection, owner }) = self.database.get_mut().take() else {
            return;
        };
        // The resource normally closes first. This last-owner fallback must not
        // release the advisory lock on an unproven native close. Retaining both
        // handles until process exit is preferable to admitting a second owner.
        match connection.close() {
            Ok(()) => drop(owner),
            Err((connection, _)) => {
                std::mem::forget(connection);
                std::mem::forget(owner);
            }
        }
    }
}
impl Backend {
    pub(crate) fn open(
        config: SqliteConfig,
        runtime: tokio::runtime::Handle,
        stop: &Stop,
    ) -> Result<Arc<Self>, SqliteError> {
        stop.check()?;
        let (path, owner) = own_file(&config.path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE,
        )
        .map_err(|_| SqliteError::Unavailable)?;
        let backend = Arc::new(Self {
            database: Mutex::new(Some(Database { connection, owner })),
            writer: Mutex::new(()),
            projection: OnceLock::new(),
            limits: config.limits,
            runtime,
            sealed: AtomicBool::new(false),
            #[cfg(test)]
            fault: Mutex::new(None),
            #[cfg(test)]
            publication_waits: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            native_busy_waits: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            history_open_waits: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            history_pull_waits: std::sync::atomic::AtomicUsize::new(0),
        });
        let configured = backend.with_connection(stop, |connection| {
            connection.busy_timeout(QUANTUM).map_err(sql_error)?;
            connection
                .set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
                .map_err(sql_error)?;
            connection
                .set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
                .map_err(sql_error)?;
            super::recovery::validate_existing_schema(connection)?;
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(sql_error)?;
            connection
                .pragma_update(None, "synchronous", "FULL")
                .map_err(sql_error)?;
            connection
                .pragma_update(None, "foreign_keys", "ON")
                .map_err(sql_error)?;
            connection
                .pragma_update(None, "fullfsync", "ON")
                .map_err(sql_error)?;
            connection
                .pragma_update(None, "checkpoint_fullfsync", "ON")
                .map_err(sql_error)?;
            verify_durability(connection)
        });
        if let Err(error) = configured {
            let _ = backend.close_blocking();
            return Err(error);
        }
        Ok(backend)
    }
    pub(crate) fn check(&self) -> Result<(), SqliteError> {
        if self.sealed.load(Ordering::Acquire) {
            Err(SqliteError::Unavailable)
        } else {
            Ok(())
        }
    }
    pub(crate) fn seal(&self) {
        self.sealed.store(true, Ordering::Release);
    }
    pub(crate) fn publication_guard(&self, stop: &Stop) -> Result<MutexGuard<'_, ()>, SqliteError> {
        #[cfg(test)]
        let mut observed_wait = false;
        loop {
            stop.check()?;
            self.check()?;
            if let Some(guard) = self.writer.try_lock_for(QUANTUM) {
                self.check()?;
                return Ok(guard);
            }
            #[cfg(test)]
            if !observed_wait {
                self.publication_waits.fetch_add(1, Ordering::Release);
                observed_wait = true;
            }
        }
    }
    pub(crate) fn with_connection<T>(
        &self,
        stop: &Stop,
        operation: impl FnOnce(&Connection) -> Result<T, SqliteError>,
    ) -> Result<T, SqliteError> {
        let database = loop {
            stop.check()?;
            self.check()?;
            if let Some(database) = self.database.try_lock_for(QUANTUM) {
                break database;
            }
        };
        let connection = &database
            .as_ref()
            .ok_or(SqliteError::Unavailable)?
            .connection;
        let progress = stop.clone();
        connection
            .progress_handler(1000, Some(move || progress.check().is_err()))
            .map_err(sql_error)?;
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(connection)));
        // No detached interrupt can reach a later command. The callback belongs
        // to this exclusive connection scope and is removed before it is released.
        if connection
            .progress_handler(0, None::<fn() -> bool>)
            .is_err()
        {
            self.seal();
            return Err(SqliteError::Unavailable);
        }
        if !connection.is_autocommit() {
            // Cleanup has its own finite allowance, independent of the caller's
            // already cancelled token. Failure seals the authority.
            let cleanup = Stop::new(
                Instant::now() + self.limits.cleanup_timeout,
                CancellationToken::new(),
            );
            let progress = cleanup.clone();
            connection
                .progress_handler(1000, Some(move || progress.check().is_err()))
                .map_err(sql_error)?;
            let rollback = connection.execute_batch("ROLLBACK");
            let cleared = connection.progress_handler(0, None::<fn() -> bool>);
            if rollback.is_err() || cleared.is_err() || !connection.is_autocommit() {
                self.seal();
                return Err(SqliteError::Unavailable);
            }
        }
        match result {
            Ok(result) => result,
            Err(panic) => {
                self.seal();
                std::panic::resume_unwind(panic)
            }
        }
    }
    /// The caller holds the publication gate. Neither cancellation nor a dropped
    /// response detaches native execution from its application work lease.
    pub(crate) fn transaction<T>(
        &self,
        stop: &Stop,
        operation: impl FnOnce(&Connection) -> Result<T, SqliteError>,
    ) -> Result<T, SqliteError> {
        self.with_connection(stop, |connection| {
            self.begin(connection, stop, "BEGIN IMMEDIATE")?;
            let output = operation(connection)?;
            stop.check()?;
            connection.execute_batch("COMMIT").map_err(sql_error)?;
            Ok(output)
        })
    }
    pub(crate) fn read<T>(
        &self,
        stop: &Stop,
        operation: impl FnOnce(&Connection) -> Result<T, SqliteError>,
    ) -> Result<T, SqliteError> {
        self.with_connection(stop, |connection| {
            self.begin(connection, stop, "BEGIN")?;
            operation(connection)
        })
    }
    pub(crate) fn verify(&self, stop: &Stop) -> Result<(), SqliteError> {
        let _publication = self.publication_guard(stop)?;
        let projection = self.projection.get().ok_or(SqliteError::Unavailable)?;
        let history = projection
            .history
            .upgrade()
            .ok_or(SqliteError::Unavailable)?;
        let history_sequence = loop {
            stop.check()?;
            if let Some(state) = history.state.try_lock_for(QUANTUM) {
                break state.sequence;
            }
        };
        let result = self.read(stop, |connection| {
            verify_durability(connection)?;
            let meta = super::recovery::metadata(connection)?;
            if meta.dataset != projection.dataset
                || meta.state != projection.graph.state()
                || meta.catalog != projection.catalog.selection_identity()
                || super::recovery::schema_version(connection)? != projection.schema_version
                || meta.history_authority != history.authority
                || meta.history_sequence != history_sequence
            {
                return Err(SqliteError::Incompatible);
            }
            Ok(())
        });
        if matches!(
            result,
            Err(SqliteError::Incompatible | SqliteError::Durability)
        ) {
            self.seal();
        }
        result
    }
    pub(crate) fn close_blocking(&self) -> Result<(), SqliteError> {
        self.seal();
        let _publication = self.writer.lock();
        let mut selected = self.database.lock();
        let Some(database) = selected.take() else {
            return Ok(());
        };
        let Database { connection, owner } = database;
        let _ = connection.progress_handler(0, None::<fn() -> bool>);
        if !connection.is_autocommit() && connection.execute_batch("ROLLBACK").is_err() {
            *selected = Some(Database { connection, owner });
            return Err(SqliteError::Unavailable);
        }
        match connection.close() {
            Ok(()) => {
                drop(owner);
                Ok(())
            }
            Err((connection, _)) => {
                *selected = Some(Database { connection, owner });
                Err(SqliteError::Unavailable)
            }
        }
    }
}

fn own_file(path: &Path) -> Result<(PathBuf, File), SqliteError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| SqliteError::Unavailable)?;
    let metadata = file.metadata().map_err(|_| SqliteError::Unavailable)?;
    if !metadata.is_file() {
        return Err(SqliteError::Configuration);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(SqliteError::Configuration);
        }
    }
    let path = path.canonicalize().map_err(|_| SqliteError::Unavailable)?;
    let mut owner_name = path.as_os_str().to_os_string();
    owner_name.push(".owner.lock");
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(PathBuf::from(owner_name))
        .map_err(|_| SqliteError::Unavailable)?;
    owner.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => SqliteError::AlreadyOwned,
        _ => SqliteError::Unavailable,
    })?;
    Ok((path, owner))
}
impl Backend {
    fn begin(&self, connection: &Connection, stop: &Stop, sql: &str) -> Result<(), SqliteError> {
        loop {
            stop.check()?;
            match connection.execute_batch(sql) {
                Ok(()) => return Ok(()),
                Err(error)
                    if matches!(
                        error.sqlite_error_code(),
                        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                    ) =>
                {
                    #[cfg(test)]
                    self.native_busy_waits.fetch_add(1, Ordering::Release);
                    std::thread::sleep(QUANTUM)
                }
                Err(error) => return Err(stop.check().err().unwrap_or_else(|| sql_error(error))),
            }
        }
    }
}
pub(crate) fn sql_error(_: rusqlite::Error) -> SqliteError {
    SqliteError::Unavailable
}
pub(crate) fn verify_durability(connection: &Connection) -> Result<(), SqliteError> {
    let integer = |name| {
        connection
            .pragma_query_value(None, name, |row| row.get::<_, i64>(0))
            .map_err(|_| SqliteError::Durability)
    };
    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|_| SqliteError::Durability)?;
    if rusqlite::version_number() < 3_053_002
        || mode != "wal"
        || integer("synchronous")? != 2
        || integer("foreign_keys")? != 1
        || integer("busy_timeout")? != 5
        || integer("fullfsync")? != 1
        || integer("checkpoint_fullfsync")? != 1
        || !connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
            .map_err(sql_error)?
        || connection
            .db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA)
            .map_err(sql_error)?
    {
        return Err(SqliteError::Durability);
    }
    Ok(())
}
