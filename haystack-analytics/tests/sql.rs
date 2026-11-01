//! Snapshot SQL over real loopback captures. Every expected value is an
//! independent fold over the source `HistoryReadResult.samples` that was
//! served to the capture, or a hand-computed literal; never over the
//! `Dataset` or Arrow output under test.
mod support;
use arrow::{
    array::{
        Array, BooleanArray, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
        TimestampNanosecondArray, UInt32Array, UInt64Array,
    },
    compute::concat_batches,
};
use haystack_analytics::{
    Error, Export, ExportOptions, Limits, MANIFEST_NAME,
    sql::{
        CANCEL_GRACE, MAX_OUTPUT_ROWS, MAX_SNAPSHOTS, MAX_SQL_BYTES, MAX_SQL_CTES, MAX_SQL_JOINS,
        MAX_SQL_RELATIONS, MAX_SQL_SET_OPERATIONS, MAX_SQL_TOKENS, QUERY_MEMORY_BYTES,
        QUERY_TIMEOUT, QueryOptions, QueryOutput, SNAPSHOTS_TABLE, SQL_RECURSION_LIMIT, SqlEngine,
    },
};
use haystack_client::history::{HistoryStreamLimits, HistoryStreamOptions};
use haystack_core::{
    codecs::history::*,
    kinds::{Kind, Number},
};
use std::{path::Path, time::Duration};
use support::*;
use tokio_util::sync::CancellationToken;

