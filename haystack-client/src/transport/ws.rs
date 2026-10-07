//! Bounded, text-only watch operations over the server's JSON v3 protocol.
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex as StateMutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use haystack_core::{
    codecs::json::v3,
    data::{HCol, HDict, HGrid},
    kinds::Kind,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{self, client::IntoClientRequest},
};
use tokio_util::sync::CancellationToken;

use crate::{error::ClientError, transport::Transport};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type Writer = Arc<Mutex<Option<futures_util::stream::SplitSink<WsStream, tungstenite::Message>>>>;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PENDING_REQUESTS: usize = 1024;
const MAX_MESSAGE_SIZE: usize = 1024 * 1024;
const PUSH_CAPACITY: usize = 64;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

/// An unsolicited change notification, separate from correlated call responses.
#[derive(Debug)]
pub struct WatchPush {
    pub watch_id: String,
    pub grid: HGrid,
}

#[derive(Clone, Copy)]
enum Terminal {
    Closed,
    Protocol,
    Io,
    Overload,
}
impl Terminal {
    fn error(self) -> ClientError {
        match self {
            Self::Closed => ClientError::ConnectionClosed,
            Self::Protocol => ClientError::Transport("invalid WebSocket message".into()),
            Self::Io => {
                ClientError::Transport("WebSocket I/O failed (delivery may have occurred)".into())
            }
            Self::Overload => {
                ClientError::Transport("WebSocket push queue capacity exceeded".into())
            }
        }
    }
}
#[derive(Clone)]
enum Expected {
    Subscribe,
    Poll(String),
    Unsubscribe(Option<String>),
}
struct Pending {
    sender: oneshot::Sender<Result<HGrid, ClientError>>,
    expected: Expected,
}
struct State {
    terminal: Option<Terminal>,
    next_id: u64,
    pending: HashMap<String, Pending>,
}
struct Shared {
    state: StateMutex<State>,
    stopped: CancellationToken,
}
impl Shared {
    fn terminate(&self, reason: Terminal) {
        let pending = {
            let mut state = self.state.lock().unwrap();
            if state.terminal.is_some() {
                return;
            }
            state.terminal = Some(reason);
            std::mem::take(&mut state.pending)
        };
        self.stopped.cancel();
        for (_, pending) in pending {
            let _ = pending.sender.send(Err(reason.error()));
        }
    }
    fn error(&self) -> ClientError {
        self.state
            .lock()
            .unwrap()
            .terminal
            .unwrap_or(Terminal::Closed)
            .error()
    }
    fn admit(
        self: &Arc<Self>,
        expected: Expected,
    ) -> Result<(PendingGuard, oneshot::Receiver<Result<HGrid, ClientError>>), ClientError> {
        let mut state = self.state.lock().unwrap();
        if let Some(reason) = state.terminal {
            return Err(reason.error());
        }
        if state.pending.len() >= MAX_PENDING_REQUESTS {
            return Err(ClientError::TooManyRequests);
        }
        let next = state
            .next_id
            .checked_add(1)
            .ok_or(ClientError::TooManyRequests)?;
        let id = state.next_id.to_string();
        state.next_id = next;
        let (sender, receiver) = oneshot::channel();
        state
            .pending
            .insert(id.clone(), Pending { sender, expected });
        Ok((
            PendingGuard {
                shared: self.clone(),
                id,
                sending: false,
            },
            receiver,
        ))
    }
}
/// Removing a waiting call is safe; cancelling a sink write has unknown delivery.
struct PendingGuard {
    shared: Arc<Shared>,
    id: String,
    sending: bool,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().pending.remove(&self.id);
        if self.sending {
            self.shared.terminate(Terminal::Io);
        }
    }
}
struct ReaderExit(Arc<Shared>);
impl Drop for ReaderExit {
    fn drop(&mut self) {
        self.0.terminate(Terminal::Closed);
    }
}

