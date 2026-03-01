use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite};

use crate::error::ClientError;
use crate::transport::Transport;
use haystack_core::codecs::codec_for;
use haystack_core::data::HGrid;
use haystack_core::kinds::Kind;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// WebSocket transport for communicating with a Haystack server.
///
/// Uses a JSON envelope with Zinc-encoded grid bodies:
/// - Request:  `{"id": "<counter>", "op": "<op_name>", "body": "<zinc_grid>"}`
/// - Response: `{"id": "<counter>", "body": "<zinc_grid>"}`
pub struct WsTransport {
    writer: Mutex<futures_util::stream::SplitSink<WsStream, tungstenite::Message>>,
    reader: Mutex<futures_util::stream::SplitStream<WsStream>>,
    next_id: AtomicU64,
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

        let (ws_stream, _response) = connect_async(request)
            .await
            .map_err(|e| ClientError::Transport(format!("WebSocket connect failed: {}", e)))?;

        let (writer, reader) = ws_stream.split();

        Ok(Self {
            writer: Mutex::new(writer),
            reader: Mutex::new(reader),
            next_id: AtomicU64::new(1),
        })
    }
}

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
        let codec = codec_for("text/zinc")
            .ok_or_else(|| ClientError::Codec("zinc codec not available".to_string()))?;

        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();

        let body = codec
            .encode_grid(req)
            .map_err(|e| ClientError::Codec(e.to_string()))?;

        let envelope = serde_json::json!({
            "id": id,
            "op": op,
            "body": body,
        });

        let msg_text =
            serde_json::to_string(&envelope).map_err(|e| ClientError::Codec(e.to_string()))?;

        // Send the request
        {
            let mut writer = self.writer.lock().await;
            writer
                .send(tungstenite::Message::Text(msg_text.into()))
                .await
                .map_err(|e| ClientError::Transport(e.to_string()))?;
        }

        // Read the response
        // In a production implementation, we would match by id to support
        // concurrent requests. For now, we read the next message sequentially.
        let resp_text = {
            let mut reader = self.reader.lock().await;
            loop {
                match reader.next().await {
                    Some(Ok(tungstenite::Message::Text(text))) => break text.to_string(),
                    Some(Ok(tungstenite::Message::Close(_))) => {
                        return Err(ClientError::ConnectionClosed);
                    }
                    Some(Ok(_)) => {
                        // Skip non-text messages (ping, pong, binary)
                        continue;
                    }
                    Some(Err(e)) => {
                        return Err(ClientError::Transport(e.to_string()));
                    }
                    None => {
                        return Err(ClientError::ConnectionClosed);
                    }
                }
            }
        };

        // Parse the JSON envelope
        let resp: serde_json::Value = serde_json::from_str(&resp_text)
            .map_err(|e| ClientError::Codec(format!("invalid JSON response: {}", e)))?;

        let resp_body = resp
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ClientError::Codec("response missing 'body' field".to_string()))?;

        // Decode the response grid
        let grid = codec
            .decode_grid(resp_body)
            .map_err(|e| ClientError::Codec(e.to_string()))?;

        // Check for error grid
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
            return Err(ClientError::ServerError(dis.to_string()));
        }

        Ok(grid)
    }

    async fn close(&self) -> Result<(), ClientError> {
        let mut writer = self.writer.lock().await;
        writer
            .send(tungstenite::Message::Close(None))
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        Ok(())
    }
}