/// Query admission and export admission are process-wide; one test at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Captures `value` over loopback HTTP, one row group of `part_rows` per part.
async fn capture(value: &HistoryReadResult, dir: &Path, part_rows: usize) {
    let server = Server::bytes(encoded(value), vec![113, 1, 4096], "").await;
    let client = client(&server.url).await;
    let session = client
        .his_read_stream(
            &HistoryReadRequest {
                id: value.metadata.id.clone(),
                range: value.metadata.requested_range.clone(),
            },
            HistoryStreamOptions {
                limits: HistoryStreamLimits {
                    batch_rows: 64,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut export = Export::start(
        session,
        dir,
        ExportOptions {
            limits: Limits {
                batch_rows: part_rows,
                groups_per_part: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    export.wait().await.unwrap();
    server.finish().await;
}
/// A point of `kind` (and `unit`) whose samples are `values`, one second
/// apart from 2024-06-01T00:00:00Z.
fn point(id: &str, kind: HistoryKind, unit: Option<&str>, values: Vec<Kind>) -> HistoryReadResult {
    let mut value = result(values.len(), 0);
    value.metadata.id = id.into();
    value.metadata.schema.kind = kind;
    value.metadata.schema.unit = unit.map(Into::into);
    for (sample, v) in value.samples.iter_mut().zip(values) {
        sample.val = v;
    }
    value
}
fn number(v: f64) -> Kind {
    Kind::Number(Number::unitless(v))
}
fn query_options() -> QueryOptions {
    QueryOptions::default()
}
async fn engine(tables: &[(&str, &Path)]) -> SqlEngine {
    let mut builder = SqlEngine::builder();
    for (name, path) in tables {
        builder.register(name, path).await.unwrap();
    }
    builder.build().unwrap()
}
async fn run(engine: &SqlEngine, sql: &str) -> RecordBatch {
    let output = engine.query(sql, query_options()).await.unwrap();
    all(&output)
}
fn all(output: &QueryOutput) -> RecordBatch {
    concat_batches(&output.schema, &output.batches).unwrap()
}
fn col<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> &'a T {
    batch
        .column_by_name(name)
        .unwrap_or_else(|| panic!("{name}"))
        .as_any()
        .downcast_ref()
        .unwrap_or_else(|| panic!("{name} type"))
}
fn i64s(batch: &RecordBatch, name: &str) -> Vec<Option<i64>> {
    let a = col::<Int64Array>(batch, name);
    (0..a.len())
        .map(|i| a.is_valid(i).then(|| a.value(i)))
        .collect()
}
fn f64s(batch: &RecordBatch, name: &str) -> Vec<Option<u64>> {
    let a = col::<Float64Array>(batch, name);
    (0..a.len())
        .map(|i| a.is_valid(i).then(|| a.value(i).to_bits()))
        .collect()
}
fn strings(batch: &RecordBatch, name: &str) -> Vec<Option<String>> {
    let a = col::<StringArray>(batch, name);
    (0..a.len())
        .map(|i| a.is_valid(i).then(|| a.value(i).to_owned()))
        .collect()
}
fn value_bits(kind: &Kind) -> Option<u64> {
    match kind {
        Kind::Number(n) => Some(n.val.to_bits()),
        _ => None,
    }
}

#[tokio::test]
async fn filter_and_aggregates_match_an_independent_fold_with_na() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    // Small fixture with hand-computed literals.
    let small = point(
        "small",
        HistoryKind::Number,
        Some("°F"),
        vec![
            number(1.5),
            Kind::NA,
            number(-2.0),
            Kind::Number(Number::new(4.0, Some("°F".into()))),
            Kind::NA,
            number(10.0),
        ],
    );
    capture(&small, &dir.path().join("small"), 2).await;
    // Larger fixture: exact binary fractions, so every SUM is exact and
    // independent of accumulation order.
    let values = (0..500)
        .map(|i| {
            if i % 7 == 3 {
                Kind::NA
            } else {
                number((i as f64 - 200.0) * 0.25)
            }
        })
        .collect();
    let large = point("large", HistoryKind::Number, Some("°F"), values);
    capture(&large, &dir.path().join("large"), 64).await;
    let engine = engine(&[
        ("small", &dir.path().join("small")),
        ("large", &dir.path().join("large")),
    ])
    .await;

    let sql = "SELECT COUNT(*) AS n, COUNT(number_value) AS v, SUM(number_value) AS s, \
               AVG(number_value) AS a, MIN(number_value) AS lo, MAX(number_value) AS hi, \
               COUNT(*) FILTER (WHERE NOT present) AS na, COUNT(unit) AS units FROM small";
    let out = run(&engine, sql).await;
    assert_eq!(i64s(&out, "n"), [Some(6)]);
    assert_eq!(i64s(&out, "v"), [Some(4)]);
    assert_eq!(f64s(&out, "s"), [Some(13.5f64.to_bits())]);
    assert_eq!(f64s(&out, "a"), [Some(3.375f64.to_bits())]);
    assert_eq!(f64s(&out, "lo"), [Some((-2.0f64).to_bits())]);
    assert_eq!(f64s(&out, "hi"), [Some(10.0f64.to_bits())]);
    assert_eq!(i64s(&out, "na"), [Some(2)]);
    assert_eq!(i64s(&out, "units"), [Some(1)]);

    // Filter on a timestamp component; NA rows have NULL values, so a value
    // predicate excludes them and an IS NULL predicate keeps them.
    let cut = large.samples[100].ts.dt.timestamp();
    let sql = format!(
        "SELECT COUNT(*) AS n, COUNT(number_value) AS v, SUM(number_value) AS s, \
         AVG(number_value) AS a, MIN(number_value) AS lo, MAX(number_value) AS hi, \
         COUNT(*) FILTER (WHERE number_value IS NULL) AS nulls \
         FROM large WHERE utc_seconds >= {cut} AND (number_value > -10 OR number_value IS NULL)"
    );
    let out = run(&engine, &sql).await;
    let kept: Vec<_> = large
        .samples
        .iter()
        .filter(|s| s.ts.dt.timestamp() >= cut)
        .filter(|s| match &s.val {
            Kind::Number(n) => n.val > -10.0,
            _ => true,
        })
        .collect();
    let vals: Vec<f64> = kept
        .iter()
        .filter_map(|s| match &s.val {
            Kind::Number(n) => Some(n.val),
            _ => None,
        })
        .collect();
    let sum: f64 = vals.iter().sum();
    assert_eq!(i64s(&out, "n"), [Some(kept.len() as i64)]);
    assert_eq!(i64s(&out, "v"), [Some(vals.len() as i64)]);
    assert_eq!(f64s(&out, "s"), [Some(sum.to_bits())]);
    assert_eq!(f64s(&out, "a"), [Some((sum / vals.len() as f64).to_bits())]);
    let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    assert_eq!(f64s(&out, "lo"), [Some(lo.to_bits())]);
    assert_eq!(f64s(&out, "hi"), [Some(hi.to_bits())]);
    assert_eq!(
        i64s(&out, "nulls"),
        [Some((kept.len() - vals.len()) as i64)]
    );

    // Group by minute against a fold over the source samples.
    let out = run(
        &engine,
        "SELECT utc_seconds / 60 AS minute, COUNT(*) AS n, SUM(number_value) AS s \
         FROM large GROUP BY minute ORDER BY minute",
    )
    .await;
    let mut expected = std::collections::BTreeMap::<i64, (i64, f64, bool)>::new();
    for s in &large.samples {
        let e = expected
            .entry(s.ts.dt.timestamp().div_euclid(60))
            .or_default();
        e.0 += 1;
        if let Kind::Number(n) = &s.val {
            e.1 += n.val;
            e.2 = true;
        }
    }
    assert_eq!(
        i64s(&out, "minute"),
        expected.keys().map(|k| Some(*k)).collect::<Vec<_>>()
    );
    assert_eq!(
        i64s(&out, "n"),
        expected.values().map(|v| Some(v.0)).collect::<Vec<_>>()
    );
    assert_eq!(
        f64s(&out, "s"),
        expected
            .values()
            .map(|v| v.2.then_some(v.1.to_bits()))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn nan_infinities_and_signed_zero_keep_their_bits() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let value = point(
        "special",
        HistoryKind::Number,
        Some("°F"),
        vec![
            number(f64::NAN),
            Kind::Number(Number::new(f64::INFINITY, Some("°F".into()))),
            number(f64::NEG_INFINITY),
            number(-0.0),
            Kind::NA,
            Kind::Number(Number::new(1.0000000000000002, Some("fahrenheit".into()))),
        ],
    );
    capture(&value, &dir.path().join("t"), 2).await;
    let engine = engine(&[("t", &dir.path().join("t"))]).await;
    let out = run(
        &engine,
        "SELECT number_value, unit, present FROM t ORDER BY ts",
    )
    .await;
    let expected: Vec<_> = value.samples.iter().map(|s| value_bits(&s.val)).collect();
    assert_eq!(f64s(&out, "number_value"), expected);
    assert_eq!(
        f64s(&out, "number_value"),
        [
            Some(0x7ff8000000000000),
            Some(0x7ff0000000000000),
            Some(0xfff0000000000000),
            Some(0x8000000000000000),
            None,
            Some(0x3ff0000000000001)
        ]
    );
    assert_eq!(
        strings(&out, "unit"),
        [
            None,
            Some("°F".into()),
            None,
            None,
            None,
            Some("fahrenheit".into())
        ]
    );
    let present = col::<BooleanArray>(&out, "present");
    assert_eq!(
        (0..present.len())
            .map(|i| present.value(i))
            .collect::<Vec<_>>(),
        [true, true, true, true, false, true]
    );
    let out = run(
        &engine,
        "SELECT COUNT(*) FILTER (WHERE isnan(number_value)) AS nan, \
         COUNT(*) FILTER (WHERE number_value = CAST('Infinity' AS DOUBLE)) AS inf, \
         MAX(number_value) AS hi, MIN(number_value) AS lo FROM t",
    )
    .await;
    assert_eq!(i64s(&out, "nan"), [Some(1)]);
    assert_eq!(i64s(&out, "inf"), [Some(1)]);
    // DataFusion orders NaN above +Infinity (documented).
    assert!(f64::from_bits(f64s(&out, "hi")[0].unwrap()).is_nan());
    assert_eq!(f64s(&out, "lo"), [Some(f64::NEG_INFINITY.to_bits())]);
}

#[tokio::test]
async fn strings_with_quotes_and_empty_values_round_trip() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let texts = ["", "it's", "say \"hi\"", "back\\slash", "é,\n$🚀"];
    let mut values: Vec<Kind> = texts.iter().map(|t| Kind::Str((*t).into())).collect();
    values.insert(2, Kind::NA);
    let value = point("text", HistoryKind::Str, None, values);
    capture(&value, &dir.path().join("t"), 2).await;
    let engine = engine(&[("t", &dir.path().join("t"))]).await;
    let out = run(
        &engine,
        "SELECT string_value, CAST(character_length(string_value) AS BIGINT) AS chars \
         FROM t ORDER BY ts",
    )
    .await;
    let expected: Vec<_> = value
        .samples
        .iter()
        .map(|s| match &s.val {
            Kind::Str(v) => Some(v.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(strings(&out, "string_value"), expected);
    assert_eq!(
        i64s(&out, "chars"),
        expected
            .iter()
            .map(|v| v.as_ref().map(|s| s.chars().count() as i64))
            .collect::<Vec<_>>()
    );
    let out = run(
        &engine,
        "SELECT COUNT(*) FILTER (WHERE string_value = '') AS empty, \
         COUNT(*) FILTER (WHERE string_value = 'it''s') AS apostrophe, \
         COUNT(*) FILTER (WHERE string_value = 'say \"hi\"') AS quoted, \
         COUNT(string_value) AS v, COUNT(*) FILTER (WHERE string_value IS NULL) AS na \
         FROM t",
    )
    .await;
    for (name, n) in [
        ("empty", 1),
        ("apostrophe", 1),
        ("quoted", 1),
        ("v", 5),
        ("na", 1),
    ] {
        assert_eq!(i64s(&out, name), [Some(n)], "{name}");
    }
}

/// Rebuilding samples from the SQL columns alone reproduces the source
/// exactly, including offsets and zone names.
#[tokio::test]
async fn sql_columns_reconstruct_the_source_samples_losslessly() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let mut value = point(
        "mixed",
        HistoryKind::Number,
        Some("°F"),
        (0..30)
            .map(|i| match i % 3 {
                0 => Kind::NA,
                1 => Kind::Number(Number::new(i as f64 / 3.0, Some("°F".into()))),
                _ => number(-(i as f64)),
            })
            .collect(),
    );
    value.samples[7].ts = date("2024-06-01T00:00:07.123456789Z UTC");
    capture(&value, &dir.path().join("t"), 4).await;
    let engine = engine(&[("t", &dir.path().join("t"))]).await;
    let out = run(
        &engine,
        "SELECT utc_seconds, nanoseconds, offset_seconds, tz, present, number_value, unit \
         FROM t ORDER BY ts",
    )
    .await;
    let seconds = i64s(&out, "utc_seconds");
    let nanos = col::<UInt32Array>(&out, "nanoseconds");
    let offsets = col::<Int32Array>(&out, "offset_seconds");
    let zones = strings(&out, "tz");
    let present = col::<BooleanArray>(&out, "present");
    let numbers = col::<Float64Array>(&out, "number_value");
    let units = strings(&out, "unit");
    let rebuilt: Vec<HistorySample> = (0..out.num_rows())
        .map(|i| {
            let utc =
                chrono::DateTime::from_timestamp(seconds[i].unwrap(), nanos.value(i)).unwrap();
            let offset = chrono::FixedOffset::east_opt(offsets.value(i)).unwrap();
            HistorySample {
                ts: haystack_core::kinds::HDateTime::new(
                    utc.with_timezone(&offset),
                    zones[i].clone().unwrap(),
                ),
                val: if present.value(i) {
                    Kind::Number(Number::new(numbers.value(i), units[i].clone()))
                } else {
                    assert!(numbers.is_null(i) && units[i].is_none());
                    Kind::NA
                },
            }
        })
        .collect();
    assert_same_samples(&rebuilt, &value.samples);
}

#[tokio::test]
async fn bool_counts_with_filter_match_the_fold() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let pattern = [Some(true), Some(false), None, Some(true), Some(true), None];
    let values = (0..60)
        .map(|i| pattern[i % 6].map_or(Kind::NA, Kind::Bool))
        .collect();
    let value = point("flag", HistoryKind::Bool, None, values);
    capture(&value, &dir.path().join("t"), 8).await;
    let engine = engine(&[("t", &dir.path().join("t"))]).await;
    let out = run(
        &engine,
        "SELECT COUNT(*) FILTER (WHERE bool_value) AS t, \
         COUNT(*) FILTER (WHERE NOT bool_value) AS f, \
         COUNT(*) FILTER (WHERE bool_value IS NULL) AS na, \
         COUNT(bool_value) AS v, COUNT(*) FILTER (WHERE present) AS p FROM t",
    )
    .await;
    let count = |f: &dyn Fn(&Kind) -> bool| value.samples.iter().filter(|s| f(&s.val)).count();
    let t = count(&|v| matches!(v, Kind::Bool(true)));
    let f = count(&|v| matches!(v, Kind::Bool(false)));
    let na = count(&|v| matches!(v, Kind::NA));
    assert_eq!((t, f, na), (30, 10, 20));
    assert_eq!(i64s(&out, "t"), [Some(t as i64)]);
    assert_eq!(i64s(&out, "f"), [Some(f as i64)]);
    assert_eq!(i64s(&out, "na"), [Some(na as i64)]);
    assert_eq!(i64s(&out, "v"), [Some((t + f) as i64)]);
    assert_eq!(i64s(&out, "p"), [Some((t + f) as i64)]);
}

/// Two snapshots joined on `ts`, against a nested-loop reference over the
/// source samples; cross, non-equi and recursive queries are refused before
/// execution while equi-joins run.
#[tokio::test]
async fn equi_join_matches_nested_loop_reference_and_runaway_joins_are_rejected() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let a = point(
        "a",
        HistoryKind::Number,
        Some("°F"),
        (0..40)
            .map(|i| {
                if i % 9 == 4 {
                    Kind::NA
                } else {
                    number(i as f64)
                }
            })
            .collect(),
    );
    // `b` keeps every third second of a minute, so the join is a strict
    // subset of both sides.
    let mut b = point(
        "b",
        HistoryKind::Number,
        Some("°F"),
        (0..60).map(|i| number(i as f64 * 10.0)).collect(),
    );
    b.samples = b.samples.into_iter().step_by(3).collect();
    b.metadata.coverage.retained_count = b.samples.len() as u64;
    b.metadata.coverage.retained_end = b.samples.last().map(|s| s.ts.clone());
    capture(&a, &dir.path().join("a"), 4).await;
    capture(&b, &dir.path().join("b"), 4).await;
    let engine = engine(&[("a", &dir.path().join("a")), ("b", &dir.path().join("b"))]).await;

    let output = engine
        .query(
            "SELECT a.utc_seconds AS s, a.number_value AS x, b.number_value AS y, \
             b.point_id AS pb FROM a JOIN b ON a.ts = b.ts ORDER BY s",
            query_options(),
        )
        .await
        .unwrap();
    let out = all(&output);
    let mut reference = vec![];
    for left in &a.samples {
        for right in &b.samples {
            if left.ts.dt == right.ts.dt {
                reference.push((
                    left.ts.dt.timestamp(),
                    value_bits(&left.val),
                    value_bits(&right.val),
                ));
            }
        }
    }
    assert_eq!(reference.len(), 14);
    assert_eq!(
        i64s(&out, "s"),
        reference.iter().map(|r| Some(r.0)).collect::<Vec<_>>()
    );
    assert_eq!(
        f64s(&out, "x"),
        reference.iter().map(|r| r.1).collect::<Vec<_>>()
    );
    assert_eq!(
        f64s(&out, "y"),
        reference.iter().map(|r| r.2).collect::<Vec<_>>()
    );
    assert!(
        strings(&out, "pb")
            .iter()
            .all(|p| p.as_deref() == Some("b"))
    );
    let names: Vec<_> = output.sources.iter().map(|s| s.table.as_str()).collect();
    assert_eq!(names, ["a", "b"]);

    // An equi-join written as a WHERE predicate is planned as a hash join.
    let out = run(&engine, "SELECT COUNT(*) AS n FROM a, b WHERE a.ts = b.ts").await;
    assert_eq!(i64s(&out, "n"), [Some(14)]);

    for sql in [
        "SELECT COUNT(*) FROM a CROSS JOIN b",
        "SELECT COUNT(*) FROM a, b",
        "SELECT COUNT(*) FROM a JOIN b ON true",
        "SELECT COUNT(*) FROM a JOIN b ON a.ts < b.ts",
        "SELECT COUNT(*) FROM a JOIN b ON a.ts <> b.ts",
        "SELECT COUNT(*) FROM a JOIN b ON a.number_value > b.number_value",
        "SELECT COUNT(*) FROM a LEFT JOIN b ON a.utc_seconds + 1 > b.utc_seconds",
        "SELECT COUNT(*) FROM a, b, a AS c WHERE a.ts = b.ts",
        "EXPLAIN ANALYZE SELECT COUNT(*) FROM a CROSS JOIN b",
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) \
         SELECT COUNT(*) FROM r",
    ] {
        let started = std::time::Instant::now();
        assert_eq!(
            engine.query(sql, query_options()).await.unwrap_err(),
            Error::Sql,
            "{sql}"
        );
        assert!(started.elapsed() < Duration::from_secs(5), "{sql}");
    }
}

/// New York fall-back: 01:00-01:59 local happens twice. `ts` groups by UTC
/// hour; `ts_local` (ts plus the stored offset) groups by wall-clock hour.
#[tokio::test]
async fn dst_fall_back_groups_differently_on_ts_and_ts_local() {
    let _serial = SERIAL.lock().await;
    let times = [
        "2024-11-03T00:30:00-04:00 New_York",
        "2024-11-03T01:15:00-04:00 New_York",
        "2024-11-03T01:45:00-04:00 New_York",
        "2024-11-03T01:15:00-05:00 New_York",
        "2024-11-03T01:45:00-05:00 New_York",
        "2024-11-03T02:30:00-05:00 New_York",
    ];
    let mut value = point(
        "ny",
        HistoryKind::Number,
        Some("°F"),
        (0..times.len()).map(|i| number(i as f64)).collect(),
    );
    value.metadata.requested_range = "2024-11-03".into();
    value.metadata.schema.timezone = "New_York".into();
    value.metadata.start = date("2024-11-03T00:00:00-04:00 New_York");
    value.metadata.end = date("2024-11-04T00:00:00-05:00 New_York");
    value.metadata.evaluated_at = value.metadata.start.clone();
    value.metadata.coverage.evicted_through = None;
    for (sample, ts) in value.samples.iter_mut().zip(times) {
        sample.ts = date(ts);
    }
    value.metadata.coverage.retained_start = value.samples.first().map(|s| s.ts.clone());
    value.metadata.coverage.retained_end = value.samples.last().map(|s| s.ts.clone());
    let dir = tempfile::tempdir().unwrap();
    capture(&value, &dir.path().join("t"), 2).await;
    let engine = engine(&[("t", &dir.path().join("t"))]).await;

    let fold = |key: &dyn Fn(&HistorySample) -> i64| {
        let mut groups = std::collections::BTreeMap::<i64, i64>::new();
        for s in &value.samples {
            *groups.entry(key(s)).or_default() += 1;
        }
        groups.into_values().map(Some).collect::<Vec<_>>()
    };
    let utc = run(
        &engine,
        "SELECT date_trunc('hour', ts) AS h, COUNT(*) AS n FROM t GROUP BY h ORDER BY h",
    )
    .await;
    let expected = fold(&|s| s.ts.dt.timestamp().div_euclid(3600));
    assert_eq!(expected, [Some(1), Some(2), Some(2), Some(1)]);
    assert_eq!(i64s(&utc, "n"), expected);
    let local = run(
        &engine,
        "SELECT date_trunc('hour', ts_local) AS h, COUNT(*) AS n FROM t GROUP BY h ORDER BY h",
    )
    .await;
    let expected = fold(&|s| s.ts.dt.naive_local().and_utc().timestamp().div_euclid(3600));
    assert_eq!(expected, [Some(1), Some(4), Some(1)]);
    assert_eq!(i64s(&local, "n"), expected);
    let h = col::<TimestampNanosecondArray>(&local, "h");
    assert_eq!(h.timezone(), None);
    // 2024-11-03T01:00 wall clock, as naive nanoseconds.
    assert_eq!(h.value(1), 1_730_595_600_000_000_000);

    let out = run(
        &engine,
        "SELECT offset_seconds, tz, ts, ts_local FROM t ORDER BY ts",
    )
    .await;
    let offsets = col::<Int32Array>(&out, "offset_seconds");
    assert_eq!(
        (0..offsets.len())
            .map(|i| offsets.value(i))
            .collect::<Vec<_>>(),
        value
            .samples
            .iter()
            .map(|s| s.ts.dt.offset().local_minus_utc())
            .collect::<Vec<_>>()
    );
    assert!(
        strings(&out, "tz")
            .iter()
            .all(|z| z.as_deref() == Some("New_York"))
    );
    let ts = col::<TimestampNanosecondArray>(&out, "ts");
    assert_eq!(ts.timezone(), Some("UTC"));
    let ts_local = col::<TimestampNanosecondArray>(&out, "ts_local");
    for (i, s) in value.samples.iter().enumerate() {
        assert_eq!(ts.value(i), s.ts.dt.timestamp_nanos_opt().unwrap());
        assert_eq!(
            ts_local.value(i),
            s.ts.dt
                .naive_local()
                .and_utc()
                .timestamp_nanos_opt()
                .unwrap()
        );
    }
}

#[tokio::test]
async fn leap_second_fractions_keep_exact_components() {
    let _serial = SERIAL.lock().await;
    let times = [
        "2016-12-31T23:59:59.999999999Z UTC",
        "2016-12-31T23:59:60.000000001Z UTC",
        "2016-12-31T23:59:60.999999999Z UTC",
    ];
    let mut value = point(
        "leap",
        HistoryKind::Number,
        Some("°F"),
        vec![number(1.0); 3],
    );
    value.metadata.requested_range = "2016-12-31".into();
    value.metadata.start = date("2016-12-31T00:00:00Z UTC");
    value.metadata.end = date("2017-01-01T00:00:00Z UTC");
    value.metadata.evaluated_at = value.metadata.start.clone();
    value.metadata.coverage.evicted_through = None;
    for (sample, ts) in value.samples.iter_mut().zip(times) {
        sample.ts = date(ts);
    }
    value.metadata.coverage.retained_start = value.samples.first().map(|s| s.ts.clone());
    value.metadata.coverage.retained_end = value.samples.last().map(|s| s.ts.clone());
    let dir = tempfile::tempdir().unwrap();
    capture(&value, &dir.path().join("t"), 2).await;
    let engine = engine(&[("t", &dir.path().join("t"))]).await;
    let out = run(
        &engine,
        "SELECT utc_seconds, nanoseconds, ts FROM t ORDER BY utc_seconds, nanoseconds",
    )
    .await;
    // 2016-12-31T23:59:59Z.
    assert_eq!(i64s(&out, "utc_seconds"), [Some(1_483_228_799); 3]);
    let nanos = col::<UInt32Array>(&out, "nanoseconds");
    assert_eq!(
        (0..3).map(|i| nanos.value(i)).collect::<Vec<_>>(),
        [999_999_999, 1_000_000_001, 1_999_999_999]
    );
    // `ts` folds a leap fraction into the following second.
    let ts = col::<TimestampNanosecondArray>(&out, "ts");
    assert_eq!(
        (0..3).map(|i| ts.value(i)).collect::<Vec<_>>(),
        [
            1_483_228_799_999_999_999,
            1_483_228_800_000_000_001,
            1_483_228_800_999_999_999
        ]
    );
}

fn rewrite_manifest(path: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let file = path.join(MANIFEST_NAME);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    edit(&mut manifest);
    std::fs::write(file, serde_json::to_vec(&manifest).unwrap()).unwrap();
}
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every kind of incomplete, corrupt or mismatched capture fails
/// registration, registers nothing, and leaves earlier tables usable.
#[tokio::test]
async fn incomplete_corrupt_and_mismatched_captures_are_never_registered() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let source = point(
        &request().id,
        HistoryKind::Str,
        None,
        (0..9).map(|i| Kind::Str(format!("v{i}"))).collect(),
    );
    let good = dir.path().join("good");
    capture(&source, &good, 2).await;
    let numbers = point(
        "point",
        HistoryKind::Number,
        Some("°F"),
        (0..9).map(|i| number(i as f64)).collect(),
    );
    let other = dir.path().join("numbers");
    capture(&numbers, &other, 2).await;

    type Corrupt = Box<dyn Fn(&Path)>;
    let other_part = other.clone();
    let cases: Vec<(&str, Corrupt)> = vec![
        (
            "missing_directory",
            Box::new(|p| std::fs::remove_dir_all(p).unwrap()),
        ),
        (
            "unpublished_manifest",
            Box::new(|p| {
                std::fs::rename(p.join(MANIFEST_NAME), p.join(".manifest.pending")).unwrap()
            }),
        ),
        (
            "truncated_manifest",
            Box::new(|p| {
                let bytes = std::fs::read(p.join(MANIFEST_NAME)).unwrap();
                std::fs::write(p.join(MANIFEST_NAME), &bytes[..bytes.len() / 2]).unwrap();
            }),
        ),
        (
            "row_count",
            Box::new(|p| rewrite_manifest(p, |m| m["rows"] = 8.into())),
        ),
        (
            "schema_version",
            Box::new(|p| rewrite_manifest(p, |m| m["schema_version"] = 2.into())),
        ),
        (
            "manifest_version",
            Box::new(|p| rewrite_manifest(p, |m| m["manifest_version"] = 2.into())),
        ),
        (
            "consistency",
            Box::new(|p| rewrite_manifest(p, |m| m["consistency"] = "bestEffort".into())),
        ),
        (
            "unknown_field",
            Box::new(|p| rewrite_manifest(p, |m| m["extra"] = true.into())),
        ),
        (
            "export_id",
            Box::new(|p| rewrite_manifest(p, |m| m["export_id"] = "not-a-uuid".into())),
        ),
        (
            "source_terminal",
            Box::new(|p| {
                rewrite_manifest(p, |m| {
                    let header = m["source_header"].as_str().unwrap().to_owned();
                    assert!(header.contains("complete"));
                    m["source_header"] = header.replacen("complete", "limited", 1).into();
                })
            }),
        ),
        (
            "dropped_part",
            Box::new(|p| {
                rewrite_manifest(p, |m| {
                    m["parts"].as_array_mut().unwrap().pop();
                })
            }),
        ),
        (
            "part_hash",
            Box::new(|p| {
                let part = p.join("part-0000.parquet");
                let mut bytes = std::fs::read(&part).unwrap();
                let middle = bytes.len() / 2;
                bytes[middle] ^= 1;
                std::fs::write(part, bytes).unwrap();
            }),
        ),
        (
            "missing_part",
            Box::new(|p| std::fs::remove_file(p.join("part-0001.parquet")).unwrap()),
        ),
        (
            // A hash-valid part of a Number capture spliced into a Str
            // capture: the manifest is consistent, the rows are not.
            "kind_mismatch",
            Box::new(move |p| {
                let bytes = std::fs::read(other_part.join("part-0000.parquet")).unwrap();
                std::fs::write(p.join("part-0000.parquet"), &bytes).unwrap();
                rewrite_manifest(p, |m| {
                    m["parts"][0]["sha256"] = sha256_hex(&bytes).into();
                    m["parts"][0]["bytes"] = bytes.len().into();
                })
            }),
        ),
    ];

    let mut builder = SqlEngine::builder();
    builder.register("good", &good).await.unwrap();
    // Control: re-serializing an unchanged manifest is still accepted, so
    // each failure below is caused by its own corruption.
    let control = dir.path().join("control");
    capture(&source, &control, 2).await;
    rewrite_manifest(&control, |_| {});
    builder.register("control", &control).await.unwrap();
    let before = builder.snapshots();
    for (name, corrupt) in &cases {
        let path = dir.path().join(name);
        capture(&source, &path, 2).await;
        assert!(path.join("part-0001.parquet").exists());
        corrupt(&path);
        let error = builder.register(name, &path).await.unwrap_err();
        assert!(
            matches!(error, Error::Invalid | Error::Io(_) | Error::Limit),
            "{name}: {error:?}"
        );
        assert_eq!(builder.snapshots(), before, "{name}");
    }
    let engine = builder.build().unwrap();
    for (name, _) in &cases {
        assert_eq!(
            engine
                .query(&format!("SELECT COUNT(*) FROM {name}"), query_options())
                .await
                .unwrap_err(),
            Error::Sql,
            "{name}"
        );
    }
    let out = run(&engine, "SELECT COUNT(*) AS n FROM good").await;
    assert_eq!(i64s(&out, "n"), [Some(9)]);
}

/// A rows² equi-join on the constant `point_id` that runs for seconds.
const SLOW: &str = "SELECT a.utc_seconds % 7 AS k, COUNT(*) FROM big a JOIN big b \
                    ON a.point_id = b.point_id GROUP BY k";

async fn big(dir: &Path) -> SqlEngine {
    let value = point(
        "big",
        HistoryKind::Number,
        Some("°F"),
        (0..MAX_ROWS).map(|i| number(i as f64)).collect(),
    );
    capture(&value, &dir.join("big"), 1024).await;
    engine(&[("big", &dir.join("big"))]).await
}

#[tokio::test]
async fn cancellation_deadline_capacity_and_options_release_admission() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = std::sync::Arc::new(big(dir.path()).await);

    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.cancel();
    });
    let started = std::time::Instant::now();
    let options = QueryOptions::new().with_cancellation(token);
    assert_eq!(
        engine.query(SLOW, options).await.unwrap_err(),
        Error::Cancelled
    );
    assert!(started.elapsed() < Duration::from_secs(3));

    let started = std::time::Instant::now();
    let options = QueryOptions::new().with_timeout(Duration::from_millis(200));
    assert_eq!(
        engine.query(SLOW, options).await.unwrap_err(),
        Error::Deadline
    );
    assert!(started.elapsed() < Duration::from_secs(3));

    // An already-cancelled token and a looser timeout never run.
    let token = CancellationToken::new();
    token.cancel();
    let options = QueryOptions::new().with_cancellation(token);
    assert_eq!(
        engine.query("SELECT 1", options).await.unwrap_err(),
        Error::Cancelled
    );
    let options = QueryOptions::new().with_timeout(QUERY_TIMEOUT + Duration::from_millis(1));
    assert_eq!(
        engine.query("SELECT 1", options).await.unwrap_err(),
        Error::Options
    );

    // Both slots are free again: two slow queries are admitted and a third
    // is refused immediately rather than queued.
    let token = CancellationToken::new();
    let mut running = vec![];
    for _ in 0..2 {
        let engine = engine.clone();
        let options = QueryOptions::new().with_cancellation(token.clone());
        running.push(tokio::spawn(
            async move { engine.query(SLOW, options).await },
        ));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    assert_eq!(
        engine.query("SELECT 1", query_options()).await.unwrap_err(),
        Error::Capacity
    );
    assert!(started.elapsed() < Duration::from_millis(100));
    token.cancel();
    for task in running {
        assert_eq!(task.await.unwrap().unwrap_err(), Error::Cancelled);
    }
    // And both are released after cancellation.
    let (x, y) = tokio::join!(
        engine.query("SELECT COUNT(*) AS n FROM big", query_options()),
        engine.query("SELECT COUNT(*) AS n FROM big", query_options())
    );
    for output in [x.unwrap(), y.unwrap()] {
        assert_eq!(i64s(&all(&output), "n"), [Some(MAX_ROWS as i64)]);
    }
}

