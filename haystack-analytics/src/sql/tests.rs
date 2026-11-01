//! Engine internals: runtime shutdown, task cleanup, memory peak and spill,
//! and registration of hash-valid captures the reader rejects only while
//! draining. Public behaviour over real loopback captures is in `tests/sql.rs`.
use super::*;
use crate::{Limits, checkpoint::Checkpoints, tests::support::*, writer::Writer};
use haystack_core::{
    codecs::history::{HistoryKind, HistoryReadHeader, HistoryReadResult, HistorySample},
    kinds::{Kind, Number},
};

/// Query admission is process-wide; these tests share one process.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn header(value: &HistoryReadResult) -> HistoryReadHeader {
    HistoryReadHeader {
        metadata: value.metadata.clone(),
        terminal: value.terminal,
        returned_count: value.samples.len() as u64,
    }
}
/// Writes `batches` with the real writer; `verify` off forges hash-valid
/// parts the reader rejects only when it decodes them.
fn write(
    path: &std::path::Path,
    header: &HistoryReadHeader,
    batches: Vec<arrow_array::RecordBatch>,
    verify: bool,
) {
    let mut writer = Writer::new(
        path.to_owned(),
        header,
        Limits::default(),
        uuid::Uuid::new_v4().to_string(),
        Checkpoints::default(),
    )
    .unwrap();
    writer.verify_parts = verify;
    for batch in batches {
        writer.write(batch).unwrap();
    }
    writer.prepare().unwrap();
    writer.publish(|| Ok(())).unwrap();
}
fn numbers(rows: usize) -> HistoryReadResult {
    let mut value = result(rows, 0);
    value.metadata.schema.kind = HistoryKind::Number;
    value.metadata.schema.unit = Some("°F".into());
    for (i, sample) in value.samples.iter_mut().enumerate() {
        sample.val = Kind::Number(Number::unitless(i as f64));
    }
    value
}
fn written(path: &std::path::Path, value: &HistoryReadResult) {
    let header = header(value);
    let batches = value
        .samples
        .chunks(Limits::default().batch_rows)
        .map(|chunk| crate::format::batch(&header, chunk, &Limits::default()).unwrap())
        .collect();
    write(path, &header, batches, true);
}
async fn engine(rows: usize) -> (tempfile::TempDir, SqlEngine) {
    let dir = tempfile::tempdir().unwrap();
    written(&dir.path().join("t"), &numbers(rows));
    let mut builder = SqlEngine::builder();
    builder.register("t", dir.path().join("t")).await.unwrap();
    (dir, builder.build().unwrap())
}
/// A self equi-join on the constant `point_id`: rows² join output that is
/// scan-driven, so it honours cancellation, and runs for seconds.
const SLOW: &str = "SELECT a.utc_seconds % 7 AS k, count(*) FROM t a JOIN t b \
                    ON a.point_id = b.point_id GROUP BY k";
async fn settle(engine: &SqlEngine) -> usize {
    let metrics = engine.runtime.as_ref().unwrap().metrics();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while metrics.num_alive_tasks() > 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    metrics.num_alive_tasks()
}

#[tokio::test]
async fn engine_drop_stops_every_runtime_thread() {
    let _serial = SERIAL.lock().await;
    let (_dir, engine) = engine(10).await;
    let output = engine
        .query("SELECT count(*) FROM t", QueryOptions::default())
        .await
        .unwrap();
    assert_eq!(output.batches[0].num_rows(), 1);
    let threads = engine.threads.clone();
    assert!(threads.load(Ordering::SeqCst) >= SQL_WORKER_THREADS);
    drop(engine);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while threads.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(threads.load(Ordering::SeqCst), 0, "runtime threads leaked");
}

