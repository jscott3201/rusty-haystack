//! Negotiated scoped state subscriptions. This transport reuses a private bearer
//! through HttpTransport; it never logs in, retries, or reconnects by itself.
use crate::{ClientError, transport::Transport};
use futures_util::{SinkExt, StreamExt};
use haystack_core::{
    codecs::subscription::{self as wire, SubscriptionOutcome, SubscriptionRequest},
    data::HGrid,
};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::sync::{Mutex, Semaphore};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{self, client::IntoClientRequest},
};
type Stream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
const TIMEOUT: Duration = Duration::from_secs(30);
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    profile: String,
    #[serde(rename = "reqId")]
    req_id: String,
    payload: String,
}
/// Calls are serialized with eight bounded callers. Cancelling a dispatched
/// call drops its socket; an uncertain partial send can never be continued.
pub struct SubscriptionWsTransport {
    socket: Mutex<Option<Stream>>,
    callers: Semaphore,
    next_id: AtomicU64,
    stopped: tokio_util::sync::CancellationToken,
}
impl SubscriptionWsTransport {
    pub(crate) async fn attach(url: &str, bearer: &str) -> Result<Self, ClientError> {
        crate::ensure_crypto_provider();
        let mut request = url.into_client_request().map_err(|_| invalid())?;
        let credential = zeroize::Zeroizing::new(format!("BEARER authToken={bearer}"));
        request
            .headers_mut()
            .insert("Authorization", crate::auth::sensitive_header(&credential)?);
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            wire::WS_PROTOCOL.parse().expect("static protocol"),
        );
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(wire::MAX_WIRE_BYTES))
            .max_frame_size(Some(wire::MAX_WIRE_BYTES))
            .max_write_buffer_size(wire::MAX_WIRE_BYTES * 2);
        let (socket, response) = tokio::time::timeout(
            TIMEOUT,
            connect_async_with_config(request, Some(config), false),
        )
        .await
        .map_err(|_| ClientError::Timeout(TIMEOUT))?
        .map_err(|_| ClientError::Connection("subscription WebSocket upgrade failed".into()))?;
        if response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok())
            != Some(wire::WS_PROTOCOL)
        {
            return Err(ClientError::Connection(
                "subscription profile was not negotiated".into(),
            ));
        }
        Ok(Self {
            socket: Mutex::new(Some(socket)),
            callers: Semaphore::new(8),
            next_id: AtomicU64::new(1),
            stopped: tokio_util::sync::CancellationToken::new(),
        })
    }
}
fn invalid() -> ClientError {
    ClientError::Transport("invalid scoped subscription response or request".into())
}
impl Transport for SubscriptionWsTransport {
    async fn call(&self, op: &str, grid: &HGrid) -> Result<HGrid, ClientError> {
        let request: SubscriptionRequest = wire::from_grid(grid).map_err(|_| invalid())?;
        if request.operation() != op {
            return Err(invalid());
        }
        let _caller = self
            .callers
            .try_acquire()
            .map_err(|_| ClientError::Transport("subscription caller capacity exceeded".into()))?;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let id = self
            .next_id
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| ClientError::ConnectionClosed)?
            .to_string();
        let payload = String::from_utf8(wire::encode(&request).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
        let frame = serde_json::to_string(&Frame {
            profile: wire::PROFILE.into(),
            req_id: id.clone(),
            payload,
        })
        .map_err(|_| invalid())?;
        if frame.len() > wire::MAX_WIRE_BYTES {
            return Err(invalid());
        }
        tokio::select! {biased;_=self.stopped.cancelled()=>Err(ClientError::ConnectionClosed),result=tokio::time::timeout_at(deadline,async{
            let mut slot=self.socket.lock().await;
            if tokio::time::Instant::now()>=deadline{return Err(ClientError::Timeout(TIMEOUT))}
            // Restore only after complete correlation and typed validation. Drop
            // on cancellation/error closes the transport without any replay.
            let mut socket=slot.take().ok_or(ClientError::ConnectionClosed)?;
            socket.send(tungstenite::Message::Text(frame.into())).await.map_err(|_|invalid())?;
            loop {match socket.next().await {
                Some(Ok(tungstenite::Message::Text(text)))=>{
                    if text.len()>wire::MAX_WIRE_BYTES{return Err(invalid())}
                    let frame:Frame=serde_json::from_str(&text).map_err(|_|invalid())?;
                    if frame.profile!=wire::PROFILE||frame.req_id!=id||frame.payload.len()>wire::MAX_PAYLOAD_BYTES{return Err(invalid())}
                    let outcome:SubscriptionOutcome=wire::decode(frame.payload.as_bytes()).map_err(|_|invalid())?;
                    wire::validate_for_request(&outcome,&request).map_err(|_|invalid())?;
                    let grid=wire::to_grid(&outcome).map_err(|_|invalid())?;
                    *slot=Some(socket);return Ok(grid)
                },
                Some(Ok(tungstenite::Message::Ping(payload)))=>socket.send(tungstenite::Message::Pong(payload)).await.map_err(|_|invalid())?,
                Some(Ok(tungstenite::Message::Pong(_)))=>{},
                _=>return Err(invalid()),
            }}
        })=>result.map_err(|_|ClientError::Timeout(TIMEOUT))?}
    }
    async fn close(&self) -> Result<(), ClientError> {
        self.stopped.cancel();
        let work = async {
            let mut slot = self.socket.lock().await;
            if let Some(mut socket) = slot.take() {
                let _ = socket.close(None).await;
            }
            Ok(())
        };
        tokio::time::timeout(Duration::from_secs(1), work)
            .await
            .map_err(|_| ClientError::Timeout(Duration::from_secs(1)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HaystackClient;
    use haystack_core::codecs::subscription::{SubscriptionDelivery, SubscriptionId};
    use std::sync::Arc;
    struct Negotiated(Option<&'static str>);
    impl tungstenite::handshake::server::Callback for Negotiated {
        fn on_request(
            self,
            request: &tungstenite::handshake::server::Request,
            mut response: tungstenite::handshake::server::Response,
        ) -> Result<
            tungstenite::handshake::server::Response,
            tungstenite::handshake::server::ErrorResponse,
        > {
            assert_eq!(
                request.headers().get("Sec-WebSocket-Protocol").unwrap(),
                wire::WS_PROTOCOL
            );
            if let Some(profile) = self.0 {
                response
                    .headers_mut()
                    .insert("Sec-WebSocket-Protocol", profile.parse().unwrap());
            }
            Ok(response)
        }
    }
    async fn peer(
        profile: Option<&'static str>,
        bad: usize,
    ) -> (String, tokio::task::JoinHandle<usize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(socket, Negotiated(profile))
                .await
                .unwrap();
            if profile != Some(wire::WS_PROTOCOL) {
                return 0;
            }
            let Some(Ok(tungstenite::Message::Text(text))) = socket.next().await else {
                panic!("request")
            };
            let request: Frame = serde_json::from_str(&text).unwrap();
            let submitted: SubscriptionRequest = wire::decode(request.payload.as_bytes()).unwrap();
            let mut watch = submitted.watch().unwrap().clone();
            if bad == 10 {
                watch.watch = [9; 16];
            }
            let outcome = match &submitted {
                SubscriptionRequest::Acknowledge {
                    scope_generation,
                    through,
                    ..
                } => SubscriptionOutcome::Acknowledged {
                    watch,
                    scope_generation: *scope_generation,
                    token: [99; 16],
                    through: *through,
                },
                _ => SubscriptionOutcome::Delivery(Arc::new(SubscriptionDelivery {
                    watch,
                    dataset: [3; 16],
                    incarnation: [4; 16],
                    catalog_generation: 1,
                    scope_generation: if bad == 5 { 3 } else { 2 },
                    token: [5; 16],
                    from: if bad == 6 { 6 } else { 5 },
                    through: 7,
                    initial: false,
                    rows: vec![],
                    removed: vec![],
                })),
            };
            let mut frame = serde_json::json!({"profile":wire::PROFILE,"reqId":request.req_id,"payload":String::from_utf8(wire::encode(&outcome).unwrap()).unwrap()});
            let message = match bad {
                0 => {
                    frame["reqId"] = serde_json::json!("wrong");
                    tungstenite::Message::Text(frame.to_string().into())
                }
                1 => {
                    frame["profile"] = serde_json::json!("legacy");
                    tungstenite::Message::Text(frame.to_string().into())
                }
                2 => {
                    frame["reqId"] = serde_json::json!("01");
                    tungstenite::Message::Text(frame.to_string().into())
                }
                3 => {
                    frame["extra"] = serde_json::json!(true);
                    tungstenite::Message::Text(frame.to_string().into())
                }
                4 => {
                    frame["payload"] = serde_json::json!("not a typed payload");
                    tungstenite::Message::Text(frame.to_string().into())
                }
                8 => {
                    frame["payload"] = serde_json::json!("x".repeat(wire::MAX_PAYLOAD_BYTES + 1));
                    tungstenite::Message::Text(frame.to_string().into())
                }
                9 => tungstenite::Message::Binary(vec![1, 2, 3].into()),
                _ => tungstenite::Message::Text(frame.to_string().into()),
            };
            let _ = socket.send(message).await;
            let again = tokio::time::timeout(Duration::from_millis(200), socket.next()).await;
            assert!(
                !matches!(again, Ok(Some(Ok(tungstenite::Message::Text(_))))),
                "client must not replay a request after a bad response"
            );
            1
        });
        (format!("ws://{address}/api/ws"), task)
    }
    fn watch() -> SubscriptionId {
        SubscriptionId {
            authority: [1; 16],
            watch: [2; 16],
            key: "known".into(),
        }
    }
    #[tokio::test]
    async fn independent_peer_downgrade_is_rejected_before_any_subscription_frame() {
        for profile in [None, Some("legacy.watch.v0")] {
            let (url, task) = peer(profile, 0).await;
            assert!(
                SubscriptionWsTransport::attach(&url, "fixture-token")
                    .await
                    .is_err()
            );
            assert_eq!(task.await.unwrap(), 0);
        }
    }
    #[tokio::test]
    async fn malformed_identity_scope_fences_and_frames_are_unknown_without_replay() {
        for bad in 0..11 {
            let (url, task) = peer(Some(wire::WS_PROTOCOL), bad).await;
            let client = HaystackClient::from_transport(
                SubscriptionWsTransport::attach(&url, "fixture-token")
                    .await
                    .unwrap(),
            );
            let request = if bad == 7 {
                SubscriptionRequest::Acknowledge {
                    watch: watch(),
                    scope_generation: 2,
                    token: [5; 16],
                    through: 7,
                }
            } else {
                SubscriptionRequest::Resume {
                    watch: watch(),
                    scope_generation: 2,
                    acknowledged: 5,
                }
            };
            assert_eq!(
                client.state_subscription(&request).await.unwrap(),
                SubscriptionOutcome::Unknown,
                "bad case {bad}"
            );
            assert_eq!(
                client.state_subscription(&request).await.unwrap(),
                SubscriptionOutcome::Unknown
            );
            assert_eq!(task.await.unwrap(), 1);
        }
    }
    #[tokio::test]
    async fn cancellation_after_dispatch_closes_uncertain_socket_without_replay() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (seen, received) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket =
                tokio_tungstenite::accept_hdr_async(stream, Negotiated(Some(wire::WS_PROTOCOL)))
                    .await
                    .unwrap();
            assert!(matches!(
                socket.next().await,
                Some(Ok(tungstenite::Message::Text(_)))
            ));
            seen.send(()).unwrap();
            let next = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap();
            assert!(!matches!(next, Some(Ok(tungstenite::Message::Text(_)))));
        });
        let client = Arc::new(HaystackClient::from_transport(
            SubscriptionWsTransport::attach(&format!("ws://{address}/api/ws"), "fixture-token")
                .await
                .unwrap(),
        ));
        let request = SubscriptionRequest::Poll { watch: watch() };
        let call = {
            let client = client.clone();
            let request = request.clone();
            tokio::spawn(async move { client.state_subscription(&request).await })
        };
        received.await.unwrap();
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        assert_eq!(
            client.state_subscription(&request).await.unwrap(),
            SubscriptionOutcome::Unknown
        );
        peer.await.unwrap();
    }
}