#[tokio::test]
async fn memory_and_output_caps_are_limit_errors() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = big(dir.path()).await;
    // Exactly the row cap is returned.
    let output = engine
        .query("SELECT utc_seconds FROM big", query_options())
        .await
        .unwrap();
    assert_eq!(all(&output).num_rows(), MAX_OUTPUT_ROWS);
    // One more row is not.
    assert_eq!(
        engine
            .query(
                "SELECT utc_seconds FROM big UNION ALL SELECT 1",
                query_options()
            )
            .await
            .unwrap_err(),
        Error::Limit
    );
    // Within the row cap but over the byte cap: 10,000 rows of over 2 KiB.
    let wide = format!("'{}'", "x".repeat(2048));
    assert_eq!(
        engine
            .query(
                &format!("SELECT {wide} AS w, utc_seconds FROM big"),
                query_options()
            )
            .await
            .unwrap_err(),
        Error::Limit
    );
    // Join output rows over MAX_JOIN_ROWS (rows² on the constant point id),
    // although the pool, the output and every batch are small.
    assert_eq!(
        engine
            .query(
                "SELECT count(*) AS n FROM big a JOIN big b ON a.point_id = b.point_id",
                query_options()
            )
            .await
            .unwrap_err(),
        Error::Limit
    );
    // Operator memory beyond the per-query pool, with spill disabled.
    assert_eq!(
        engine
            .query(
                "SELECT a.utc_seconds, b.utc_seconds, COUNT(*) FROM big a JOIN big b \
                 ON a.point_id = b.point_id GROUP BY 1, 2 ORDER BY 1, 2",
                query_options()
            )
            .await
            .unwrap_err(),
        Error::Limit
    );
    let out = run(&engine, "SELECT COUNT(*) AS n FROM big").await;
    assert_eq!(i64s(&out, "n"), [Some(MAX_ROWS as i64)]);
}

