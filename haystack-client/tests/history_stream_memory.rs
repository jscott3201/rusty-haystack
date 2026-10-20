//! Process-isolated retained RSS experiment with a deliberately collecting
//! positive control. No allocator replacement or first-party unsafe code.
mod history_stream_support;
use haystack_client::history::{HistoryStreamEnd, HistoryStreamLimits, HistoryStreamOptions};
use haystack_core::{codecs::history::HistoryTerminal, kinds::Kind};
use history_stream_support::*;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::oneshot};

fn barrier(name: &str) {
    println!("MEMORY_{name}");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    assert!(!line.is_empty());
}
#[test]
fn history_memory_client_worker() {
    let Ok(url) = std::env::var("HAYSTACK_HISTORY_MEMORY_URL") else {
        return;
    };
    let mode = std::env::var("HAYSTACK_HISTORY_MEMORY_MODE").unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let client = client(&url).await;
        let raw = config().build_reqwest_client().unwrap();
        let options = HistoryStreamOptions {
            timeout: Duration::from_secs(60),
            limits: HistoryStreamLimits {
                batch_rows: 4,
                batch_bytes: 8192,
                ..Default::default()
            },
            ..Default::default()
        };
        // Warm exactly the same parser, transport and small-session paths in
        // each fresh process before the baseline RSS barrier.
        let mut warm = client
            .his_read_stream(&request(), options.clone())
            .await
            .unwrap();
        assert!(matches!(
            warm.next().await.end,
            Some(HistoryStreamEnd::Remote(_))
        ));
        warm.close().await;
        drop(warm);
        barrier("BASELINE");
        if mode == "stream" {
            let mut session = client.his_read_stream(&request(), options).await.unwrap();
            let batch = session.next().await;
            assert_eq!(batch.samples.len(), 4);
            assert!(batch.end.is_none());
            drop(batch);
            let before = session.stats();
            println!("MEMORY_STATS before={before:?}");
            barrier("READY");
            let idle = session.stats();
            assert_eq!(before.admitted_rows, idle.admitted_rows);
            assert_eq!(before.received_bytes, idle.received_bytes);
            let mut rows = 4;
            loop {
                let batch = session.next().await;
                rows += batch.samples.len();
                if let Some(end) = batch.end {
                    assert_eq!(end, HistoryStreamEnd::Remote(HistoryTerminal::Complete));
                    break;
                }
            }
            assert_eq!(rows, 10000);
            let stats = session.stats();
            assert!(
                stats.peak_batch_rows <= 4
                    && stats.peak_batch_bytes <= 8192
                    && stats.peak_frame_bytes <= 512 * 1024
                    && stats.peak_line_bytes <= 128 * 1024
            );
            println!("MEMORY_STATS final={stats:?}");
            session.close().await;
        } else {
            // Deliberately retain a complete body at the same measurement
            // barrier. This must produce a size-dependent RSS increase.
            let bytes = raw
                .post(format!("{url}/hisRead"))
                .basic_auth("user", Some("password"))
                .body("control")
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            barrier("READY");
            std::hint::black_box(&bytes);
            assert!(bytes.len() > 1_000_000);
        }
    });
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn read_barrier(output: &mut BufReader<std::process::ChildStdout>, name: &str) {
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child exited before {name}"
        );
        if line.contains("MEMORY_STATS") {
            print!("{line}");
        }
        if line.contains(&format!("MEMORY_{name}")) {
            break;
        }
    }
}
fn rss_kib(pid: u32) -> u64 {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .expect("RSS process inspection");
    assert!(
        output.status.success(),
        "RSS observation unavailable: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn body(repetitions: usize) -> Vec<u8> {
    let mut value = result(10000, 0);
    for sample in &mut value.samples {
        sample.val = Kind::Str("x\\\n".repeat(repetitions));
    }
    encoded(&value)
}
async fn measure(mode: &str, repetitions: usize) -> (u64, usize, usize) {
    let bytes = body(repetitions);
    let size = bytes.len();
    let prefix_end = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(5)
        .unwrap()
        .0
        + 1;
    let warm = encoded(&result(1, 0));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (release, wait) = oneshot::channel();
    let written = Arc::new(AtomicUsize::new(0));
    let count = written.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",warm.len()).as_bytes()).await.unwrap();
        socket.write_all(&warm).await.unwrap();
        socket.shutdown().await.unwrap();
        drop(socket);
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",size).as_bytes()).await.unwrap();
        socket.write_all(&bytes[..prefix_end]).await.unwrap();
        count.store(prefix_end, Ordering::SeqCst);
        wait.await.unwrap();
        for chunk in bytes[prefix_end..].chunks(8192) {
            socket.write_all(chunk).await.unwrap();
            count.fetch_add(chunk.len(), Ordering::SeqCst);
        }
        socket.shutdown().await.unwrap();
    });
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "history_memory_client_worker", "--nocapture"])
            .env("HAYSTACK_HISTORY_MEMORY_URL", &url)
            .env("HAYSTACK_HISTORY_MEMORY_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    read_barrier(&mut output, "BASELINE");
    let baseline = rss_kib(child.0.id());
    writeln!(child.0.stdin.as_mut().unwrap(), "go").unwrap();
    let mut release = Some(release);
    if mode == "collect" {
        release.take().unwrap().send(()).unwrap();
    }
    read_barrier(&mut output, "READY");
    if let Some(release) = release.take() {
        release.send(()).unwrap();
    }
    // Fixed predeclared observation window, three RSS samples. The server is a
    // different process from the measured client, including its entire body.
    let mut retained = 0;
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        retained = retained.max(rss_kib(child.0.id()));
    }
    let idle_written = written.load(Ordering::SeqCst);
    writeln!(child.0.stdin.as_mut().unwrap(), "finish").unwrap();
    let mut remaining = String::new();
    std::io::Read::read_to_string(&mut output, &mut remaining).unwrap();
    print!("{remaining}");
    assert!(child.0.wait().unwrap().success());
    server.await.unwrap();
    let delta = retained.saturating_sub(baseline);
    println!(
        "RSS mode={mode} repetitions={repetitions} response_bytes={size} baseline_kib={baseline} retained_kib={retained} delta_kib={delta} server_written_while_idle={idle_written}"
    );
    (delta, size, idle_written)
}
/// This explicit qualification uses native process RSS and child execution. It
/// is ignored in ordinary tests because sandbox/process inspection policies vary;
/// run it explicitly on a host that permits inspection of these owned children.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit isolated native RSS qualification"]
async fn retained_memory_scales_with_batches_not_complete_response() {
    // Predeclared thresholds: streaming large-minus-small <=4MiB; the collecting
    // control must grow >=6MiB. Every one of three fresh-process pairs must pass.
    for repeat in 0..3 {
        let (small, small_bytes, _) = measure("stream", 20).await;
        let (large, large_bytes, written) = measure("stream", 280).await;
        let (control_small, _, _) = measure("collect", 20).await;
        let (control_large, _, _) = measure("collect", 280).await;
        assert!(large_bytes > 10 * small_bytes);
        assert!(
            large.saturating_sub(small) <= 4096,
            "repeat {repeat}: streaming RSS grew with response size"
        );
        assert!(
            control_large.saturating_sub(control_small) >= 6144,
            "repeat {repeat}: collecting positive control did not distinguish retained memory"
        );
        assert!(
            written < large_bytes,
            "large body unexpectedly completed socket writes during idle demand"
        );
    }
}
