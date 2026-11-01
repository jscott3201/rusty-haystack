mod support;
#[path = "support/worst_case.rs"]
mod worst_case;
use haystack_analytics::{Dataset, Export, ExportOptions, Limits, MANIFEST_NAME};
use haystack_client::history::{HistoryStreamLimits, HistoryStreamOptions};
use haystack_core::codecs::history::*;
use support::*;
use worst_case::*;
// Cargo test runs these in one process; share the API admission capacity.
static TEST_ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

async fn capture(
    value: &HistoryReadResult,
    dir: &std::path::Path,
) -> haystack_analytics::ExportReceipt {
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
                    batch_rows: 3,
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
                batch_rows: 2,
                groups_per_part: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let receipt = export.wait().await.unwrap();
    server.finish().await;
    receipt
}
#[tokio::test]
async fn loopback_multiple_parts_preserve_independent_values_and_provenance() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let expected = result(7, 3);
    let receipt = capture(&expected, &path).await;
    assert!(receipt.manifest.parts.len() >= 3);
    assert_eq!(receipt.manifest.rows, 7);
    assert_eq!(receipt.manifest.manifest_version, 1);
    let mut reader = Dataset::open(&path).unwrap();
    assert!(!reader.semantically_complete());
    assert_eq!(reader.header().metadata, expected.metadata);
    assert_eq!(reader.header().metadata.graph.revision, 9007199254740993);
    assert_eq!(
        reader.header().metadata.history.generation,
        9007199254740995
    );
    assert!(!reader.header().metadata.capabilities.snapshots);
    let mut rows = vec![];
    while let Some(batch) = reader.next_batch().unwrap() {
        rows.extend(batch);
    }
    assert!(reader.semantically_complete());
    assert_same_samples(&rows, &expected.samples);
    assert!(path.join(MANIFEST_NAME).exists());
}
#[tokio::test]
async fn empty_complete_and_nonempty_terminal_batch_commit() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    for n in [0, 1, 2] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let expected = result(n, 0);
        let receipt = capture(&expected, &path).await;
        assert_eq!(receipt.manifest.rows, n as u64);
        let mut reader = Dataset::open(path).unwrap();
        let mut count = 0;
        while let Some(batch) = reader.next_batch().unwrap() {
            count += batch.len();
        }
        assert_eq!(count, n);
        assert!(reader.semantically_complete());
    }
}