#[tokio::test]
async fn side_effects_files_and_oversize_or_deep_sql_are_rejected() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let value = point(
        "t",
        HistoryKind::Number,
        Some("°F"),
        (0..4).map(|i| number(i as f64)).collect(),
    );
    capture(&value, &dir.path().join("t"), 2).await;
    let engine = engine(&[("t", &dir.path().join("t"))]).await;
    let target = dir.path().join("out.csv");
    let target = target.display();
    for sql in [
        "CREATE EXTERNAL TABLE x STORED AS CSV LOCATION '/etc'".to_owned(),
        "CREATE EXTERNAL TABLE x STORED AS ARROW LOCATION '/etc'".to_owned(),
        format!("COPY t TO '{target}' STORED AS CSV"),
        format!("COPY (SELECT 1) TO '{target}'"),
        "INSERT INTO t SELECT * FROM t".to_owned(),
        "DELETE FROM t".to_owned(),
        "UPDATE t SET present = false".to_owned(),
        "DROP TABLE t".to_owned(),
        "CREATE TABLE y AS SELECT 1".to_owned(),
        "CREATE VIEW v AS SELECT 1".to_owned(),
        "SET datafusion.execution.batch_size = 1".to_owned(),
        "SET datafusion.runtime.memory_limit = '1G'".to_owned(),
        "PREPARE p AS SELECT 1".to_owned(),
        "BEGIN TRANSACTION".to_owned(),
        "SELECT 1; SELECT 2".to_owned(),
        "SELECT * FROM generate_series(1, 10)".to_owned(),
        "SELECT * FROM range(1, 10)".to_owned(),
        "SELECT repeat('x', 2147483647)".to_owned(),
        "SELECT lpad('x', 2147483647)".to_owned(),
        "SELECT rpad('x', 2147483647)".to_owned(),
        "SELECT * FROM '/etc/hosts'".to_owned(),
        "SELECT * FROM information_schema.tables".to_owned(),
        "SELECT * FROM missing".to_owned(),
        "SELECT nope FROM t".to_owned(),
        "not sql".to_owned(),
    ] {
        assert_eq!(
            engine.query(&sql, query_options()).await.unwrap_err(),
            Error::Sql,
            "{sql}"
        );
    }
    assert!(!dir.path().join("out.csv").exists());
    let out = run(&engine, "SELECT COUNT(*) AS n FROM t").await;
    assert_eq!(i64s(&out, "n"), [Some(4)]);

    // The SQL text bound is checked before parsing.
    let pad = |n: usize| format!("SELECT 1{}", " ".repeat(n - "SELECT 1".len()));
    assert!(
        engine
            .query(&pad(MAX_SQL_BYTES), query_options())
            .await
            .is_ok()
    );
    assert_eq!(
        engine
            .query(&pad(MAX_SQL_BYTES + 1), query_options())
            .await
            .unwrap_err(),
        Error::Limit
    );
    // Nesting within the token cap but past the parser's recursion limit is
    // rejected by the parser; long chains are covered by
    // `long_chains_are_limits_before_planning_and_never_overflow`.
    let depth = SQL_RECURSION_LIMIT + 10;
    let deep = format!("SELECT {}1{}", "(".repeat(depth), ")".repeat(depth));
    assert!(tokens(&deep) <= MAX_SQL_TOKENS);
    assert_eq!(
        engine.query(&deep, query_options()).await.unwrap_err(),
        Error::Sql
    );
    // Past the token cap, nothing is parsed.
    let depth = 5_000;
    let deep = format!("SELECT {}1{}", "(".repeat(depth), ")".repeat(depth));
    assert!(deep.len() <= MAX_SQL_BYTES);
    assert_eq!(
        engine.query(&deep, query_options()).await.unwrap_err(),
        Error::Limit
    );
    let deep = format!("SELECT {}1", "- ".repeat((MAX_SQL_BYTES - 10) / 2));
    assert_eq!(
        engine.query(&deep, query_options()).await.unwrap_err(),
        Error::Limit
    );
    // `--` starts a comment: one token, rejected as SQL rather than counted.
    let comment = format!("SELECT {}1", "-".repeat(MAX_SQL_BYTES - 10));
    assert_eq!(
        engine.query(&comment, query_options()).await.unwrap_err(),
        Error::Sql
    );
    assert!(
        engine
            .query("SELECT ((((1))))", query_options())
            .await
            .is_ok()
    );
}

