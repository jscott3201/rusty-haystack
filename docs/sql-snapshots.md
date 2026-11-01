# SQL over committed snapshots

The optional `sql` feature of `rusty-haystack-analytics` (module
`haystack_analytics::sql`) runs bounded, read-only SQL with
[Apache DataFusion](https://datafusion.apache.org/) over
[Parquet history captures](parquet-history.md) that have been fully validated
and sealed in memory. It is a Rust library profile only: there is no SQL
server, HTTP route, CLI command, MCP tool or Python binding, and it does not
push queries down to a live history provider.

Without the feature no DataFusion code is compiled and the crate gains no
dependency. One change reaches every build: the workspace's exact Arrow and
Parquet pins moved from 59.3.0 to 59.3.2, so the capture format and DataFusion
share one family. With the feature, the crate depends on `datafusion` (pinned
exactly, default features off, only `sql` and `recursive_protection`; not
DataFusion's `parquet` feature) and the `arrow` umbrella crate from that same
family. No other workspace crate gains a dependency.

```toml
rusty-haystack-analytics = { version = "0.9", features = ["sql"] }
```

## Register and query

```rust,no_run
use haystack_analytics::sql::{QueryOptions, SqlEngine};

# async fn demo() -> Result<(), haystack_analytics::Error> {
let mut builder = SqlEngine::builder();
builder.register("supply_temp", "/data/captures/supply").await?;
builder.register("return_temp", "/data/captures/return").await?;
let engine = builder.build()?;

let output = engine
    .query(
        "SELECT date_trunc('hour', ts) AS hour, AVG(number_value) AS avg \
         FROM supply_temp WHERE present GROUP BY hour ORDER BY hour",
        QueryOptions::default(),
    )
    .await?;
for source in &output.sources {
    println!("{} = {} @ {}", source.table, source.export_id, source.manifest_sha256);
}
# Ok(()) }
```

- `SqlEngineBuilder::register(&mut self, name, directory)` opens the capture
  with `Dataset::open`, drains every batch through the validating reader on
  `spawn_blocking`, and requires `Dataset::semantically_complete()` before the
  table exists. Any failure (missing or unpublished manifest, a manifest or
  part that fails verification, rows the reader rejects, a coverage
  contradiction found only after the last row, a bound) returns the error and
  leaves the builder exactly as it was. Called outside a Tokio runtime it
  returns `Error::Runtime`.
- Table names match `^[a-z][a-z0-9_]{0,62}$`, are unique, and are not
  `snapshots`; otherwise `Error::Options`.
- `build()` freezes the set and starts the engine's dedicated query runtime.
  The engine is immutable; register everything first.
- `SqlEngine::query(sql, options)` returns a complete
  `QueryOutput { schema, batches, sources, pool_peak_bytes }`. Build options
  with `QueryOptions::new().with_timeout(…).with_cancellation(…)` (the struct
  is `#[non_exhaustive]`). `sources` lists the snapshot identity (table,
  export id, manifest SHA-256) of every table the logical plan scans,
  including through CTEs and subqueries; reading only the `snapshots` table
  contributes none. `pool_peak_bytes` is the largest reservation of the
  query's memory pool: it counts only what operators reserve (hash tables,
  sort buffers, aggregate and window state), not expression temporaries or
  batches in flight, so it is not the query's memory use.
- `SqlEngine` and `SqlEngineBuilder` implement `Debug` (registered
  identities only).

### Sealed snapshots

The validated samples are converted once, at registration, into the SQL
schema below and held in memory behind a read-only `TableProvider` that
serves them through DataFusion's `MemorySourceConfig`. A query never opens a
file: a `LIMIT` or early termination cannot expose a row the reader had not
validated, and rewriting or deleting the capture directory afterwards does not
change any result. The identity reported is the one verified at registration.

## Schema

Every snapshot table has the same columns and no schema metadata, so
snapshots of different points can be combined with `UNION ALL`.

| Column | Type | Meaning |
|---|---|---|
| `point_id` | `Utf8` | The captured point id |
| `ts` | `Timestamp(Nanosecond, "UTC")` | The instant: `utc_seconds * 1e9 + nanoseconds` |
| `utc_seconds` | `Int64` | Seconds since the Unix epoch, as captured |
| `nanoseconds` | `UInt32` | Sub-second nanoseconds, as captured; a leap-second fraction is ≥ 1,000,000,000 |
| `offset_seconds` | `Int32` | The sample's stored UTC offset |
| `ts_local` | `Timestamp(Nanosecond, None)` | Wall-clock time: `ts` plus `offset_seconds` |
| `tz` | `Utf8` | The Haystack timezone name (for example `New_York`); not an IANA path |
| `present` | `Boolean` | `false` for NA |
| `bool_value` | `Boolean`, nullable | The value of a Bool point |
| `number_value` | `Float64`, nullable | The value of a Number point, exact binary64 bits |
| `string_value` | `Utf8`, nullable | The value of a Str point |
| `unit` | `Utf8`, nullable | A Number sample's own unit spelling, if any |

The `snapshots` table has one row per snapshot: `table_name`, `export_id`,
`manifest_sha256`, `point_id`, `kind` (`Bool`, `Number`, `Str`), `timezone`,
`point_unit` (nullable), `requested_range`, `rows` (`UInt64`),
`history_authority` and `history_incarnation` (lowercase hex) and
`history_generation` (`UInt64`).

### NA, NULL, NaN and infinities

- **NA is NULL.** An NA sample has `present = false` and NULL in every value
  column and in `unit`. A present sample fills only its point kind's value
  column; the other two are NULL. So `COUNT(number_value)` counts present
  numbers, `COUNT(*)` counts samples, and aggregates skip NA as SQL skips NULL.
- **NaN and ±Infinity are values, not NULL.** They keep their exact bits.
  DataFusion orders NaN above +Infinity, so `MAX` of a column containing NaN
  is NaN and `SUM`/`AVG` propagate it. Use `isnan(number_value)` to filter.
- `-0.0` keeps its sign bit.

### Timezones and `ts_local`

The session time zone is `SESSION_TIME_ZONE` (UTC). `ts` is a UTC instant;
`date_trunc('hour', ts)` groups by UTC hour. `ts_local` is the captured wall
clock (the stored offset applied, no zone rules evaluated at query time), so
`date_trunc('hour', ts_local)` groups by local hour. Around a DST fall-back,
`ts_local` repeats: the two 01:00–01:59 hours in New York fall into one local
hour while `ts` keeps them apart. Use `offset_seconds` to distinguish them.

### Leap seconds

`utc_seconds` and `nanoseconds` are exactly what was captured, including a
leap-second fraction (`nanoseconds` ≥ 1,000,000,000). `ts` is computed as
`utc_seconds * 1e9 + nanoseconds`, so a leap fraction lands in the following
second and two distinct captured components can map near each other; use the
components when that matters. An instant outside the `i64` nanosecond range
(about years 1677 to 2262) fails registration with `Error::Limit`.

## Bounds

The constants are the documentation; they live in
`haystack-analytics/src/sql/mod.rs`.

| Bound | Constant | Over the bound |
|---|---|---|
| Concurrent queries per process, no wait queue | `SQL_QUERIES` | `Error::Capacity` |
| Worker threads of each engine's query runtime | `SQL_WORKER_THREADS` | — |
| Memory pool per query (`TrackConsumersPool<GreedyMemoryPool>`), spill disabled | `QUERY_MEMORY_BYTES` | `Error::Limit` |
| Default and largest timeout, from admission | `QUERY_TIMEOUT` | a larger option is `Error::Options`; expiry is `Error::Deadline` |
| Wait for an aborted query to stop before returning | `CANCEL_GRACE` | the call returns; see below |
| Rows returned, counted while draining | `MAX_OUTPUT_ROWS` | `Error::Limit` |
| Arrow bytes returned (`get_array_memory_size`, conservative for slices) | `MAX_OUTPUT_BYTES` | `Error::Limit` |
| SQL text bytes, before parsing | `MAX_SQL_BYTES` | `Error::Limit` |
| Non-whitespace SQL tokens, by an iterative tokenizer before parsing | `MAX_SQL_TOKENS` | `Error::Limit` |
| Relations (tables, derived tables, subqueries), CTE references expanded | `MAX_SQL_RELATIONS` | `Error::Limit` |
| Joins (explicit, comma and inside parentheses), CTE references expanded | `MAX_SQL_JOINS` | `Error::Limit` |
| `UNION`/`INTERSECT`/`EXCEPT` operations, CTE references expanded | `MAX_SQL_SET_OPERATIONS` | `Error::Limit` |
| CTE definitions | `MAX_SQL_CTES` | `Error::Limit` |
| Parser recursion (`datafusion.sql_parser.recursion_limit`) | `SQL_RECURSION_LIMIT` | `Error::Sql` |
| Stack of each query-runtime thread | `SQL_THREAD_STACK_BYTES` | — |
| Snapshots per engine | `MAX_SNAPSHOTS` | `Error::Limit` |
| Sealed Arrow bytes per snapshot / per engine | `MAX_SNAPSHOT_BYTES`, `MAX_ENGINE_BYTES` | `Error::Limit` at registration |
| Batch size operators build (a scan delivers the capture's own batches, up to its `batch_rows`) | `SQL_BATCH_ROWS` | — |
| Arrow bytes of one batch from any operator with an input | `MAX_BATCH_BYTES` | `Error::Limit` |
| Arrow bytes from all operators with an input, summed over the query | `MAX_PLAN_BYTES` | `Error::Limit` |
| Rows from all hash and sort-merge joins, summed over the query | `MAX_JOIN_ROWS` | `Error::Limit` |
| Table name length | `MAX_TABLE_NAME_BYTES` | `Error::Options` |
| Scalar functions the session registers and plans may call | `ALLOWED_FUNCTIONS` | anything else is `Error::Sql` |
| Aggregate functions, as aggregates or over a window | `ALLOWED_AGGREGATES` | anything else is `Error::Sql` |
| Window functions | `ALLOWED_WINDOW_FUNCTIONS` | anything else is `Error::Sql` |
| Binary operators: comparison, arithmetic, boolean, bitwise | (crate-private list) | anything else, including `\|\|`, `~` and `~~`, is `Error::Sql` |

The session also fixes `target_partitions = 1`, turns every repartitioning
option off, keeps `information_schema` off and disables recursive CTEs. The
capture's own bounds (`MAX_ROWS` per capture and the `Limits` of
[Parquet history captures](parquet-history.md#bounds)) apply first.

### Before planning

Planning cannot be cancelled and DataFusion's planner, optimizer, plan display
and drop all recurse once per tree level, so the size of a statement is
bounded before any of them run, in this order:

1. `MAX_SQL_BYTES`, then `MAX_SQL_TOKENS` with sqlparser's tokenizer, which is
   a loop with no recursion. The parser builds a left-deep chain (`1+1+…`,
   `a OR b OR …`, `x::bigint::bigint…`, `s||s||…`) without recursion, so
   `SQL_RECURSION_LIMIT` never sees it, but every later pass recurses once
   per term. Before the token cap, 600 terms of `1+1+…` (about 1.2 KB)
   aborted the whole process with a stack overflow in an unoptimized build.
   At the cap the deepest chain is 512 terms; an unoptimized build with
   `SQL_THREAD_STACK_BYTES` planned 4,000 for 30 s without overflowing, and
   overflowed at 7,900.
2. Parsing, with `SQL_RECURSION_LIMIT` for nesting such as parentheses.
3. One walk of the parsed statement. Only a query, or `EXPLAIN` of one, is
   accepted; every other statement is `Error::Sql` here (and `SQLOptions`
   refuses DDL, DML and statements again after planning). `CUBE`, `ROLLUP`
   and `GROUPING SETS` (2^N groupings for `CUBE` of N expressions, planned
   before any limit applies), `TABLESAMPLE`, locking clauses (`FOR UPDATE`,
   `FOR SHARE`) and recursive CTEs are `Error::Sql`, rather than planned
   exponentially or silently ignored. Relations, joins and set operations are
   counted with each CTE reference expanded to its body, because a CTE
   referenced twice by the next doubles the plan at every level (16 such
   levels reached 1.4 GB before this bound).

A 300-way join chain previously spent more than 25 s in one optimizer rule
and then ran past its deadline; it is now `Error::Limit` in under a
millisecond, and a chain at `MAX_SQL_JOINS` plans in tens of milliseconds
unoptimized.

### Functions and operators

Plans may call only the scalar functions in `ALLOWED_FUNCTIONS`, the
aggregates in `ALLOWED_AGGREGATES` and the window functions in
`ALLOWED_WINDOW_FUNCTIONS`, and may use only comparison, arithmetic, boolean
and bitwise operators. The session registers only those functions, and both
the planned and the optimized logical plan are checked, because the SQL
planner builds some functions directly (`OVERLAY … PLACING`, struct
literals) and an allowlist means a DataFusion upgrade cannot add one
silently. The physical plan is built from that checked optimized plan;
nothing optimizes it again.

**String building is not available in SQL.** `||` and `concat`, `concat_ws`,
`replace`, `regexp_replace`, `overlay`, `translate`, `repeat`, `lpad`, `rpad`,
`to_hex`, `encode` and `to_char`/`date_format` are rejected (`Error::Sql`):
each can make its output larger than its input, outside the memory pool, and
nested they compound (ten CTE levels of an eightfold `s||s||…` reached 4.7 GB
of resident memory; one 1,500-term `||` expression held a thread for 8.5 s).
Select the parts as columns and concatenate them after the query, in the
caller. Inspection, slicing and case functions (`length`, `strpos`, `left`,
`right`, `substr_index`, `split_part`, `btrim`, `upper`, `lower`, `reverse`,
`starts_with`, `ends_with`, `contains`, …) are available; Unicode case
mapping can grow a string by a small constant factor once, never
compounding. `arrow_cast` and the other arbitrary-type casts are rejected
(`arrow_cast(NULL, 'FixedSizeBinary(100000000)')` allocated gigabytes), as is
`levenshtein` (quadratic work in one row).

**Regular expressions and pattern matching are not available in SQL.**
`regexp_like`, `regexp_count`, `regexp_instr`, `regexp_match`, the operators
`~`, `~*`, `!~`, `!~*`, `SIMILAR TO` (which DataFusion plans as a regex
match), `LIKE`, `ILIKE`, their negations and the `~~` operator family are all
`Error::Sql`. Compiling and running a regex allocates outside the pool and
cannot be bounded by the pattern's length: `regexp_like(s, '(?i)\p{L}{200}')`
took 5.8 s and 581 MB of resident memory with the pool at zero, and
`regexp_match` with nested capture groups grew its input about 250-fold (82 MB
to 698 MB). DataFusion evaluates `LIKE` and `ILIKE` with Arrow's kernels,
which match a pattern with no inner wildcard (`'abc'`, `'abc%'`, `'%abc'`,
`'%abc%'`) without a regex but compile any other pattern into one, with no
bound but the regex crate's size limit: a 14 KB `'%a%a…'` pattern took 6.6 s
and 327 MB. Use `starts_with`, `ends_with`, `contains`, `strpos`, `=` and
`lower()`/`upper()` for case-insensitive tests, or match in the caller. The
optimizer itself rewrites `starts_with(x, 'literal')` into a prefix `LIKE`
that Arrow matches without a regex (or with a regex of literals only, when
the prefix contains `%`, `_` or `\`); that one form is accepted in the
optimized plan.

**Aggregates.** `ALLOWED_AGGREGATES` holds the aggregates whose state has a
fixed size or holds at most the values they receive; as an aggregate, that
state is reserved from the pool. Left out are
`approx_percentile_cont` and `approx_percentile_cont_with_weight`, which
allocate their literal centroid count up front
(`approx_percentile_cont(v, 0.5, 100000000000000)` aborted the process
allocating 1.6 PB); `array_agg` and `string_agg`, which build a value from
every row they receive, so over a growing window frame their output is
quadratic and outside the pool; `nth_value` as an aggregate (the window
function of that name is allowed), which keeps up to a literal `n` rows of
list state; and `grouping`, which only grouping sets use.

**Window functions.** Every DataFusion 55 window function is allowed: none
allocates by a literal argument (`ntile(n)` and `nth_value(x, n)` only
compare with `n`, and `lead`/`lag` clamp their offset to the partition).
Every window expression, including an aggregate over a window, must return a
fixed-width type (numbers, booleans, timestamps, dates, durations, intervals,
decimals); a string, binary or list window is `Error::Sql`. A window
repeats a value for every row of its partition, and its output is not
reserved: `first_value('<1 KB literal>') OVER ()` over a million-row join
held 2.5 GB with the pool at zero, and the same with a 15 KB literal
overflowed Arrow's 2 GiB string offsets. Use the window to find the row
(`row_number()`, `lag(ts)`, a fixed-width key) and join or select the string
by it. The fixed-width columns a window adds are reserved from the pool per
input row, as its input arrives (see below).

**`SUBSTRING` is not available.** The SQL parser turns `substr(…)`,
`substring(…)` and `SUBSTRING(x FROM … FOR …)` alike into a `SUBSTRING`
expression, whose planner needs a DataFusion feature this build does not
enable, so all three are `Error::Sql`. Use `left(x, n)`, `right(x, n)` or
`substr_index(x, delimiter, count)`. `POSITION(a IN b)` and
`EXTRACT(field FROM ts)` are `Error::Sql` for the same reason; use
`strpos(b, a)` and `date_part('field', ts)`.

### Subqueries

DataFusion decorrelates some subqueries into joins without equality keys,
which the runaway-join policy below rejects (`Error::Sql`): an uncorrelated
`EXISTS`, comparisons with `ALL`/`ANY` such as `x > ALL (SELECT …)`, and a
CTE joined on a non-equality (`FROM t JOIN m ON t.v > m.a`). Write them as
`IN (SELECT …)`, as a scalar subquery inside `WHERE` or the select list
(`WHERE v > (SELECT avg(v) FROM t)`), or as a join on keys.

### What is and is not memory-bounded

`QUERY_MEMORY_BYTES` bounds the state DataFusion's operators reserve from the
pool: hash tables of joins and aggregates, sort buffers and the like. With
spill disabled, an operator that needs more fails with `Error::Limit` and its
reservations are released (tested, including that the observed peak stays
within the pool and that the disk manager cannot create temporary files).

Window operators buffer their input without reserving it. The engine wraps
the input of every `WindowAggExec` and `BoundedWindowAggExec` in a guard that
reserves each batch from the query's pool, cumulatively, until the stream
ends; before this, `count(*) OVER ()` over a join or a `UNION ALL` fan-out
held hundreds of megabytes while the pool read zero. With each batch the
guard also reserves the window columns the operator will add for its rows
(their fixed width plus a validity byte per row), before the operator builds
them, so forty `sum(i) OVER ()` columns over a large join are `Error::Limit`
at the pool rather than a gigabyte outside it. The reservation is
conservative for bounded windows, which release input as they go. A window
that needs its whole partition (`WindowAggExec`) also concatenates its
buffered input once, a copy of at most the reserved bytes that is not
itself reserved, and the state of an aggregate over a window (for example
`median`'s values) is not reserved either; neither is larger than the
window's input.

The same guard wraps every operator that has an input and fails the query
with `Error::Limit` past `MAX_BATCH_BYTES` in one batch, `MAX_PLAN_BYTES`
produced over the whole plan (fan-out of one scan through many `UNION ALL`
branches, which no join count sees) or `MAX_JOIN_ROWS` produced by all hash
and sort-merge joins (rows² output on a low-cardinality key). Batches are
measured by the slices they reference, so a projection of a sealed batch is
not charged for buffers it shares.

What remains unbounded is the transient memory of expression evaluation, which
DataFusion does not account: the batch cap applies after a batch exists, so it
limits how far an oversized intermediate propagates, not its first allocation.
The functions that could make one disproportionately large are not on the
allowlists. Operators build batches of at most `SQL_BATCH_ROWS` (256) rows; an
expression directly over a scan evaluates the snapshot's stored batches, which
are up to the capture's `batch_rows` (at most 1,024 rows). Sealed snapshot
data is accounted separately by `MAX_SNAPSHOT_BYTES` and `MAX_ENGINE_BYTES`.

### Panics

An Arrow or DataFusion kernel that panics (for example an offset overflow past
2 GiB in one array) unwinds out of the query task, which the engine reports as
`Error::Worker`; the process keeps running. That containment needs
`panic = "unwind"`: an embedding binary built with `panic = "abort"` would
abort the whole process instead.

## Read-only surface

- DDL, DML (including `COPY`) and statements (`SET`, transactions, `PREPARE`)
  are refused by DataFusion's `SQLOptions` at the logical-plan stage
  (`Error::Sql`). Only one statement per call is accepted.
- The session has no table functions (no `generate_series`/`range`), no table
  factories (so `CREATE EXTERNAL TABLE … LOCATION '/etc'` has no target even
  in principle), no file formats, and URL tables (`SELECT * FROM '/path'`) are
  never enabled. Only the registered snapshots and `snapshots` exist.
- Unknown tables, columns and functions are `Error::Sql`. Execution failures
  that are not a bound (for example a failed cast) are `Error::Query`.
- Errors never carry DataFusion's message text, which can echo literals or
  data.

## Runaway queries

Some operators produce output that is not bounded by their input, and
DataFusion 55 does not yield inside their output loops: a cross join can hold
a thread for minutes after its deadline, and dropping its stream does not stop
it promptly. The engine therefore:

1. **Rejects before execution** (`Error::Sql`) any join without equality keys
   in the optimized logical plan (cross joins, `JOIN … ON` a non-equi
   predicate, `ON true`), any recursive query, and any `CrossJoinExec`,
   `NestedLoopJoinExec`, `PiecewiseMergeJoinExec` or `RecursiveQueryExec` in
   the physical plan. This includes `EXPLAIN ANALYZE` of such a query. A
   scalar subquery that DataFusion plans as a keyless join is rejected too.
   Equi-joins (hash and sort-merge joins), including `FROM a, b WHERE a.ts =
   b.ts`, are allowed.
2. **Wraps every non-leaf operator in `CooperativeExec`** so each output batch
   consumes Tokio's cooperative budget, and **every leaf in a guard that
   yields before each batch** (see below). Without `CooperativeExec`, an equi-join on a
   low-cardinality key (for example `point_id`, which is constant within a
   snapshot) emits millions of rows per probe batch without yielding; the
   cancellation test failed before this wrapping and passes with it.
3. **Runs every query on the engine's own multi-thread runtime** of
   `SQL_WORKER_THREADS` workers. A query that does not stop occupies one of
   those threads, never the caller's or a server's.

On timeout, cancellation or a dropped `query` future, the query task is
aborted. The call waits up to `CANCEL_GRACE` for it to stop and then returns
`Error::Deadline` or `Error::Cancelled` regardless. The admission permit is
held by the task itself, so an operator that has not yet returned keeps its
slot until it does; admission then refuses new queries rather than letting
them pile up. The deadline is therefore **best-effort**: abort takes effect only
when the query task next yields, and two kinds of work never yield.

- **Planning** (parsing, logical planning, optimization, physical planning)
  runs to completion once started. The pre-planning bounds above keep it
  short; tests assert that over-cap statements are rejected within a second.
- **Work within one poll**: an operator that does an input batch's worth of
  work before yielding holds its thread for that long. A filtered equi-join,
  for example, evaluates its predicate for every candidate pair of a probe
  batch and loops over empty results without producing output. Tokio's
  cooperative budget lets a leaf deliver 128 batches before it yields, so such
  a join used to run to completion past its deadline (an expensive predicate
  kept a permit about 10 s past it). Every leaf is now wrapped to yield before
  each batch, so the abort lands within one probe batch. A probe batch is a
  stored batch of the snapshot, up to the capture's `batch_rows` (at most
  1,024 rows), when the join reads a scan directly, and at most
  `SQL_BATCH_ROWS` (256) rows when it reads another operator. The work of a
  single batch still cannot be interrupted.

Tests show cancellation, task cleanup and permit release within
`CANCEL_GRACE` for a rows² equi-join and for a filtered equi-join that matches
nothing (which fails that bound without the leaf yield); they do not prove it
for every operator.

Admission is process-wide, not per engine: `SQL_QUERIES` permits are shared by
every `SqlEngine` in the process. A query that has not stopped keeps its
permit past `Error::Deadline` and even past dropping its engine, until the
operator returns. A few hostile queries can therefore deny SQL to every
engine in the process for as long as they run; admission refuses rather than
queues, so callers see `Error::Capacity` rather than a hang.

Dropping the engine calls `shutdown_background` on its runtime: idle workers
exit at once (tested), and a worker inside a non-yielding operator exits when
that operator next returns.

## Determinism

- Without `ORDER BY`, row order is unspecified. With one partition and
  in-memory scans it is stable in practice; do not rely on it.
- Floating-point `SUM`/`AVG` may accumulate in an order different from a
  sequential fold, so results can differ in the last bits for values that are
  not exactly representable. Tests use exact binary fractions for that reason.
- `now()`, `random()`, `uuid()` and the `current_*` functions are not
  deterministic.
- `ts_local` reflects the offset captured with each sample, not timezone rules
  at query time.

## Native code and the zstd advisories

DataFusion requires `arrow-ipc`, which depends on `zstd` (C, built by
`zstd-sys`) and the pure-Rust `lz4_flex` unconditionally; `recursive_protection`
adds `psm` (via `stacker`). DataFusion also compiles `object_store`, whose local
filesystem store is reachable only through listing or URL tables, which this
session does not have. `zstd 0.13` is
affected by RUSTSEC-2026-0391, -0398 and -0403, fixed only on Arrow 60, which no
DataFusion release supports yet. `deny.toml` ignores exactly those three IDs,
tracked in [#131](https://github.com/jscott3201/rusty-haystack/issues/131).

They are unreachable from this feature: zstd is used only for Arrow IPC
(de)compression, and the SQL path never reads or writes IPC. Snapshots are
read from uncompressed Parquet by the capture reader; each query's runtime has
`DiskManagerMode::Disabled`, so no spill file is ever written or read; and
DDL, `COPY`, table factories, file formats and URL tables are all unavailable.
`no_zstd_path_is_reachable_from_the_sql_module` asserts the disabled disk
manager, the empty factory, file-format and table-function registries and the
function allowlist on the session the engine builds, and that the module never
names an IPC reader or a spill directory mode. Without the `sql` feature,
`zstd` is not in the dependency graph.

## Evidence

`cargo test -p rusty-haystack-analytics --features sql` runs, besides the
capture tests:

- `tests/sql.rs`, over real loopback captures made with `Export`. Expected
  values are an independent fold over the `HistoryReadResult.samples` that
  were served, or hand-computed literals: filters and `COUNT(*)`,
  `COUNT(col)`, `SUM`/`AVG`/`MIN`/`MAX` with NA; NaN/±Infinity/`-0.0` bits;
  empty strings, quotes and Unicode; `COUNT(*) FILTER` over Bool; a
  two-snapshot equi-join on `ts` against a nested-loop reference; New York DST
  fall-back grouped by `ts` and `ts_local`; leap-second components; lossless
  reconstruction of the source samples from the SQL columns; a rejection
  matrix of incomplete, corrupt and mismatched captures (each registers
  nothing, with a positive control); cancellation, deadline, admission
  capacity and permit release, including a filtered join that never produces
  output; the output caps; window input beyond the pool;
  DDL/DML/`COPY`/ `SET`/`CREATE EXTERNAL TABLE`/URL-table rejection with no
  file written; oversize SQL text and deep nesting; six left-deep chain shapes
  at the token cap (run, unoptimized, without overflow) and one term past it
  (`Error::Limit`); joins, joins inside parentheses, comma joins, `UNION ALL`,
  `EXCEPT`, CTEs, subquery relations and CTE doubling at and one past each
  statement bound, rejected within a second; `CUBE`/`ROLLUP`/`GROUPING
  SETS`/`TABLESAMPLE`/locking clauses and non-query statements; `||`, every
  string-building function and every spelling of `substr` rejected, with
  allowed string functions as a control; every regex function, regex operator,
  `SIMILAR TO`, `LIKE` and `ILIKE` rejected, with `starts_with` and the
  literal tests as a control; the `approx_percentile_cont` abort and the other
  aggregates off the allowlist rejected, with allowed aggregates, aliases and
  window functions as controls; variable-width window expressions rejected and
  wide fixed-width windows over a large join `Error::Limit` at the pool;
  cross, non-equi and recursive query rejection; identity, the `snapshots`
  table and repeated results after the capture is rewritten and deleted;
  table-name validation and the snapshot count bound.
- Unit tests in `src/sql/tests.rs`: engine drop stops every runtime thread;
  cancellation, deadline and a dropped future return the runtime's alive tasks
  to zero and the permits to `SQL_QUERIES`; a pool overrun is `Error::Limit`
  with the observed peak within `QUERY_MEMORY_BYTES`, reservations released and
  temporary files disabled; engines default to `MAX_JOIN_ROWS` and
  `QUERY_MEMORY_BYTES`; through the public query path, rows² joins return
  `Error::Limit` from an engine whose join-row cap is lowered (checked at and
  one probe row past it) or whose pool is (at the production caps these joins
  do seconds of unoptimized work first, and raced `QUERY_TIMEOUT` on a starved
  CPU); the batch-byte, plan-byte and join-row guards at
  and past each cap, slice accounting, and window-input reservation and its
  release, over synthetic streams; every operator with an input guarded and
  cooperative in real plans, with each window's output width reserved; the
  physical plan is built from the checked optimized plan; the session
  registers exactly `ALLOWED_FUNCTIONS`, `ALLOWED_AGGREGATES` and
  `ALLOWED_WINDOW_FUNCTIONS`; hash-valid captures that fail only while draining
  (including a coverage contradiction visible only after the last row) never
  register; out-of-range timestamps; the zstd reachability evidence above.

CI runs these in the `core-features` job, with Clippy `-D warnings` for the
feature, and `./.agents/gate.sh` mirrors it. They run on Ubuntu only; on
`main`, macOS and Windows compile the feature (`cargo check`) without running
its tests.

## Not claimed

- **No live provider pushdown.** Queries read sealed captures only.
- **No SQL server or remote access.** Nothing listens; callers choose the
  directories registered, and remote users get no filesystem access through
  SQL.
- **No spill.** Queries that need more than the pool fail.
- **No Python, CLI, MCP or server surface.**
- **No memory bound for expression temporaries.** Expression evaluation
  allocates outside the pool; the allowlist and the batch cap limit how large
  and how far, not the first allocation.
- **No cancellation of planning or of work within one poll**, and no
  guaranteed prompt stop for operators that do not yield (see above). Planning
  time is bounded only by the pre-planning caps.
- **No per-engine admission.** Permits are process-wide, and a stuck query
  holds one past its deadline and past its engine's drop.
- **No string building in SQL**; concatenate after the query.
- **No containment of panics under `panic = "abort"`.**
- **No cross-snapshot consistency.** Each snapshot is one capture of one
  point; joining two says nothing about their relative capture times.
- **No qualification on macOS or Windows CI**: those jobs only compile the
  feature. Local runs were on macOS.