#[tokio::test]
async fn cancellation_deadline_and_abandonment_return_tasks_and_permits() {
    let _serial = SERIAL.lock().await;
    let (_dir, engine) = engine(4000).await;
    assert_eq!(settle(&engine).await, 0);
    assert_eq!(QUERIES.available_permits(), SQL_QUERIES);

    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    });
    let started = std::time::Instant::now();
    let options = QueryOptions {
        cancellation: token,
        ..Default::default()
    };
    assert_eq!(
        engine.query(SLOW, options).await.unwrap_err(),
        Error::Cancelled
    );
    assert!(started.elapsed() < Duration::from_millis(200) + CANCEL_GRACE);
    // The aborted task was joined within the grace: its permit is back.
    assert_eq!(QUERIES.available_permits(), SQL_QUERIES);
    assert_eq!(settle(&engine).await, 0, "query tasks left running");

    let started = std::time::Instant::now();
    let options = QueryOptions {
        timeout: Duration::from_millis(200),
        ..Default::default()
    };
    assert_eq!(
        engine.query(SLOW, options).await.unwrap_err(),
        Error::Deadline
    );
    assert!(started.elapsed() < Duration::from_millis(200) + CANCEL_GRACE);
    assert_eq!(QUERIES.available_permits(), SQL_QUERIES);
    assert_eq!(settle(&engine).await, 0, "query tasks left running");
    // Abandoning the call (dropping its future) aborts the query too.
    let abandoned = tokio::time::timeout(
        Duration::from_millis(200),
        engine.query(SLOW, Default::default()),
    );
    assert!(abandoned.await.is_err());
    assert_eq!(settle(&engine).await, 0, "abandoned query left running");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while QUERIES.available_permits() < SQL_QUERIES && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(QUERIES.available_permits(), SQL_QUERIES);
}

#[tokio::test]
async fn memory_limit_is_a_limit_error_within_the_pool_and_never_spills() {
    let _serial = SERIAL.lock().await;
    let (_dir, engine) = engine(4000).await;
    let inner = engine.inner.clone();
    let pool = Arc::new(PeakPool::new(QUERY_MEMORY_BYTES));
    let context = session(&inner, pool.clone()).unwrap();
    assert!(!context.runtime_env().disk_manager.tmp_files_enabled());
    drop(context);
    // 16 million distinct groups, then a sort: far beyond the pool, and
    // spilling is the only way DataFusion could continue.
    let sql = "SELECT a.utc_seconds, b.utc_seconds, count(*) FROM t a JOIN t b \
               ON a.point_id = b.point_id GROUP BY 1, 2 ORDER BY 1, 2";
    let error = engine
        .runtime
        .as_ref()
        .unwrap()
        .spawn({
            let pool = pool.clone();
            async move { execute(&inner, sql, pool).await }
        })
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error, Error::Limit);
    assert!(pool.peak() > 0);
    assert!(
        pool.peak() <= QUERY_MEMORY_BYTES,
        "peak {} over the pool",
        pool.peak()
    );
    assert_eq!(pool.reserved(), 0, "reservations released");
    // The public path reports the same bound.
    assert_eq!(
        engine
            .query(sql, QueryOptions::default())
            .await
            .unwrap_err(),
        Error::Limit
    );
}

#[tokio::test]
async fn successful_query_reports_peak_within_the_pool() {
    let _serial = SERIAL.lock().await;
    let (_dir, engine) = engine(1000).await;
    let output = engine
        .query(
            "SELECT utc_seconds FROM t ORDER BY number_value DESC",
            QueryOptions::default(),
        )
        .await
        .unwrap();
    assert!(output.pool_peak_bytes > 0);
    assert!(output.pool_peak_bytes <= QUERY_MEMORY_BYTES);
}

