#[path = "../tests/support/mod.rs"]
mod support;
use crate::{
    checkpoint::{Action, Checkpoints, Hook, Phase},
    *,
};
use haystack_client::history::{HistoryStreamLimits, HistoryStreamOptions};
use std::time::Duration;
use support::*;
async fn pending(future: impl std::future::Future) -> bool {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| std::task::Poll::Ready(future.as_mut().poll(cx).is_pending())).await
}
thread_local! {
    /// Lets a test observe guard behaviour without the production panic
    /// backstop in `crate::contain`; only the current thread is affected.
    pub(crate) static BACKSTOP_DISABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
static TEST_ADMISSION: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

async fn start(
    path: &std::path::Path,
    hook: std::sync::Arc<Hook>,
    options: Option<ExportOptions>,
) -> (Export, Server) {
    let server = Server::bytes(encoded(&result(17, 1)), vec![1, 59, 1024], "").await;
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
    let mut options = options.unwrap_or_default();
    options.limits.groups_per_part = 1;
    options.checkpoints = Checkpoints { hook: Some(hook) };
    (Export::start(source, path, options).unwrap(), server)
}
#[tokio::test]
async fn held_writer_stops_pulls_and_cancelled_wait_retains_join() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let hook = Hook::new(Phase::Write, 2, Action::Hold);
    let (mut export, server) = start(&path, hook.clone(), None).await;
    tokio::time::timeout(Duration::from_secs(5), hook.reached.notified())
        .await
        .unwrap();
    assert_eq!(hook.count(Phase::Pull), 2);
    // Polling and abandoning the wait cannot lose or duplicate writer ownership.
    assert!(pending(export.wait()).await);
    export.cancel();
    assert!(pending(export.close()).await);
    assert_eq!(hook.count(Phase::Pull), 2);
    hook.release();
    assert_eq!(export.close().await.unwrap_err(), Error::Cancelled);
    assert!(!path.join(MANIFEST_NAME).exists());
    assert!(Dataset::open(path).is_err());
    server.finish().await;
}
#[tokio::test]
async fn deterministic_io_failures_classify_publication_boundary() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    for phase in [
        Phase::Pull,
        Phase::WaitingPull,
        Phase::Write,
        Phase::Footer,
        Phase::FileSync,
        Phase::SourceJoined,
        Phase::DirectorySync,
        Phase::ManifestWrite,
        Phase::ManifestSync,
        Phase::BeforePublish,
        Phase::AfterPublish,
        Phase::PublishedDirectorySync,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let hook = Hook::new(phase, 1, Action::Fail);
        let (mut export, server) = start(&path, hook, None).await;
        let error = export.wait().await.unwrap_err();
        if matches!(phase, Phase::AfterPublish | Phase::PublishedDirectorySync) {
            assert!(
                matches!(error, Error::PublishedUncertain { .. }),
                "{phase:?}: {error:?}"
            );
            let mut reader = Dataset::open(path).unwrap();
            let mut count = 0;
            while let Some(batch) = reader.next_batch().unwrap() {
                count += batch.len();
            }
            assert_eq!(count, 17);
            assert!(reader.semantically_complete());
        } else {
            assert_eq!(error, Error::Io(std::io::ErrorKind::Other), "{phase:?}");
            assert!(!path.join(MANIFEST_NAME).exists());
            assert!(Dataset::open(path).is_err());
        }
        server.finish().await;
    }
}
#[tokio::test]
async fn cancellation_at_every_boundary_retains_postpublication_success() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    for phase in [
        Phase::Pull,
        Phase::WaitingPull,
        Phase::Write,
        Phase::Footer,
        Phase::FileSync,
        Phase::SourceJoined,
        Phase::DirectorySync,
        Phase::ManifestWrite,
        Phase::ManifestSync,
        Phase::BeforePublish,
        Phase::AfterPublish,
        Phase::PublishedDirectorySync,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let token = tokio_util::sync::CancellationToken::new();
        let hook = Hook::new(phase, 1, Action::Cancel(token.clone()));
        let (mut export, server) = start(
            &path,
            hook,
            Some(ExportOptions {
                cancellation: token,
                ..Default::default()
            }),
        )
        .await;
        let outcome = export.wait().await;
        if matches!(phase, Phase::AfterPublish | Phase::PublishedDirectorySync) {
            assert_eq!(outcome.unwrap().manifest.rows, 17);
            assert!(Dataset::open(path).is_ok());
        } else {
            assert_eq!(outcome.unwrap_err(), Error::Cancelled, "{phase:?}");
            assert!(!path.join(MANIFEST_NAME).exists());
        }
        server.finish().await;
    }
}

