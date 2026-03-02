use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, oneshot};
use tokio_tungstenite::{connect_async, tungstenite};

use crate::error::ClientError;
use crate::transport::Transport;
use haystack_core::codecs::codec_for;
use haystack_core::data::HGrid;
use haystack_core::kinds::Kind;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Default timeout for a single WS request-response round-trip.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of concurrent in-flight requests.
const MAX_PENDING_REQUESTS: usize = 1024;

/// WebSocket transport for communicating with a Haystack server.
///
/// Uses a JSON envelope with Zinc-encoded grid bodies:
/// - Request:  `{"id": "<counter>", "op": "<op_name>", "body": "<zinc_grid>"}`
/// - Response: `{"id": "<counter>", "body": "<zinc_grid>"}`
///
/// Supports concurrent in-flight requests by matching response IDs to pending
/// oneshot channels via a background reader task.
pub struct WsTransport {
    writer: Mutex<futures_util::stream::SplitSink<WsStream, tungstenite::Message>>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<HGrid, ClientError>>>>,
    next_id: AtomicU64,
    /// Per-request timeout duration.
    request_timeout: Duration,
    /// Handle to the background reader task (kept alive for the transport's lifetime).
    _reader_handle: tokio::task::JoinHandle<()>,
    /// Cancellation token for graceful shutdown of the reader task.
    shutdown: tokio_util::sync::CancellationToken,
}

impl WsTransport {
    /// Connect to a Haystack server via WebSocket.
    ///
    /// `url` should be a `ws://` or `wss://` URL to the server's WebSocket endpoint.
    /// `auth_token` is the bearer token obtained from SCRAM authentication.
    pub async fn connect(url: &str, auth_token: &str) -> Result<Self, ClientError> {
        let request = tungstenite::http::Request::builder()
            .uri(url)
            .header("Authorization", format!("BEARER authToken={}", auth_token))
            .header(
                "Sec-WebSocket-Key",
                tungstenite::handshake::client::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Host", extract_host(url).unwrap_or_default())
            .body(())
            .map_err(|e| ClientError::Transport(e.to_string()))?;

        let (ws_stream, _response) =
            tokio::time::timeout(Duration::from_secs(15), connect_async(request))
                .await
                .map_err(|_| ClientError::Transport("WebSocket connect timed out".to_string()))?
                .map_err(|e| ClientError::Transport(format!("WebSocket connect failed: {}", e)))?;

        let (writer, reader) = ws_stream.split();
        let pending: Arc<DashMap<u64, oneshot::Sender<Result<HGrid, ClientError>>>> =
            Arc::new(DashMap::new());

        let shutdown = tokio_util::sync::CancellationToken::new();
        let reader_handle = spawn_reader_task(reader, Arc::clone(&pending), shutdown.child_token());

        Ok(Self {
            writer: Mutex::new(writer),
            pending,
            next_id: AtomicU64::new(1),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            _reader_handle: reader_handle,
            shutdown,
        })
    }

    /// Connect with a custom request timeout.
    pub async fn connect_with_timeout(
        url: &str,
        auth_token: &str,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let mut transport = Self::connect(url, auth_token).await?;
        transport.request_timeout = timeout;
        Ok(transport)
    }
}

/// Spawn a background task that reads WS messages and dispatches responses
/// to the appropriate pending oneshot channel by matching the response `id`.
fn spawn_reader_task(
    mut reader: futures_util::stream::SplitStream<WsStream>,
    pending: Arc<DashMap<u64, oneshot::Sender<Result<HGrid, ClientError>>>>,
    shutdown: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let codec = codec_for("text/zinc");

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    drain_pending(&pending, ClientError::ConnectionClosed);
                    break;
                }
                msg = reader.next() => {
                    let Some(msg) = msg else { break };
                    match msg {
                        Ok(tungstenite::Message::Text(text)) => {
                            handle_text_message(&text, codec, &pending);
                        }
                        Ok(tungstenite::Message::Binary(data)) => {
                            // Compressed message: deflate-compressed JSON envelope.
                            if let Ok(decompressed) = decompress_deflate(&data) {
                                handle_text_message(&decompressed, codec, &pending);
                            }
                        }
                        Ok(tungstenite::Message::Close(_)) => {
                            drain_pending(&pending, ClientError::ConnectionClosed);
                            break;
                        }
                        Err(e) => {
                            drain_pending(&pending, ClientError::Transport(e.to_string()));
                            break;
                        }
                        _ => continue, // ping/pong handled by tungstenite
                    }
                }
            }
        }
    })
}