/// Text-only `watchSub`, `watchPoll`, and `watchUnsub` transport.
///
/// Requests carry a string `reqId`; response rows use Haystack JSON v3 values.
/// Frames and reassembled messages are limited to 1 MiB. Binary/compressed
/// application messages and generic Haystack operations are unsupported.
pub struct WsTransport {
    writer: Writer,
    shared: Arc<Shared>,
    request_timeout: Duration,
    reader: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pushes: Mutex<mpsc::Receiver<WatchPush>>,
}
impl WsTransport {
    /// Connect using a bearer token obtained through HTTP authentication.
    pub async fn connect(url: &str, auth_token: &str) -> Result<Self, ClientError> {
        Self::connect_with_timeout(url, auth_token, DEFAULT_REQUEST_TIMEOUT).await
    }
    /// Connect with a whole-call deadline covering admission, writing, and response.
    pub async fn connect_with_timeout(
        url: &str,
        auth_token: &str,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        crate::ensure_crypto_provider();
        let mut request = url
            .into_client_request()
            .map_err(|_| ClientError::Connection("invalid WebSocket URL".into()))?;
        request.headers_mut().insert(
            "Authorization",
            format!("BEARER authToken={auth_token}")
                .parse()
                .map_err(|_| ClientError::Connection("invalid WebSocket credentials".into()))?,
        );
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_SIZE))
            .max_frame_size(Some(MAX_MESSAGE_SIZE))
            .max_write_buffer_size(MAX_MESSAGE_SIZE * 2);
        let (stream, _) = tokio::time::timeout(
            Duration::from_secs(15),
            connect_async_with_config(request, Some(config), false),
        )
        .await
        .map_err(|_| ClientError::Connection("WebSocket connect timed out".into()))?
        .map_err(|_| ClientError::Connection("WebSocket connect failed".into()))?;
        let (writer, reader) = stream.split();
        let writer = Arc::new(Mutex::new(Some(writer)));
        let shared = Arc::new(Shared {
            state: StateMutex::new(State {
                terminal: None,
                next_id: 1,
                pending: HashMap::new(),
            }),
            stopped: CancellationToken::new(),
        });
        let (push_tx, push_rx) = mpsc::channel(PUSH_CAPACITY);
        let task = spawn_reader(reader, writer.clone(), shared.clone(), push_tx);
        Ok(Self {
            writer,
            shared,
            request_timeout: timeout,
            reader: Mutex::new(Some(task)),
            pushes: Mutex::new(push_rx),
        })
    }
    /// Whether this connection has entered its permanent terminal state.
    pub fn is_closed(&self) -> bool {
        self.shared.stopped.is_cancelled()
    }
    /// Wait for the next unsolicited push. Overflow terminates the connection.
    pub async fn next_push(&self) -> Result<WatchPush, ClientError> {
        tokio::select! {
            biased;
            _ = self.shared.stopped.cancelled() => Err(self.shared.error()),
            push = async { self.pushes.lock().await.recv().await } => push.ok_or_else(|| self.shared.error()),
        }
    }
    async fn call_until(
        &self,
        op: &str,
        req: &HGrid,
        deadline: tokio::time::Instant,
    ) -> Result<HGrid, ClientError> {
        let (mut envelope, expected) = encode_request(op, req)?;
        let (mut guard, receiver) = self.shared.admit(expected)?;
        envelope["reqId"] = Value::String(guard.id.clone());
        let text = envelope.to_string();
        if text.len() > MAX_MESSAGE_SIZE {
            return Err(invalid_request());
        }
        let work = async {
            let mut writer = self.writer.lock().await;
            if self.is_closed() {
                return Err(self.shared.error());
            }
            let sink = writer.as_mut().ok_or(ClientError::ConnectionClosed)?;
            guard.sending = true;
            if sink
                .send(tungstenite::Message::Text(text.into()))
                .await
                .is_err()
            {
                self.shared.terminate(Terminal::Io);
                return Err(self.shared.error());
            }
            guard.sending = false;
            drop(writer);
            receiver.await.unwrap_or_else(|_| Err(self.shared.error()))
        };
        tokio::select! {
            biased;
            _ = self.shared.stopped.cancelled() => Err(self.shared.error()),
            result = tokio::time::timeout_at(deadline, work) => result.unwrap_or(Err(ClientError::Timeout(self.request_timeout))),
        }
    }
}

