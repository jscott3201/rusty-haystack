mod history_stream_support;
use haystack_client::history::*;
use history_stream_support::*;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};
use tokio_util::sync::CancellationToken;

struct Stalled {
    url: String,
    ready: oneshot::Receiver<()>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for Stalled {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Stalled {
    async fn start(prefix: Option<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, ready) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            if let Some(prefix) = prefix {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
                socket
                    .write_all(format!("{:x}\r\n", prefix.len()).as_bytes())
                    .await
                    .unwrap();
                socket.write_all(&prefix).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            tx.send(()).unwrap();
            let mut byte = [0];
            let retired = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
                .await
                .expect("incomplete H1 body must retire independently of fixture shutdown");
            assert!(
                matches!(retired, Ok(0) | Err(_)),
                "unexpected request replay or data"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err(),
                "request was replayed"
            );
        });
        Self {
            url,
            ready,
            task: Some(task),
        }
    }
    async fn finish(mut self) {
        self.task.as_mut().unwrap().await.unwrap();
        self.task = None;
    }
}
fn prefix(rows: usize, partial: bool) -> Vec<u8> {
    let bytes = encoded(&result(10, 2));
    let end = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(rows + 1)
        .unwrap()
        .0
        + 1;
    bytes[..end + if partial { 20 } else { 0 }].to_vec()
}
fn options(cancel: CancellationToken) -> HistoryStreamOptions {
    HistoryStreamOptions {
        cancellation: cancel,
        limits: HistoryStreamLimits {
            batch_rows: 1,
            ..Default::default()
        },
        ..Default::default()
    }
}
#[tokio::test]
async fn cancel_during_headers_and_partial_envelope_retires_actual_request() {
    for prefix in [None, Some(b"ver:\"3.0\" history:\"unfinished".to_vec())] {
        let mut server = Stalled::start(prefix).await;
        let client = client(&server.url).await;
        let cancel = CancellationToken::new();
        let control = cancel.clone();
        let request = request();
        let (opened, ()) =
            tokio::join!(client.his_read_stream(&request, options(control)), async {
                (&mut server.ready).await.unwrap();
                cancel.cancel();
            });
        assert!(matches!(opened, Err(HistoryStreamError::Cancelled)));
        server.finish().await;
    }
}
#[tokio::test]
async fn cancelled_open_future_drops_owned_response_without_a_worker() {
    let mut server = Stalled::start(Some(b"ver:\"3.0\" history:\"unfinished".to_vec())).await;
    let client = client(&server.url).await;
    {
        let request = request();
        let opening = client.his_read_stream(&request, Default::default());
        tokio::pin!(opening);
        tokio::select! {biased; _=&mut server.ready=>{}, _=&mut opening=>panic!("unexpected complete opening")};
        // The actual future (not just a pinned reference) is dropped here.
    }
    server.finish().await;
}
#[tokio::test]
async fn pending_pull_cancel_close_and_idle_deadline_join_and_retire() {
    for mode in ["cancel", "close", "drop", "deadline"] {
        let mut server = Stalled::start(Some(prefix(1, true))).await;
        let client = client(&server.url).await;
        let cancel = CancellationToken::new();
        let mut options = options(cancel.clone());
        if mode == "deadline" {
            options.timeout = Duration::from_millis(180);
        }
        let mut session = client.his_read_stream(&request(), options).await.unwrap();
        (&mut server.ready).await.unwrap();
        assert_eq!(session.next().await.samples.len(), 1);
        if mode == "cancel" {
            let (batch, ()) = {
                let wait = session.next();
                tokio::pin!(wait);
                let stopped = async {
                    tokio::task::yield_now().await;
                    cancel.cancel();
                };
                tokio::join!(wait, stopped)
            };
            assert_eq!(
                batch.end,
                Some(HistoryStreamEnd::Local(HistoryStreamError::Cancelled))
            );
            assert_eq!(
                session.close().await,
                HistoryStreamEnd::Local(HistoryStreamError::Cancelled)
            );
            assert_eq!(
                session.close().await,
                HistoryStreamEnd::Local(HistoryStreamError::Cancelled)
            );
        } else if mode == "close" {
            // Abandon an in-progress pull; close still joins that same owner.
            assert!(
                tokio::time::timeout(Duration::from_millis(20), session.next())
                    .await
                    .is_err()
            );
            assert_eq!(
                session.close().await,
                HistoryStreamEnd::Local(HistoryStreamError::Cancelled)
            );
            assert_eq!(
                session.close().await,
                HistoryStreamEnd::Local(HistoryStreamError::Cancelled)
            );
        } else if mode == "drop" {
            drop(session);
        } else {
            // No next() is pending: the owner must enforce idle expiry itself.
            tokio::time::sleep(Duration::from_millis(220)).await;
            assert_eq!(
                session.next().await.end,
                Some(HistoryStreamEnd::Local(HistoryStreamError::Deadline))
            );
            assert_eq!(
                session.close().await,
                HistoryStreamEnd::Local(HistoryStreamError::Deadline)
            );
        }
        server.finish().await;
    }
}
#[tokio::test]
async fn abandoned_pull_wait_preserves_the_requested_batch() {
    let bytes = encoded(&result(3, 2));
    let split = bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(1)
        .unwrap()
        .0
        + 1;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (release, wait) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
        socket.write_all(&bytes[..split]).await.unwrap();
        wait.await.unwrap();
        socket.write_all(&bytes[split..]).await.unwrap();
    });
    let client = client(&url).await;
    let mut session = client
        .his_read_stream(&request(), options(CancellationToken::new()))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), session.next())
            .await
            .is_err()
    );
    release.send(()).unwrap();
    let mut samples = vec![];
    loop {
        let batch = session.next().await;
        samples.extend(batch.samples);
        if let Some(end) = batch.end {
            assert!(matches!(end, HistoryStreamEnd::Remote(_)));
            break;
        }
    }
    assert_eq!(samples, result(3, 2).samples);
    session.close().await;
    server.await.unwrap();
}