fn forged_dataset(
    path: &std::path::Path,
    header: &haystack_core::codecs::history::HistoryReadHeader,
    batches: Vec<arrow_array::RecordBatch>,
) {
    let mut writer = crate::writer::Writer::new(
        path.to_owned(),
        header,
        Limits::default(),
        uuid::Uuid::new_v4().to_string(),
        Checkpoints::default(),
    )
    .unwrap();
    writer.verify_parts = false;
    for batch in batches {
        writer.write(batch).unwrap();
    }
    writer.prepare().unwrap();
    writer.publish(|| Ok(())).unwrap();
}
fn header(
    value: &haystack_core::codecs::history::HistoryReadResult,
) -> haystack_core::codecs::history::HistoryReadHeader {
    haystack_core::codecs::history::HistoryReadHeader {
        metadata: value.metadata.clone(),
        terminal: value.terminal,
        returned_count: value.samples.len() as u64,
    }
}
#[test]
fn hash_valid_semantic_corruption_is_rejected_and_never_complete() {
    use arrow_array::{
        ArrayRef, BooleanArray, Int32Array, Int64Array, RecordBatch, StringArray, UInt32Array,
    };
    use std::sync::Arc;
    let value = result(3, 0);
    let header = header(&value);
    let good = crate::format::batch(&header, &value.samples, &Limits::default()).unwrap();
    let cases: Vec<(usize, ArrayRef)> = vec![
        (
            0,
            Arc::new(StringArray::from(vec!["wrong", "point", "point"])),
        ),
        (1, Arc::new(Int64Array::from(vec![0, 1, 2]))),
        (
            1,
            Arc::new(Int64Array::from(vec![1717200000, 1717200000, 1717200002])),
        ),
        (2, Arc::new(UInt32Array::from(vec![2_000_000_000, 0, 0]))),
        (3, Arc::new(Int32Array::from(vec![3600, 0, 0]))),
        (4, Arc::new(BooleanArray::from(vec![false, true, true]))),
        (
            5,
            Arc::new(BooleanArray::from(vec![Some(true), None, None])),
        ),
        (
            7,
            Arc::new(StringArray::from(vec![None, Some("x"), Some("y")])),
        ),
        (8, Arc::new(StringArray::from(vec![Some("°F"), None, None]))),
    ];
    for (column, array) in cases {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad");
        let mut cols = good.columns().to_vec();
        cols[column] = array;
        let batch = RecordBatch::try_new(good.schema(), cols).unwrap();
        forged_dataset(&path, &header, vec![batch]);
        let mut reader = Dataset::open(path).unwrap();
        assert!(reader.next_batch().is_err(), "column {column}");
        assert!(!reader.semantically_complete());
        assert!(reader.next_batch().is_err());
    }
    // Contradictory complete coverage is detected only after all rows, so an
    // open or partially consumed reader must never claim semantic completion.
    let mut bad_header = header.clone();
    bad_header.metadata.coverage.retained_count = 4;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("coverage");
    forged_dataset(&path, &bad_header, vec![good]);
    let mut reader = Dataset::open(path).unwrap();
    assert_eq!(reader.next_batch().unwrap().unwrap().len(), 3);
    assert!(!reader.semantically_complete());
    assert!(reader.next_batch().is_err());
    assert!(!reader.semantically_complete());
}
#[test]
fn unsupported_values_and_conversion_limits_fail_closed() {
    use haystack_core::kinds::Kind;
    let value = result(1, 0);
    let header = header(&value);
    for val in [
        Kind::None,
        Kind::Ref(haystack_core::kinds::HRef::from_val("x")),
        Kind::List(vec![]),
        Kind::Null,
        Kind::Int(1),
        Kind::Float(haystack_core::kinds::Float::new(1.0)),
        Kind::Remove,
        Kind::Marker,
        Kind::Bool(true),
    ] {
        let mut samples = value.samples.clone();
        samples[0].val = val;
        assert!(crate::format::batch(&header, &samples, &Limits::default()).is_err());
    }
    let mut samples = value.samples.clone();
    samples[0].val = Kind::Str("x".repeat(32769));
    assert_eq!(
        crate::format::batch(&header, &samples, &Limits::default()).unwrap_err(),
        Error::Limit
    );
    let limits = Limits {
        batch_rows: 0,
        ..Default::default()
    };
    assert_eq!(limits.validate().unwrap_err(), Error::Options);
}
#[test]
fn manifest_and_part_corruption_are_rejected_before_rows() {
    let value = result(3, 0);
    let header = header(&value);
    let batch = crate::format::batch(&header, &value.samples, &Limits::default()).unwrap();
    for case in 0..15 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad");
        forged_dataset(&path, &header, vec![batch.clone()]);
        let manifest_path = path.join(MANIFEST_NAME);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let part = path.join("part-0000.parquet");
        match case {
            0 => manifest["manifest_version"] = 2.into(),
            1 => manifest["schema_version"] = 2.into(),
            2 => manifest["parts"][0]["name"] = "../escape.parquet".into(),
            3 => manifest["parts"][0]["ordinal"] = 1.into(),
            4 => manifest["parts"][0]["sha256"] = "a".repeat(64).into(),
            5 => manifest["parts"][0]["bytes"] = 1.into(),
            6 => manifest["parts"][0]["rows"] = 4.into(),
            7 => manifest["rows"] = 4.into(),
            8 => {
                let first = manifest["parts"][0].clone();
                manifest["parts"].as_array_mut().unwrap().push(first);
            }
            9 => {
                std::fs::remove_file(part).unwrap();
            }
            10 => {
                let f = std::fs::OpenOptions::new().write(true).open(part).unwrap();
                f.set_len(13).unwrap();
            }
            11 => {
                let mut bytes = std::fs::read(&part).unwrap();
                bytes[20] ^= 1;
                std::fs::write(part, bytes).unwrap();
            }
            12 => manifest["consistency"] = "atomicSnapshot".into(),
            13 => manifest["limits"]["batch_rows"] = 1_000_000.into(),
            14 => manifest["unknown"] = true.into(),
            _ => unreachable!(),
        }
        std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(Dataset::open(path).is_err(), "case {case}");
    }
    // Hash-valid but incompatible physical schema is also rejected.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("schema");
    let mut fields: Vec<_> = batch
        .schema()
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    fields[0] = arrow_schema::Field::new("foreign_id", arrow_schema::DataType::Utf8, false);
    let bad = arrow_array::RecordBatch::try_new(
        std::sync::Arc::new(arrow_schema::Schema::new(fields)),
        batch.columns().to_vec(),
    )
    .unwrap();
    forged_dataset(&path, &header, vec![bad]);
    assert!(Dataset::open(path).is_err());
}
#[tokio::test]
async fn fresh_destination_and_manifest_publication_never_clobber() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("exists");
    std::fs::create_dir(&existing).unwrap();
    std::fs::write(existing.join("sentinel"), b"keep").unwrap();
    let hook = Hook::new(Phase::Write, 1, Action::Fail);
    let (mut export, server) = start(&existing, hook, None).await;
    assert_eq!(
        export.wait().await.unwrap_err(),
        Error::Io(std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(std::fs::read(existing.join("sentinel")).unwrap(), b"keep");
    server.finish().await;
    let path = dir.path().join("race");
    let hook = Hook::new(Phase::BeforePublish, 1, Action::Hold);
    let (mut export, server) = start(&path, hook.clone(), None).await;
    tokio::time::timeout(Duration::from_secs(5), hook.reached.notified())
        .await
        .unwrap();
    std::fs::write(path.join(MANIFEST_NAME), b"existing").unwrap();
    hook.release();
    assert_eq!(
        export.wait().await.unwrap_err(),
        Error::Io(std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(
        std::fs::read(path.join(MANIFEST_NAME)).unwrap(),
        b"existing"
    );
    server.finish().await;
}

#[tokio::test(start_paused = true)]
async fn absolute_deadline_waits_for_owned_filesystem_work() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    // Pause only after loopback setup so Tokio's idle-time advance cannot
    // expire HTTP deadlines while the native network is progressing.
    tokio::time::resume();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deadline");
    let hook = Hook::new(Phase::Write, 1, Action::Hold);
    let (mut export, server) = start(
        &path,
        hook.clone(),
        Some(ExportOptions {
            timeout: Duration::from_secs(30),
            ..Default::default()
        }),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), hook.reached.notified())
        .await
        .unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    assert!(pending(export.wait()).await);
    assert_eq!(hook.count(Phase::Pull), 1);
    hook.release();
    assert_eq!(export.wait().await.unwrap_err(), Error::Deadline);
    tokio::time::resume();
    assert!(!path.join(MANIFEST_NAME).exists());
    server.finish().await;
}
// Process-kill evidence is Unix-only: Windows durability is not qualified.
#[cfg(unix)]
#[test]
fn crash_child() {
    let Ok(stage) = std::env::var("HAYSTACK_ANALYTICS_CRASH_STAGE") else {
        return;
    };
    let path = std::path::PathBuf::from(std::env::var_os("HAYSTACK_ANALYTICS_CHILD_PATH").unwrap());
    let marker =
        std::path::PathBuf::from(std::env::var_os("HAYSTACK_ANALYTICS_CHILD_MARKER").unwrap());
    let phase = match stage.as_str() {
        "before" => Phase::BeforePublish,
        "after" => Phase::AfterPublish,
        _ => panic!("invalid child stage"),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let hook = Hook::new(phase, 1, Action::CrashReady(marker));
        let (mut export, _server) = start(&path, hook, None).await;
        let _ = export.wait().await;
    });
    panic!("parent must kill child at checkpoint");
}
#[cfg(unix)]
#[test]
fn process_kill_before_and_after_publication_reopens_exactly() {
    for stage in ["before", "after"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let marker = dir.path().join("ready");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::crash_child", "--nocapture"])
            .env("HAYSTACK_ANALYTICS_CRASH_STAGE", stage)
            .env("HAYSTACK_ANALYTICS_CHILD_PATH", &path)
            .env("HAYSTACK_ANALYTICS_CHILD_MARKER", &marker)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !marker.exists() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("child exited before checkpoint: {status}");
            }
            if std::time::Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("child never reached checkpoint");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        if stage == "before" {
            assert!(Dataset::open(path).is_err());
        } else {
            let mut reader = Dataset::open(path).unwrap();
            let mut samples = vec![];
            while let Some(batch) = reader.next_batch().unwrap() {
                samples.extend(batch);
            }
            assert_same_samples(&samples, &result(17, 1).samples);
            assert!(reader.semantically_complete());
        }
    }
}

