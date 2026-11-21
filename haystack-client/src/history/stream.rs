//! Incremental consumption of the existing finite Zinc history response.
use super::decoder::Decoder;
use crate::transport::http::HttpTransport;
use haystack_core::codecs::history::{
    self as wire, HistoryReadHeader, HistoryReadRequest, HistorySample, HistoryTerminal,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

/// Maximum simultaneous opening or live history sessions on one HTTP transport.
/// Admission is immediate; no queue or worker is created when full.
pub const MAX_HISTORY_SESSIONS: usize = 16;

/// Limits for the selected scalar Zinc profile. They may tighten, never enlarge,
/// its 16 MiB response and 10,000-row ceilings. The transport can allocate one
/// native frame before its size is observable; oversized frames are rejected
/// before copying. These limits do not control TLS/HTTP/kernel buffering.
#[derive(Debug, Clone)]
pub struct HistoryStreamLimits {
    pub response_bytes: usize,
    pub rows: usize,
    pub header_bytes: usize,
    pub row_bytes: usize,
    pub field_bytes: usize,
    pub frame_bytes: usize,
    pub batch_rows: usize,
    /// Conservative source bytes, including 512 bytes per sample.
    pub batch_bytes: usize,
    pub source_bytes: usize,
    /// Encoded bytes times four plus conservative admitted source bytes.
    pub work: u64,
}
impl Default for HistoryStreamLimits {
    fn default() -> Self {
        Self {
            response_bytes: wire::MAX_GRID_BYTES,
            rows: wire::MAX_ROWS,
            header_bytes: 128 * 1024,
            row_bytes: 64 * 1024,
            field_bytes: 32 * 1024,
            frame_bytes: 512 * 1024,
            batch_rows: 64,
            batch_bytes: 64 * 1024,
            source_bytes: wire::MAX_GRID_BYTES,
            work: 256 * 1024 * 1024,
        }
    }
}
impl HistoryStreamLimits {
    fn validate(&self) -> Result<(), HistoryStreamError> {
        if self.response_bytes == 0
            || self.response_bytes > wire::MAX_GRID_BYTES
            || self.rows == 0
            || self.rows > wire::MAX_ROWS
            || self.header_bytes == 0
            || self.header_bytes > 128 * 1024
            || self.row_bytes == 0
            || self.row_bytes > 1024 * 1024
            || self.field_bytes == 0
            || self.field_bytes > self.row_bytes
            || self.frame_bytes == 0
            || self.frame_bytes > wire::MAX_GRID_BYTES
            || self.batch_rows == 0
            || self.batch_rows > self.rows
            || self.batch_bytes == 0
            || self.batch_bytes > self.source_bytes
            || self.source_bytes == 0
            || self.source_bytes > wire::MAX_GRID_BYTES
            || self.work == 0
            || self.work > 256 * 1024 * 1024
        {
            Err(HistoryStreamError::InvalidOptions)
        } else {
            Ok(())
        }
    }
}
/// One original deadline includes dispatch, headers, decoding and consumer idle
/// time. Cancel the token to stop even while opening; closing a session cancels
/// its child token without cancelling other uses of this token.
#[derive(Debug, Clone)]
pub struct HistoryStreamOptions {
    pub limits: HistoryStreamLimits,
    pub timeout: Duration,
    pub cancellation: CancellationToken,
}
impl Default for HistoryStreamOptions {
    fn default() -> Self {
        Self {
            limits: HistoryStreamLimits::default(),
            timeout: Duration::from_secs(30),
            cancellation: CancellationToken::new(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryStreamLimit {
    ResponseBytes,
    Rows,
    HeaderBytes,
    RowBytes,
    FieldBytes,
    FrameBytes,
    BatchBytes,
    SourceBytes,
    Work,
}
/// Static diagnostics deliberately exclude credentials, URLs and remote text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HistoryStreamError {
    #[error("invalid history stream options")]
    InvalidOptions,
    #[error("history streaming requires the configured first-party Zinc transport")]
    UnsupportedTransport,
    #[error("history session capacity exhausted")]
    Capacity,
    #[error("history stream cancelled")]
    Cancelled,
    #[error("history stream deadline expired")]
    Deadline,
    #[error("history HTTP credentials rejected; reconnect explicitly")]
    Credentials,
    #[error("history HTTP status {0}")]
    Http(u16),
    #[error("history transport failed; no request was replayed")]
    Transport,
    #[error("invalid or incomplete history response")]
    Protocol,
    #[error("history stream limit exceeded: {0:?}")]
    Limit(HistoryStreamLimit),
    #[error("history session worker failed")]
    Worker,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryStreamEnd {
    /// The server's declared terminal, confirmed against all rows at EOF.
    Remote(HistoryTerminal),
    /// Local failure after zero or more admitted samples. Never remote Complete.
    Local(HistoryStreamError),
}
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryStreamBatch {
    pub samples: Vec<HistorySample>,
    pub end: Option<HistoryStreamEnd>,
}
/// High-water observations for client-owned framing/batch storage. Native HTTP,
/// TLS, allocator and kernel memory are not represented by these counters.
#[derive(Debug, Default, Clone, Copy)]
pub struct HistoryStreamStats {
    pub received_bytes: u64,
    pub admitted_rows: u64,
    pub peak_frame_bytes: u64,
    pub peak_line_bytes: u64,
    pub peak_batch_bytes: u64,
    pub peak_batch_rows: u64,
    pub work: u64,
}
#[derive(Default)]
pub(super) struct Stats {
    pub received: AtomicU64,
    pub rows: AtomicU64,
    pub frame: AtomicU64,
    pub line: AtomicU64,
    pub batch: AtomicU64,
    pub batch_rows: AtomicU64,
    pub work: AtomicU64,
}
impl Stats {
    fn snapshot(&self) -> HistoryStreamStats {
        HistoryStreamStats {
            received_bytes: self.received.load(Ordering::Relaxed),
            admitted_rows: self.rows.load(Ordering::Relaxed),
            peak_frame_bytes: self.frame.load(Ordering::Relaxed),
            peak_line_bytes: self.line.load(Ordering::Relaxed),
            peak_batch_bytes: self.batch.load(Ordering::Relaxed),
            peak_batch_rows: self.batch_rows.load(Ordering::Relaxed),
            work: self.work.load(Ordering::Relaxed),
        }
    }
}
#[derive(Clone)]
pub(super) struct Control {
    pub cancel: CancellationToken,
    pub deadline: Instant,
}
impl Control {
    pub fn check(&self) -> Result<(), HistoryStreamError> {
        if self.cancel.is_cancelled() {
            Err(HistoryStreamError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(HistoryStreamError::Deadline)
        } else {
            Ok(())
        }
    }
    pub async fn run<T>(
        &self,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, HistoryStreamError> {
        self.check()?;
        tokio::select! { biased;
            _ = self.cancel.cancelled() => Err(HistoryStreamError::Cancelled),
            _ = tokio::time::sleep_until(self.deadline) => Err(HistoryStreamError::Deadline),
            value = future => { self.check()?; Ok(value) }
        }
    }
}
/// Owns one finite HTTP response. The header preserves remote observations; its
/// terminal is provisional. Call `next` until an end, or `close` to await actual
/// response/parser task destruction. Dropping a pending `next` future preserves
/// that requested batch for the next call. Drop requests cancellation/abort; it
/// cannot synchronously join. The parent client's close does not close sessions.
pub struct HttpHistorySession {
    header: HistoryReadHeader,
    demand: mpsc::Sender<oneshot::Sender<HistoryStreamBatch>>,
    pending: Option<oneshot::Receiver<HistoryStreamBatch>>,
    ready: Option<HistoryStreamBatch>,
    task: Option<JoinHandle<HistoryStreamEnd>>,
    control: Control,
    stats: Arc<Stats>,
    end: Option<HistoryStreamEnd>,
}
impl HttpHistorySession {
    pub fn header(&self) -> &HistoryReadHeader {
        &self.header
    }
    pub fn stats(&self) -> HistoryStreamStats {
        self.stats.snapshot()
    }
    pub async fn next(&mut self) -> HistoryStreamBatch {
        if self.ready.is_none() {
            if let Some(end) = self.end {
                return HistoryStreamBatch {
                    samples: vec![],
                    end: Some(end),
                };
            }
            if self.pending.is_none() {
                let (tx, rx) = oneshot::channel();
                // One exclusive borrower means one demand, including after an
                // abandoned wait. Admission never spawns a waiting sender.
                if self.demand.try_send(tx).is_err() {
                    return self.finished().await;
                }
                self.pending = Some(rx);
            }
            let result = self
                .pending
                .as_mut()
                .expect("pending demand installed")
                .await;
            self.pending = None;
            match result {
                Ok(batch) => self.ready = Some(batch),
                Err(_) => return self.finished().await,
            }
        }
        // Keep the ready batch in the handle across the completion await too.
        // Cancelling that wait must not discard a final batch of samples.
        let ready = self.ready.as_mut().expect("received batch installed");
        if ready.end.is_none()
            && let Err(error) = self.control.check()
        {
            ready.end = Some(HistoryStreamEnd::Local(error));
        }
        if let Some(end) = ready.end {
            self.join().await;
            self.end = Some(end);
        }
        self.ready.take().expect("batch retained through join")
    }
    async fn join(&mut self) -> HistoryStreamEnd {
        if let Some(task) = self.task.as_mut() {
            let end = task
                .await
                .unwrap_or(HistoryStreamEnd::Local(HistoryStreamError::Worker));
            self.task = None;
            self.end = Some(end);
            end
        } else {
            self.end
                .unwrap_or(HistoryStreamEnd::Local(HistoryStreamError::Worker))
        }
    }
    async fn finished(&mut self) -> HistoryStreamBatch {
        let end = self.join().await;
        HistoryStreamBatch {
            samples: vec![],
            end: Some(end),
        }
    }
    /// Idempotent actual completion receipt. Cancelling this wait is safe: the
    /// join handle remains owned and a later close can finish waiting.
    pub async fn close(&mut self) -> HistoryStreamEnd {
        self.control.cancel.cancel();
        self.pending = None;
        self.ready = None;
        self.join().await
    }
}
impl Drop for HttpHistorySession {
    fn drop(&mut self) {
        self.control.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
pub(crate) async fn open(
    transport: &HttpTransport,
    request: &HistoryReadRequest,
    options: HistoryStreamOptions,
) -> Result<HttpHistorySession, HistoryStreamError> {
    options.limits.validate()?;
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .ok_or(HistoryStreamError::InvalidOptions)?;
    let control = Control {
        cancel: options.cancellation.child_token(),
        deadline,
    };
    control.check()?;
    let (dispatch, permit) = transport.history_stream_request(request)?;
    let response = control
        .run(dispatch.send())
        .await?
        .map_err(|_| HistoryStreamError::Transport)?;
    let stats = Arc::new(Stats::default());
    let mut decoder = Decoder::open(
        response,
        request,
        options.limits,
        control.clone(),
        stats.clone(),
    )
    .await?;
    let header = decoder.header().clone();
    control.check()?;
    let (demand, mut rx) = mpsc::channel::<oneshot::Sender<HistoryStreamBatch>>(1);
    let owner_control = control.clone();
    let task = tokio::spawn(async move {
        // Resources are dropped before the task's join receipt is ready.
        let _permit: OwnedSemaphorePermit = permit;
        loop {
            let request = match owner_control.run(rx.recv()).await {
                Ok(Some(request)) => request,
                Ok(None) => return HistoryStreamEnd::Local(HistoryStreamError::Cancelled),
                Err(error) => return HistoryStreamEnd::Local(error),
            };
            let batch = decoder.next_batch().await;
            let end = batch.end;
            if request.send(batch).is_err() {
                return HistoryStreamEnd::Local(HistoryStreamError::Cancelled);
            }
            if let Some(end) = end {
                return end;
            }
        }
    });
    Ok(HttpHistorySession {
        header,
        demand,
        pending: None,
        ready: None,
        task: Some(task),
        control,
        stats,
        end: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthMode, ClientConfig, HaystackClient};
    use haystack_core::{
        codecs::{Codec, zinc::ZincCodec},
        graph::GraphState,
        kinds::{HDateTime, Kind},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    fn date(text: &str) -> HDateTime {
        let Kind::DateTime(value) = ZincCodec.decode_scalar(text).unwrap() else {
            panic!()
        };
        value
    }
    #[tokio::test]
    async fn cancellation_after_observed_decoding_stops_before_an_unbounded_batch() {
        let start = date("2024-06-01T00:00:00Z UTC");
        let samples: Vec<_> = (0..5000)
            .map(|i| wire::HistorySample {
                ts: date(&format!(
                    "2024-06-01T{:02}:{:02}:{:02}Z UTC",
                    i / 3600,
                    i / 60 % 60,
                    i % 60
                )),
                val: Kind::Bool(true),
            })
            .collect();
        let value = wire::HistoryReadResult {
            metadata: wire::HistoryMetadata {
                id: "point".into(),
                requested_range: "2024-06-01".into(),
                evaluated_at: start.clone(),
                start,
                end: date("2024-06-02T00:00:00Z UTC"),
                schema: wire::HistorySchema {
                    kind: wire::HistoryKind::Bool,
                    unit: None,
                    timezone: "UTC".into(),
                },
                capabilities: wire::HistoryCapabilities::BOUNDED_LIVE,
                bounds: wire::HistoryBounds {
                    batch_rows: 5000,
                    batch_bytes: 4 * 1024 * 1024,
                    total_rows: 5000,
                    total_bytes: 4 * 1024 * 1024,
                    total_work: 8 * 1024 * 1024,
                    response_bytes: 4 * 1024 * 1024,
                },
                graph: GraphState {
                    incarnation: [1; 16],
                    revision: 1,
                    catalog_generation: 1,
                },
                policy_observation: [2; 16],
                history: wire::HistoryState {
                    authority: [3; 16],
                    incarnation: [4; 16],
                    generation: 1,
                },
                coverage: wire::HistoryCoverage {
                    retained_start: Some(samples[0].ts.clone()),
                    retained_end: Some(samples[4999].ts.clone()),
                    retained_count: 5000,
                    evicted_through: None,
                },
            },
            samples,
            terminal: HistoryTerminal::Complete,
        };
        let bytes = wire::encode_result(&value, &ZincCodec).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8(request).unwrap();
            let len = text
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|n| n.parse::<usize>().unwrap())
                })
                .unwrap();
            let mut body = vec![0; len];
            socket.read_exact(&mut body).await.unwrap();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()+1).as_bytes()).await.unwrap();
            let _ = socket.write_all(&bytes).await;
            let mut byte = [0];
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
                    .await
                    .unwrap(),
                Ok(0) | Err(_)
            ));
        });
        let client = HaystackClient::connect_with_config(
            &url,
            "user",
            "password",
            &ClientConfig {
                auth_mode: AuthMode::Basic,
                allow_plaintext_basic: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let cancel = CancellationToken::new();
        let mut session = client
            .his_read_stream(
                &HistoryReadRequest {
                    id: "point".into(),
                    range: "2024-06-01".into(),
                },
                HistoryStreamOptions {
                    cancellation: cancel.clone(),
                    limits: HistoryStreamLimits {
                        batch_rows: 5000,
                        batch_bytes: 4 * 1024 * 1024,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let stats = session.stats.clone();
        let cancel_after_decode = tokio::spawn(async move {
            while stats.rows.load(Ordering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
            cancel.cancel();
        });
        let batch = session.next().await;
        cancel_after_decode.await.unwrap();
        assert!(!batch.samples.is_empty() && batch.samples.len() < 5000);
        assert_eq!(
            batch.end,
            Some(HistoryStreamEnd::Local(HistoryStreamError::Cancelled))
        );
        session.close().await;
        server.await.unwrap();
    }
}
