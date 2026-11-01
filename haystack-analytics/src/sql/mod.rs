//! Bounded, read-only SQL over sealed snapshots (feature `sql`).
//!
//! Registration opens a committed capture with [`Dataset::open`], drains
//! every batch through the validating reader and requires
//! [`Dataset::semantically_complete`] before anything becomes queryable. The
//! validated samples are converted once into SQL-facing Arrow batches and
//! sealed in memory, so a query never touches the filesystem and no LIMIT or
//! early termination can expose a row the reader had not validated.
//!
//! Every bound below is a named constant; they are the documentation. See
//! `docs/sql-snapshots.md` in the repository for the schema, the NULL/NA and
//! timezone mappings, the runaway-query policy and what is not claimed.
mod table;

#[cfg(test)]
mod tests;

use crate::{Dataset, Error, Result};
use datafusion::{
    common::tree_node::{Transformed, TreeNode, TreeNodeRecursion},
    error::DataFusionError,
    execution::{
        SessionStateDefaults,
        disk_manager::{DiskManagerBuilder, DiskManagerMode},
        memory_pool::{
            GreedyMemoryPool, MemoryConsumer, MemoryLimit, MemoryPool, MemoryReservation,
            TrackConsumersPool,
        },
        runtime_env::RuntimeEnvBuilder,
        session_state::SessionStateBuilder,
    },
    logical_expr::LogicalPlan,
    physical_plan::{
        ExecutionPlan,
        coop::CooperativeExec,
        joins::{CrossJoinExec, NestedLoopJoinExec, PiecewiseMergeJoinExec},
        recursive_query::RecursiveQueryExec,
    },
    prelude::{SQLOptions, SessionConfig, SessionContext},
};
use futures_util::StreamExt;
use std::{
    any::Any,
    collections::{BTreeSet, HashMap},
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use table::{Provenance, Snapshot};
use tokio::{sync::Semaphore, time::Instant};
use tokio_util::sync::CancellationToken;

pub use arrow::{array::RecordBatch, datatypes::SchemaRef};

/// Concurrently executing queries per process. Admission never queues: the
/// next query beyond this returns `Error::Capacity`. A permit is held by the
/// executing task itself, so a query that has not yet stopped after a
/// timeout or cancellation keeps its slot until it actually ends.
pub const SQL_QUERIES: usize = 2;
/// Worker threads of each engine's dedicated query runtime. Queries never
/// run on the caller's runtime, so a stuck operator occupies one of these
/// rather than a server thread.
pub const SQL_WORKER_THREADS: usize = SQL_QUERIES;
/// Memory pool of one query (`GreedyMemoryPool`). Spill is disabled, so an
/// operator that exceeds it fails with `Error::Limit`.
pub const QUERY_MEMORY_BYTES: usize = 64 * 1024 * 1024;
/// `QueryOptions::default().timeout`, and the largest accepted: options may
/// only tighten it. Measured from admission.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// After a timeout or cancellation the query task is aborted and awaited for
/// at most this long before the call returns, so a cooperative query has
/// released its permit by then. A non-cooperative operator may run longer.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);
/// Rows a query may return, counted while draining.
pub const MAX_OUTPUT_ROWS: usize = 10_000;
/// Arrow memory (`get_array_memory_size`) a query may return, counted while
/// draining. Slices are charged for their whole buffers, so this is
/// conservative.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// SQL text length in bytes, checked before parsing.
pub const MAX_SQL_BYTES: usize = 16 * 1024;
/// `datafusion.sql_parser.recursion_limit`: parser nesting depth.
pub const SQL_RECURSION_LIMIT: usize = 50;
/// Snapshots per engine (the `snapshots` provenance table is not counted).
pub const MAX_SNAPSHOTS: usize = 16;
/// Sealed Arrow memory of one snapshot (`get_array_memory_size`).
pub const MAX_SNAPSHOT_BYTES: usize = 96 * 1024 * 1024;
/// Sealed Arrow memory of all snapshots of one engine.
pub const MAX_ENGINE_BYTES: usize = 256 * 1024 * 1024;
/// `datafusion.execution.batch_size`.
pub const SQL_BATCH_ROWS: usize = 1024;
/// Session time zone (`datafusion.execution.time_zone`).
pub const SESSION_TIME_ZONE: &str = "+00:00";
/// Reserved name of the read-only provenance table.
pub const SNAPSHOTS_TABLE: &str = "snapshots";
/// Longest table name; names match `^[a-z][a-z0-9_]{0,62}$`.
pub const MAX_TABLE_NAME_BYTES: usize = 63;
/// Scalar functions removed from the session: each turns a short literal
/// into a value of up to 2 GiB in one call, outside the memory pool.
pub const REMOVED_FUNCTIONS: [&str; 3] = ["lpad", "repeat", "rpad"];
/// Memory consumers named in a `TrackConsumersPool` report.
const TRACKED_CONSUMERS: usize = 5;

static QUERIES: Semaphore = Semaphore::const_new(SQL_QUERIES);

/// Identity of a registered snapshot: the capture's export id and the SHA-256
/// of the exact manifest bytes that were verified (the manifest pins every
/// part hash).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub struct SnapshotIdentity {
    pub table: String,
    pub export_id: String,
    pub manifest_sha256: String,
}