mod native {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        process::{Command, Stdio},
    };
    fn barrier(name: &str) {
        println!("ANALYTICS_{name}");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        assert!(!line.is_empty());
    }
    #[test]
    fn memory_child() {
        let Ok(url) = std::env::var("HAYSTACK_ANALYTICS_MEMORY_URL") else {
            return;
        };
        let warm_url = std::env::var("HAYSTACK_ANALYTICS_WARM_URL").unwrap();
        let mode = std::env::var("HAYSTACK_ANALYTICS_MEMORY_MODE").unwrap();
        let path =
            std::path::PathBuf::from(std::env::var_os("HAYSTACK_ANALYTICS_CHILD_PATH").unwrap());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = HistoryStreamOptions {
                limits: HistoryStreamLimits {
                    batch_rows: 32,
                    batch_bytes: 65536,
                    ..Default::default()
                },
                timeout: Duration::from_secs(60),
                ..Default::default()
            };
            let export_options = ExportOptions {
                limits: Limits {
                    batch_rows: 32,
                    batch_bytes: 65536,
                    groups_per_part: 4,
                    part_bytes: 512 * 1024,
                    ..Default::default()
                },
                ..Default::default()
            };
            let warm = client(&warm_url)
                .await
                .his_read_stream(&request(), options.clone())
                .await
                .unwrap();
            Export::start(warm, path.join("warm"), export_options.clone())
                .unwrap()
                .wait()
                .await
                .unwrap();
            barrier("BASELINE");
            let mut source = client(&url)
                .await
                .his_read_stream(&request(), options)
                .await
                .unwrap();
            if mode == "export" {
                let hook = Hook::new(Phase::BeforePublish, 1, Action::Hold);
                let mut export = Export::start(
                    source,
                    path.join("capture"),
                    ExportOptions {
                        checkpoints: Checkpoints {
                            hook: Some(hook.clone()),
                        },
                        ..export_options
                    },
                )
                .unwrap();
                hook.reached.notified().await;
                barrier("READY");
                hook.release();
                let receipt = export.wait().await.unwrap();
                assert!(receipt.stats.peak_source_batch_bytes <= 65536);
                assert!(receipt.stats.peak_arrow_bytes <= 65536);
                println!(
                    "ANALYTICS_STATS rows={} bytes={} parts={} stats={:?}",
                    receipt.manifest.rows,
                    receipt.manifest.parts.iter().map(|p| p.bytes).sum::<u64>(),
                    receipt.manifest.parts.len(),
                    receipt.stats
                );
            } else {
                let mut retained = vec![];
                loop {
                    let batch = source.next().await;
                    retained.extend(batch.samples);
                    if let Some(end) = batch.end {
                        assert_eq!(
                            end,
                            haystack_client::history::HistoryStreamEnd::Remote(
                                haystack_core::codecs::history::HistoryTerminal::Complete
                            )
                        );
                        break;
                    }
                }
                source.close().await;
                barrier("READY");
                std::hint::black_box(&retained);
            }
        });
    }
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn read_barrier(output: &mut impl BufRead, name: &str) {
        loop {
            let mut line = String::new();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "child exited before {name}"
            );
            if line.contains(&format!("ANALYTICS_{name}")) {
                break;
            }
        }
    }
    fn rss(pid: u32) -> u64 {
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "native child RSS unavailable: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    async fn measure(mode: &str, rows: usize) -> u64 {
        use haystack_core::kinds::Kind;
        let mut value = result(rows, 0);
        for sample in &mut value.samples {
            sample.val = Kind::Str("x".repeat(1000));
        }
        let bytes = encoded(&value);
        let wire_bytes = bytes.len();
        let server = Server::bytes(bytes, vec![8192], "").await;
        let warm = Server::bytes(encoded(&result(32, 0)), vec![8192], "").await;
        let dir = tempfile::tempdir().unwrap();
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::native::memory_child", "--nocapture"])
                .env("HAYSTACK_ANALYTICS_MEMORY_URL", &server.url)
                .env("HAYSTACK_ANALYTICS_WARM_URL", &warm.url)
                .env("HAYSTACK_ANALYTICS_MEMORY_MODE", mode)
                .env("HAYSTACK_ANALYTICS_CHILD_PATH", dir.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let mut output = BufReader::new(child.0.stdout.take().unwrap());
        read_barrier(&mut output, "BASELINE");
        let baseline = rss(child.0.id());
        writeln!(child.0.stdin.as_mut().unwrap(), "go").unwrap();
        read_barrier(&mut output, "READY");
        let mut retained = 0;
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            retained = retained.max(rss(child.0.id()));
        }
        writeln!(child.0.stdin.as_mut().unwrap(), "finish").unwrap();
        let mut rest = String::new();
        output.read_to_string(&mut rest).unwrap();
        print!("{rest}");
        assert!(child.0.wait().unwrap().success());
        server.finish().await;
        warm.finish().await;
        let delta = retained.saturating_sub(baseline);
        println!(
            "ANALYTICS_RSS mode={mode} rows={rows} wire_bytes={wire_bytes} baseline_kib={baseline} retained_kib={retained} delta_kib={delta}"
        );
        delta
    }
    /// Native RSS is separate from owned-buffer accounting and does not qualify
    /// other allocators, runtimes, machines or concurrent process pressure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "explicit isolated native RSS qualification"]
    async fn memory_scales_with_fixed_batches_groups_and_parts() {
        // Predeclared: three fresh process pairs, fixed batch/group/part limits;
        // export large-minus-small <=4MiB; collecting control grows >=6MiB.
        for repeat in 0..3 {
            let small = measure("export", 500).await;
            let large = measure("export", 9000).await;
            let control_small = measure("collect", 500).await;
            let control_large = measure("collect", 9000).await;
            assert!(
                large.saturating_sub(small) <= 4096,
                "repeat {repeat}: export retained RSS scaled with output"
            );
            assert!(
                control_large.saturating_sub(control_small) >= 6144,
                "repeat {repeat}: positive control did not distinguish accumulation"
            );
        }
    }
}