/// Hash-valid parts and headers the reader rejects only while decoding, or
/// only after the last row: registration must drain to completion, so none
/// of these becomes a table.
#[tokio::test]
async fn registration_rejects_captures_that_fail_only_while_draining() {
    use arrow_array::{ArrayRef, BooleanArray, Int64Array, RecordBatch, StringArray};
    let value = result(3, 0);
    let header = header(&value);
    let good = crate::format::batch(&header, &value.samples, &Limits::default()).unwrap();
    let mut forged: Vec<(&str, HistoryReadHeader, RecordBatch)> = Vec::new();
    for (name, column, array) in [
        (
            "wrong_point",
            0,
            Arc::new(StringArray::from(vec!["wrong", "point", "point"])) as ArrayRef,
        ),
        (
            "out_of_order",
            1,
            Arc::new(Int64Array::from(vec![1717200002, 1717200000, 1717200001])),
        ),
        (
            "na_with_value",
            4,
            Arc::new(BooleanArray::from(vec![false, true, true])),
        ),
        (
            "wrong_kind",
            5,
            Arc::new(BooleanArray::from(vec![Some(true), None, None])),
        ),
    ] {
        let mut columns = good.columns().to_vec();
        columns[column] = array;
        let batch = RecordBatch::try_new(good.schema(), columns).unwrap();
        forged.push((name, header.clone(), batch));
    }
    // Contradictory coverage is detectable only after every row decodes.
    let mut coverage = header.clone();
    coverage.metadata.coverage.retained_count = 4;
    forged.push(("coverage", coverage, good.clone()));

    let dir = tempfile::tempdir().unwrap();
    written(&dir.path().join("good"), &value);
    let mut builder = SqlEngine::builder();
    builder
        .register("good", dir.path().join("good"))
        .await
        .unwrap();
    for (name, header, batch) in forged {
        let path = dir.path().join(name);
        write(&path, &header, vec![batch], false);
        assert!(
            builder.register(name, &path).await.is_err(),
            "{name} registered"
        );
        assert_eq!(builder.snapshots().len(), 1, "{name}");
        assert_eq!(builder.bytes, builder.snapshots[0].bytes, "{name}");
    }
}

#[test]
fn timestamps_outside_i64_nanoseconds_are_a_limit() {
    assert_eq!(
        table::epoch_nanos(1_483_228_799, 1_000_000_001).unwrap(),
        1_483_228_800_000_000_001
    );
    assert_eq!(
        table::epoch_nanos(i64::MAX / 1_000_000_000 + 1, 0),
        Err(Error::Limit)
    );
    assert_eq!(
        table::epoch_nanos(i64::MIN / 1_000_000_000 - 1, 0),
        Err(Error::Limit)
    );
    let mut value = numbers(1);
    value.samples[0] = HistorySample {
        ts: date("2263-01-01T00:00:00Z UTC"),
        val: Kind::NA,
    };
    assert_eq!(
        table::sample_batch(&header(&value), &value.samples).unwrap_err(),
        Error::Limit
    );
}

/// The zstd/zstd-safe advisories (see `deny.toml`) are reachable only
/// through Arrow IPC decompression: DataFusion spill files and Arrow-format
/// tables. Spill is disabled, DDL, COPY and URL tables are refused, no table
/// factories or file formats are registered, and this module never names an
/// IPC reader.
#[test]
fn no_zstd_path_is_reachable_from_the_sql_module() {
    for source in [include_str!("mod.rs"), include_str!("table.rs")] {
        for forbidden in [
            "ipc::",
            "arrow_ipc",
            "enable_url_table",
            "DiskManagerMode::OsTmpDirectory",
            "DiskManagerMode::Directories",
            "ListingTable",
            "register_arrow",
            "read_arrow",
        ] {
            assert!(!source.contains(forbidden), "{forbidden}");
        }
    }
    let inner = Inner {
        snapshots: vec![],
        provenance: Arc::new(table::Provenance::new(&[]).unwrap()),
    };
    let context = session(&inner, Arc::new(PeakPool::new(QUERY_MEMORY_BYTES))).unwrap();
    assert!(!context.runtime_env().disk_manager.tmp_files_enabled());
    let state = context.state();
    assert!(state.table_functions().is_empty());
    assert!(state.table_factories().is_empty());
    // Exactly the allowlist is registered, and every entry of it still
    // exists in DataFusion (a rename would otherwise silently drop it).
    let mut registered: Vec<&str> = state
        .scalar_functions()
        .values()
        .map(|f| f.name())
        .collect();
    registered.sort_unstable();
    registered.dedup();
    let mut allowed = ALLOWED_FUNCTIONS.to_vec();
    allowed.sort_unstable();
    assert_eq!(registered, allowed);
    let mut registered: Vec<&str> = state
        .aggregate_functions()
        .values()
        .map(|f| f.name())
        .collect();
    registered.sort_unstable();
    registered.dedup();
    let mut allowed = ALLOWED_AGGREGATES.to_vec();
    allowed.sort_unstable();
    assert_eq!(registered, allowed);
    let mut registered: Vec<&str> = state
        .window_functions()
        .values()
        .map(|f| f.name())
        .collect();
    registered.sort_unstable();
    registered.dedup();
    let mut allowed = ALLOWED_WINDOW_FUNCTIONS.to_vec();
    allowed.sort_unstable();
    assert_eq!(registered, allowed);
    for name in [
        "approx_percentile_cont",
        "approx_percentile_cont_with_weight",
        "array_agg",
        "string_agg",
        "nth_value",
        "grouping",
    ] {
        assert!(!state.aggregate_functions().contains_key(name), "{name}");
    }
    for name in [
        "concat",
        "concat_ws",
        "replace",
        "regexp_replace",
        "overlay",
        "translate",
        "repeat",
        "lpad",
        "rpad",
        "to_hex",
        "encode",
        "to_char",
        "date_format",
        "arrow_cast",
        "arrow_try_cast",
        "cast_to_type",
        "levenshtein",
        "regexp_like",
        "regexp_count",
        "regexp_instr",
        "regexp_match",
        "substr",
    ] {
        assert!(!state.scalar_functions().contains_key(name), "{name}");
    }
    for op in [
        Operator::StringConcat,
        Operator::RegexMatch,
        Operator::RegexIMatch,
        Operator::RegexNotMatch,
        Operator::RegexNotIMatch,
        Operator::LikeMatch,
        Operator::ILikeMatch,
        Operator::NotLikeMatch,
        Operator::NotILikeMatch,
    ] {
        assert!(!ALLOWED_OPERATORS.contains(&op), "{op}");
    }
    let options = state.config().options();
    assert_eq!(options.execution.target_partitions, 1);
    assert_eq!(options.execution.batch_size.get(), SQL_BATCH_ROWS);
    assert_eq!(
        options.sql_parser.recursion_limit.get(),
        SQL_RECURSION_LIMIT
    );
    assert_eq!(
        options.execution.time_zone.as_deref(),
        Some(SESSION_TIME_ZONE)
    );
    assert!(!options.catalog.information_schema);
    assert!(!options.execution.enable_recursive_ctes);
    assert!(!options.optimizer.repartition_joins);
}