#[tokio::test]
async fn active_session_capacity_is_immediate_and_recovers_after_actual_close() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(AtomicUsize::new(0));
    let count = observed.clone();
    let server = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        for _ in 0..MAX_HISTORY_SESSIONS + 1 {
            let (mut socket, _) = listener.accept().await.unwrap();
            count.fetch_add(1, Ordering::SeqCst);
            handlers.spawn(async move {
                read_request(&mut socket).await;let prefix=prefix(0,false);
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
                socket.write_all(format!("{:x}\r\n",prefix.len()).as_bytes()).await.unwrap();socket.write_all(&prefix).await.unwrap();socket.write_all(b"\r\n").await.unwrap();
                let mut byte=[0];assert!(matches!(tokio::time::timeout(Duration::from_secs(5),socket.read(&mut byte)).await.unwrap(),Ok(0)|Err(_)));
            });
        }
        while let Some(result) = handlers.join_next().await {
            result.unwrap();
        }
    });
    let client = client(&url).await;
    let mut sessions = vec![];
    for _ in 0..MAX_HISTORY_SESSIONS {
        sessions.push(
            client
                .his_read_stream(&request(), Default::default())
                .await
                .unwrap(),
        );
    }
    assert!(matches!(
        client.his_read_stream(&request(), Default::default()).await,
        Err(HistoryStreamError::Capacity)
    ));
    assert_eq!(observed.load(Ordering::SeqCst), MAX_HISTORY_SESSIONS);
    sessions[0].close().await;
    let mut replacement = client
        .his_read_stream(&request(), Default::default())
        .await
        .unwrap();
    replacement.close().await;
    for session in &mut sessions {
        session.close().await;
    }
    server.await.unwrap();
    assert_eq!(observed.load(Ordering::SeqCst), MAX_HISTORY_SESSIONS + 1);
}

#[tokio::test]
async fn transport_loss_after_delivered_prefix_remains_partial_and_is_not_replayed() {
    let bytes = prefix(2, false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()+5000).as_bytes()).await.unwrap();
        socket.write_all(&bytes).await.unwrap();
        socket.shutdown().await.unwrap();
        drop(socket);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "partial request was replayed"
        );
    });
    let client = client(&url).await;
    let mut session = client
        .his_read_stream(&request(), options(CancellationToken::new()))
        .await
        .unwrap();
    let mut rows = vec![];
    loop {
        let batch = session.next().await;
        rows.extend(batch.samples);
        if let Some(end) = batch.end {
            assert_eq!(end, HistoryStreamEnd::Local(HistoryStreamError::Transport));
            break;
        }
    }
    assert_eq!(rows, result(10, 2).samples[..2]);
    session.close().await;
    server.await.unwrap();
}