/// Sealed: identity and results are stable across executions and survive
/// the capture directory being rewritten and then deleted.
#[tokio::test]
async fn repeated_queries_keep_identity_and_sealed_data_survives_capture_removal() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let a = point(
        "alpha",
        HistoryKind::Number,
        Some("°F"),
        (0..12).map(|i| number(i as f64)).collect(),
    );
    let b = point(
        "beta",
        HistoryKind::Bool,
        None,
        (0..5).map(|i| Kind::Bool(i % 2 == 0)).collect(),
    );
    capture(&a, &dir.path().join("a"), 2).await;
    capture(&b, &dir.path().join("b"), 2).await;
    let manifest = |p: &str| {
        let bytes = std::fs::read(dir.path().join(p).join(MANIFEST_NAME)).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (
            json["export_id"].as_str().unwrap().to_owned(),
            sha256_hex(&bytes),
        )
    };
    let (a_id, a_sha) = manifest("a");
    let (b_id, b_sha) = manifest("b");
    let engine = engine(&[("a", &dir.path().join("a")), ("b", &dir.path().join("b"))]).await;
    let identities = engine.snapshots();
    let fields: Vec<_> = identities
        .iter()
        .map(|i| {
            (
                i.table.as_str(),
                i.export_id.as_str(),
                i.manifest_sha256.as_str(),
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("a", a_id.as_str(), a_sha.as_str()),
            ("b", b_id.as_str(), b_sha.as_str())
        ]
    );

    let sql = "SELECT point_id, SUM(number_value) AS s, COUNT(*) AS n FROM a GROUP BY point_id";
    let first = engine.query(sql, query_options()).await.unwrap();
    assert_eq!(first.sources, identities[..1]);
    assert_eq!(f64s(&all(&first), "s"), [Some(66.0f64.to_bits())]);
    // Tables read through a CTE and a scalar subquery are sources too.
    let both = engine
        .query(
            "WITH x AS (SELECT COUNT(*) AS n FROM b) \
             SELECT (SELECT n FROM x) AS nb, COUNT(*) AS na FROM a",
            query_options(),
        )
        .await;
    match both {
        Ok(both) => assert_eq!(both.sources, identities),
        // A scalar subquery may be planned as a keyless join (rejected).
        Err(error) => assert_eq!(error, Error::Sql),
    }
    let both = engine
        .query(
            "WITH x AS (SELECT utc_seconds FROM b) \
             SELECT COUNT(*) AS n FROM a WHERE utc_seconds IN (SELECT utc_seconds FROM x)",
            query_options(),
        )
        .await
        .unwrap();
    assert_eq!(both.sources, identities);
    assert_eq!(i64s(&all(&both), "n"), [Some(5)]);

    let provenance = run(
        &engine,
        &format!("SELECT * FROM {SNAPSHOTS_TABLE} ORDER BY table_name"),
    )
    .await;
    let text = |name: &str| strings(&provenance, name);
    assert_eq!(text("table_name"), [Some("a".into()), Some("b".into())]);
    assert_eq!(text("export_id"), [Some(a_id.clone()), Some(b_id.clone())]);
    assert_eq!(
        text("manifest_sha256"),
        [Some(a_sha.clone()), Some(b_sha.clone())]
    );
    assert_eq!(
        text("point_id"),
        [Some("alpha".into()), Some("beta".into())]
    );
    assert_eq!(text("kind"), [Some("Number".into()), Some("Bool".into())]);
    assert_eq!(text("timezone"), [Some("UTC".into()), Some("UTC".into())]);
    assert_eq!(text("point_unit"), [Some("°F".into()), None]);
    assert_eq!(
        text("requested_range"),
        [Some("2024-06-01".into()), Some("2024-06-01".into())]
    );
    assert_eq!(
        text("history_authority"),
        [Some("33".repeat(16)), Some("33".repeat(16))]
    );
    assert_eq!(
        text("history_incarnation"),
        [Some("44".repeat(16)), Some("44".repeat(16))]
    );
    let rows = col::<UInt64Array>(&provenance, "rows");
    assert_eq!((rows.value(0), rows.value(1)), (12, 5));
    let generation = col::<UInt64Array>(&provenance, "history_generation");
    assert_eq!(generation.value(0), 9007199254740995);
    let only = engine
        .query(&format!("SELECT * FROM {SNAPSHOTS_TABLE}"), query_options())
        .await
        .unwrap();
    assert!(only.sources.is_empty());

    // Overwrite every file of the capture, then delete it: nothing is re-read.
    for entry in std::fs::read_dir(dir.path().join("a")).unwrap() {
        std::fs::write(entry.unwrap().path(), b"garbage").unwrap();
    }
    let rewritten = engine.query(sql, query_options()).await.unwrap();
    std::fs::remove_dir_all(dir.path().join("a")).unwrap();
    let deleted = engine.query(sql, query_options()).await.unwrap();
    for output in [&rewritten, &deleted] {
        assert_eq!(all(output), all(&first));
        assert_eq!(output.sources, first.sources);
    }
    assert_eq!(engine.snapshots(), identities);
}