/// The physical plan is built from the one optimized plan that was
/// checked: `SessionState::create_physical_plan` and
/// `DataFrame::into_optimized_plan` both optimize again, and that third,
/// unchecked pass is what the physical plan used to be built from.
#[test]
fn the_physical_plan_is_built_from_the_checked_optimized_plan() {
    let source = include_str!("mod.rs");
    let execute = &source[source.find("async fn execute(").unwrap()..];
    let execute = &execute[..execute.find("\n}\n").unwrap()];
    for forbidden in ["into_optimized_plan", "execute_logical_plan", "DataFrame"] {
        assert!(!source.contains(forbidden), "{forbidden}");
    }
    assert_eq!(execute.matches(".optimize(").count(), 1);
    assert_eq!(execute.matches("create_physical_plan(").count(), 1);
    let optimize = execute.find(".optimize(").unwrap();
    let checked = execute
        .find("check_expressions(&optimized, Stage::Optimized)")
        .unwrap();
    let keyless = execute.find("reject_keyless_joins(&optimized)").unwrap();
    let planner = execute.find(".query_planner()").unwrap();
    let physical = execute
        .find("create_physical_plan(&optimized, &state)")
        .unwrap();
    assert!(optimize < checked && checked < keyless && keyless < planner && planner < physical);
    assert!(execute[physical..].contains("reject_runaway(&physical)"));
}