/// Process a text (or decompressed) JSON envelope and dispatch to the pending channel.
fn handle_text_message(
    text: &str,
    codec: Option<&'static dyn haystack_core::codecs::Codec>,
    pending: &DashMap<u64, oneshot::Sender<Result<HGrid, ClientError>>>,
) {
    let resp: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    let resp_id: u64 = match resp.get("id").and_then(|v| {
        v.as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v.as_u64())
    }) {
        Some(id) => id,
        None => return,
    };

    let result = match (codec, resp.get("body").and_then(|v| v.as_str())) {
        (Some(c), Some(body)) => match c.decode_grid(body) {
            Ok(grid) => {
                if grid.is_err() {
                    let dis = grid
                        .meta
                        .get("dis")
                        .and_then(|k| {
                            if let Kind::Str(s) = k {
                                Some(s.as_str())
                            } else {
                                None
                            }
                        })
                        .unwrap_or("unknown server error");
                    Err(ClientError::ServerError(dis.to_string()))
                } else {
                    Ok(grid)
                }
            }
            Err(e) => Err(ClientError::Codec(e.to_string())),
        },
        _ => Err(ClientError::Codec(
            "response missing 'body' field".to_string(),
        )),
    };

    if let Some((_, sender)) = pending.remove(&resp_id) {
        let _ = sender.send(result);
    }
}

/// Notify all pending requests with the given error and clear the map.
fn drain_pending(
    pending: &DashMap<u64, oneshot::Sender<Result<HGrid, ClientError>>>,
    error: ClientError,
) {
    let keys: Vec<u64> = pending.iter().map(|r| *r.key()).collect();
    for key in keys {
        if let Some((_, sender)) = pending.remove(&key) {
            let _ = sender.send(Err(ClientError::Transport(error.to_string())));
        }
    }
}

/// Compress data with deflate (flate2).
fn compress_deflate(data: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::DeflateEncoder;
    use std::io::Write;

    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_else(|_| data.to_vec())
}

/// Maximum decompressed payload size (10 MB) to prevent zip bomb attacks.
const MAX_DECOMPRESSED_SIZE: u64 = 10 * 1024 * 1024;

/// Decompress deflate-compressed data.
fn decompress_deflate(data: &[u8]) -> Result<String, std::io::Error> {
    use flate2::read::DeflateDecoder;
    use std::io::Read;

    let decoder = DeflateDecoder::new(data);
    let mut limited = decoder.take(MAX_DECOMPRESSED_SIZE);
    let mut output = String::new();
    limited.read_to_string(&mut output)?;
    Ok(output)
}

/// Minimum payload size (bytes) to consider compressing with deflate.
const COMPRESSION_THRESHOLD: usize = 512;

/// Extract the host (with optional port) from a URL string.
fn extract_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_string();
    match parsed.port() {
        Some(port) => Some(format!("{}:{}", host, port)),
        None => Some(host),
    }
}

impl Transport for WsTransport {
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        // Bounded pending map check.
        if self.pending.len() >= MAX_PENDING_REQUESTS {
            return Err(ClientError::TooManyRequests);
        }

        let codec = codec_for("text/zinc")
            .ok_or_else(|| ClientError::Codec("zinc codec not available".to_string()))?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let body = codec
            .encode_grid(req)
            .map_err(|e| ClientError::Codec(e.to_string()))?;

        let envelope = serde_json::json!({
            "id": id.to_string(),
            "op": op,
            "body": body,
        });

        let msg_text =
            serde_json::to_string(&envelope).map_err(|e| ClientError::Codec(e.to_string()))?;

        // Compress large payloads and send as binary frame.
        let ws_msg = if msg_text.len() >= COMPRESSION_THRESHOLD {
            let compressed = compress_deflate(msg_text.as_bytes());
            if compressed.len() < msg_text.len() {
                tungstenite::Message::Binary(compressed.into())
            } else {
                tungstenite::Message::Text(msg_text.into())
            }
        } else {
            tungstenite::Message::Text(msg_text.into())
        };

        // Register a oneshot channel for this request.
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);

        // Send the request.
        {
            let mut writer = self.writer.lock().await;
            if let Err(e) = writer.send(ws_msg).await {
                self.pending.remove(&id);
                return Err(ClientError::Transport(e.to_string()));
            }
        }

        // Await the response with a timeout.
        let timeout = self.request_timeout;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ClientError::Transport(
                "response channel closed unexpectedly".to_string(),
            )),
            Err(_) => {
                self.pending.remove(&id);
                Err(ClientError::Timeout(timeout))
            }
        }
    }

    async fn close(&self) -> Result<(), ClientError> {
        self.shutdown.cancel();
        let mut writer = self.writer.lock().await;
        writer
            .send(tungstenite::Message::Close(None))
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(())
    }
}