#[tokio::test]
async fn drop_requests_cleanup_while_worker_ownership_is_retained() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drop");
    let hook = Hook::new(Phase::Write, 1, Action::Hold);
    let (export, server) = start(&path, hook.clone(), None).await;
    tokio::time::timeout(Duration::from_secs(5), hook.reached.notified())
        .await
        .unwrap();
    drop(export);
    assert_eq!(hook.count(Phase::Cleanup), 0);
    assert_eq!(hook.count(Phase::Pull), 1);
    hook.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        while hook.count(Phase::Cleanup) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!path.join(MANIFEST_NAME).exists());
    server.finish().await;
}
#[tokio::test]
async fn admission_has_no_wait_queue_and_releases_after_close() {
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut exports = vec![];
    for n in 0..4 {
        let hook = Hook::new(Phase::Write, 1, Action::Hold);
        let (export, server) = start(&dir.path().join(n.to_string()), hook.clone(), None).await;
        tokio::time::timeout(Duration::from_secs(5), hook.reached.notified())
            .await
            .unwrap();
        exports.push((export, server, hook));
    }
    let server = Server::bytes(encoded(&result(1, 0)), vec![8192], "").await;
    let source = client(&server.url)
        .await
        .his_read_stream(&request(), Default::default())
        .await
        .unwrap();
    assert!(matches!(
        Export::start(source, dir.path().join("fifth"), Default::default()),
        Err(Error::Capacity)
    ));
    assert!(!dir.path().join("fifth").exists());
    server.finish().await;
    for (mut export, server, hook) in exports {
        export.cancel();
        hook.release();
        assert_eq!(export.close().await.unwrap_err(), Error::Cancelled);
        server.finish().await;
    }
    let hook = Hook::new(Phase::Pull, 1, Action::Fail);
    let (mut export, server) = start(&dir.path().join("after"), hook, None).await;
    assert_eq!(
        export.wait().await.unwrap_err(),
        Error::Io(std::io::ErrorKind::Other)
    );
    server.finish().await;
}