fn invalid_request() -> ClientError {
    ClientError::Transport("unsupported WebSocket operation or arguments".into())
}
fn valid_watch_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
}
fn encode_request(op: &str, grid: &HGrid) -> Result<(Value, Expected), ClientError> {
    let watch = match op {
        "watchSub" if grid.meta.is_empty() => None,
        "watchPoll" | "watchUnsub" if grid.meta.len() == 1 => match grid.meta.get("watchId") {
            Some(Kind::Str(id)) if valid_watch_id(id) => Some(id.clone()),
            _ => return Err(invalid_request()),
        },
        _ => return Err(invalid_request()),
    };
    if op == "watchPoll" {
        if !grid.rows.is_empty() || !grid.cols.is_empty() {
            return Err(invalid_request());
        }
        let watch = watch.unwrap();
        return Ok((json!({"op":op,"watchId":watch}), Expected::Poll(watch)));
    }
    if grid.rows.len() > 1000
        || (op == "watchSub" && grid.rows.is_empty())
        || grid.cols.len() != 1
        || grid.cols[0].name != "id"
        || !grid.cols[0].meta.is_empty()
    {
        return Err(invalid_request());
    }
    let mut ids = Vec::with_capacity(grid.rows.len());
    for row in &grid.rows {
        if row.len() != 1 {
            return Err(invalid_request());
        }
        let Some(Kind::Ref(id)) = row.get("id") else {
            return Err(invalid_request());
        };
        let id = id.val.strip_prefix('@').unwrap_or(&id.val);
        if id.is_empty() || id.len() > 1024 {
            return Err(invalid_request());
        }
        ids.push(id);
    }
    if op == "watchSub" {
        Ok((json!({"op":op,"ids":ids}), Expected::Subscribe))
    } else {
        Ok((
            json!({"op":op,"watchId":watch,"ids":ids}),
            Expected::Unsubscribe(if ids.is_empty() { None } else { watch }),
        ))
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    #[serde(rename = "reqId")]
    req_id: Option<String>,
    #[serde(rename = "watchId")]
    watch_id: Option<String>,
    rows: Option<Vec<Value>>,
    error: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}
fn decode_rows(rows: Vec<Value>, watch: Option<&str>) -> Result<HGrid, Terminal> {
    let mut cols = BTreeSet::new();
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        let Value::Object(tags) = row else {
            return Err(Terminal::Protocol);
        };
        let mut dict = HDict::new();
        for (name, value) in tags {
            cols.insert(name.clone());
            dict.set(
                name,
                v3::decode_kind(&value).map_err(|_| Terminal::Protocol)?,
            );
        }
        decoded.push(dict);
    }
    let mut meta = HDict::new();
    if let Some(watch) = watch {
        meta.set("watchId", Kind::Str(watch.into()));
    }
    Ok(HGrid::from_parts(
        meta,
        cols.into_iter().map(HCol::new).collect(),
        decoded,
    ))
}
fn dispatch(text: &str, shared: &Shared, pushes: &mpsc::Sender<WatchPush>) -> Result<(), Terminal> {
    let message: Envelope = serde_json::from_str(text).map_err(|_| Terminal::Protocol)?;
    if message
        .watch_id
        .as_deref()
        .is_some_and(|id| !valid_watch_id(id))
    {
        return Err(Terminal::Protocol);
    }
    if let Some(kind) = message.kind {
        if kind != "push" || message.req_id.is_some() || message.error.is_some() {
            return Err(Terminal::Protocol);
        }
        let watch_id = message.watch_id.ok_or(Terminal::Protocol)?;
        let grid = decode_rows(message.rows.ok_or(Terminal::Protocol)?, Some(&watch_id))?;
        return pushes
            .try_send(WatchPush { watch_id, grid })
            .map_err(|_| Terminal::Overload);
    }
    let id = message
        .req_id
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or(Terminal::Protocol)?;
    let result = if message.error.is_some() {
        if message.rows.is_some() || message.watch_id.is_some() {
            return Err(Terminal::Protocol);
        }
        Err(ClientError::ServerError(
            "WebSocket watch operation rejected".into(),
        ))
    } else {
        Ok(decode_rows(
            message.rows.ok_or(Terminal::Protocol)?,
            message.watch_id.as_deref(),
        )?)
    };
    let mut state = shared.state.lock().unwrap();
    // Valid late, duplicate, or unknown IDs cannot complete another request.
    let Some(pending) = state.pending.get(&id) else {
        return Ok(());
    };
    if let Ok(grid) = &result {
        let valid = match &pending.expected {
            Expected::Subscribe => message.watch_id.is_some(),
            Expected::Poll(watch) => message.watch_id.as_ref() == Some(watch),
            Expected::Unsubscribe(watch) => message.watch_id == *watch && grid.rows.is_empty(),
        };
        if !valid {
            return Err(Terminal::Protocol);
        }
    }
    let pending = state.pending.remove(&id).unwrap();
    drop(state);
    let _ = pending.sender.send(result);
    Ok(())
}
fn spawn_reader(
    mut reader: futures_util::stream::SplitStream<WsStream>,
    writer: Writer,
    shared: Arc<Shared>,
    pushes: mpsc::Sender<WatchPush>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _exit = ReaderExit(shared.clone());
        let reason = loop {
            tokio::select! {
                biased;
                _ = shared.stopped.cancelled() => break Terminal::Closed,
                message = reader.next() => match message {
                    Some(Ok(tungstenite::Message::Text(text))) => if let Err(reason) = dispatch(&text, &shared, &pushes) { break reason; },
                    Some(Ok(tungstenite::Message::Ping(_))) => {
                        // Tungstenite queues the mandatory pong while reading.
                        let flush = async { let mut writer = writer.lock().await; match writer.as_mut() { Some(sink) => sink.flush().await.is_ok(), None => false } };
                        tokio::select! { _ = shared.stopped.cancelled() => break Terminal::Closed, result = tokio::time::timeout(CLOSE_TIMEOUT, flush) => if !matches!(result, Ok(true)) { break Terminal::Io; } }
                    },
                    Some(Ok(tungstenite::Message::Pong(_))) => {},
                    Some(Ok(tungstenite::Message::Close(_))) | None => break Terminal::Closed,
                    Some(Ok(_)) => break Terminal::Protocol,
                    Some(Err(_)) => break Terminal::Io,
                }
            }
        };
        shared.terminate(reason);
        let _ = tokio::time::timeout(CLOSE_TIMEOUT / 2, async {
            if let Some(mut sink) = writer.lock().await.take() {
                let _ = sink.send(tungstenite::Message::Close(None)).await;
            }
        })
        .await;
    })
}
impl Transport for WsTransport {
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        self.call_until(op, req, tokio::time::Instant::now() + self.request_timeout)
            .await
    }
    async fn close(&self) -> Result<(), ClientError> {
        self.shared.terminate(Terminal::Closed);
        // Keep the handle stored while awaiting: cancellation of close leaves it joinable.
        let mut reader = self.reader.lock().await;
        if let Some(task) = reader.as_mut()
            && tokio::time::timeout(CLOSE_TIMEOUT, &mut *task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
        reader.take();
        self.writer.lock().await.take();
        Ok(())
    }
}
impl Drop for WsTransport {
    fn drop(&mut self) {
        self.shared.terminate(Terminal::Closed);
        if let Some(task) = self.reader.get_mut().take() {
            task.abort();
        }
    }
}