impl Drop for WsTransport {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

// ---------------------------------------------------------------------------
// Reconnecting transport wrapper
// ---------------------------------------------------------------------------

/// Initial backoff delay before the first reconnection attempt.
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);
/// Maximum backoff delay between reconnection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Maximum number of consecutive reconnection attempts before giving up.
const MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// A WebSocket transport that automatically reconnects on connection loss.
///
/// Uses exponential backoff with jitter between reconnection attempts.
/// Requests that arrive during reconnection are queued and retried once the
/// connection is re-established.
pub struct ReconnectingWsTransport {
    url: String,
    auth_token: String,
    request_timeout: Duration,
    inner: Mutex<Option<WsTransport>>,
}

impl ReconnectingWsTransport {
    /// Create a new reconnecting transport.  An initial connection is
    /// established immediately; use [`Self::connect`] for the async builder.
    pub async fn connect(url: &str, auth_token: &str) -> Result<Self, ClientError> {
        let transport = WsTransport::connect(url, auth_token).await?;
        Ok(Self {
            url: url.to_string(),
            auth_token: auth_token.to_string(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            inner: Mutex::new(Some(transport)),
        })
    }

    /// Create a new reconnecting transport with a custom request timeout.
    pub async fn connect_with_timeout(
        url: &str,
        auth_token: &str,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        let transport = WsTransport::connect_with_timeout(url, auth_token, timeout).await?;
        Ok(Self {
            url: url.to_string(),
            auth_token: auth_token.to_string(),
            request_timeout: timeout,
            inner: Mutex::new(Some(transport)),
        })
    }

    /// Try to reconnect using exponential backoff with jitter.
    /// Returns `Ok(())` when a new connection is established, or `Err` after
    /// exhausting all attempts.
    async fn reconnect(&self) -> Result<(), ClientError> {
        use rand::RngExt;

        let mut backoff = INITIAL_BACKOFF;

        for attempt in 1..=MAX_RECONNECT_ATTEMPTS {
            // Add random jitter: ±25% of current backoff.
            let jitter_range = backoff.as_millis() as u64 / 4;
            let jitter = if jitter_range > 0 {
                let offset = rand::rng().random_range(0..jitter_range * 2);
                Duration::from_millis(offset)
            } else {
                Duration::ZERO
            };
            let delay = backoff
                .saturating_add(jitter)
                .saturating_sub(Duration::from_millis(jitter_range));
            tokio::time::sleep(delay).await;

            match WsTransport::connect_with_timeout(
                &self.url,
                &self.auth_token,
                self.request_timeout,
            )
            .await
            {
                Ok(transport) => {
                    *self.inner.lock().await = Some(transport);
                    return Ok(());
                }
                Err(_) if attempt < MAX_RECONNECT_ATTEMPTS => {
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
                Err(e) => {
                    return Err(ClientError::Transport(format!(
                        "reconnection failed after {MAX_RECONNECT_ATTEMPTS} attempts: {e}"
                    )));
                }
            }
        }

        Err(ClientError::Transport(
            "reconnection failed: max attempts exhausted".to_string(),
        ))
    }
}

impl Transport for ReconnectingWsTransport {
    async fn call(&self, op: &str, req: &HGrid) -> Result<HGrid, ClientError> {
        // Fast path: use existing connection.
        {
            let guard = self.inner.lock().await;
            if let Some(ref transport) = *guard {
                match transport.call(op, req).await {
                    Ok(grid) => return Ok(grid),
                    Err(ClientError::Timeout(d)) => return Err(ClientError::Timeout(d)),
                    Err(ClientError::ServerError(e)) => return Err(ClientError::ServerError(e)),
                    Err(ClientError::TooManyRequests) => {
                        return Err(ClientError::TooManyRequests);
                    }
                    Err(_) => {
                        // Connection-level error; fall through to reconnect.
                    }
                }
            }
        }

        // Drop current transport and reconnect.
        *self.inner.lock().await = None;
        self.reconnect().await?;

        // Retry the request on the new connection.
        let guard = self.inner.lock().await;
        match guard.as_ref() {
            Some(transport) => transport.call(op, req).await,
            None => Err(ClientError::ConnectionClosed),
        }
    }

    async fn close(&self) -> Result<(), ClientError> {
        if let Some(transport) = self.inner.lock().await.take() {
            transport.close().await
        } else {
            Ok(())
        }
    }
}