#[tokio::test]
async fn scalar_bits_na_units_strings_and_offsets_have_independent_expectations() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    use haystack_core::kinds::{Kind, Number};
    use parquet::{
        arrow::arrow_reader::ParquetRecordBatchReaderBuilder,
        basic::{Compression, Encoding},
    };
    let values = [
        Kind::Number(Number::unitless(0.0)),
        Kind::Number(Number::new(-0.0, Some("fahrenheit".into()))),
        Kind::Number(Number::new(f64::INFINITY, Some("°F".into()))),
        Kind::Number(Number::unitless(f64::NEG_INFINITY)),
        Kind::Number(Number::unitless(f64::NAN)),
        Kind::NA,
        Kind::Number(Number::new(1.0000000000000002, Some("°F".into()))),
    ];
    let mut expected = result(values.len(), 0);
    expected.metadata.schema.kind = HistoryKind::Number;
    expected.metadata.schema.unit = Some("°F".into());
    for (sample, value) in expected.samples.iter_mut().zip(values) {
        sample.val = value;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let receipt = capture(&expected, &path).await;
    // Independent Parquet reader inspects physical values and encodings, without
    // using the product's readback conversion or HDateTime/Number equality.
    let mut bits = vec![];
    let mut units = vec![];
    for part in &receipt.manifest.parts {
        let builder = ParquetRecordBatchReaderBuilder::try_new(
            std::fs::File::open(path.join(&part.name)).unwrap(),
        )
        .unwrap();
        for group in builder.metadata().row_groups() {
            let col = group.column(6);
            assert_eq!(col.compression(), Compression::UNCOMPRESSED);
            assert!(col.encodings().any(|v| v == Encoding::PLAIN));
            assert!(
                col.encodings()
                    .all(|v| matches!(v, Encoding::PLAIN | Encoding::RLE))
            );
            assert!(col.dictionary_page_offset().is_none());
            assert!(col.statistics().is_none());
            assert!(col.bloom_filter_offset().is_none());
        }
        for batch in builder.build().unwrap() {
            use arrow_array::Array;
            let batch = batch.unwrap();
            let numbers = batch
                .column(6)
                .as_any()
                .downcast_ref::<arrow_array::Float64Array>()
                .unwrap();
            let strings = batch
                .column(8)
                .as_any()
                .downcast_ref::<arrow_array::StringArray>()
                .unwrap();
            for i in 0..batch.num_rows() {
                bits.push(numbers.is_valid(i).then(|| numbers.value(i).to_bits()));
                units.push(strings.is_valid(i).then(|| strings.value(i).to_owned()));
            }
        }
    }
    assert_eq!(
        bits,
        [
            Some(0x0000000000000000),
            Some(0x8000000000000000),
            Some(0x7ff0000000000000),
            Some(0xfff0000000000000),
            Some(0x7ff8000000000000),
            None,
            Some(0x3ff0000000000001)
        ]
    );
    assert_eq!(
        units,
        [
            None,
            Some("fahrenheit".into()),
            Some("°F".into()),
            None,
            None,
            None,
            Some("°F".into())
        ]
    );
    for (kind, values) in [
        (
            HistoryKind::Bool,
            vec![Kind::Bool(true), Kind::Bool(false), Kind::NA],
        ),
        (
            HistoryKind::Str,
            vec![
                Kind::Str("é\u{0000}\t\r\n\"\\🚀".into()),
                Kind::Str(String::new()),
                Kind::NA,
            ],
        ),
        (HistoryKind::Bool, vec![Kind::NA; 3]),
        (HistoryKind::Str, vec![Kind::NA; 3]),
    ] {
        let mut value = result(values.len(), 0);
        value.metadata.schema.kind = kind;
        for (sample, v) in value.samples.iter_mut().zip(values) {
            sample.val = v;
        }
        let path = dir.path().join(format!("{}", uuid::Uuid::new_v4()));
        capture(&value, &path).await;
        let mut reader = Dataset::open(path).unwrap();
        let mut actual = vec![];
        while let Some(batch) = reader.next_batch().unwrap() {
            actual.extend(batch);
        }
        assert_same_samples(&actual, &value.samples);
    }
}
#[tokio::test]
async fn timestamp_components_preserve_dst_fractional_endpoints_and_leap_fraction() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let cases = [
        (
            "2024-11-03",
            "New_York",
            "2024-11-03T00:00:00-04:00 New_York",
            "2024-11-04T00:00:00-05:00 New_York",
            vec![
                "2024-11-03T01:45:00-04:00 New_York",
                "2024-11-03T01:15:00-05:00 New_York",
            ],
        ),
        (
            "2016-12-31",
            "UTC",
            "2016-12-31T00:00:00Z UTC",
            "2017-01-01T00:00:00Z UTC",
            vec![
                "2016-12-31T23:59:59.999999999Z UTC",
                "2016-12-31T23:59:60.000000001Z UTC",
                "2016-12-31T23:59:60.999999999Z UTC",
            ],
        ),
        (
            "2024-06-01T00:00:00.123456789Z UTC,2024-06-01T00:00:01.987654321Z UTC",
            "UTC",
            "2024-06-01T00:00:00.123456789Z UTC",
            "2024-06-01T00:00:01.987654321Z UTC",
            vec![
                "2024-06-01T00:00:00.123456789Z UTC",
                "2024-06-01T00:00:01.987654320Z UTC",
            ],
        ),
    ];
    for (range, zone, start, end, times) in cases {
        let mut value = result(times.len(), 0);
        value.metadata.requested_range = range.into();
        value.metadata.schema.timezone = zone.into();
        value.metadata.start = date(start);
        value.metadata.end = date(end);
        value.metadata.evaluated_at = date(start);
        value.metadata.coverage.evicted_through = None;
        for (sample, ts) in value.samples.iter_mut().zip(times) {
            sample.ts = date(ts);
        }
        value.metadata.coverage.retained_start = value.samples.first().map(|s| s.ts.clone());
        value.metadata.coverage.retained_end = value.samples.last().map(|s| s.ts.clone());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        capture(&value, &path).await;
        let mut reader = Dataset::open(path).unwrap();
        let mut actual = vec![];
        while let Some(batch) = reader.next_batch().unwrap() {
            actual.extend(batch);
        }
        assert_eq!(actual.len(), value.samples.len());
        for (a, b) in actual.iter().zip(&value.samples) {
            assert_eq!(a.ts.dt.timestamp(), b.ts.dt.timestamp());
            assert_eq!(
                a.ts.dt.timestamp_subsec_nanos(),
                b.ts.dt.timestamp_subsec_nanos()
            );
            assert_eq!(
                a.ts.dt.offset().local_minus_utc(),
                b.ts.dt.offset().local_minus_utc()
            );
            assert_eq!(a.ts.tz_name, zone);
        }
        if zone == "New_York" {
            assert_eq!(actual[0].ts.dt.offset().local_minus_utc(), -14400);
            assert_eq!(actual[1].ts.dt.offset().local_minus_utc(), -18000);
        } else if range == "2016-12-31" {
            assert_eq!(actual[1].ts.dt.timestamp_subsec_nanos(), 1_000_000_001);
            assert_eq!(actual[2].ts.dt.timestamp_subsec_nanos(), 1_999_999_999);
        }
    }
}
#[tokio::test]
async fn noncomplete_and_bad_trailing_data_never_publish_prefixes() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    for terminal in [
        HistoryTerminal::Limited(HistoryReason::Rows),
        HistoryTerminal::Interrupted(HistoryReason::GenerationChanged),
        HistoryTerminal::Failed(HistoryReason::Provider),
        HistoryTerminal::Complete,
    ] {
        let mut value = result(17, 1);
        value.terminal = terminal;
        let mut bytes = encoded(&value);
        if terminal == HistoryTerminal::Complete {
            bytes.extend_from_slice(b"garbage\n");
        }
        let server = Server::bytes(bytes, vec![1, 4096], "").await;
        let client = client(&server.url).await;
        let source = client
            .his_read_stream(
                &request(),
                HistoryStreamOptions {
                    limits: HistoryStreamLimits {
                        batch_rows: 2,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let mut export = Export::start(
            source,
            &path,
            ExportOptions {
                limits: Limits {
                    groups_per_part: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(
            export.wait().await,
            Err(haystack_analytics::Error::Source(_))
        ));
        assert!(!path.join(MANIFEST_NAME).exists());
        assert!(std::fs::read_dir(&path).unwrap().count() >= 3);
        assert!(Dataset::open(path).is_err());
        server.finish().await;
    }
}

async fn try_capture(
    value: &HistoryReadResult,
    dir: &std::path::Path,
    limits: Limits,
) -> Result<haystack_analytics::ExportReceipt, haystack_analytics::Error> {
    let server = Server::bytes(encoded(value), vec![4096], "").await;
    let client = client(&server.url).await;
    let session = client
        .his_read_stream(
            &request(),
            HistoryStreamOptions {
                limits: HistoryStreamLimits {
                    batch_rows: 16,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let outcome = match Export::start(
        session,
        dir,
        ExportOptions {
            limits,
            ..Default::default()
        },
    ) {
        Ok(mut export) => export.wait().await,
        Err(error) => Err(error),
    };
    server.finish().await;
    outcome
}
/// Byte-bound limits: a two-sample `batch_bytes` against ~1 KiB strings, and a
/// part only a little larger than its writer reserve.
fn byte_limits() -> Limits {
    Limits {
        batch_bytes: 4096,
        footer_bytes: 32 * 1024,
        part_bytes: 128 * 1024,
        ..Default::default()
    }
}
#[tokio::test]
async fn byte_limits_split_batches_and_parts_and_round_trip() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let value = result(200, 150);
    let limits = byte_limits();
    let receipt = try_capture(&value, &path, limits.clone()).await.unwrap();
    let parts = &receipt.manifest.parts;
    assert!(parts.len() >= 2, "{} parts", parts.len());
    // Parts closed by bytes, not by the group count; batches split by bytes,
    // not by `batch_rows`.
    for part in &parts[..parts.len() - 1] {
        assert!(part.groups < limits.groups_per_part, "{part:?}");
    }
    for part in parts {
        assert!(part.bytes <= limits.part_bytes);
        assert!(part.rows <= 2 * part.groups as u64, "{part:?}");
    }
    assert_eq!(parts.iter().map(|p| p.rows).sum::<u64>(), 200);
    assert!(receipt.stats.peak_source_batch_bytes > limits.batch_bytes as u64);
    let mut reader = Dataset::open(&path).unwrap();
    let mut rows = vec![];
    while let Some(batch) = reader.next_batch().unwrap() {
        assert!(batch.len() <= 2);
        rows.extend(batch);
    }
    assert!(reader.semantically_complete());
    assert_same_samples(&rows, &value.samples);
}
#[tokio::test]
async fn write_side_limits_fail_closed_without_publishing() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let value = result(200, 150);
    let base = byte_limits();
    let cases = [
        (
            "total_bytes",
            Limits {
                total_bytes: base.part_bytes,
                ..base.clone()
            },
        ),
        (
            "parts",
            Limits {
                parts: 1,
                ..base.clone()
            },
        ),
    ];
    // `Limits::validate` derives the manifest and footer floors from `parts`
    // and `groups_per_part`, and the manifest floor reserves more than the
    // largest header the bounded-history codec admits (measured by
    // construction with `worst_case_results`, whose headers publish at the
    // minimum limits in `src/tests.rs`), so no validating `Limits` can overrun
    // either after pulling the source. The values that used to overrun here
    // are refused up front instead; the writer-side backstops are unit-tested
    // with unvalidated limits in `src/tests.rs`.
    for (name, limits) in [
        (
            "manifest_bytes",
            Limits {
                manifest_bytes: 1024,
                ..base.clone()
            },
        ),
        (
            "footer_bytes",
            Limits {
                footer_bytes: 128,
                ..Default::default()
            },
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        assert_eq!(
            try_capture(&value, &path, limits).await.unwrap_err(),
            haystack_analytics::Error::Options,
            "{name}"
        );
        assert!(!path.exists(), "{name}");
    }
    for (name, limits) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        assert_eq!(
            try_capture(&value, &path, limits).await.unwrap_err(),
            haystack_analytics::Error::Limit,
            "{name}"
        );
        assert!(!path.join(MANIFEST_NAME).exists(), "{name}");
        assert!(Dataset::open(&path).is_err(), "{name}");
    }
}
/// Extreme but admitted headers export and reopen to exactly the served
/// header: every worst-case header a real session can carry (the client
/// refuses non-ASCII and CR/LF ranges), and zeroed graph and history state.
#[tokio::test]
async fn extreme_admitted_headers_round_trip() {
    use haystack_core::codecs::{Codec, zinc::ZincCodec};
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let mut cases: Vec<_> = worst_case_results(4)
        .into_iter()
        .filter(|value| {
            let range = &value.metadata.requested_range;
            range.is_ascii() && !range.contains(['\r', '\n'])
        })
        .collect();
    // Tab, VT, FF and space.
    assert_eq!(cases.len(), 4);
    let mut zero = result(4, 0);
    zero.metadata.graph.incarnation = [0; 16];
    zero.metadata.graph.revision = 0;
    zero.metadata.graph.catalog_generation = 0;
    zero.metadata.history.generation = 0;
    zero.metadata.policy_observation = [0xff; 16];
    cases.push(zero);
    let text = |header: &HistoryReadHeader| {
        ZincCodec
            .encode_grid(&result_header_grid(header).unwrap())
            .unwrap()
    };
    for value in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let receipt = capture(&value, &path).await;
        let served = header(&value);
        assert_eq!(receipt.manifest.source_header, text(&served));
        let mut reader = Dataset::open(&path).unwrap();
        // `HDateTime ==` compares instants only; the text pins zones and offsets.
        assert_eq!(reader.header(), &served);
        assert_eq!(text(reader.header()), text(&served));
        let mut rows = vec![];
        while let Some(batch) = reader.next_batch().unwrap() {
            rows.extend(batch);
        }
        assert!(reader.semantically_complete());
        assert_same_samples(&rows, &value.samples);
    }
}
/// The client admits a non-ASCII point id, but the recorded header profile is
/// ASCII outside quoted strings (Haystack Ref ids are ASCII), so the reader
/// would refuse the dataset. The export fails before creating the directory.
#[tokio::test]
async fn header_the_reader_would_refuse_fails_before_publishing() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let mut value = result(2, 0);
    value.metadata.id = "\u{e9}".into();
    let server = Server::bytes(encoded(&value), vec![8192], "").await;
    let session = client(&server.url)
        .await
        .his_read_stream(&request_for(&value), Default::default())
        .await
        .unwrap();
    assert_eq!(session.header(), &header(&value));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    assert_eq!(
        Export::start(session, &path, ExportOptions::default()).err(),
        Some(haystack_analytics::Error::Invalid)
    );
    assert!(!path.exists());
}
