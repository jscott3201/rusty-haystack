# SQL over committed snapshots

The optional `sql` feature of `rusty-haystack-analytics` (module
`haystack_analytics::sql`) runs bounded, read-only SQL with
[Apache DataFusion](https://datafusion.apache.org/) over
[Parquet history captures](parquet-history.md) that have been fully validated
and sealed in memory. It is a Rust library profile only: there is no SQL
server, HTTP route, CLI command, MCP tool or Python binding, and it does not
push queries down to a live history provider.

Without the feature nothing changes: no DataFusion code is compiled and the
dependency tree is the same as before. With it, the crate depends on
`datafusion` (pinned exactly, default features off, only `sql` and
`recursive_protection`; not DataFusion's `parquet` feature) and the `arrow`
umbrella crate, from the same exact Arrow/Parquet family the capture format
already uses. No other workspace crate gains a dependency.

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
- `SqlEngine::query(sql, QueryOptions { timeout, cancellation })` returns a
  complete `QueryOutput { schema, batches, sources, peak_memory_bytes }`.
  `sources` lists the snapshot identity (table, export id, manifest SHA-256)
  of every table the logical plan scans, including through CTEs and
  subqueries; reading only the `snapshots` table contributes none.

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
| Parser recursion (`datafusion.sql_parser.recursion_limit`) | `SQL_RECURSION_LIMIT` | `Error::Sql` |
| Snapshots per engine | `MAX_SNAPSHOTS` | `Error::Limit` |
| Sealed Arrow bytes per snapshot / per engine | `MAX_SNAPSHOT_BYTES`, `MAX_ENGINE_BYTES` | `Error::Limit` at registration |
| Execution batch size | `SQL_BATCH_ROWS` | — |
| Table name length | `MAX_TABLE_NAME_BYTES` | `Error::Options` |
| Scalar functions removed from the session | `REMOVED_FUNCTIONS` | `Error::Sql` |

The session also fixes `target_partitions = 1`, turns every repartitioning
option off, keeps `information_schema` off and disables recursive CTEs. The
capture's own bounds (`MAX_ROWS` per capture and the `Limits` of
[Parquet history captures](parquet-history.md#bounds)) apply first.

### What is and is not memory-bounded

`QUERY_MEMORY_BYTES` bounds the state DataFusion's operators reserve from the
pool: hash tables of joins and aggregates, sort buffers and the like. With
spill disabled, an operator that needs more fails with `Error::Limit` and its
reservations are released (tested, including that the observed peak stays
within the pool and that the disk manager cannot create temporary files).

It does not bound transient buffers of expression evaluation, which DataFusion
does not account in the pool. The direct amplifiers `repeat`, `lpad` and `rpad`
(one call turns a short literal into up to 2 GiB) are removed, and the output
caps bound what is returned, but nested string expressions such as repeated
`replace` or `concat` can still build large intermediate arrays, limited only by
Arrow's per-array offset range. Sealed snapshot data is accounted separately by
`MAX_SNAPSHOT_BYTES` and `MAX_ENGINE_BYTES`.

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
   consumes Tokio's cooperative budget. Without it, an equi-join on a
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
them pile up. The deadline is therefore **best-effort for operators that do
not yield**: tests show prompt cancellation, task cleanup and permit release
for scan- and join-driven queries; they do not prove it for every operator.
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
removed functions on the session the engine builds, and that the module never
names an IPC reader or a spill directory mode. Without the `sql` feature,
`zstd` is not in the dependency graph.

## Evidence

`cargo test -p rusty-haystack-analytics --features sql` runs, besides the
capture tests:

- `tests/sql.rs`, over real loopback captures made with `Export`. Expected
  values are an independent fold over the `HistoryReadResult.samples` that were
  served, or hand-computed literals: filters and `COUNT(*)`, `COUNT(col)`,
  `SUM`/`AVG`/`MIN`/`MAX` with NA; NaN/±Infinity/`-0.0` bits; empty strings,
  quotes and Unicode; `COUNT(*) FILTER` over Bool; a two-snapshot equi-join on
  `ts` against a nested-loop reference; New York DST fall-back grouped by `ts`
  and `ts_local`; leap-second components; lossless reconstruction of the source
  samples from the SQL columns; a rejection matrix of incomplete, corrupt and
  mismatched captures (each registers nothing, with a positive control);
  cancellation, deadline, admission capacity and permit release; memory and
  output caps; DDL/DML/`COPY`/`SET`/`CREATE EXTERNAL TABLE`/URL-table and
  removed-function rejection with no file written; oversize SQL text and deep
  nesting; cross, non-equi and recursive query rejection; identity, the
  `snapshots` table and repeated results after the capture is rewritten and
  deleted; table-name validation and the snapshot count bound.
- Unit tests in `src/sql/tests.rs`: engine drop stops every runtime thread;
  cancellation, deadline and a dropped future return the runtime's alive tasks
  to zero and the permits to `SQL_QUERIES`; a pool overrun is `Error::Limit`
  with the observed peak within `QUERY_MEMORY_BYTES`, reservations released and
  temporary files disabled; hash-valid captures that fail only while draining
  (including a coverage contradiction visible only after the last row) never
  register; out-of-range timestamps; the zstd reachability evidence above.

CI runs these in the `core-features` job, with Clippy `-D warnings` for the
feature, and `./.agents/gate.sh` mirrors it. They run on Ubuntu only.

## Not claimed

- **No live provider pushdown.** Queries read sealed captures only.
- **No SQL server or remote access.** Nothing listens; callers choose the
  directories registered, and remote users get no filesystem access through
  SQL.
- **No spill.** Queries that need more than the pool fail.
- **No Python, CLI, MCP or server surface.**
- **No complete memory bound for expression evaluation**, and no guaranteed
  prompt stop for operators that do not yield (see above).
- **No cross-snapshot consistency.** Each snapshot is one capture of one
  point; joining two says nothing about their relative capture times.
- **No qualification on macOS or Windows CI**; local runs were on macOS.
