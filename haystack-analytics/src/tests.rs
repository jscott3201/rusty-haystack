#[path = "../tests/support/mod.rs"]
pub(crate) mod support;
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
/// Disables the backstop on this thread until dropped, then restores the
/// previous setting, so a failing assertion (a panic) cannot leave it
/// disabled for a later test and guards may nest.
struct BackstopDisabled(bool);
impl BackstopDisabled {
    fn new() -> Self {
        Self(BACKSTOP_DISABLED.with(|v| v.replace(true)))
    }
}
impl Drop for BackstopDisabled {
    fn drop(&mut self) {
        BACKSTOP_DISABLED.with(|v| v.set(self.0));
    }
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
/// Independent Compact Thrift walker for hostile-input tests. It shares no
/// code with the reader's preflight: it walks any well-formed message
/// generically and reports where each integer, binary length, list count and
/// field header sits, so a test can substitute extreme values in place.
mod sites {
    pub enum Kind {
        Int,
        Length,
        /// List header with its element type.
        Count(u8),
        /// Field header with its wire type.
        Header(u8),
    }
    pub struct Site {
        pub kind: Kind,
        pub start: usize,
        pub end: usize,
        pub id: i16,
        pub depth: usize,
        pub value: i64,
    }
    struct Walk<'a> {
        bytes: &'a [u8],
        pos: usize,
        out: Vec<Site>,
    }
    fn zigzag(n: u64) -> i64 {
        ((n >> 1) as i64) ^ -((n & 1) as i64)
    }
    impl Walk<'_> {
        fn byte(&mut self) -> u8 {
            self.pos += 1;
            self.bytes[self.pos - 1]
        }
        fn varint(&mut self) -> u64 {
            let mut n = 0;
            for shift in (0..64).step_by(7) {
                let b = self.byte();
                n |= u64::from(b & 127) << shift;
                if b & 128 == 0 {
                    break;
                }
            }
            n
        }
        fn push(&mut self, kind: Kind, start: usize, id: i16, depth: usize, value: i64) {
            let end = self.pos;
            self.out.push(Site {
                kind,
                start,
                end,
                id,
                depth,
                value,
            });
        }
        fn structure(&mut self, depth: usize) {
            let mut last = 0i16;
            loop {
                let start = self.pos;
                let header = self.byte();
                if header == 0 {
                    return;
                }
                last = if header >> 4 == 0 {
                    zigzag(self.varint()) as i16
                } else {
                    last + i16::from(header >> 4)
                };
                self.push(Kind::Header(header & 15), start, last, depth, 0);
                self.value(header & 15, last, depth, false);
            }
        }
        fn value(&mut self, ty: u8, id: i16, depth: usize, element: bool) {
            let start = self.pos;
            match ty {
                1 | 2 if element => {
                    self.byte();
                }
                1 | 2 => {}
                3 => {
                    self.byte();
                }
                4..=6 => {
                    let v = zigzag(self.varint());
                    self.push(Kind::Int, start, id, depth, v);
                }
                7 => self.pos += 8,
                8 => {
                    let n = self.varint();
                    self.push(Kind::Length, start, id, depth, n as i64);
                    self.pos += n as usize;
                }
                9 | 10 => {
                    let h = self.byte();
                    let n = if h >> 4 == 15 {
                        self.varint()
                    } else {
                        u64::from(h >> 4)
                    };
                    self.push(Kind::Count(h & 15), start, id, depth, n as i64);
                    for _ in 0..n {
                        self.value(h & 15, 0, depth + 1, true);
                    }
                }
                11 => {
                    let n = self.varint();
                    if n > 0 {
                        let kv = self.byte();
                        for _ in 0..n {
                            self.value(kv >> 4, 0, depth + 1, true);
                            self.value(kv & 15, 0, depth + 1, true);
                        }
                    }
                }
                12 => self.structure(depth + 1),
                _ => panic!("wire type {ty} in a writer-produced message"),
            }
        }
    }
    /// Sites of the struct at `bytes[start..]` and the offset just past it.
    pub fn walk(bytes: &[u8], start: usize) -> (Vec<Site>, usize) {
        let mut walk = Walk {
            bytes,
            pos: start,
            out: vec![],
        };
        walk.structure(0);
        (walk.out, walk.pos)
    }
}
fn uvarint(mut n: u64) -> Vec<u8> {
    let mut out = vec![];
    while n >= 0x80 {
        out.push(n as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
    out
}
/// Every integer-extreme and wire-type substitution of the Thrift sites in
/// `region` (offsets are into `region`): each integer becomes 0, ±1, the
/// i32/u32/i64 extremes and its own value ±1; each binary length and list
/// count becomes 0, 1, its value ±1 and the i32/u32/u64 extremes; each field
/// header's wire type becomes every other scalar or container type.
fn site_mutants(region: &[u8], found: &[sites::Site]) -> Vec<(String, Vec<u8>)> {
    const SIGNED: [i64; 8] = [
        0,
        1,
        -1,
        i32::MAX as i64,
        i32::MIN as i64,
        u32::MAX as i64,
        i64::MAX,
        i64::MIN,
    ];
    const UNSIGNED: [u64; 5] = [0, 1, i32::MAX as u64, u32::MAX as u64, u64::MAX];
    let mut out = vec![];
    let mut splice = |site: &sites::Site, label: String, replacement: Vec<u8>| {
        let mut bytes = region[..site.start].to_vec();
        bytes.extend(replacement);
        bytes.extend(&region[site.end..]);
        if bytes != region {
            out.push((format!("@{} {label}", site.start), bytes));
        }
    };
    for site in found {
        let v = site.value;
        match site.kind {
            sites::Kind::Int => {
                for x in SIGNED
                    .into_iter()
                    .chain([v.wrapping_sub(1), v.wrapping_add(1)])
                {
                    splice(
                        site,
                        format!("int {v}->{x}"),
                        uvarint(((x << 1) ^ (x >> 63)) as u64),
                    );
                }
            }
            sites::Kind::Length => {
                for x in UNSIGNED
                    .into_iter()
                    .chain([(v as u64).wrapping_sub(1), v as u64 + 1])
                {
                    splice(site, format!("len {v}->{x}"), uvarint(x));
                }
            }
            sites::Kind::Count(element) => {
                for x in UNSIGNED
                    .into_iter()
                    .chain([(v as u64).wrapping_sub(1), v as u64 + 1])
                {
                    let mut header = vec![0xf0 | element];
                    header.extend(uvarint(x));
                    splice(site, format!("count {v}->{x}"), header);
                }
            }
            sites::Kind::Header(ty) => {
                for alt in [1, 3, 5, 6, 8, 9, 12] {
                    if alt != ty {
                        let first = region[site.start] & 0xf0 | alt;
                        let mut header = vec![first];
                        header.extend(&region[site.start + 1..site.end]);
                        splice(site, format!("type {ty}->{alt}"), header);
                    }
                }
            }
        }
    }
    out
}
/// Integer-extreme and wire-type mutants of every footer and page-header
/// Thrift site in a writer-produced `part`, as whole part files.
fn thrift_site_mutants(part: &[u8]) -> Vec<(String, Vec<u8>)> {
    let n = part.len();
    let footer_len = u32::from_le_bytes(part[n - 8..n - 4].try_into().unwrap()) as usize;
    let footer_start = n - 8 - footer_len;
    let footer = &part[footer_start..n - 8];
    let (found, end) = sites::walk(footer, 0);
    assert_eq!(end, footer.len());
    let mut out = vec![];
    for (label, footer) in site_mutants(footer, &found) {
        let mut bytes = part[..footer_start].to_vec();
        bytes.extend(&footer);
        bytes.extend((footer.len() as u32).to_le_bytes());
        bytes.extend(b"PAR1");
        out.push((format!("footer {label}"), bytes));
    }
    // Page headers are contiguous: header, then `compressed_page_size`
    // (field 3) body bytes.
    let mut pos = 4;
    while pos < footer_start {
        let (found, end) = sites::walk(part, pos);
        let body = found
            .iter()
            .find(|s| matches!(s.kind, sites::Kind::Int) && s.id == 3 && s.depth == 0)
            .unwrap()
            .value as usize;
        for (label, bytes) in site_mutants(part, &found) {
            out.push((format!("page {label}"), bytes));
        }
        pos = end + body;
    }
    assert_eq!(pos, footer_start);
    out
}
/// Runs each mutant of a written dataset's only part through the reader with
/// the backstop disabled. A mutant the guards reject (`checked_metadata`, as
/// `Dataset::open` runs it) never reaches upstream decoding; one they admit
/// is then installed with its hash and consumed end to end through
/// `Dataset`. Returns the labels of mutants that panicked, and how many were
/// admitted.
fn sweep_parts(path: &std::path::Path, mutants: Vec<(String, Vec<u8>)>) -> (Vec<String>, usize) {
    let _backstop = BackstopDisabled::new();
    let part_path = path.join("part-0000.parquet");
    let original = std::fs::read(&part_path).unwrap();
    let manifest_text = std::fs::read_to_string(path.join(MANIFEST_NAME)).unwrap();
    let manifest: Manifest = serde_json::from_str(&manifest_text).unwrap();
    assert_eq!(manifest.parts.len(), 1);
    let header = crate::format::decode_header(&manifest.source_header).unwrap();
    let mut panics = vec![];
    let mut admitted = 0;
    for (label, bytes) in mutants {
        let part = Part {
            bytes: bytes.len() as u64,
            sha256: crate::format::sha256_hex(&bytes),
            ..manifest.parts[0].clone()
        };
        let data = bytes::Bytes::from(bytes);
        let checked = std::panic::catch_unwind(|| {
            crate::reader::checked_metadata(&data, &part, &header, &manifest.limits).is_ok()
        });
        match checked {
            Err(_) => panics.push(label),
            Ok(false) => {}
            Ok(true) => {
                admitted += 1;
                std::fs::write(&part_path, &data).unwrap();
                let mut edited = manifest.clone();
                edited.parts[0] = part;
                std::fs::write(
                    path.join(MANIFEST_NAME),
                    serde_json::to_vec(&edited).unwrap(),
                )
                .unwrap();
                if std::panic::catch_unwind(|| drain(path)).is_err() {
                    panics.push(label);
                }
            }
        }
    }
    std::fs::write(&part_path, &original).unwrap();
    std::fs::write(path.join(MANIFEST_NAME), &manifest_text).unwrap();
    (panics, admitted)
}
/// Largest growth of the process's peak virtual size the Thrift-site sweep
/// may cause. Upstream reserves `Vec` capacity from decoded counts, so a
/// count the guards miss shows up here (or aborts the child) long before it
/// is ever touched; the sweep itself holds one small part at a time.
#[cfg(target_os = "linux")]
const LARGE_ALLOCATION_BYTES: u64 = 64 * 1024 * 1024;
/// A `/proc/self/status` size field (`VmSize`, `VmPeak`) in bytes.
#[cfg(target_os = "linux")]
fn vm_status(key: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let line = status.lines().find(|l| l.starts_with(key)).unwrap();
    let kib: u64 = line[key.len()..]
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .unwrap();
    kib * 1024
}
/// Child of `hostile_thrift_sites_never_reserve_large_allocations`: runs the
/// Thrift-site sweep alone on one thread so `VmPeak` (the high-water mark of
/// the address space, which records even a reservation freed at once) is
/// attributable to it. No counting allocator: the workspace forbids `unsafe`.
#[cfg(target_os = "linux")]
#[test]
fn allocation_child() {
    if std::env::var_os("HAYSTACK_ANALYTICS_ALLOCATION_CHILD").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    written_dataset(&path, &mixed_numbers(10), 4);
    let part = std::fs::read(path.join("part-0000.parquet")).unwrap();
    // The corpus (10,215 mutants of this part) is allocated before the
    // baseline below, so its own memory never counts as growth; the floor
    // keeps a shrinking generator from passing vacuously.
    let mutants = thrift_site_mutants(&part);
    assert!(mutants.len() > 5000, "{}", mutants.len());
    // Warm up allocator arenas and lazily initialized state first.
    drain(&path).unwrap();
    sweep_parts(&path, mutants[..32].to_vec());
    let before = vm_status("VmPeak:").max(vm_status("VmSize:"));
    let (panics, admitted) = sweep_parts(&path, mutants);
    let after = vm_status("VmPeak:");
    println!("ANALYTICS_VM before={before} after={after} admitted={admitted}");
    assert!(panics.is_empty(), "{panics:?}");
    assert!(
        after.saturating_sub(before) <= LARGE_ALLOCATION_BYTES,
        "peak virtual size grew {} bytes",
        after - before
    );
}
#[cfg(target_os = "linux")]
#[test]
fn hostile_thrift_sites_never_reserve_large_allocations() {
    // An allocation the system refuses aborts the child (uncatchable, which
    // is the failure this guards against); one it grants raises `VmPeak`.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::allocation_child", "--nocapture"])
        .args(["--test-threads", "1"])
        .env("HAYSTACK_ANALYTICS_ALLOCATION_CHILD", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("ANALYTICS_VM"),
        "{}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn hostile_byte_mutations_are_rejected_by_guards_without_panicking() {
    // Disable the production catch_unwind backstop on this thread so any
    // upstream panic reaches this test's own catch_unwind: the assertion is
    // that the guards reject every mutation before upstream can panic.
    let backstop = BackstopDisabled::new();
    let mut seed = 0x9e37_79b9_7f4a_7c15;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("capture");
    let value = mixed_numbers(10);
    written_dataset(&path, &value, 4);
    let part_path = path.join("part-0000.parquet");
    let part = std::fs::read(&part_path).unwrap();
    let manifest = std::fs::read(path.join(MANIFEST_NAME)).unwrap();
    let recorded: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let manifest = String::from_utf8(manifest).unwrap();
    // Every byte of the part, with the manifest hash recomputed so the
    // mutation reaches the footer, layout, page and level guards and then
    // decoding. 0xff at every byte (both reviewer repros were 0xff), plus one
    // further value per byte drawn deterministically from the classic set.
    let mut mutants = vec![];
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
            if val != original {
                let mut bytes = part.clone();
                bytes[pos] = val;
                mutants.push((format!("part pos={pos} val={val}"), bytes));
            }
        }
    }
    let (mut panics, mut accepted) = sweep_parts(&path, mutants);
    // Every footer and page-header integer, length and count replaced by
    // extremes, and every field's wire type swapped: upstream sizes
    // allocations from these, so a guard must bound each one first.
    let (site_panics, site_admitted) = sweep_parts(&path, thrift_site_mutants(&part));
    panics.extend(site_panics);
    accepted += site_admitted;
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
    drop(backstop);
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
    let reserve = crate::format::writer_reserve(
        (base.batch_bytes + crate::format::ARROW_BATCH_BYTES) as u64,
        base.footer_bytes,
    );
    assert_eq!(
        reserve,
        crate::format::min_part_bytes(base.batch_bytes, base.footer_bytes)
    );
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
/// The smallest value of every option `Limits::validate` admits.
fn minimum_limits() -> Limits {
    use crate::format::*;
    let footer_bytes = min_footer_bytes(1);
    let part_bytes = min_part_bytes(MIN_BATCH_BYTES, footer_bytes);
    Limits {
        batch_rows: 1,
        batch_bytes: MIN_BATCH_BYTES,
        field_bytes: 1,
        groups_per_part: 1,
        part_bytes,
        total_bytes: part_bytes,
        parts: 1,
        manifest_bytes: min_manifest_bytes(1),
        footer_bytes,
    }
}
#[test]
fn minimum_limits_are_exactly_the_validation_floor() {
    let min = minimum_limits();
    min.validate().unwrap();
    // One below any floor is refused, so `minimum_limits` is the floor.
    let below: [fn(&mut Limits); 9] = [
        |l| l.batch_rows -= 1,
        |l| l.batch_bytes -= 1,
        |l| l.field_bytes -= 1,
        |l| l.groups_per_part -= 1,
        |l| l.part_bytes -= 1,
        |l| l.total_bytes -= 1,
        |l| l.parts -= 1,
        |l| l.manifest_bytes -= 1,
        |l| l.footer_bytes -= 1,
    ];
    for (i, edit) in below.into_iter().enumerate() {
        let mut limits = min.clone();
        edit(&mut limits);
        assert_eq!(limits.validate().unwrap_err(), Error::Options, "{i}");
    }
    // The floors scale with the part and group counts they must hold.
    let wide = Limits {
        groups_per_part: crate::format::MAX_GROUPS_PER_PART,
        parts: Limits::default().parts,
        ..min.clone()
    };
    assert_eq!(wide.validate().unwrap_err(), Error::Options);
}
#[tokio::test]
async fn minimum_valid_limits_publish_empty_and_one_row_captures() {
    use haystack_core::{
        codecs::history::HistoryKind,
        kinds::{Kind, Number},
    };
    let _permit = TEST_ADMISSION.acquire().await.unwrap();
    let mut one = result(1, 0);
    one.metadata.schema.kind = HistoryKind::Bool;
    one.samples[0].val = Kind::Bool(true);
    // Every worst-case header too, empty and with one sample charged exactly
    // `MIN_BATCH_BYTES` (a unitless number).
    let worst = worst_case_results(0)
        .into_iter()
        .chain(worst_case_results(1).into_iter().map(|mut value| {
            value.samples[0].val = Kind::Number(Number::unitless(1.0));
            value
        }));
    for value in [result(0, 0), one].into_iter().chain(worst) {
        let rows = value.samples.len();
        let server = Server::bytes(encoded(&value), vec![4096], "").await;
        let range = &value.metadata.requested_range;
        let Ok(source) = client(&server.url)
            .await
            .his_read_stream(&request_for(&value), Default::default())
            .await
        else {
            // The client refuses non-ASCII and raw CR/LF ranges before dispatch.
            assert!(
                !range.is_ascii() || range.contains(['\r', '\n']),
                "{range:?}"
            );
            continue;
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let options = ExportOptions {
            limits: minimum_limits(),
            ..Default::default()
        };
        let receipt = Export::start(source, &path, options)
            .unwrap()
            .wait()
            .await
            .unwrap();
        server.finish().await;
        assert_eq!(receipt.manifest.rows, rows as u64);
        assert_eq!(receipt.manifest.parts.len(), rows);
        assert_same_samples(&drain(&path).unwrap(), &value.samples);
    }
}
/// Writes `value` one row per group through the real writer under `limits`
/// (which `Writer` itself never validates), returning the publish outcome.
fn write_rows(
    path: &std::path::Path,
    value: &haystack_core::codecs::history::HistoryReadResult,
    limits: Limits,
) -> Result<crate::writer::Writer> {
    let header = header(value);
    let mut writer = crate::writer::Writer::new(
        path.to_owned(),
        &header,
        limits.clone(),
        uuid::Uuid::new_v4().to_string(),
        Checkpoints::default(),
    )?;
    for sample in &value.samples {
        writer.write(crate::format::batch(
            &header,
            std::slice::from_ref(sample),
            &limits,
        )?)?;
    }
    writer.prepare()?;
    writer.publish(|| Ok(()))?;
    Ok(writer)
}
#[test]
fn floors_hold_the_largest_header_and_a_full_part() {
    use crate::format::{MANIFEST_HEADER_BYTES, MAX_GROUPS_PER_PART, recordable_header};
    use haystack_core::codecs::history::{HistoryReadHeader, MAX_ROWS};
    let limits = Limits {
        groups_per_part: MAX_GROUPS_PER_PART,
        parts: 1,
        manifest_bytes: crate::format::min_manifest_bytes(1),
        footer_bytes: crate::format::min_footer_bytes(MAX_GROUPS_PER_PART),
        ..Limits::default()
    };
    limits.validate().unwrap();
    let cases = worst_case_results(MAX_GROUPS_PER_PART);
    assert_eq!(cases.len(), trim_whitespace().len());
    let (mut largest, mut refused) = (0, vec![]);
    for value in cases {
        let pad = value.metadata.requested_range.chars().next().unwrap();
        let bytes = header_json_bytes(&header(&value));
        // The same header at the widest `returnedCount` the codec admits.
        let widest = header_json_bytes(&HistoryReadHeader {
            returned_count: MAX_ROWS as u64,
            ..header(&value)
        });
        largest = largest.max(widest);
        assert!(widest <= MANIFEST_HEADER_BYTES, "{pad:?}: {widest}");
        if recordable_header(&header(&value)).is_err() {
            refused.push(pad);
            continue;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        let writer = write_rows(&path, &value, limits.clone()).unwrap();
        assert_eq!(writer.manifest.parts.len(), 1, "{pad:?}");
        assert_eq!(writer.manifest.parts[0].groups, MAX_GROUPS_PER_PART);
        let recorded = serde_json::to_vec(&writer.manifest.source_header).unwrap();
        assert_eq!(recorded.len(), bytes, "{pad:?}");
        assert_same_samples(&drain(&path).unwrap(), &value.samples);
    }
    // Only NEL, a C1 control the Zinc encoder writes raw and the preflight
    // refuses, cannot be recorded. The client never sends it (it refuses
    // non-ASCII ranges), and `recordable_header` would refuse it before export.
    assert_eq!(refused, ['\u{85}']);
    // The VT-padded header: the measurement `MANIFEST_HEADER_BYTES` documents.
    assert_eq!(largest, 12_626);
}
#[test]
fn writer_backstops_refuse_manifest_and_footer_overruns() {
    // `Writer` does not validate its limits, so limits far below the floors
    // exercise the publish-time checks that a validating `Limits` never hits.
    for (name, limits) in [
        (
            "manifest",
            Limits {
                manifest_bytes: 1024,
                ..Limits::default()
            },
        ),
        (
            "footer",
            Limits {
                footer_bytes: 128,
                ..Limits::default()
            },
        ),
    ] {
        assert_eq!(limits.validate().unwrap_err(), Error::Options, "{name}");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture");
        assert_eq!(
            write_rows(&path, &result(2, 0), limits).err(),
            Some(Error::Limit),
            "{name}"
        );
        assert!(!path.join(MANIFEST_NAME).exists(), "{name}");
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

#[test]
fn non_canonical_source_header_is_rejected() {
    let text = crate::format::encode_header(&header(&result(1, 0))).unwrap();
    crate::format::decode_header(&text).unwrap();
    // Each decodes to a valid header (or one the decoder normalizes), but
    // is not the exact text the writer records for it.
    let cases = [
        ("ver string", r#"ver:"3.0""#, r#"ver:"2.0""#),
        ("ver number", r#"ver:"3.0""#, "ver:3.0"),
        ("duplicate tag", " id:@point\n", " id:@point id:@point\n"),
        ("spacing", " id:@point", "  id:@point"),
        (
            "control extra key",
            r#"{\"version\":1,"#,
            r#"{\"version\":1,\"kind\":5,"#,
        ),
        ("control spacing", r#"\"nanos\":0,"#, r#"\"nanos\": 0,"#),
    ];
    for (name, from, to) in cases {
        let hostile = text.replacen(from, to, 1);
        assert_ne!(hostile, text, "{name}: anchor missing");
        assert_eq!(
            crate::format::decode_header(&hostile).unwrap_err(),
            Error::Invalid,
            "{name}"
        );
    }
}