/// One `Utf8` column `s` of `rows` strings of `len` bytes each.
fn text_batch(rows: usize, len: usize) -> RecordBatch {
    let value = "x".repeat(len);
    let column = arrow::array::StringArray::from_iter_values((0..rows).map(|_| value.as_str()));
    RecordBatch::try_from_iter([("s", Arc::new(column) as arrow::array::ArrayRef)]).unwrap()
}
/// Drains `watch` over `batches` with its own budget and a pool of
/// `pool_bytes`, returning the rows that passed and the pool left reserved.
async fn drain(
    batches: Vec<RecordBatch>,
    watch: Watch,
    budget: &Arc<Budget>,
    pool_bytes: usize,
) -> (datafusion::error::Result<usize>, usize) {
    let schema = batches[0].schema();
    let source =
        datafusion::datasource::memory::MemorySourceConfig::try_new_exec(&[batches], schema, None)
            .unwrap();
    let guard = GuardExec::wrap(source, watch, budget);
    let pool: Arc<dyn MemoryPool> = Arc::new(GreedyMemoryPool::new(pool_bytes));
    let runtime = RuntimeEnvBuilder::new()
        .with_memory_pool(pool.clone())
        .build_arc()
        .unwrap();
    let context = Arc::new(TaskContext::default().with_runtime(runtime));
    let result = async {
        let mut stream = guard.execute(0, context)?;
        let mut rows = 0;
        while let Some(batch) = stream.next().await {
            rows += batch?.num_rows();
        }
        Ok(rows)
    }
    .await;
    (result, pool.reserved())
}
fn is_limit<T: std::fmt::Debug>(result: &datafusion::error::Result<T>) -> bool {
    match result {
        Err(error) => {
            matches!(error.find_root(), DataFusionError::ResourcesExhausted(_))
                && classify(error) == Error::Limit
        }
        Ok(_) => false,
    }
}

#[tokio::test]
async fn guard_caps_batch_bytes_plan_bytes_and_join_rows() {
    let output = Watch::Output { join: false };
    let pool = 1 << 30;
    // One batch over MAX_BATCH_BYTES fails; one just under passes.
    let budget = Arc::new(Budget::default());
    let (over, _) = drain(
        vec![text_batch(1, MAX_BATCH_BYTES + 1)],
        output,
        &budget,
        pool,
    )
    .await;
    assert!(is_limit(&over), "{over:?}");
    let budget = Arc::new(Budget::default());
    let (under, _) = drain(
        vec![text_batch(1, MAX_BATCH_BYTES - 1024)],
        output,
        &budget,
        pool,
    )
    .await;
    assert_eq!(under.unwrap(), 1);
    // A slice is charged for what it references, not the buffer it shares:
    // of a batch of 1.5 caps, one half-row slice passes and the whole fails.
    let wide = text_batch(2, MAX_BATCH_BYTES / 4 * 3);
    let budget = Arc::new(Budget::default());
    let (half, _) = drain(vec![wide.slice(0, 1)], output, &budget, pool).await;
    assert_eq!(half.unwrap(), 1);
    let (whole, _) = drain(vec![wide], output, &budget, pool).await;
    assert!(is_limit(&whole), "{whole:?}");

    // MAX_PLAN_BYTES is summed over every guard of a plan.
    let small = text_batch(4, 16);
    let bytes = batch_bytes(&small);
    let budget = Arc::new(Budget::default());
    budget
        .plan_bytes
        .store(MAX_PLAN_BYTES - bytes, Ordering::SeqCst);
    let (at, _) = drain(vec![small.clone()], output, &budget, pool).await;
    assert_eq!(at.unwrap(), 4);
    let (past, _) = drain(vec![small.clone()], output, &budget, pool).await;
    assert!(is_limit(&past), "{past:?}");

    // MAX_JOIN_ROWS counts join output only.
    let budget = Arc::new(Budget::default());
    budget.join_rows.store(MAX_JOIN_ROWS - 4, Ordering::SeqCst);
    let (at, _) = drain(
        vec![small.clone()],
        Watch::Output { join: true },
        &budget,
        pool,
    )
    .await;
    assert_eq!(at.unwrap(), 4);
    let (other, _) = drain(vec![small.clone()], output, &budget, pool).await;
    assert_eq!(other.unwrap(), 4);
    let (past, _) = drain(vec![small], Watch::Output { join: true }, &budget, pool).await;
    assert!(is_limit(&past), "{past:?}");
}