/// Per-query options. `timeout` may only be tightened from [`QUERY_TIMEOUT`].
#[derive(Clone, Debug)]
pub struct QueryOptions {
    pub timeout: Duration,
    pub cancellation: CancellationToken,
}
impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            timeout: QUERY_TIMEOUT,
            cancellation: CancellationToken::new(),
        }
    }
}

/// A complete, capped query result.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct QueryOutput {
    pub schema: SchemaRef,
    pub batches: Vec<RecordBatch>,
    /// Snapshots the query's logical plan scans, by table name. Reading only
    /// the `snapshots` provenance table contributes none.
    pub sources: Vec<SnapshotIdentity>,
    /// Largest memory-pool reservation the query reached.
    pub peak_memory_bytes: usize,
}

/// Collects snapshots for an immutable [`SqlEngine`]. A failed registration
/// leaves the builder exactly as it was.
#[derive(Default)]
pub struct SqlEngineBuilder {
    snapshots: Vec<Arc<Snapshot>>,
    bytes: usize,
}
impl SqlEngineBuilder {
    /// Opens and fully validates the committed capture at `directory`, then
    /// seals it in memory as table `name`. Blocking work runs on
    /// `spawn_blocking` of the caller's runtime; called outside a Tokio
    /// runtime it returns `Error::Runtime`.
    pub async fn register(&mut self, name: &str, directory: impl Into<PathBuf>) -> Result<()> {
        if !valid_table_name(name)
            || name == SNAPSHOTS_TABLE
            || self.snapshots.iter().any(|s| s.identity.table == name)
        {
            return Err(Error::Options);
        }
        if self.snapshots.len() >= MAX_SNAPSHOTS {
            return Err(Error::Limit);
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error::Runtime)?;
        let budget = MAX_SNAPSHOT_BYTES.min(MAX_ENGINE_BYTES.saturating_sub(self.bytes));
        let name = name.to_owned();
        let directory = directory.into();
        let snapshot = runtime
            .spawn_blocking(move || Snapshot::load(name, Dataset::open(directory)?, budget))
            .await
            .map_err(|_| Error::Worker)??;
        self.bytes += snapshot.bytes;
        self.snapshots.push(Arc::new(snapshot));
        Ok(())
    }
    /// Registered identities, in registration order.
    pub fn snapshots(&self) -> Vec<SnapshotIdentity> {
        self.snapshots.iter().map(|s| s.identity.clone()).collect()
    }
    /// Starts the engine's dedicated query runtime.
    pub fn build(self) -> Result<SqlEngine> {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder
            .worker_threads(SQL_WORKER_THREADS)
            .max_blocking_threads(SQL_WORKER_THREADS)
            .thread_name("haystack-sql")
            .enable_time();
        #[cfg(test)]
        let threads = Arc::new(AtomicUsize::new(0));
        #[cfg(test)]
        {
            let (started, stopped) = (threads.clone(), threads.clone());
            builder
                .on_thread_start(move || {
                    started.fetch_add(1, Ordering::SeqCst);
                })
                .on_thread_stop(move || {
                    stopped.fetch_sub(1, Ordering::SeqCst);
                });
        }
        let runtime = builder.build().map_err(|_| Error::Runtime)?;
        let provenance = Arc::new(Provenance::new(&self.snapshots)?);
        Ok(SqlEngine {
            inner: Arc::new(Inner {
                snapshots: self.snapshots,
                provenance,
            }),
            runtime: Some(runtime),
            #[cfg(test)]
            threads,
        })
    }
}

struct Inner {
    snapshots: Vec<Arc<Snapshot>>,
    provenance: Arc<Provenance>,
}