/// A dataset written by the real writer (self-verification on).
fn written_dataset(
    path: &std::path::Path,
    value: &haystack_core::codecs::history::HistoryReadResult,
    rows_per_batch: usize,
) {
    let header = header(value);
    let mut writer = crate::writer::Writer::new(
        path.to_owned(),
        &header,
        Limits::default(),
        uuid::Uuid::new_v4().to_string(),
        Checkpoints::default(),
    )
    .unwrap();
    for chunk in value.samples.chunks(rows_per_batch) {
        writer
            .write(crate::format::batch(&header, chunk, &Limits::default()).unwrap())
            .unwrap();
    }
    writer.prepare().unwrap();
    writer.publish(|| Ok(())).unwrap();
}
fn drain(path: &std::path::Path) -> Result<Vec<haystack_core::codecs::history::HistorySample>> {
    let mut reader = Dataset::open(path)?;
    let mut rows = vec![];
    while let Some(batch) = reader.next_batch()? {
        rows.extend(batch);
    }
    assert!(reader.semantically_complete());
    Ok(rows)
}
/// Number point with units, NA and unitless samples: optional columns carry
/// mixed definition levels, so both RLE and bit-packed runs are exercised.
fn mixed_numbers(rows: usize) -> haystack_core::codecs::history::HistoryReadResult {
    use haystack_core::kinds::{Kind, Number};
    let mut value = result(rows, 0);
    value.metadata.schema.kind = haystack_core::codecs::history::HistoryKind::Number;
    value.metadata.schema.unit = Some("°F".into());
    for (i, sample) in value.samples.iter_mut().enumerate() {
        sample.val = match i % 4 {
            0 => Kind::Number(Number::new(i as f64 + 0.5, Some("°F".into()))),
            1 => Kind::NA,
            2 => Kind::Number(Number::unitless(-(i as f64))),
            _ => Kind::Number(Number::new(f64::INFINITY, Some("°F".into()))),
        };
    }
    value
}
fn rewrite_manifest(path: &std::path::Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let file = path.join(MANIFEST_NAME);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    edit(&mut manifest);
    std::fs::write(file, serde_json::to_vec(&manifest).unwrap()).unwrap();
}
/// Deterministic xorshift64 for reproducible mutation sampling.
fn next_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}
#[test]
fn hostile_byte_mutations_are_rejected_by_guards_without_panicking() {
    // Disable the production catch_unwind backstop on this thread so any
    // upstream panic reaches this test's own catch_unwind: the assertion is
    // that the guards reject every mutation before upstream can panic.
    BACKSTOP_DISABLED.with(|v| v.set(true));
    let mut seed = 0x9e37_79b9_7f4a_7c15;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let value = mixed_numbers(10);
    written_dataset(&path, &value, 4);
    let part_path = path.join("part-0000.parquet");
    let part = std::fs::read(&part_path).unwrap();
    let manifest = std::fs::read(path.join(MANIFEST_NAME)).unwrap();
    let recorded: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let hash = recorded["parts"][0]["sha256"].as_str().unwrap().to_owned();
    let manifest = String::from_utf8(manifest).unwrap();
    let mut panics = vec![];
    let mut accepted = 0;
    // Every byte of the part, with the manifest hash recomputed so the
    // mutation reaches the footer, layout, page and level guards and then
    // decoding. 0xff at every byte (both reviewer repros were 0xff), plus one
    // further value per byte drawn deterministically from the classic set.
    for pos in 0..part.len() {
        let original = part[pos];
        let classic = [
            0x00,
            0x7f,
            0x80,
            original ^ 1,
            original ^ 0x80,
            original.wrapping_add(1),
            original.wrapping_sub(1),
        ];
        let extra = classic[next_random(&mut seed) as usize % classic.len()];
        for val in [0xff, extra] {
            if val == original {
                continue;
            }
            let mut bytes = part.clone();
            bytes[pos] = val;
            std::fs::write(&part_path, &bytes).unwrap();
            let edited = manifest.replace(&hash, &crate::format::sha256_hex(&bytes));
            std::fs::write(path.join(MANIFEST_NAME), edited).unwrap();
            match std::panic::catch_unwind(|| drain(&path)) {
                Err(_) => panics.push(format!("part pos={pos} val={val}")),
                Ok(Ok(_)) => accepted += 1,
                Ok(Err(_)) => {}
            }
        }
    }
    std::fs::write(&part_path, &part).unwrap();
    std::fs::write(path.join(MANIFEST_NAME), &manifest).unwrap();
    // The recorded header: every character replaced by each character the
    // lexical preflight must reject outside strings, plus a seeded sample of
    // grammar characters that reach the Zinc parser. `decode_header` is the
    // only consumer of `source_header` in `Dataset::open`.
    let header: Vec<char> = recorded["source_header"]
        .as_str()
        .unwrap()
        .chars()
        .collect();
    let mut decode = |text: String, label: String| {
        if std::panic::catch_unwind(|| crate::format::decode_header(&text)).is_err() {
            panics.push(label);
        }
    };
    for pos in 0..header.len() {
        for c in ['é', '🚀', '\0', '\r', '\u{7f}'] {
            let mut text = header.clone();
            text[pos] = c;
            decode(
                text.into_iter().collect(),
                format!("header pos={pos} {c:?}"),
            );
        }
    }
    let grammar = ['"', '\\', '[', '9', 'Z', '-', ':', ' ', '\n', '+', '.', 'T'];
    for _ in 0..1000 {
        let pos = next_random(&mut seed) as usize % header.len();
        let c = grammar[next_random(&mut seed) as usize % grammar.len()];
        let mut text = header.clone();
        text[pos] = c;
        decode(
            text.into_iter().collect(),
            format!("header pos={pos} {c:?}"),
        );
    }
    // The reviewer's exact header repro, end to end through `Dataset::open`.
    let text: String = header.iter().collect();
    let hostile = text.replace(
        "hisStart:2024-06-01T00:00:00+00:00 UTC",
        "hisStart:2024-06-01T00:00:00+0é:00 UTC",
    );
    assert_ne!(hostile, text, "repro anchor missing from header");
    rewrite_manifest(&path, |m| m["source_header"] = hostile.into());
    match std::panic::catch_unwind(|| Dataset::open(&path).err()) {
        Ok(error) => assert_eq!(error, Some(Error::Invalid)),
        Err(_) => panics.push("header repro".into()),
    }
    BACKSTOP_DISABLED.with(|v| v.set(false));
    assert!(
        panics.is_empty(),
        "{} panics: {:?}",
        panics.len(),
        &panics[..panics.len().min(20)]
    );
    // Sanity: the sweep reached decoding, not only early rejection.
    std::fs::write(path.join(MANIFEST_NAME), &manifest).unwrap();
    assert_eq!(drain(&path).unwrap().len(), 10);
    assert!(accepted > 0);
}
#[test]
fn part_replaced_after_open_is_rejected_before_decode() {
    use haystack_core::kinds::Kind;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let value = result(6, 0);
    written_dataset(&path, &value, 3);
    // A structurally valid, same-length substitute: same timestamps and
    // kinds, different text of equal width. Only a hash can tell it apart.
    let mut other = value.clone();
    for (i, sample) in other.samples.iter_mut().enumerate() {
        sample.val = Kind::Str(format!("{:05}:", 90000 + i));
    }
    let other_path = dir.path().join("other");
    written_dataset(&other_path, &other, 3);
    let substitute = std::fs::read(other_path.join("part-0000.parquet")).unwrap();
    // Control: with its hash recorded the substitute decodes cleanly, so
    // only the read-time hash can catch the swap below.
    let control = dir.path().join("control");
    std::fs::create_dir(&control).unwrap();
    std::fs::copy(path.join(MANIFEST_NAME), control.join(MANIFEST_NAME)).unwrap();
    std::fs::write(control.join("part-0000.parquet"), &substitute).unwrap();
    rewrite_manifest(&control, |m| {
        m["parts"][0]["bytes"] = substitute.len().into();
        m["parts"][0]["sha256"] = crate::format::sha256_hex(&substitute).into();
    });
    assert_same_samples(&drain(&control).unwrap(), &other.samples);

    let mut reader = Dataset::open(&path).unwrap();
    let identity = reader.manifest_sha256().to_owned();
    assert_eq!(
        identity,
        crate::format::sha256_hex(&std::fs::read(path.join(MANIFEST_NAME)).unwrap())
    );
    assert_eq!(substitute.len() as u64, reader.manifest().parts[0].bytes);
    std::fs::write(path.join("part-0000.parquet"), &substitute).unwrap();
    assert_eq!(reader.next_batch().unwrap_err(), Error::Invalid);
    assert!(!reader.semantically_complete());
    assert_eq!(reader.next_batch().unwrap_err(), Error::Invalid);
    assert_eq!(reader.manifest_sha256(), identity);
}
#[test]
fn reader_limits_reject_oversize_manifest_and_row_totals() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    written_dataset(&path, &result(3, 0), 3);
    let manifest = std::fs::read(path.join(MANIFEST_NAME)).unwrap();
    rewrite_manifest(&path, |m| {
        m["rows"] = (haystack_core::codecs::history::MAX_ROWS as u64 + 1).into()
    });
    assert_eq!(Dataset::open(&path).err(), Some(Error::Limit));
    // Pad with JSON whitespace past the hard manifest ceiling.
    let mut padded = manifest.clone();
    padded.resize(Limits::default().manifest_bytes + 1, b' ');
    std::fs::write(path.join(MANIFEST_NAME), &padded).unwrap();
    assert_eq!(Dataset::open(&path).err(), Some(Error::Limit));
    std::fs::write(path.join(MANIFEST_NAME), &manifest).unwrap();
    assert_eq!(drain(&path).unwrap().len(), 3);
}
#[test]
fn limits_require_a_part_to_fit_one_full_batch_reserve() {
    let base = Limits::default();
    let reserve = crate::format::writer_reserve(base.batch_bytes as u64, base.footer_bytes);
    for (part_bytes, ok) in [(reserve, true), (reserve - 1, false)] {
        let limits = Limits {
            part_bytes,
            total_bytes: part_bytes,
            ..base.clone()
        };
        assert_eq!(limits.validate().is_ok(), ok, "{part_bytes}");
        if !ok {
            assert_eq!(limits.validate().unwrap_err(), Error::Options);
        }
    }
}
#[test]
fn start_outside_a_runtime_is_an_error_not_a_panic() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (source, server) = runtime.block_on(async {
        let server = Server::bytes(encoded(&result(1, 0)), vec![8192], "").await;
        let source = client(&server.url)
            .await
            .his_read_stream(&request(), Default::default())
            .await
            .unwrap();
        (source, server)
    });
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        Export::start(source, dir.path().join("capture"), Default::default()),
        Err(Error::Runtime)
    ));
    assert!(!dir.path().join("capture").exists());
    drop(server);
}
#[cfg(unix)]
#[test]
fn capture_directory_and_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    written_dataset(&path, &result(3, 0), 3);
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o700);
    assert_eq!(mode(&path.join("part-0000.parquet")), 0o600);
    assert_eq!(mode(&path.join(MANIFEST_NAME)), 0o600);
    assert!(!path.join(crate::format::TEMP_MANIFEST).exists());
}