#[tokio::test]
async fn window_input_is_reserved_cumulatively_and_released() {
    let batches = || (0..8).map(|_| text_batch(64, 4096)).collect::<Vec<_>>();
    let total: usize = batches().iter().map(batch_bytes).sum();
    let budget = Arc::new(Budget::default());
    // Within the pool: everything passes, and the reservation is released
    // when the stream ends.
    let input = Watch::WindowInput { row_bytes: 0 };
    let (rows, reserved) = drain(batches(), input, &budget, total).await;
    assert_eq!(rows.unwrap(), 8 * 64);
    assert_eq!(reserved, 0);
    // One byte short: the last batch cannot be reserved.
    let (rows, reserved) = drain(batches(), input, &budget, total - 1).await;
    assert!(is_limit(&rows), "{rows:?}");
    assert_eq!(reserved, 0);
    // The window columns are reserved per row, before they are built: the
    // same input with 9 bytes per row of output needs that much more.
    let input = Watch::WindowInput { row_bytes: 9 };
    let (rows, _) = drain(batches(), input, &budget, total + 9 * 8 * 64).await;
    assert_eq!(rows.unwrap(), 8 * 64);
    let (rows, reserved) = drain(batches(), input, &budget, total + 9 * 8 * 64 - 1).await;
    assert!(is_limit(&rows), "{rows:?}");
    assert_eq!(reserved, 0);
    // A window input guard does not charge the plan budget.
    assert_eq!(budget.plan_bytes.load(Ordering::SeqCst), 0);
}

/// Every operator with inputs is guarded and cooperative, every leaf yields
/// through a guard, a join's guard counts rows and a window operator reads
/// through a reserving guard.
#[tokio::test]
async fn physical_plans_are_guarded_at_every_operator() {
    let _serial = SERIAL.lock().await;
    let (_dir, engine) = engine(10).await;
    let inner = engine.inner.clone();
    let context = session(&inner, Arc::new(PeakPool::new(QUERY_MEMORY_BYTES))).unwrap();
    let state = context.state();
    for sql in [
        "SELECT a.ts, count(*) OVER () FROM t a JOIN t b ON a.ts = b.ts",
        "SELECT ts, row_number() OVER (ORDER BY ts) FROM t",
    ] {
        let plan = state.create_logical_plan(sql).await.unwrap();
        let plan = state.optimize(&plan).unwrap();
        let plan = state
            .query_planner()
            .create_physical_plan(&plan, &state)
            .await
            .unwrap();
        let plan = guarded(plan, &Arc::new(Budget::default())).unwrap();
        let (mut joins, mut windows) = (0, 0);
        let mut pending = vec![(plan, None::<Arc<dyn ExecutionPlan>>)];
        while let Some((node, parent)) = pending.pop() {
            let any: &dyn Any = node.as_ref();
            let parent_guard = parent
                .as_ref()
                .and_then(|p| (p.as_ref() as &dyn Any).downcast_ref::<GuardExec>());
            if !node.children().is_empty() && !any.is::<GuardExec>() && !any.is::<CooperativeExec>()
            {
                let guard = parent_guard.expect("operator with inputs is not guarded");
                assert_eq!(
                    guard.watch,
                    Watch::Output {
                        join: is_join(node.as_ref())
                    }
                );
                joins += usize::from(is_join(node.as_ref()));
            }
            if is_window(node.as_ref()) {
                windows += 1;
                for child in node.children() {
                    let guard = (child.as_ref() as &dyn Any).downcast_ref::<GuardExec>();
                    // One Int64 or UInt64 window column: 8 bytes and a
                    // validity bit, rounded up.
                    assert_eq!(
                        guard.map(|g| g.watch),
                        Some(Watch::WindowInput { row_bytes: 9 }),
                        "{sql}"
                    );
                }
            }
            if node.children().is_empty() {
                assert_eq!(parent_guard.map(|g| g.watch), Some(Watch::Leaf), "{sql}");
            }
            let leaf_guard = (node.as_ref() as &dyn Any)
                .downcast_ref::<GuardExec>()
                .is_some_and(|g| g.watch == Watch::Leaf);
            if any.is::<GuardExec>() && !leaf_guard {
                let grand = parent.as_ref().map(|p| p.as_ref() as &dyn Any);
                assert!(
                    grand.is_some_and(
                        |p| p.is::<CooperativeExec>() || is_window(parent.as_deref().unwrap())
                    ),
                    "guard without CooperativeExec above it"
                );
            }
            for child in node.children() {
                pending.push((child.clone(), Some(node.clone())));
            }
        }
        assert_eq!(windows, 1, "{sql}");
        assert_eq!(joins, usize::from(sql.contains("JOIN")), "{sql}");
    }
}