#[tokio::test]
async fn table_names_are_validated_unique_and_bounded() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let value = point("t", HistoryKind::Number, Some("°F"), vec![number(1.0)]);
    let path = dir.path().join("t");
    capture(&value, &path, 2).await;
    let mut builder = SqlEngine::builder();
    let longest = format!("a{}", "_".repeat(62));
    builder.register(&longest, &path).await.unwrap();
    let too_long = format!("a{}", "_".repeat(63));
    for name in [
        "",
        "A",
        "1a",
        "_a",
        "a-b",
        "a b",
        "a.b",
        "é",
        "\"a\"",
        SNAPSHOTS_TABLE,
        too_long.as_str(),
        longest.as_str(),
    ] {
        assert_eq!(
            builder.register(name, &path).await.unwrap_err(),
            Error::Options,
            "{name:?}"
        );
    }
    for i in 1..MAX_SNAPSHOTS {
        builder.register(&format!("t{i}"), &path).await.unwrap();
    }
    assert_eq!(
        builder.register("one_more", &path).await.unwrap_err(),
        Error::Limit
    );
    let engine = builder.build().unwrap();
    assert_eq!(engine.snapshots().len(), MAX_SNAPSHOTS);
    let out = run(
        &engine,
        &format!("SELECT COUNT(*) AS n FROM {longest} UNION ALL SELECT COUNT(*) FROM t15"),
    )
    .await;
    assert_eq!(i64s(&out, "n"), [Some(1), Some(1)]);
}

/// Non-whitespace tokens, counted with the tokenizer DataFusion parses with.
fn tokens(sql: &str) -> usize {
    use datafusion::sql::sqlparser::{
        dialect::GenericDialect,
        tokenizer::{Token, Tokenizer},
    };
    Tokenizer::new(&GenericDialect {}, sql)
        .tokenize()
        .unwrap()
        .iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .count()
}
fn joined(items: impl Iterator<Item = String>, separator: &str) -> String {
    items.collect::<Vec<_>>().join(separator)
}
type Shape = Box<dyn Fn(usize) -> String>;

/// A four-row Number snapshot `t` of point `t`.
async fn small(dir: &Path) -> SqlEngine {
    let value = point(
        "t",
        HistoryKind::Number,
        Some("°F"),
        (0..4).map(|i| number(i as f64)).collect(),
    );
    capture(&value, &dir.join("t"), 2).await;
    engine(&[("t", &dir.join("t"))]).await
}

/// Left-deep chains are built by the parser without recursion, so its
/// recursion limit never sees them, and every later pass recurses once per
/// term. At the token cap each shape plans and runs (or is rejected for what
/// it is) on the engine's runtime; this suite runs unoptimized, where frames
/// are largest. One more term is `Error::Limit` before parsing. Before the
/// cap, 600 terms of `1+1+…` aborted the process with a stack overflow.
#[tokio::test]
async fn long_chains_are_limits_before_planning_and_never_overflow() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = small(dir.path()).await;
    const X: &str = "FROM (SELECT number_value AS x FROM t) q";
    let shapes: Vec<(&str, Shape, Option<Error>)> = vec![
        (
            "1+1+…",
            Box::new(|n| format!("SELECT {}", vec!["1"; n].join("+"))),
            None,
        ),
        (
            "x+x+…",
            Box::new(|n| format!("SELECT sum({}) {X}", vec!["x"; n].join("+"))),
            None,
        ),
        (
            "x*1*…",
            Box::new(|n| format!("SELECT sum(x{}) {X}", "*1".repeat(n))),
            None,
        ),
        (
            "OR chain",
            Box::new(|n| {
                let terms = joined((0..n).map(|i| format!("x = {i}")), " OR ");
                format!("SELECT count(*) {X} WHERE {terms}")
            }),
            None,
        ),
        (
            "cast chain",
            Box::new(|n| format!("SELECT 1{}", "::bigint".repeat(n))),
            None,
        ),
        (
            "|| chain",
            Box::new(|n| {
                let terms = vec!["s"; n].join("||");
                format!("SELECT length({terms}) FROM (SELECT point_id AS s FROM t) q")
            }),
            Some(Error::Sql),
        ),
    ];
    for (name, shape, at_cap) in &shapes {
        let mut n = 1;
        while tokens(&shape(n + 1)) <= MAX_SQL_TOKENS {
            n += 1;
        }
        assert!(n > 200, "{name}: only {n} terms fit");
        let result = engine.query(&shape(n), query_options()).await;
        match at_cap {
            None => assert!(result.is_ok(), "{name} at {n} terms: {result:?}"),
            Some(error) => assert_eq!(result.unwrap_err(), *error, "{name} at {n} terms"),
        }
        assert_eq!(
            engine
                .query(&shape(n + 1), query_options())
                .await
                .unwrap_err(),
            Error::Limit,
            "{name} at {} terms",
            n + 1
        );
    }
}

/// Joins, set operations, CTEs and relations are counted on the parsed
/// statement, with CTE references expanded, before any planning: planning
/// cannot be cancelled, and a 300-way join chain spent over 25 s in one
/// optimizer rule. At each cap the statement runs; one over is
/// `Error::Limit`, returned quickly.
#[tokio::test]
async fn planning_bounds_reject_wide_statements_quickly() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = small(dir.path()).await;
    let shapes: Vec<(&str, Shape, usize)> = vec![
        (
            "JOIN chain",
            Box::new(|n| {
                let joins = joined(
                    (1..=n).map(|i| format!("JOIN t j{i} ON j{i}.ts = j0.ts")),
                    " ",
                );
                format!("SELECT count(*) AS n FROM t j0 {joins}")
            }),
            MAX_SQL_JOINS,
        ),
        (
            // Parentheses make the chain one nested join factor.
            "parenthesized JOIN chain",
            Box::new(|n| {
                let joins = joined(
                    (1..=n).map(|i| format!("JOIN t j{i} ON j{i}.ts = j0.ts")),
                    " ",
                );
                format!("SELECT count(*) AS n FROM (t j0 {joins})")
            }),
            MAX_SQL_JOINS,
        ),
        (
            "JOIN of a parenthesized chain",
            Box::new(|n| {
                let joins = joined(
                    (2..=n).map(|i| format!("JOIN t j{i} ON j{i}.ts = j1.ts")),
                    " ",
                );
                format!("SELECT count(*) AS n FROM t j0 JOIN ((t j1 {joins})) ON j0.ts = j1.ts")
            }),
            MAX_SQL_JOINS,
        ),
        (
            "comma joins",
            Box::new(|n| {
                let tables = joined((0..=n).map(|i| format!("t c{i}")), ", ");
                let keys = joined((1..=n).map(|i| format!("c{i}.ts = c0.ts")), " AND ");
                format!("SELECT count(*) AS n FROM {tables} WHERE {keys}")
            }),
            MAX_SQL_JOINS,
        ),
        (
            "UNION ALL",
            Box::new(|n| vec!["SELECT 1 AS a"; n + 1].join(" UNION ALL ")),
            MAX_SQL_SET_OPERATIONS,
        ),
        (
            "EXCEPT",
            Box::new(|n| vec!["SELECT 1 AS a"; n + 1].join(" EXCEPT ")),
            MAX_SQL_SET_OPERATIONS,
        ),
        (
            "CTE chain",
            Box::new(|n| {
                let ctes = joined(
                    (1..n).map(|i| format!("a{i} AS (SELECT * FROM a{})", i - 1)),
                    ", ",
                );
                format!(
                    "WITH a0 AS (SELECT * FROM t), {ctes} SELECT count(*) AS n FROM a{}",
                    n - 1
                )
            }),
            MAX_SQL_CTES,
        ),
        (
            // Each `IN (SELECT … FROM t)` is a subquery and a table: 1 + 2k.
            "IN subqueries",
            Box::new(|k| {
                let terms = joined(
                    (0..k).map(|_| "utc_seconds IN (SELECT utc_seconds FROM t)".to_owned()),
                    " AND ",
                );
                format!("SELECT count(*) AS n FROM t WHERE {terms}")
            }),
            (MAX_SQL_RELATIONS - 1) / 2,
        ),
    ];
    for (name, shape, cap) in &shapes {
        assert!(tokens(&shape(cap + 1)) <= MAX_SQL_TOKENS, "{name}");
        let result = engine.query(&shape(*cap), query_options()).await;
        assert!(result.is_ok(), "{name} at {cap}: {result:?}");
        for sql in [shape(cap + 1), format!("EXPLAIN {}", shape(cap + 1))] {
            let started = std::time::Instant::now();
            assert_eq!(
                engine.query(&sql, query_options()).await.unwrap_err(),
                Error::Limit,
                "{name} at {}",
                cap + 1
            );
            assert!(started.elapsed() < Duration::from_secs(1), "{name}");
        }
    }

    // A CTE referenced twice by the next doubles the plan at each level:
    // a_i expands to 2 * (a_{i-1} + 1) relations, so a3 is 22 (plus its
    // reference, 23) and a4 is 46. Counting only the text would see 9.
    let doubling = |levels: usize| {
        let ctes = joined(
            (1..=levels).map(|i| {
                format!(
                    "a{i} AS (SELECT * FROM a{p} UNION ALL SELECT * FROM a{p})",
                    p = i - 1
                )
            }),
            ", ",
        );
        format!("WITH a0 AS (SELECT * FROM t), {ctes} SELECT count(*) AS n FROM a{levels}")
    };
    let out = run(&engine, &doubling(3)).await;
    assert_eq!(i64s(&out, "n"), [Some(4 * 8)]);
    assert_eq!(
        engine
            .query(&doubling(4), query_options())
            .await
            .unwrap_err(),
        Error::Limit
    );

    // The re-review's bypass: 30 joins inside parentheses ran.
    let nested = {
        let joins = joined(
            (1..=30).map(|i| format!("JOIN t j{i} ON j{i}.ts = j0.ts")),
            " ",
        );
        format!("SELECT count(*) FROM (t j0 {joins})")
    };
    assert_eq!(
        engine.query(&nested, query_options()).await.unwrap_err(),
        Error::Limit
    );

    // The reviewer's 300-way join chain: rejected without planning.
    let wide = {
        let joins = joined(
            (1..300).map(|i| format!("JOIN t j{i} ON j{i}.ts = j0.ts")),
            " ",
        );
        format!("SELECT count(*) FROM t j0 {joins}")
    };
    let started = std::time::Instant::now();
    assert_eq!(
        engine.query(&wide, query_options()).await.unwrap_err(),
        Error::Limit
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// Clauses DataFusion would plan exponentially (`CUBE` of N expressions is
/// 2^N groupings) or silently ignore (`TABLESAMPLE`, locking clauses) are
/// rejected before planning, as is anything but a query.
#[tokio::test]
async fn grouping_sets_samples_locks_and_non_queries_are_rejected() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = small(dir.path()).await;
    // `Debug` shows identities only.
    let debug = format!("{engine:?}");
    assert!(
        debug.starts_with("SqlEngine") && debug.contains("table: \"t\""),
        "{debug}"
    );
    for sql in [
        "SELECT count(*) FROM t GROUP BY CUBE(utc_seconds)",
        "SELECT count(*) FROM t GROUP BY ROLLUP(utc_seconds, present)",
        "SELECT count(*) FROM t GROUP BY GROUPING SETS ((utc_seconds), (present))",
        "SELECT count(*) FROM t GROUP BY utc_seconds WITH ROLLUP",
        "SELECT count(*) FROM t TABLESAMPLE BERNOULLI (50)",
        "SELECT count(*) FROM t TABLESAMPLE SYSTEM (10 ROWS)",
        "SELECT * FROM t FOR UPDATE",
        "SELECT * FROM t FOR SHARE",
        "SHOW TABLES",
        "DESCRIBE t",
        "EXPLAIN COPY t TO 'out.csv'",
    ] {
        assert_eq!(
            engine.query(sql, query_options()).await.unwrap_err(),
            Error::Sql,
            "{sql}"
        );
    }
    let out = run(
        &engine,
        "SELECT present, count(*) AS n FROM t GROUP BY present",
    )
    .await;
    assert_eq!(i64s(&out, "n"), [Some(4)]);
    assert!(
        engine
            .query("EXPLAIN SELECT count(*) FROM t", query_options())
            .await
            .is_ok()
    );
}