/// Reconnects only before a new call when a previous connection is terminal.
/// A dispatched call is never replayed. Explicit close permanently disables reconnect.
pub struct ReconnectingWsTransport {
    url: String,
    auth_token: zeroize::Zeroizing<String>,
    request_timeout: Duration,
    inner: Mutex<Option<Arc<WsTransport>>>,
    stopped: CancellationToken,
    admission: Semaphore,
}
impl ReconnectingWsTransport {
    pub async fn connect(url: &str, auth_token: &str) -> Result<Self, ClientError> {
        Self::connect_with_timeout(url, auth_token, DEFAULT_REQUEST_TIMEOUT).await
    }
    pub async fn connect_with_timeout(
        url: &str,
        auth_token: &str,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let transport = WsTransport::connect_with_timeout(url, auth_token, timeout).await?;
        Ok(Self {
            url: url.into(),
            auth_token: zeroize::Zeroizing::new(auth_token.into()),
            request_timeout: timeout,
            inner: Mutex::new(Some(Arc::new(transport))),
            stopped: CancellationToken::new(),
            admission: Semaphore::new(MAX_PENDING_REQUESTS),
        })
    }
    pub async fn next_push(&self) -> Result<WatchPush, ClientError> {
        let work = async {
            let transport = self
                .inner
                .lock()
                .await
                .as_ref()
                .cloned()
                .ok_or(ClientError::ConnectionClosed)?;
            transport.next_push().await
        };
        tokio::select! { biased; _ = self.stopped.cancelled() => Err(ClientError::ConnectionClosed), result = work => result }
    }
}
impl Transport for ReconnectingWsTransport {
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        let deadline = tokio::time::Instant::now() + self.request_timeout;
        encode_request(op, req)?;
        let _permit = self
            .admission
            .try_acquire()
            .map_err(|_| ClientError::TooManyRequests)?;
        let work = async {
            let transport = {
                let mut inner = self.inner.lock().await;
                if inner.as_ref().is_none_or(|transport| transport.is_closed()) {
                    inner.take();
                    let transport = WsTransport::connect_with_timeout(
                        &self.url,
                        &self.auth_token,
                        self.request_timeout,
                    )
                    .await?;
                    if self.stopped.is_cancelled() {
                        return Err(ClientError::ConnectionClosed);
                    }
                    *inner = Some(Arc::new(transport));
                }
                inner.as_ref().unwrap().clone()
            };
            transport.call_until(op, req, deadline).await
        };
        tokio::select! {
            biased;
            _ = self.stopped.cancelled() => Err(ClientError::ConnectionClosed),
            result = tokio::time::timeout_at(deadline, work) => result.unwrap_or(Err(ClientError::Timeout(self.request_timeout))),
        }
    }
    async fn close(&self) -> Result<(), ClientError> {
        self.stopped.cancel();
        let transport = self.inner.lock().await.take();
        if let Some(transport) = transport {
            transport.close().await?;
        }
        Ok(())
    }
}
impl Drop for ReconnectingWsTransport {
    fn drop(&mut self) {
        self.stopped.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            state: StateMutex::new(State {
                terminal: None,
                next_id: 1,
                pending: HashMap::new(),
            }),
            stopped: CancellationToken::new(),
        })
    }
    #[test]
    fn admission_is_atomic_bounded_and_cancellation_reclaims_slots() {
        let shared = shared();
        let calls = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    let shared = shared.clone();
                    scope.spawn(move || {
                        (0..256)
                            .filter_map(|_| shared.admit(Expected::Subscribe).ok())
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(calls.len(), MAX_PENDING_REQUESTS);
        assert_eq!(
            shared.state.lock().unwrap().pending.len(),
            MAX_PENDING_REQUESTS
        );
        assert!(matches!(
            shared.admit(Expected::Subscribe),
            Err(ClientError::TooManyRequests)
        ));
        drop(calls);
        assert!(shared.state.lock().unwrap().pending.is_empty());
        let (guard, mut receiver) = shared.admit(Expected::Subscribe).unwrap();
        shared.terminate(Terminal::Closed);
        assert!(matches!(
            receiver.try_recv(),
            Ok(Err(ClientError::ConnectionClosed))
        ));
        assert!(shared.state.lock().unwrap().pending.is_empty());
        assert!(matches!(
            shared.admit(Expected::Subscribe),
            Err(ClientError::ConnectionClosed)
        ));
        drop(guard);
    }
    #[tokio::test]
    async fn writer_queue_wait_obeys_whole_call_deadline_and_close_joins_reader() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            assert!(ws.next().await.unwrap().unwrap().is_close());
        });
        let transport = WsTransport::connect_with_timeout(&url, "token", Duration::from_millis(30))
            .await
            .unwrap();
        let held = transport.writer.lock().await;
        let mut row = HDict::new();
        row.set("id", Kind::Ref(haystack_core::kinds::HRef::from_val("p")));
        let grid = HGrid::from_parts(HDict::new(), vec![HCol::new("id")], vec![row]);
        let result = tokio::time::timeout(
            Duration::from_millis(300),
            transport.call("watchSub", &grid),
        )
        .await
        .unwrap();
        assert!(matches!(result, Err(ClientError::Timeout(_))));
        assert!(transport.shared.state.lock().unwrap().pending.is_empty());
        assert!(!transport.is_closed());
        // Cancellation before the writer is available also unregisters promptly.
        let mut call = Box::pin(transport.call("watchSub", &grid));
        tokio::select! { _ = &mut call => panic!("writer is held"), _ = tokio::time::sleep(Duration::from_millis(1)) => {} }
        assert_eq!(transport.shared.state.lock().unwrap().pending.len(), 1);
        drop(call);
        assert!(transport.shared.state.lock().unwrap().pending.is_empty());
        drop(held);
        transport.close().await.unwrap();
        assert!(transport.reader.lock().await.is_none());
        assert!(transport.shared.state.lock().unwrap().pending.is_empty());
        tokio::time::timeout(Duration::from_secs(1), peer)
            .await
            .unwrap()
            .unwrap();
    }
    #[test]
    fn cancellation_during_write_seals_connection_and_settles_other_calls() {
        let shared = shared();
        let (mut writing, _) = shared.admit(Expected::Subscribe).unwrap();
        let (_other, mut receiver) = shared.admit(Expected::Subscribe).unwrap();
        writing.sending = true;
        drop(writing);
        assert!(shared.stopped.is_cancelled());
        assert!(receiver.try_recv().unwrap().is_err());
        assert!(shared.state.lock().unwrap().pending.is_empty());
    }
}