/// Immutable set of sealed snapshots and the runtime that queries them.
///
/// Dropping the engine shuts its runtime down without blocking: idle worker
/// threads exit, and a worker still inside a non-cooperative operator exits
/// when that operator next returns.
pub struct SqlEngine {
    inner: Arc<Inner>,
    runtime: Option<tokio::runtime::Runtime>,
    /// Live threads of `runtime` (workers and blocking), for shutdown tests.
    #[cfg(test)]
    threads: Arc<AtomicUsize>,
}
impl Drop for SqlEngine {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
impl SqlEngine {
    pub fn builder() -> SqlEngineBuilder {
        SqlEngineBuilder::default()
    }
    /// Registered identities, in registration order.
    pub fn snapshots(&self) -> Vec<SnapshotIdentity> {
        self.inner
            .snapshots
            .iter()
            .map(|s| s.identity.clone())
            .collect()
    }
    /// Plans and runs one read-only query on the engine's runtime.
    ///
    /// Rejections happen before execution: text over [`MAX_SQL_BYTES`]
    /// (`Error::Limit`), a timeout above [`QUERY_TIMEOUT`] (`Error::Options`),
    /// no free slot (`Error::Capacity`), and DDL, DML, statements, cross
    /// joins, non-equi joins and recursive queries (`Error::Sql`). During
    /// execution, memory or output over its cap is `Error::Limit`, and the
    /// deadline and cancellation token are `Error::Deadline` and
    /// `Error::Cancelled`. DataFusion's message text is never returned,
    /// because it can echo data. Dropping the returned future aborts the
    /// query. The deadline is timed on the caller's runtime, which must have
    /// its timer enabled.
    pub async fn query(&self, sql: &str, options: QueryOptions) -> Result<QueryOutput> {
        if sql.len() > MAX_SQL_BYTES {
            return Err(Error::Limit);
        }
        if options.timeout > QUERY_TIMEOUT {
            return Err(Error::Options);
        }
        let deadline = Instant::now()
            .checked_add(options.timeout)
            .ok_or(Error::Options)?;
        if options.cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let handle = self.runtime.as_ref().ok_or(Error::Runtime)?.handle();
        let permit = QUERIES.try_acquire().map_err(|_| Error::Capacity)?;
        let inner = self.inner.clone();
        let sql = sql.to_owned();
        let pool = Arc::new(PeakPool::new(QUERY_MEMORY_BYTES));
        let mut task = AbortOnDrop(handle.spawn(async move {
            let _permit = permit;
            execute(&inner, &sql, pool).await
        }));
        let stopped = tokio::select! {
            biased;
            result = &mut task.0 => return result.unwrap_or(Err(Error::Worker)),
            _ = options.cancellation.cancelled() => Error::Cancelled,
            _ = tokio::time::sleep_until(deadline) => Error::Deadline,
        };
        task.0.abort();
        let _ = tokio::time::timeout(CANCEL_GRACE, &mut task.0).await;
        Err(stopped)
    }
}

/// Aborts the query task when the `query` future is dropped, so abandoning
/// a call stops its query as cancellation would (without the grace wait).
struct AbortOnDrop(tokio::task::JoinHandle<Result<QueryOutput>>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn valid_table_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_TABLE_NAME_BYTES
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// DDL, DML (including COPY) and statements (SET, transactions, PREPARE) are
/// refused at the logical-plan stage.
fn sql_options() -> SQLOptions {
    SQLOptions::new()
        .with_allow_ddl(false)
        .with_allow_dml(false)
        .with_allow_statements(false)
}

fn session(inner: &Inner, pool: Arc<dyn MemoryPool>) -> Result<SessionContext> {
    let runtime = RuntimeEnvBuilder::new()
        .with_memory_pool(pool)
        .with_disk_manager_builder(
            DiskManagerBuilder::default().with_mode(DiskManagerMode::Disabled),
        )
        .build_arc()
        .map_err(|_| Error::Query)?;
    let mut config = SessionConfig::new()
        .with_target_partitions(1)
        .with_batch_size(SQL_BATCH_ROWS)
        .with_information_schema(false)
        .with_repartition_joins(false)
        .with_repartition_aggregations(false)
        .with_repartition_windows(false)
        .with_repartition_sorts(false)
        .with_repartition_file_scans(false);
    let options = config.options_mut();
    options.execution.time_zone = Some(SESSION_TIME_ZONE.into());
    options.execution.enable_recursive_ctes = false;
    options
        .set(
            "datafusion.sql_parser.recursion_limit",
            &SQL_RECURSION_LIMIT.to_string(),
        )
        .map_err(|_| Error::Query)?;
    // Default functions and planners, except no table functions (no
    // `generate_series`/`range` row generators), none of
    // `REMOVED_FUNCTIONS`, no table factories (no `CREATE EXTERNAL TABLE`
    // target even if DDL were allowed) and no file formats. URL tables are
    // never enabled.
    let functions = SessionStateDefaults::default_scalar_functions()
        .into_iter()
        .filter(|f| !REMOVED_FUNCTIONS.contains(&f.name()))
        .collect();
    let state = SessionStateBuilder::new_with_default_features()
        .with_scalar_functions(functions)
        .with_config(config)
        .with_runtime_env(runtime)
        .with_table_functions(HashMap::new())
        .with_table_factories(HashMap::new())
        .with_file_formats(Vec::new())
        .build();
    let context = SessionContext::new_with_state(state);
    for snapshot in &inner.snapshots {
        context
            .register_table(snapshot.identity.table.as_str(), snapshot.clone())
            .map_err(|_| Error::Query)?;
    }
    context
        .register_table(SNAPSHOTS_TABLE, inner.provenance.clone())
        .map_err(|_| Error::Query)?;
    Ok(context)
}

async fn execute(inner: &Inner, sql: &str, pool: Arc<PeakPool>) -> Result<QueryOutput> {
    let context = session(inner, pool.clone())?;
    let plan = context
        .state()
        .create_logical_plan(sql)
        .await
        .map_err(|e| classify(&e))?;
    sql_options().verify_plan(&plan).map_err(|_| Error::Sql)?;
    let sources = sources(inner, &plan)?;
    let frame = context
        .execute_logical_plan(plan)
        .await
        .map_err(|e| classify(&e))?;
    let state = frame.task_ctx();
    let optimized = frame.into_optimized_plan().map_err(|e| classify(&e))?;
    reject_keyless_joins(&optimized)?;
    let physical = context
        .state()
        .create_physical_plan(&optimized)
        .await
        .map_err(|e| classify(&e))?;
    reject_runaway(&physical)?;
    let physical = cooperative(physical)?;
    let schema = physical.schema();
    let mut stream = datafusion::physical_plan::execute_stream(physical, Arc::new(state))
        .map_err(|e| classify(&e))?;
    let (mut rows, mut bytes, mut batches) = (0usize, 0usize, Vec::new());
    while let Some(batch) = stream.next().await {
        let batch = batch.map_err(|e| classify(&e))?;
        if batch.num_rows() == 0 {
            continue;
        }
        rows = rows.saturating_add(batch.num_rows());
        bytes = bytes.saturating_add(batch.get_array_memory_size());
        if rows > MAX_OUTPUT_ROWS || bytes > MAX_OUTPUT_BYTES {
            return Err(Error::Limit);
        }
        batches.push(batch);
    }
    drop(stream);
    Ok(QueryOutput {
        schema,
        batches,
        sources,
        peak_memory_bytes: pool.peak(),
    })
}

/// Snapshot identities of every `TableScan`, including inside subqueries.
fn sources(inner: &Inner, plan: &LogicalPlan) -> Result<Vec<SnapshotIdentity>> {
    let mut names = BTreeSet::new();
    plan.apply_with_subqueries(|node| {
        if let LogicalPlan::TableScan(scan) = node {
            names.insert(scan.table_name.table().to_owned());
        }
        Ok(TreeNodeRecursion::Continue)
    })
    .map_err(|_| Error::Query)?;
    Ok(inner
        .snapshots
        .iter()
        .filter(|s| names.contains(&s.identity.table))
        .map(|s| s.identity.clone())
        .collect())
}

/// A join without equality keys (a cross join, or a join on a non-equi
/// predicate) in the optimized logical plan. Checked before physical
/// planning because statistics can replace such a join by a constant (for
/// example `COUNT(*)` of a cross join) and hide it from the physical check;
/// the policy is the same either way.
fn reject_keyless_joins(plan: &LogicalPlan) -> Result<()> {
    let mut keyless = false;
    plan.apply_with_subqueries(|node| {
        keyless |= matches!(node, LogicalPlan::Join(join) if join.on.is_empty())
            || matches!(node, LogicalPlan::RecursiveQuery(_));
        Ok(if keyless {
            TreeNodeRecursion::Stop
        } else {
            TreeNodeRecursion::Continue
        })
    })
    .map_err(|_| Error::Query)?;
    if keyless { Err(Error::Sql) } else { Ok(()) }
}

/// Operators whose output is not bounded by an input (cross and nested-loop
/// joins, range joins) or whose work is not (recursive queries). DataFusion
/// 55 does not yield inside a cross join's output loop, so such a query
/// would neither honour its deadline nor stop promptly on drop.
fn runaway(plan: &dyn ExecutionPlan) -> bool {
    let plan: &dyn Any = plan;
    plan.is::<CrossJoinExec>()
        || plan.is::<NestedLoopJoinExec>()
        || plan.is::<PiecewiseMergeJoinExec>()
        || plan.is::<RecursiveQueryExec>()
}

fn reject_runaway(plan: &Arc<dyn ExecutionPlan>) -> Result<()> {
    let mut pending = vec![plan];
    while let Some(node) = pending.pop() {
        if runaway(node.as_ref()) {
            return Err(Error::Sql);
        }
        pending.extend(node.children());
    }
    Ok(())
}

/// Wraps every operator that has inputs in a `CooperativeExec`, so each
/// output batch it produces consumes Tokio's cooperative budget. DataFusion
/// 55 makes only leaves cooperative; an equi-join on a low-cardinality key
/// can emit millions of rows per probe batch without polling a leaf, and
/// would otherwise ignore abort for that long.
fn cooperative(plan: Arc<dyn ExecutionPlan>) -> Result<Arc<dyn ExecutionPlan>> {
    plan.transform_up(|node| {
        let any: &dyn Any = node.as_ref();
        Ok(
            if node.children().is_empty() || any.is::<CooperativeExec>() {
                Transformed::no(node)
            } else {
                Transformed::yes(Arc::new(CooperativeExec::new(node)) as Arc<dyn ExecutionPlan>)
            },
        )
    })
    .map(|t| t.data)
    .map_err(|_| Error::Query)
}

/// Maps a DataFusion error to a variant without its text, which can echo
/// literals or data.
fn classify(error: &DataFusionError) -> Error {
    match error.find_root() {
        DataFusionError::ResourcesExhausted(_) => Error::Limit,
        DataFusionError::SQL(..)
        | DataFusionError::Plan(_)
        | DataFusionError::SchemaError(..)
        | DataFusionError::NotImplemented(_)
        | DataFusionError::Configuration(_) => Error::Sql,
        DataFusionError::External(inner) => inner
            .downcast_ref::<Error>()
            .cloned()
            .unwrap_or(Error::Query),
        _ => Error::Query,
    }
}

/// A `TrackConsumersPool<GreedyMemoryPool>` that also records its largest
/// reservation, so a query reports its peak and tests can prove the limit
/// held.
#[derive(Debug)]
struct PeakPool {
    inner: TrackConsumersPool<GreedyMemoryPool>,
    peak: AtomicUsize,
}
impl PeakPool {
    fn new(limit: usize) -> Self {
        Self {
            inner: TrackConsumersPool::new(
                GreedyMemoryPool::new(limit),
                NonZeroUsize::new(TRACKED_CONSUMERS).unwrap_or(NonZeroUsize::MIN),
            ),
            peak: AtomicUsize::new(0),
        }
    }
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
    fn observe(&self) {
        self.peak.fetch_max(self.inner.reserved(), Ordering::SeqCst);
    }
}
impl std::fmt::Display for PeakPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner)
    }
}
impl MemoryPool for PeakPool {
    fn name(&self) -> &str {
        "haystack-sql"
    }
    fn register(&self, consumer: &MemoryConsumer) {
        self.inner.register(consumer);
    }
    fn unregister(&self, consumer: &MemoryConsumer) {
        self.inner.unregister(consumer);
    }
    fn grow(&self, reservation: &MemoryReservation, additional: usize) {
        self.inner.grow(reservation, additional);
        self.observe();
    }
    fn shrink(&self, reservation: &MemoryReservation, shrink: usize) {
        self.inner.shrink(reservation, shrink);
    }
    fn try_grow(
        &self,
        reservation: &MemoryReservation,
        additional: usize,
    ) -> datafusion::error::Result<()> {
        self.inner.try_grow(reservation, additional)?;
        self.observe();
        Ok(())
    }
    fn reserved(&self) -> usize {
        self.inner.reserved()
    }
    fn memory_limit(&self) -> MemoryLimit {
        self.inner.memory_limit()
    }
}