/// String building is not available: `||` and every function whose output
/// can outgrow its input are rejected, including those the SQL planner
/// constructs without the registry (`OVERLAY … PLACING`, struct literals).
/// Inspection, slicing and case work. (`SUBSTRING`, `POSITION … IN` and
/// `EXTRACT` need DataFusion's `unicode_expressions`/`datetime_expressions`
/// planners, which this build does not enable; they were never available.)
#[tokio::test]
async fn string_building_functions_and_operators_are_rejected() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = small(dir.path()).await;
    for sql in [
        "SELECT 'a' || 'b'",
        "SELECT point_id || 'x' FROM t",
        "SELECT count(*) FROM t WHERE point_id = 'a' || 'b'",
        "SELECT concat(point_id, 'x') FROM t",
        "SELECT concat_ws(',', point_id, tz) FROM t",
        "SELECT replace(point_id, 't', 'tt') FROM t",
        "SELECT regexp_replace(point_id, 't', 'tt') FROM t",
        "SELECT overlay(point_id PLACING 'xx' FROM 1) FROM t",
        "SELECT translate(point_id, 't', 'u') FROM t",
        "SELECT to_hex(utc_seconds) FROM t",
        "SELECT encode(point_id, 'hex') FROM t",
        "SELECT to_char(ts, '%Y') FROM t",
        "SELECT levenshtein(point_id, tz) FROM t",
        "SELECT repeat('x', 3)",
        "SELECT lpad('x', 10)",
        "SELECT rpad('x', 10)",
        "SELECT arrow_cast(1, 'Int8')",
        "SELECT count(arrow_cast(NULL, 'FixedSizeBinary(100000000)')) FROM t",
        "SELECT named_struct('a', 1)",
        "SELECT struct(1, 2)",
        "SELECT version()",
        // Every spelling of `substr` becomes `SUBSTRING`, which is not planned.
        "SELECT substr(point_id, 1, 1) FROM t",
        "SELECT substring(point_id, 1, 1) FROM t",
        "SELECT substring(point_id FROM 1 FOR 1) FROM t",
    ] {
        assert_eq!(
            engine.query(sql, query_options()).await.unwrap_err(),
            Error::Sql,
            "{sql}"
        );
    }
    let out = run(
        &engine,
        "SELECT upper(point_id) AS u, lower(tz) AS l, length(point_id) AS n, \
         CAST(strpos(point_id, 't') AS BIGINT) AS p, TRIM(point_id) AS tr, left(point_id, 1) AS lf, \
         right(point_id, 1) AS rt, split_part(point_id, 'x', 1) AS sp, reverse(point_id) AS rv, \
         starts_with(point_id, 't') AS sw, date_part('year', ts) AS y, \
         date_trunc('hour', ts) AS h, abs(number_value) AS a, coalesce(number_value, 0) AS c, \
         round(number_value) AS r, arrow_typeof(ts) AS ty FROM t ORDER BY utc_seconds",
    )
    .await;
    assert_eq!(strings(&out, "u"), vec![Some("T".to_owned()); 4]);
    for column in ["tr", "lf", "rt", "sp", "rv"] {
        assert_eq!(
            strings(&out, column),
            vec![Some("t".to_owned()); 4],
            "{column}"
        );
    }
    assert_eq!(i64s(&out, "p"), [Some(1); 4]);
}

/// Window operators buffer their input without reserving it; the engine
/// reserves every input batch from the query's pool, so `UNION ALL` fan-out
/// into `count(*) OVER ()` is `Error::Limit` at the pool instead of
/// gigabytes of unaccounted memory.
#[tokio::test]
async fn window_input_beyond_the_pool_is_a_limit() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    const ROWS: usize = 4000;
    const LEN: usize = 1029;
    let value = point(
        "s",
        HistoryKind::Str,
        None,
        (0..ROWS)
            .map(|i| Kind::Str(format!("{i:05}{}", "x".repeat(LEN - 5))))
            .collect(),
    );
    capture(&value, &dir.path().join("s"), 1024).await;
    let engine = engine(&[("s", &dir.path().join("s"))]).await;
    let fan = |copies: usize| {
        let union = vec!["SELECT * FROM s"; copies].join(" UNION ALL ");
        format!(
            "SELECT max(length(string_value) + c) AS m FROM \
             (SELECT string_value, count(*) OVER () AS c FROM ({union}) u) q"
        )
    };
    let output = engine.query(&fan(2), query_options()).await.unwrap();
    assert_eq!(i64s(&all(&output), "m"), [Some((LEN + 2 * ROWS) as i64)]);
    assert!(
        output.pool_peak_bytes >= 2 * ROWS * LEN,
        "window input not reserved: {}",
        output.pool_peak_bytes
    );
    let copies = QUERY_MEMORY_BYTES / (ROWS * LEN) + 2;
    assert!(copies <= MAX_SQL_SET_OPERATIONS);
    assert_eq!(
        engine
            .query(&fan(copies), query_options())
            .await
            .unwrap_err(),
        Error::Limit
    );
}

/// A filtered equi-join whose predicate matches nothing never produces an
/// output batch: it evaluates rows² candidate pairs inside its own loop and
/// polls the probe side only once per batch, so Tokio's cooperative budget
/// alone would let it run to completion (seconds, unoptimized) after its
/// deadline while holding its admission permit. Leaves yield before every
/// batch, so the abort lands within one probe batch and the permit is back
/// before the call returns.
#[tokio::test]
async fn a_join_that_matches_nothing_still_stops_at_its_deadline() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = big(dir.path()).await;
    let sql = "SELECT count(*) AS n FROM big a JOIN big b ON a.point_id = b.point_id \
               AND sqrt(a.number_value) * ln(b.number_value + 2) < -1";
    let started = std::time::Instant::now();
    let options = QueryOptions::new().with_timeout(Duration::from_millis(200));
    assert_eq!(
        engine.query(sql, options).await.unwrap_err(),
        Error::Deadline
    );
    assert!(started.elapsed() < Duration::from_millis(200) + CANCEL_GRACE);
    let (x, y) = tokio::join!(
        engine.query("SELECT COUNT(*) AS n FROM big", query_options()),
        engine.query("SELECT COUNT(*) AS n FROM big", query_options())
    );
    for output in [x.unwrap(), y.unwrap()] {
        assert_eq!(i64s(&all(&output), "n"), [Some(MAX_ROWS as i64)]);
    }
}

