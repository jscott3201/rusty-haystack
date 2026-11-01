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
use arrow::{array::Array, datatypes::DataType};
use datafusion::{
    common::{
        ScalarValue,
        tree_node::{Transformed, TreeNode, TreeNodeRecursion},
    },
    error::DataFusionError,
    execution::{
        SessionStateDefaults, TaskContext,
        disk_manager::{DiskManagerBuilder, DiskManagerMode},
        memory_pool::{
            GreedyMemoryPool, MemoryConsumer, MemoryLimit, MemoryPool, MemoryReservation,
            TrackConsumersPool,
        },
        runtime_env::RuntimeEnvBuilder,
        session_state::SessionStateBuilder,
    },
    logical_expr::{Expr, Like, LogicalPlan, Operator, WindowFunctionDefinition},
    physical_plan::{
        DisplayAs, DisplayFormatType, ExecutionPlan, PhysicalExpr, PlanProperties,
        SendableRecordBatchStream,
        coop::CooperativeExec,
        joins::{
            CrossJoinExec, HashJoinExec, NestedLoopJoinExec, PiecewiseMergeJoinExec,
            SortMergeJoinExec,
        },
        recursive_query::RecursiveQueryExec,
        stream::RecordBatchStreamAdapter,
        windows::{BoundedWindowAggExec, WindowAggExec},
    },
    prelude::{SQLOptions, SessionConfig, SessionContext},
    sql::{
        parser::Statement as DfStatement,
        sqlparser::{
            ast::{
                Expr as SqlExpr, GroupByExpr, Query, Select, SetExpr, Statement as SqlStatement,
                TableFactor, TableWithJoins, Visit, Visitor,
            },
            dialect::GenericDialect,
            tokenizer::{Token, Tokenizer},
        },
    },
};
use futures_util::StreamExt;
use std::{
    any::Any,
    collections::{BTreeSet, HashMap},
    fmt,
    num::NonZeroUsize,
    ops::ControlFlow,
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
/// Non-whitespace tokens of the SQL text, counted by an iterative tokenizer
/// before parsing (`Error::Limit`). This bounds the depth of every syntax
/// tree DataFusion builds and walks recursively after parsing: a left-deep
/// chain such as `1+1+…`, `a OR b OR …` or `x::bigint::bigint…` has about
/// one level per two tokens, which the parser builds without recursion and
/// [`SQL_RECURSION_LIMIT`] therefore does not see; a postfix chain such as
/// `3 ! ! …` has one level per token. At this cap the deepest tree is
/// therefore about 1,000 levels (512 for a binary chain), far inside
/// [`SQL_THREAD_STACK_BYTES`]: an unoptimized build
/// planned 4,000 terms for 30 s without overflowing, and overflowed at
/// 7,900. Planning time is also superlinear in chain length (2,000 terms
/// took 7 s unoptimized).
pub const MAX_SQL_TOKENS: usize = 1024;
/// Relations a query may reference after expanding CTEs (`Error::Limit`):
/// table references, derived tables and subquery expressions, each CTE
/// reference counted as its whole body. Expansion is counted because a CTE
/// referenced twice by the next CTE doubles the plan at each level.
pub const MAX_SQL_RELATIONS: usize = 32;
/// Joins after expanding CTEs, explicit and comma-separated (`Error::Limit`):
/// enough to join every snapshot of a full engine and the `snapshots` table.
/// Join planning is superlinear in the join count (a 300-way chain spent
/// over 25 s in one optimizer rule) and cannot be cancelled.
pub const MAX_SQL_JOINS: usize = MAX_SNAPSHOTS;
/// `UNION`/`INTERSECT`/`EXCEPT` operations after expanding CTEs
/// (`Error::Limit`). Bounds planning work and the fan-out of one scan.
pub const MAX_SQL_SET_OPERATIONS: usize = 32;
/// CTE definitions in one statement, at any nesting (`Error::Limit`).
pub const MAX_SQL_CTES: usize = 16;
/// `datafusion.sql_parser.recursion_limit`: parser nesting depth, such as
/// parentheses and nested function calls.
pub const SQL_RECURSION_LIMIT: usize = 50;
/// Stack of each thread of an engine's query runtime. Planning, optimizing,
/// displaying and dropping a plan recurse once per tree level; the default
/// 2 MiB overflows, unoptimized, on a 600-term `1+1+…`. The token cap keeps
/// every tree far inside this. Reserved address space, committed on use.
pub const SQL_THREAD_STACK_BYTES: usize = 32 * 1024 * 1024;
/// Snapshots per engine (the `snapshots` provenance table is not counted).
pub const MAX_SNAPSHOTS: usize = 16;
/// Sealed Arrow memory of one snapshot (`get_array_memory_size`).
pub const MAX_SNAPSHOT_BYTES: usize = 96 * 1024 * 1024;
/// Sealed Arrow memory of all snapshots of one engine.
pub const MAX_ENGINE_BYTES: usize = 256 * 1024 * 1024;
/// `datafusion.execution.batch_size`: the size of batches that operators
/// build. The scan itself delivers the batches the snapshot was sealed with,
/// up to the capture's `batch_rows` (at most 1,024 rows), so an operator
/// directly over a scan sees those. Small, because abort and the deadline
/// take effect only at a yield, and an operator does up to one input
/// batch's work between yields (leaves yield before every batch): a filtered
/// equi-join evaluates its predicate for every candidate pair of a probe
/// batch in one poll.
pub const SQL_BATCH_ROWS: usize = 256;
/// Arrow bytes of one batch produced by any operator with an input
/// (`Error::Limit`). Measured as the referenced slice, not shared buffers.
/// The check runs after the batch exists, so it limits how far an oversized
/// intermediate propagates, not its first allocation.
pub const MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;
/// Arrow bytes produced by all operators with an input, summed over the
/// whole query (`Error::Limit`). A work bound rather than a residency bound:
/// it stops fan-out (one scan fed through many `UNION ALL` branches) that
/// neither the join-row cap nor the pool sees. Eight full passes over
/// [`MAX_ENGINE_BYTES`].
pub const MAX_PLAN_BYTES: usize = 8 * MAX_ENGINE_BYTES;
/// Rows produced by all hash and sort-merge joins of a query, summed
/// (`Error::Limit`). An equi-join on a low-cardinality key produces rows²
/// output that the pool does not see; this is a thousand times
/// `haystack_core`'s `MAX_ROWS`, the largest capture.
pub const MAX_JOIN_ROWS: usize = 10_000_000;
/// Session time zone (`datafusion.execution.time_zone`).
pub const SESSION_TIME_ZONE: &str = "+00:00";
/// Reserved name of the read-only provenance table.
pub const SNAPSHOTS_TABLE: &str = "snapshots";
/// Longest table name; names match `^[a-z][a-z0-9_]{0,62}$`.
pub const MAX_TABLE_NAME_BYTES: usize = 63;
/// The only scalar functions the session registers and the logical plan may
/// call, by canonical name (DataFusion's aliases of these, such as `pow` or
/// `length`, come with them). Anything else is `Error::Sql`, including a
/// function a DataFusion upgrade adds and one the SQL planner constructs
/// directly (`OVERLAY`, struct literals).
///
/// Left out because their output can outgrow their input, outside the
/// memory pool: string building (`concat`, `concat_ws`, `replace`,
/// `regexp_replace`, `overlay`, `translate`, `repeat`, `lpad`, `rpad`,
/// `to_hex`, `encode`, `to_char`/`date_format`, whose format may expand);
/// arbitrary target types (`arrow_cast`, `arrow_try_cast`, `cast_to_type`,
/// `try_cast_to_type`); quadratic work per row (`levenshtein`). Every
/// regex-backed function (`regexp_like`, `regexp_count`, `regexp_instr`,
/// `regexp_match`) is left out: compiling a pattern allocates and works
/// outside the pool (`'(?i)\p{L}{200}'` took 5.8 s and 581 MB), and
/// `regexp_match` builds a list per row that nested capture groups grow
/// about 250-fold. Also left out as unneeded surface: `decode`, struct,
/// union, metadata and file functions, and `version`. `substr` is not
/// listed because the SQL parser turns every spelling of it into
/// `SUBSTRING`, whose planner this build does not enable.
pub const ALLOWED_FUNCTIONS: &[&str] = &[
    // Math.
    "abs",
    "acos",
    "acosh",
    "asin",
    "asinh",
    "atan",
    "atan2",
    "atanh",
    "cbrt",
    "ceil",
    "cos",
    "cosh",
    "cot",
    "degrees",
    "exp",
    "factorial",
    "floor",
    "gcd",
    "isnan",
    "iszero",
    "lcm",
    "ln",
    "log",
    "log10",
    "log2",
    "nanvl",
    "pi",
    "power",
    "radians",
    "random",
    "round",
    "signum",
    "sin",
    "sinh",
    "sqrt",
    "tan",
    "tanh",
    "trunc",
    // Conditional.
    "coalesce",
    "greatest",
    "least",
    "nullif",
    "nvl",
    "nvl2",
    // Strings: inspection, slicing and case, none regex-backed. A result
    // is a slice of its input, a number or a boolean, except that Unicode
    // case mapping (`upper`, `lower`, `initcap`) can grow a string by a
    // small constant factor and `chr` builds one character.
    "ascii",
    "bit_length",
    "btrim",
    "character_length",
    "chr",
    "contains",
    "ends_with",
    "find_in_set",
    "initcap",
    "left",
    "lower",
    "ltrim",
    "octet_length",
    "reverse",
    "right",
    "rtrim",
    "split_part",
    "starts_with",
    "strpos",
    "substr_index",
    "upper",
    // Date and time.
    "current_date",
    "current_time",
    "date_bin",
    "date_part",
    "date_trunc",
    "from_unixtime",
    "make_date",
    "make_time",
    "now",
    "to_date",
    "to_local_time",
    "to_time",
    "to_timestamp",
    "to_timestamp_micros",
    "to_timestamp_millis",
    "to_timestamp_nanos",
    "to_timestamp_seconds",
    "to_unixtime",
    // Other.
    "arrow_typeof",
    "uuid",
];
/// The only aggregate functions the session registers and the logical plan
/// may call, as aggregates or over a window, by canonical name (aliases such
/// as `var_samp`, `stddev_samp` or `mean` come with them). Each keeps state
/// of a fixed size, or holds at most the values it receives; as an
/// aggregate that state is reserved from the pool.
///
/// Left out: `approx_percentile_cont` and
/// `approx_percentile_cont_with_weight`, whose literal centroid count is
/// allocated up front (`approx_percentile_cont(v, 0.5, 100000000000000)`
/// aborted the process allocating 1.6 PB); `array_agg` and `string_agg`,
/// which build a value from every row they receive, so over a growing
/// window frame their output is quadratic and outside the pool; `nth_value`
/// as an aggregate, which keeps up to a literal `n` rows of list state
/// (the window function of that name is allowed); and `grouping`, which is
/// meaningful only with the grouping sets that are rejected.
pub const ALLOWED_AGGREGATES: &[&str] = &[
    "any_value",
    "approx_distinct",
    "approx_median",
    "avg",
    "bit_and",
    "bit_or",
    "bit_xor",
    "bool_and",
    "bool_or",
    "corr",
    "count",
    "covar_pop",
    "covar_samp",
    "first_value",
    "last_value",
    "max",
    "median",
    "min",
    "percentile_cont",
    "regr_avgx",
    "regr_avgy",
    "regr_count",
    "regr_intercept",
    "regr_r2",
    "regr_slope",
    "regr_sxx",
    "regr_sxy",
    "regr_syy",
    "stddev",
    "stddev_pop",
    "sum",
    "var",
    "var_pop",
];
/// The only window functions the session registers and the logical plan
/// may call, by canonical name. This is every DataFusion 55 default: none
/// allocates by a literal argument (`ntile(n)` and `nth_value(x, n)` only
/// compare against `n`, and `lead`/`lag` clamp their offset to the
/// partition). Every window expression, including an aggregate over a
/// window, must also return a fixed-width type (see `docs/sql-snapshots.md`):
/// a window repeats one value per row of its partition, so
/// `first_value('<1 KB literal>') OVER ()` over a million-row join held
/// 2.5 GB while the pool read zero.
pub const ALLOWED_WINDOW_FUNCTIONS: &[&str] = &[
    "cume_dist",
    "dense_rank",
    "first_value",
    "lag",
    "last_value",
    "lead",
    "nth_value",
    "ntile",
    "percent_rank",
    "rank",
    "row_number",
];
/// The only binary operators the logical plan may use: comparison,
/// arithmetic, boolean and bitwise. `||` (`Operator::StringConcat`) is not
/// among them (see [`ALLOWED_FUNCTIONS`]); neither are the regex operators
/// (`~`, `~*`, `!~`, `!~*`), the `LIKE` operators (`~~` and the like), the
/// array and the JSON operators. Crate-private because it names a
/// DataFusion type.
pub(crate) const ALLOWED_OPERATORS: &[Operator] = &[
    Operator::Eq,
    Operator::NotEq,
    Operator::Lt,
    Operator::LtEq,
    Operator::Gt,
    Operator::GtEq,
    Operator::Plus,
    Operator::Minus,
    Operator::Multiply,
    Operator::Divide,
    Operator::Modulo,
    Operator::And,
    Operator::Or,
    Operator::IsDistinctFrom,
    Operator::IsNotDistinctFrom,
    Operator::BitwiseAnd,
    Operator::BitwiseOr,
    Operator::BitwiseXor,
    Operator::BitwiseShiftRight,
    Operator::BitwiseShiftLeft,
];
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
/// Start from [`QueryOptions::new`] (or `Default`) and the `with_` setters.
#[derive(Clone, Debug)]
#[non_exhaustive]
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
impl QueryOptions {
    /// [`QUERY_TIMEOUT`] and a fresh, uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self {
        self.cancellation = cancellation;
        self
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
    /// Largest reservation the query's memory pool reached. It counts only
    /// what operators reserve from the pool (hash tables, sort buffers,
    /// aggregate and window state); expression temporaries and batches in
    /// flight are not reserved and not included, so this is not the query's
    /// memory use.
    pub pool_peak_bytes: usize,
}

/// Collects snapshots for an immutable [`SqlEngine`]. A failed registration
/// leaves the builder exactly as it was.
#[derive(Default)]
pub struct SqlEngineBuilder {
    snapshots: Vec<Arc<Snapshot>>,
    bytes: usize,
    /// Always the production caps outside unit tests, which lower them so
    /// that a cap fires after little work rather than racing the deadline.
    pub(crate) caps: Caps,
}

/// Work caps one engine applies to each of its queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Caps {
    /// [`MAX_JOIN_ROWS`].
    pub(crate) join_rows: usize,
    /// [`QUERY_MEMORY_BYTES`].
    pub(crate) memory_bytes: usize,
}
impl Default for Caps {
    fn default() -> Self {
        Self {
            join_rows: MAX_JOIN_ROWS,
            memory_bytes: QUERY_MEMORY_BYTES,
        }
    }
}
impl fmt::Debug for SqlEngineBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlEngineBuilder")
            .field("snapshots", &self.snapshots())
            .field("bytes", &self.bytes)
            .finish()
    }
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
        // Before the runtime exists: a failure here must not drop a runtime,
        // which panics when the caller is itself inside one.
        let provenance = Arc::new(Provenance::new(&self.snapshots)?);
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder
            .worker_threads(SQL_WORKER_THREADS)
            .max_blocking_threads(SQL_WORKER_THREADS)
            .thread_stack_size(SQL_THREAD_STACK_BYTES)
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
        Ok(SqlEngine {
            inner: Arc::new(Inner {
                snapshots: self.snapshots,
                provenance,
                caps: self.caps,
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
    caps: Caps,
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
impl fmt::Debug for SqlEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlEngine")
            .field("snapshots", &self.snapshots())
            .field("running", &self.runtime.is_some())
            .finish()
    }
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
    /// Rejections happen before execution: text over [`MAX_SQL_BYTES`] or
    /// [`MAX_SQL_TOKENS`], or a statement over a planning bound
    /// ([`MAX_SQL_RELATIONS`], [`MAX_SQL_JOINS`], [`MAX_SQL_SET_OPERATIONS`],
    /// [`MAX_SQL_CTES`]) is `Error::Limit`; a timeout above [`QUERY_TIMEOUT`]
    /// is `Error::Options`; no free slot is `Error::Capacity`; anything but
    /// a query (or `EXPLAIN` of one), a function outside
    /// [`ALLOWED_FUNCTIONS`], [`ALLOWED_AGGREGATES`] and
    /// [`ALLOWED_WINDOW_FUNCTIONS`], a window expression of variable-width
    /// type, an operator outside the allowed set (no `||`, regex or `LIKE`
    /// operators), `LIKE`, `ILIKE`, `SIMILAR TO`, `CUBE`, `ROLLUP`,
    /// `GROUPING SETS`, `TABLESAMPLE`, locking clauses, cross joins,
    /// non-equi joins and recursive queries are `Error::Sql`. During
    /// execution, memory, intermediate or output over its cap is
    /// `Error::Limit`, and the deadline and cancellation token are
    /// `Error::Deadline` and `Error::Cancelled`. DataFusion's message text is
    /// never returned, because it can echo data. Dropping the returned future
    /// aborts the query. The deadline is timed on the caller's runtime, which
    /// must have its timer enabled.
    pub async fn query(&self, sql: &str, options: QueryOptions) -> Result<QueryOutput> {
        if sql.len() > MAX_SQL_BYTES {
            return Err(Error::Limit);
        }
        check_tokens(sql)?;
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
        let pool = Arc::new(PeakPool::new(inner.caps.memory_bytes));
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
    // Default planners; only the scalar, aggregate and window functions on
    // the allowlists; no table functions (no `generate_series`/`range` row
    // generators), no table factories (no `CREATE EXTERNAL TABLE` target
    // even if DDL were allowed) and no file formats. URL tables are never
    // enabled.
    let functions = SessionStateDefaults::default_scalar_functions()
        .into_iter()
        .filter(|f| ALLOWED_FUNCTIONS.contains(&f.name()))
        .collect();
    let aggregates = SessionStateDefaults::default_aggregate_functions()
        .into_iter()
        .filter(|f| ALLOWED_AGGREGATES.contains(&f.name()))
        .collect();
    let windows = SessionStateDefaults::default_window_functions()
        .into_iter()
        .filter(|f| ALLOWED_WINDOW_FUNCTIONS.contains(&f.name()))
        .collect();
    let state = SessionStateBuilder::new_with_default_features()
        .with_scalar_functions(functions)
        .with_aggregate_functions(aggregates)
        .with_window_functions(windows)
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
    let state = context.state();
    let dialect = state.config().options().sql_parser.dialect;
    let statement = state
        .sql_to_statement(sql, &dialect)
        .map_err(|e| classify(&e))?;
    check_statement(&statement)?;
    let plan = state
        .statement_to_plan(statement)
        .await
        .map_err(|e| classify(&e))?;
    sql_options().verify_plan(&plan).map_err(|_| Error::Sql)?;
    check_expressions(&plan, Stage::Planned)?;
    let sources = sources(inner, &plan)?;
    // Optimized exactly once, here, and the physical plan is built from this
    // checked plan by the query planner itself: `SessionState::
    // create_physical_plan` would optimize again, unchecked.
    let optimized = state.optimize(&plan).map_err(|e| classify(&e))?;
    check_expressions(&optimized, Stage::Optimized)?;
    reject_keyless_joins(&optimized)?;
    let physical = state
        .query_planner()
        .create_physical_plan(&optimized, &state)
        .await
        .map_err(|e| classify(&e))?;
    reject_runaway(&physical)?;
    let physical = guarded(physical, &Arc::new(Budget::new(inner.caps.join_rows)))?;
    let schema = physical.schema();
    let task = Arc::new(TaskContext::from(&state));
    let mut stream =
        datafusion::physical_plan::execute_stream(physical, task).map_err(|e| classify(&e))?;
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
        pool_peak_bytes: pool.peak(),
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

/// Counts non-whitespace tokens with sqlparser's tokenizer, which is a loop
/// over the text with no recursion, so this runs before anything recursive
/// sees the SQL.
fn check_tokens(sql: &str) -> Result<()> {
    let tokens = Tokenizer::new(&GenericDialect {}, sql)
        .tokenize()
        .map_err(|_| Error::Sql)?;
    let count = tokens
        .iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .count();
    if count > MAX_SQL_TOKENS {
        Err(Error::Limit)
    } else {
        Ok(())
    }
}

/// Accepts only a query, or `EXPLAIN` of one, within the planning bounds,
/// before any planning. DataFusion's statements (`COPY`, `CREATE EXTERNAL
/// TABLE`, `RESET`) and every other SQL statement are `Error::Sql` here;
/// `SQLOptions` refuses them again after planning.
fn check_statement(statement: &DfStatement) -> Result<()> {
    let statement = match statement {
        DfStatement::Statement(statement) => statement.as_ref(),
        DfStatement::Explain(explain) => return check_statement(&explain.statement),
        _ => return Err(Error::Sql),
    };
    if !matches!(statement, SqlStatement::Query(_)) {
        return Err(Error::Sql);
    }
    match statement.visit(&mut Gate::default()) {
        ControlFlow::Continue(()) => Ok(()),
        ControlFlow::Break(error) => Err(error),
    }
}

/// Planning size of a query or CTE body, with each CTE reference counted as
/// that CTE's whole body plus one.
#[derive(Clone, Copy, Default)]
struct Size {
    relations: usize,
    joins: usize,
    set_operations: usize,
}
impl Size {
    fn add(&mut self, other: Size) {
        self.relations = self.relations.saturating_add(other.relations);
        self.joins = self.joins.saturating_add(other.joins);
        self.set_operations = self.set_operations.saturating_add(other.set_operations);
    }
    fn max(self, other: Size) -> Size {
        Size {
            relations: self.relations.max(other.relations),
            joins: self.joins.max(other.joins),
            set_operations: self.set_operations.max(other.set_operations),
        }
    }
    fn check(&self) -> ControlFlow<Error> {
        if self.relations > MAX_SQL_RELATIONS
            || self.joins > MAX_SQL_JOINS
            || self.set_operations > MAX_SQL_SET_OPERATIONS
        {
            ControlFlow::Break(Error::Limit)
        } else {
            ControlFlow::Continue(())
        }
    }
}

/// One pass over the parsed statement. The tree it walks is bounded by
/// [`MAX_SQL_TOKENS`]; each node costs a constant. CTE names are matched
/// case-insensitively and across scopes, taking the largest body of a
/// name, so shadowing can only overcount.
#[derive(Default)]
struct Gate {
    /// Sizes of the queries being visited, innermost last.
    open: Vec<Size>,
    /// CTE bodies awaiting their size, by address of their `Query`.
    bodies: Vec<(*const Query, String)>,
    /// Size of each CTE body, by lowercased name.
    ctes: HashMap<String, Size>,
    defined: usize,
}
impl Gate {
    fn top(&mut self) -> &mut Size {
        if self.open.is_empty() {
            self.open.push(Size::default());
        }
        let last = self.open.len() - 1;
        &mut self.open[last]
    }
}
impl Visitor for Gate {
    type Break = Error;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Error> {
        // `FOR UPDATE`/`FOR SHARE`, `FOR XML`/`FOR JSON` and dialect clauses
        // that DataFusion would otherwise ignore.
        if !query.locks.is_empty()
            || query.for_clause.is_some()
            || query.settings.is_some()
            || query.format_clause.is_some()
            || !query.pipe_operators.is_empty()
        {
            return ControlFlow::Break(Error::Sql);
        }
        if let Some(with) = &query.with {
            if with.recursive {
                return ControlFlow::Break(Error::Sql);
            }
            for cte in &with.cte_tables {
                self.defined += 1;
                if self.defined > MAX_SQL_CTES {
                    return ControlFlow::Break(Error::Limit);
                }
                self.bodies.push((
                    std::ptr::from_ref(cte.query.as_ref()),
                    cte.alias.name.value.to_lowercase(),
                ));
            }
        }
        let mut size = Size::default();
        let mut pending = vec![query.body.as_ref()];
        while let Some(body) = pending.pop() {
            if let SetExpr::SetOperation { left, right, .. } = body {
                size.set_operations += 1;
                pending.push(left);
                pending.push(right);
            }
        }
        self.open.push(size);
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, query: &Query) -> ControlFlow<Error> {
        let size = self.open.pop().unwrap_or_default();
        size.check()?;
        let address = std::ptr::from_ref(query);
        if let Some(i) = self.bodies.iter().position(|(body, _)| *body == address) {
            let (_, name) = self.bodies.swap_remove(i);
            let entry = self.ctes.entry(name).or_default();
            *entry = entry.max(size);
        } else if let Some(parent) = self.open.last_mut() {
            parent.add(size);
            parent.check()?;
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Error> {
        // CUBE, ROLLUP and GROUPING SETS plan one grouping per subset: 2^N
        // groupings for CUBE of N expressions, before any limit applies.
        if let GroupByExpr::All(modifiers) | GroupByExpr::Expressions(_, modifiers) =
            &select.group_by
            && !modifiers.is_empty()
        {
            return ControlFlow::Break(Error::Sql);
        }
        let joins = select
            .from
            .iter()
            .map(joins_in)
            .fold(0, usize::saturating_add)
            .saturating_add(select.from.len().saturating_sub(1));
        let top = self.top();
        top.joins = top.joins.saturating_add(joins);
        top.check()
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Error> {
        let mut size = Size {
            relations: 1,
            ..Size::default()
        };
        match factor {
            TableFactor::Table { name, sample, .. } => {
                if sample.is_some() {
                    return ControlFlow::Break(Error::Sql);
                }
                if let [part] = name.0.as_slice()
                    && let Some(ident) = part.as_ident()
                    && let Some(body) = self.ctes.get(&ident.value.to_lowercase())
                {
                    size.add(*body);
                }
            }
            TableFactor::Derived { sample, .. } if sample.is_some() => {
                return ControlFlow::Break(Error::Sql);
            }
            // Parentheses around joins: its tables are visited, and its
            // joins counted by the enclosing `SELECT`.
            TableFactor::NestedJoin { .. } => return ControlFlow::Continue(()),
            _ => {}
        }
        let top = self.top();
        top.add(size);
        top.check()
    }

    fn pre_visit_expr(&mut self, expr: &SqlExpr) -> ControlFlow<Error> {
        match expr {
            SqlExpr::Cube(_) | SqlExpr::Rollup(_) | SqlExpr::GroupingSets(_) => {
                ControlFlow::Break(Error::Sql)
            }
            SqlExpr::Subquery(_) | SqlExpr::InSubquery { .. } | SqlExpr::Exists { .. } => {
                let top = self.top();
                top.relations = top.relations.saturating_add(1);
                top.check()
            }
            _ => ControlFlow::Continue(()),
        }
    }
}

/// Joins of one `FROM` item, including those inside parentheses
/// (`FROM (a JOIN b …)`), which are a nested join factor rather than joins
/// of the item. Recursion is bounded by the parser's recursion limit.
fn joins_in(from: &TableWithJoins) -> usize {
    let nested = |factor: &TableFactor| match factor {
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => joins_in(table_with_joins),
        _ => 0,
    };
    from.joins
        .iter()
        .map(|join| nested(&join.relation))
        .fold(nested(&from.relation), usize::saturating_add)
        .saturating_add(from.joins.len())
}

/// Which logical plan [`check_expressions`] is checking.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// As the SQL planner built it: no `LIKE` at all.
    Planned,
    /// After optimization, where `starts_with(x, 'literal')` has become
    /// `x LIKE 'literal%'`.
    Optimized,
}

/// Every function, operator and pattern match of `plan`, including inside
/// subqueries, must be allowed, and every window expression must have a
/// fixed-width type. Checked on the planned and the optimized plan, because
/// the SQL planner and the optimizer construct functions directly rather
/// than through the session registry.
fn check_expressions(plan: &LogicalPlan, stage: Stage) -> Result<()> {
    let mut allowed = true;
    plan.apply_with_subqueries(|node| {
        if let LogicalPlan::Window(window) = node {
            // The window columns follow the input's in the node's schema.
            let first = window.input.schema().fields().len();
            allowed &= window.schema.fields()[first.min(window.schema.fields().len())..]
                .iter()
                .all(|field| fixed_width(field.data_type()).is_some());
        }
        if !allowed {
            return Ok(TreeNodeRecursion::Stop);
        }
        node.apply_expressions(|expr| {
            expr.apply(|expr| {
                allowed &= allowed_expression(expr, stage);
                Ok(if allowed {
                    TreeNodeRecursion::Continue
                } else {
                    TreeNodeRecursion::Stop
                })
            })
        })
    })
    .map_err(|_| Error::Query)?;
    if allowed { Ok(()) } else { Err(Error::Sql) }
}

fn allowed_expression(expr: &Expr, stage: Stage) -> bool {
    match expr {
        Expr::ScalarFunction(call) => ALLOWED_FUNCTIONS.contains(&call.func.name()),
        Expr::AggregateFunction(call) => ALLOWED_AGGREGATES.contains(&call.func.name()),
        Expr::WindowFunction(window) => match &window.fun {
            WindowFunctionDefinition::AggregateUDF(f) => ALLOWED_AGGREGATES.contains(&f.name()),
            WindowFunctionDefinition::WindowUDF(f) => ALLOWED_WINDOW_FUNCTIONS.contains(&f.name()),
        },
        Expr::BinaryExpr(binary) => ALLOWED_OPERATORS.contains(&binary.op),
        // `SIMILAR TO` is planned as a regex match of the pattern.
        Expr::SimilarTo(_) => false,
        // `LIKE` and `ILIKE` compile a general pattern into a regex, outside
        // the pool. Only the prefix form the optimizer derives from
        // `starts_with` is allowed, which arrow matches without a regex or
        // with one of literals only.
        Expr::Like(like) => stage == Stage::Optimized && prefix_like(like),
        _ => true,
    }
}

/// `x LIKE 'literal%'`, case-sensitive, with the default escape: the only
/// `LIKE` the optimizer constructs (from `starts_with`).
fn prefix_like(like: &Like) -> bool {
    if like.case_insensitive || like.escape_char.is_some() {
        return false;
    }
    let mut pattern = like.pattern.as_ref();
    while let Expr::Cast(cast) = pattern {
        pattern = cast.expr.as_ref();
    }
    let Expr::Literal(
        ScalarValue::Utf8(Some(pattern))
        | ScalarValue::LargeUtf8(Some(pattern))
        | ScalarValue::Utf8View(Some(pattern)),
        _,
    ) = pattern
    else {
        return false;
    };
    let Some(prefix) = pattern.strip_suffix('%') else {
        return false;
    };
    let mut chars = prefix.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.next().is_some() => {}
            '%' | '_' | '\\' => return false,
            _ => {}
        }
    }
    true
}

/// Bytes per row of a window column of `data_type`, with its validity bit
/// rounded up, or `None` for a variable-width type, which a window would
/// repeat for every row of its partition.
fn fixed_width(data_type: &DataType) -> Option<usize> {
    match data_type {
        DataType::Null | DataType::Boolean => Some(1),
        other if other.is_primitive() => other.primitive_width().map(|width| width + 1),
        _ => None,
    }
}

/// Per-query counters shared by every [`GuardExec`] of one plan, and the
/// join-row cap they are checked against ([`MAX_JOIN_ROWS`] by default).
#[derive(Debug)]
struct Budget {
    plan_bytes: AtomicUsize,
    join_rows: AtomicUsize,
    max_join_rows: usize,
}
impl Budget {
    fn new(max_join_rows: usize) -> Self {
        Self {
            plan_bytes: AtomicUsize::new(0),
            join_rows: AtomicUsize::new(0),
            max_join_rows,
        }
    }
}
impl Default for Budget {
    fn default() -> Self {
        Self::new(MAX_JOIN_ROWS)
    }
}

/// Bytes a batch references: the slices its arrays cover, not the whole
/// buffers they share (a projection of a sealed batch allocates nothing).
fn batch_bytes(batch: &RecordBatch) -> usize {
    batch
        .columns()
        .iter()
        .map(|column| {
            column
                .to_data()
                .get_slice_memory_size()
                .unwrap_or_else(|_| column.get_array_memory_size())
        })
        .fold(0, usize::saturating_add)
}

fn exhausted(what: &str) -> DataFusionError {
    DataFusionError::ResourcesExhausted(format!("{what} exceeds its haystack-sql cap"))
}

/// What a [`GuardExec`] checks on the batches its input produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Watch {
    /// The output of an operator with inputs: [`MAX_BATCH_BYTES`] per batch
    /// and [`MAX_PLAN_BYTES`] over the plan; for a hash or sort-merge join,
    /// also [`MAX_JOIN_ROWS`].
    Output { join: bool },
    /// The input of a window operator, which buffers it without reserving:
    /// every batch is reserved from the query's pool until the stream ends,
    /// together with `row_bytes` per row for the window columns the
    /// operator will add to it (all fixed-width, see [`check_expressions`]),
    /// before it builds them.
    WindowInput { row_bytes: usize },
    /// The output of a leaf: yields to the runtime before each batch. A
    /// parent can do a whole input batch's work without producing output (a
    /// filtered join probe that matches nothing loops internally), and
    /// Tokio's cooperative budget lets a leaf deliver 128 batches before it
    /// yields, so without this a query might never reach a point where abort
    /// and the deadline take effect.
    Leaf,
}

/// Wraps one operator's output stream and fails it with
/// `ResourcesExhausted` (`Error::Limit`) when a [`Watch`] bound is passed.
/// The plan runs one partition, so each guard executes once per query.
#[derive(Debug)]
struct GuardExec {
    input: Arc<dyn ExecutionPlan>,
    watch: Watch,
    budget: Arc<Budget>,
    properties: Arc<PlanProperties>,
}
impl GuardExec {
    fn wrap(input: Arc<dyn ExecutionPlan>, watch: Watch, budget: &Arc<Budget>) -> Arc<Self> {
        Arc::new(Self {
            properties: input.properties().clone(),
            input,
            watch,
            budget: budget.clone(),
        })
    }
}
impl DisplayAs for GuardExec {
    fn fmt_as(&self, _: DisplayFormatType, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GuardExec: {:?}", self.watch)
    }
}
impl ExecutionPlan for GuardExec {
    fn name(&self) -> &str {
        "GuardExec"
    }
    fn properties(&self) -> &Arc<PlanProperties> {
        &self.properties
    }
    fn maintains_input_order(&self) -> Vec<bool> {
        vec![true]
    }
    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![&self.input]
    }
    fn apply_expressions(
        &self,
        _: &mut dyn FnMut(&Arc<dyn PhysicalExpr>) -> datafusion::error::Result<TreeNodeRecursion>,
    ) -> datafusion::error::Result<TreeNodeRecursion> {
        Ok(TreeNodeRecursion::Continue)
    }
    fn with_new_children(
        self: Arc<Self>,
        mut children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        match (children.pop(), children.is_empty()) {
            (Some(input), true) => Ok(GuardExec::wrap(input, self.watch, &self.budget)),
            _ => Err(DataFusionError::Internal("GuardExec has one child".into())),
        }
    }
    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> datafusion::error::Result<SendableRecordBatchStream> {
        let input = self.input.execute(partition, context.clone())?;
        let schema = input.schema();
        if self.watch == Watch::Leaf {
            let yielding = input.then(|batch| async move {
                tokio::task::yield_now().await;
                batch
            });
            return Ok(Box::pin(RecordBatchStreamAdapter::new(schema, yielding)));
        }
        let (watch, budget) = (self.watch, self.budget.clone());
        let mut reservation = matches!(watch, Watch::WindowInput { .. }).then(|| {
            MemoryConsumer::new("haystack-sql window input").register(context.memory_pool())
        });
        let checked = input.map(move |batch| {
            let batch = batch?;
            let bytes = batch_bytes(&batch);
            match watch {
                Watch::Leaf => {}
                Watch::WindowInput { row_bytes } => {
                    if let Some(reservation) = reservation.as_mut() {
                        let output = batch.num_rows().saturating_mul(row_bytes);
                        reservation.try_grow(bytes.saturating_add(output))?;
                    }
                }
                Watch::Output { join } => {
                    if bytes > MAX_BATCH_BYTES {
                        return Err(exhausted("an intermediate batch"));
                    }
                    let total = budget.plan_bytes.fetch_add(bytes, Ordering::Relaxed);
                    if total.saturating_add(bytes) > MAX_PLAN_BYTES {
                        return Err(exhausted("intermediate bytes"));
                    }
                    let rows = if join { batch.num_rows() } else { 0 };
                    let total = budget.join_rows.fetch_add(rows, Ordering::Relaxed);
                    if total.saturating_add(rows) > budget.max_join_rows {
                        return Err(exhausted("join output rows"));
                    }
                }
            }
            Ok(batch)
        });
        Ok(Box::pin(RecordBatchStreamAdapter::new(schema, checked)))
    }
}

fn is_join(plan: &dyn ExecutionPlan) -> bool {
    let plan: &dyn Any = plan;
    plan.is::<HashJoinExec>() || plan.is::<SortMergeJoinExec>()
}

fn is_window(plan: &dyn ExecutionPlan) -> bool {
    let plan: &dyn Any = plan;
    plan.is::<WindowAggExec>() || plan.is::<BoundedWindowAggExec>()
}

/// Bytes per row of the columns a window operator appends to its input. A
/// variable-width column, which the logical check rejects, is charged as
/// [`MAX_BATCH_BYTES`] so that it fails at the first row.
fn window_row_bytes(window: &dyn ExecutionPlan) -> usize {
    let first = window
        .children()
        .first()
        .map_or(0, |input| input.schema().fields().len());
    window
        .schema()
        .fields()
        .iter()
        .skip(first)
        .map(|field| fixed_width(field.data_type()).unwrap_or(MAX_BATCH_BYTES))
        .fold(0, usize::saturating_add)
}

/// Wraps every leaf in a yielding [`GuardExec`], every operator that has
/// inputs in a checking one and then a `CooperativeExec`, and feeds each
/// window operator through a reserving guard. `CooperativeExec` makes each
/// output batch consume Tokio's
/// cooperative budget: DataFusion 55 makes only leaves cooperative, and an
/// equi-join on a low-cardinality key can emit millions of rows per probe
/// batch without polling a leaf, ignoring abort for that long.
fn guarded(plan: Arc<dyn ExecutionPlan>, budget: &Arc<Budget>) -> Result<Arc<dyn ExecutionPlan>> {
    plan.transform_up(|node| {
        let any: &dyn Any = node.as_ref();
        if node.children().is_empty() {
            return Ok(Transformed::yes(
                GuardExec::wrap(node, Watch::Leaf, budget) as Arc<dyn ExecutionPlan>
            ));
        }
        if any.is::<CooperativeExec>() || any.is::<GuardExec>() {
            return Ok(Transformed::no(node));
        }
        let node = if is_window(node.as_ref()) {
            let row_bytes = window_row_bytes(node.as_ref());
            let inputs = node
                .children()
                .into_iter()
                .map(|child| {
                    GuardExec::wrap(child.clone(), Watch::WindowInput { row_bytes }, budget)
                        as Arc<dyn ExecutionPlan>
                })
                .collect();
            datafusion::physical_plan::replace_children_if_necessary(node, inputs)?
        } else {
            node
        };
        let join = is_join(node.as_ref());
        let guard = GuardExec::wrap(node, Watch::Output { join }, budget);
        Ok(Transformed::yes(
            Arc::new(CooperativeExec::new(guard)) as Arc<dyn ExecutionPlan>
        ))
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