/// Aggregates and window functions are allowlisted like scalars. Before,
/// `approx_percentile_cont`'s literal centroid count was allocated up front
/// and the first query below aborted the process (`memory allocation of
/// 1600000000000000 bytes failed`). `array_agg` and `string_agg` build a
/// value from every row, which a growing window frame makes quadratic.
#[tokio::test]
async fn aggregates_and_windows_outside_the_allowlists_are_rejected() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = small(dir.path()).await;
    for sql in [
        "SELECT approx_percentile_cont(number_value, 0.5, 100000000000000) FROM t",
        "SELECT approx_percentile_cont_with_weight(number_value, 1.0, 0.5, 1000000000) FROM t",
        "SELECT approx_percentile_cont(0.5, 100000000000000) WITHIN GROUP (ORDER BY number_value) \
         FROM t",
        "SELECT approx_percentile_cont(number_value, 0.5, 100000000000000) OVER () FROM t",
        "SELECT approx_percentile_cont(number_value, 0.5) FROM t",
        "SELECT array_agg(number_value) FROM t",
        "SELECT count(*) FROM (SELECT array_agg(number_value) OVER (ORDER BY ts) AS a FROM t) q",
        "SELECT string_agg(point_id, ',') FROM t",
        "SELECT nth_value(number_value, 2 ORDER BY ts) FROM t",
        "SELECT grouping(present) FROM t GROUP BY present",
    ] {
        assert_eq!(
            engine.query(sql, query_options()).await.unwrap_err(),
            Error::Sql,
            "{sql}"
        );
    }
    // Controls: allowed aggregates, including through aliases, and allowed
    // window functions and aggregates over windows.
    let out = run(
        &engine,
        "SELECT count(*) AS n, sum(number_value) AS s, avg(number_value) AS a, \
         median(number_value) AS m, var_samp(number_value) AS v, \
         approx_median(number_value) AS am, approx_distinct(utc_seconds) AS ad, \
         percentile_cont(0.5) WITHIN GROUP (ORDER BY number_value) AS p FROM t",
    )
    .await;
    assert_eq!(i64s(&out, "n"), [Some(4)]);
    assert_eq!(f64s(&out, "s"), [Some(6f64.to_bits())]);
    assert_eq!(f64s(&out, "m"), [Some(1.5f64.to_bits())]);
    assert_eq!(f64s(&out, "p"), [Some(1.5f64.to_bits())]);
    let out = run(
        &engine,
        "SELECT number_value - lag(number_value) OVER (ORDER BY ts) AS d, \
         sum(number_value) OVER (ORDER BY ts) AS s, \
         nth_value(number_value, 2) OVER (ORDER BY ts) AS nv, \
         CAST(row_number() OVER (ORDER BY ts) AS BIGINT) AS r, \
         CAST(ntile(2) OVER (ORDER BY ts) AS BIGINT) AS nt, \
         count(*) OVER () AS c FROM t ORDER BY ts",
    )
    .await;
    assert_eq!(
        f64s(&out, "d"),
        [
            None,
            Some(1f64.to_bits()),
            Some(1f64.to_bits()),
            Some(1f64.to_bits())
        ]
    );
    assert_eq!(
        f64s(&out, "s"),
        [0f64, 1., 3., 6.].map(|v| Some(v.to_bits()))
    );
    assert_eq!(
        f64s(&out, "nv"),
        [
            None,
            Some(1f64.to_bits()),
            Some(1f64.to_bits()),
            Some(1f64.to_bits())
        ]
    );
    assert_eq!(i64s(&out, "r"), [1, 2, 3, 4].map(Some));
    assert_eq!(i64s(&out, "nt"), [1, 1, 2, 2].map(Some));
    assert_eq!(i64s(&out, "c"), [Some(4); 4]);
}

/// A window repeats a value for every row of its partition, outside the
/// pool, so every window expression must be of a fixed-width type: before,
/// `first_value('<1 KB literal>') OVER ()` over a million-row join held
/// 2.5 GB with the pool at zero. The fixed-width columns a window adds are
/// reserved per row as its input arrives, so many of them over a large
/// join are `Error::Limit` at the pool.
#[tokio::test]
async fn window_outputs_are_fixed_width_and_reserved() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = small(dir.path()).await;
    let literal = "x".repeat(1000);
    for sql in [
        "SELECT first_value(point_id) OVER () FROM t".to_owned(),
        "SELECT max(point_id) OVER () FROM t".to_owned(),
        "SELECT lag(point_id) OVER (ORDER BY ts) FROM t".to_owned(),
        "SELECT lag(string_value, 1, 'x') OVER (ORDER BY ts) FROM t".to_owned(),
        format!("SELECT count(*) FROM (SELECT first_value('{literal}') OVER () AS v FROM t) q"),
        format!("SELECT max(length(v)) FROM (SELECT max('{literal}') OVER () AS v FROM t) q"),
    ] {
        assert_eq!(
            engine.query(&sql, query_options()).await.unwrap_err(),
            Error::Sql,
            "{sql}"
        );
    }
    let out = run(
        &engine,
        "SELECT first_value(ts) OVER (ORDER BY ts) = min(ts) OVER () AS f, \
         last_value(present) OVER (ORDER BY ts) AS l FROM t",
    )
    .await;
    assert_eq!(out.num_rows(), 4);

    let dir = tempfile::tempdir().unwrap();
    let engine = big(dir.path()).await;
    // A million join rows; each `sum(i) OVER ()` adds 9 bytes per row.
    let wide = |columns: usize| {
        let inner = joined(
            (0..columns).map(|i| format!("sum({i}) OVER () AS c{i}")),
            ", ",
        );
        let outer = joined((0..columns).map(|i| format!("max(c{i}) AS m{i}")), ", ");
        format!(
            "SELECT {outer} FROM (SELECT {inner} FROM big a JOIN big b \
             ON a.point_id = b.point_id WHERE a.number_value < 100) q"
        )
    };
    let output = engine.query(&wide(1), query_options()).await.unwrap();
    assert_eq!(i64s(&all(&output), "m0"), [Some(0)]);
    assert!(
        output.pool_peak_bytes >= 9 * 100 * MAX_ROWS,
        "window output not reserved: {}",
        output.pool_peak_bytes
    );
    let columns = QUERY_MEMORY_BYTES / (9 * 100 * MAX_ROWS) + 1;
    assert_eq!(
        engine
            .query(&wide(columns), query_options())
            .await
            .unwrap_err(),
        Error::Limit
    );
}

/// Regex compilation and matching allocate outside the pool and can take
/// seconds for one short pattern (`regexp_like(s, '(?i)\p{L}{200}')` took
/// 5.8 s and 581 MB; a 14 KB `LIKE` pattern took 6.6 s and 327 MB), and
/// `regexp_match` with nested groups grew its input about 250-fold. Every
/// regex-backed function, operator and pattern match is rejected; the
/// literal tests are the alternative, including `starts_with`, which the
/// optimizer itself turns into a prefix `LIKE`.
#[tokio::test]
async fn regex_functions_operators_and_pattern_matches_are_rejected() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = small(dir.path()).await;
    for sql in [
        "SELECT regexp_like(point_id, '^t') FROM t",
        "SELECT regexp_count(point_id, 't') FROM t",
        "SELECT regexp_instr(point_id, 't') FROM t",
        "SELECT regexp_match(point_id, '((.*))') FROM t",
        "SELECT count(*) FROM t WHERE point_id ~ '^t'",
        "SELECT count(*) FROM t WHERE point_id ~* '^T'",
        "SELECT count(*) FROM t WHERE point_id !~ '^x'",
        "SELECT count(*) FROM t WHERE point_id !~* '^X'",
        "SELECT count(*) FROM t WHERE point_id SIMILAR TO 't%'",
        "SELECT count(*) FROM t WHERE point_id NOT SIMILAR TO 'x%'",
        "SELECT count(*) FROM t WHERE point_id LIKE 't%'",
        "SELECT count(*) FROM t WHERE point_id LIKE '%a%a%a'",
        "SELECT count(*) FROM t WHERE point_id NOT LIKE 'x'",
        "SELECT count(*) FROM t WHERE point_id ILIKE 'T%'",
        "SELECT count(*) FROM t WHERE point_id NOT ILIKE 'x%'",
        "SELECT count(*) FROM t WHERE point_id LIKE point_id",
        "SELECT count(*) FROM t WHERE point_id ~~ 't%'",
        "SELECT count(*) FROM t WHERE point_id !~~* 'x%'",
    ] {
        assert_eq!(
            engine.query(sql, query_options()).await.unwrap_err(),
            Error::Sql,
            "{sql}"
        );
    }
    let out = run(
        &engine,
        "SELECT count(*) AS n FROM t WHERE starts_with(point_id, 't') AND \
         NOT starts_with(point_id, '%_\\') AND ends_with(point_id, 't') AND \
         contains(point_id, 't') AND strpos(point_id, 't') = 1 AND lower(point_id) = 't'",
    )
    .await;
    assert_eq!(i64s(&out, "n"), [Some(4)]);
}
