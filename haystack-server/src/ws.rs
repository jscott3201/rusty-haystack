//! Bounded WebSocket attachments to the application subscription owner.
//! Queues contain commands, never previously authorized payloads. The writer
//! executes each command with fresh admission and current policy before emission.
use crate::state::SharedState;
use axum::{
    Extension,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::{FutureExt, SinkExt, StreamExt};
use haystack_app::{
    CancellationToken, LegacySubscriptionRequest, ReadAdmission, ReadContext, ReadError,
    StateSubscriptionService, SubscriptionSession, WorkGuard,
};
use haystack_core::{
    codecs::{json::v3 as json_v3, subscription as wire},
    data::HDict,
    kinds::Kind,
};
use serde_json::{Map, Value};
use std::time::{Duration, Instant};
const MAX_ENTITY_IDS_PER_WATCH: usize = 1000;
const MAX_MESSAGE_SIZE: usize = 1024 * 1024;
const CHANNEL_CAPACITY: usize = 8;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const PING_INTERVAL: Duration = Duration::from_secs(30);
const PONG_TIMEOUT: Duration = Duration::from_secs(10);
/// Incoming JSON message from a WebSocket client.
#[derive(serde::Deserialize, Debug)]
#[serde(deny_unknown_fields)]
struct WsRequest {
    op: String,
    #[serde(rename = "reqId")]
    req_id: String,
    #[serde(rename = "watchId")]
    watch_id: Option<String>,
    ids: Option<Vec<String>>,
}

/// Outgoing JSON message sent to a WebSocket client.
#[derive(serde::Serialize, Debug)]
struct WsResponse {
    #[serde(rename = "reqId", skip_serializing_if = "Option::is_none")]
    req_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<Vec<Value>>,
    #[serde(rename = "watchId", skip_serializing_if = "Option::is_none")]
    watch_id: Option<String>,
}

impl WsResponse {
    /// Build an error response, preserving the request ID for correlation.
    fn error(req_id: Option<String>, msg: impl Into<String>) -> Self {
        Self {
            req_id,
            error: Some(msg.into()),
            rows: None,
            watch_id: None,
        }
    }

    /// Build a success response with rows and an optional watch ID.
    fn ok(req_id: Option<String>, rows: Vec<Value>, watch_id: Option<String>) -> Self {
        Self {
            req_id,
            error: None,
            rows: Some(rows),
            watch_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Entity encoding helper
// ---------------------------------------------------------------------------

/// Encode an `HDict` entity as a JSON object using the Haystack JSON v3
/// encoding for individual tag values.
fn encode_entity(entity: &HDict) -> Value {
    let mut m = Map::new();
    let mut keys: Vec<&String> = entity.tags().keys().collect();
    keys.sort();
    for k in keys {
        let v = &entity.tags()[k];
        if let Ok(encoded) = json_v3::encode_kind(v) {
            m.insert(k.clone(), encoded);
        }
    }
    Value::Object(m)
}

fn valid_arguments(req: &WsRequest) -> bool {
    let valid_ids = req.ids.as_ref().is_none_or(|ids| {
        ids.len() <= MAX_ENTITY_IDS_PER_WATCH
            && ids.iter().all(|id| {
                let id = id.strip_prefix('@').unwrap_or(id);
                !id.is_empty() && id.len() <= 1024
            })
    });
    let valid_watch = req
        .watch_id
        .as_ref()
        .is_some_and(|id| !id.is_empty() && id.len() <= 128);
    valid_ids
        && match req.op.as_str() {
            "watchSub" => {
                req.watch_id.is_none() && req.ids.as_ref().is_some_and(|ids| !ids.is_empty())
            }
            "watchPoll" => valid_watch && req.ids.is_none(),
            "watchUnsub" => valid_watch,
            _ => true, // Report an explicit unsupported-operation error with the request ID.
        }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedFrame {
    profile: String,
    #[serde(rename = "reqId")]
    req_id: String,
    payload: String,
}
impl ScopedFrame {
    fn valid(&self) -> bool {
        self.profile == wire::PROFILE
            && self
                .req_id
                .parse::<u64>()
                .is_ok_and(|id| id != 0 && id.to_string() == self.req_id)
            && self.payload.len() <= wire::MAX_PAYLOAD_BYTES
    }
}
enum Command {
    Scoped(ScopedFrame),
    Legacy(WsRequest),
    Push,
    PushWatch(String),
    Control(Message),
}
struct Attachment {
    service: StateSubscriptionService,
    connection: [u8; 16],
}
impl Drop for Attachment {
    fn drop(&mut self) {
        self.service.detach_legacy_connection(self.connection);
    }
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
    headers: HeaderMap,
    session: Option<Extension<SubscriptionSession>>,
    admission: Option<Extension<std::sync::Arc<WorkGuard>>>,
) -> Response {
    let Some(service) = state.subscription_service.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let scoped = headers
        .get("Sec-WebSocket-Protocol")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim() == wire::WS_PROTOCOL)
        });
    if !scoped && state.profile == crate::capabilities::ServiceProfile::ScopedReadService {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let session = match session {
        Some(Extension(session)) => session,
        None if !scoped => service.anonymous_legacy_session(),
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let Some(Extension(guard)) = admission else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let closing = state
        .application
        .as_ref()
        .map(|app| app.closing())
        .unwrap_or_default();
    let limit = if scoped {
        wire::MAX_WIRE_BYTES
    } else {
        MAX_MESSAGE_SIZE
    };
    ws.protocols([wire::WS_PROTOCOL])
        .max_message_size(limit)
        .max_frame_size(limit)
        .max_write_buffer_size(limit * 2)
        .on_upgrade(move |socket| {
            handle_socket(socket, service, session, guard.child(), closing, scoped)
        })
}
async fn begin(
    service: &StateSubscriptionService,
    session: &SubscriptionSession,
    guard: &WorkGuard,
) -> Result<ReadAdmission, ReadError> {
    service
        .read_service()
        .begin_admitted(
            ReadContext::new(
                session.principal().clone(),
                Instant::now() + service.read_service().limits().max_duration,
                CancellationToken::new(),
            ),
            guard.child(),
        )
        .await
}
async fn legacy_response(
    request: WsRequest,
    service: &StateSubscriptionService,
    session: &SubscriptionSession,
    connection: [u8; 16],
    guard: &WorkGuard,
) -> Result<WsResponse, ReadError> {
    let id = Some(request.req_id.clone());
    if !valid_arguments(&request) {
        return Ok(WsResponse::error(id, "unsupported watch arguments"));
    }
    let ids = request
        .ids
        .unwrap_or_default()
        .into_iter()
        .map(|id| id.strip_prefix('@').unwrap_or(&id).to_string())
        .collect();
    let operation = match request.op.as_str() {
        "watchSub" => LegacySubscriptionRequest::Subscribe { watch: None, ids },
        "watchPoll" => LegacySubscriptionRequest::Poll {
            watch: request.watch_id.expect("validated watch"),
        },
        "watchUnsub" => LegacySubscriptionRequest::Unsubscribe {
            watch: request.watch_id.expect("validated watch"),
            ids,
        },
        _ => return Ok(WsResponse::error(id, "unsupported WebSocket operation")),
    };
    let result = async {
        let admission = begin(service, session, guard).await?;
        service
            .legacy_admitted(admission, session.clone(), operation, Some(connection))
            .await
    }
    .await;
    match result {
        Ok(grid) => {
            let watch = match grid.meta.get("watchId") {
                Some(Kind::Str(watch)) => Some(watch.clone()),
                _ => None,
            };
            Ok(WsResponse::ok(
                id,
                grid.rows.iter().map(encode_entity).collect(),
                watch,
            ))
        }
        Err(error @ ReadError::Budget(_)) => Err(error),
        Err(_) => Ok(WsResponse::error(id, "watch unavailable or rejected")),
    }
}
async fn process(
    command: Command,
    service: &StateSubscriptionService,
    session: &SubscriptionSession,
    connection: [u8; 16],
    guard: &WorkGuard,
) -> Result<Option<Message>, ReadError> {
    match command {
        Command::Control(message) => Ok(Some(message)),
        Command::Legacy(request) => {
            let response = legacy_response(request, service, session, connection, guard).await?;
            Ok(Some(Message::Text(
                serde_json::to_string(&response)
                    .expect("JSON response")
                    .into(),
            )))
        }
        Command::Scoped(frame) => {
            let admission = begin(service, session, guard).await?;
            let body = service
                .payload_admitted(admission, session.clone(), frame.payload.into_bytes())
                .await?;
            let payload = String::from_utf8(body).map_err(|_| ReadError::Unavailable)?;
            let response = ScopedFrame {
                profile: wire::PROFILE.into(),
                req_id: frame.req_id,
                payload,
            };
            Ok(Some(Message::Text(
                serde_json::to_string(&response)
                    .expect("JSON response")
                    .into(),
            )))
        }
        Command::Push => Err(ReadError::InvalidQuery("unexpanded legacy push")),
        Command::PushWatch(watch) => {
            let admission = begin(service, session, guard).await?;
            let message = service
                .legacy_push_admitted(admission, session.clone(), connection, watch)
                .await?;
            Ok(message.map(|text| Message::Text(text.into())))
        }
    }
}
struct WriterContext {
    service: StateSubscriptionService,
    session: SubscriptionSession,
    connection: [u8; 16],
    guard: WorkGuard,
    closing: CancellationToken,
    scoped: bool,
}
async fn write_commands<S>(
    mut sender: S,
    mut rx: tokio::sync::mpsc::Receiver<Command>,
    mut stopped: tokio::sync::watch::Receiver<Option<u16>>,
    context: WriterContext,
) where
    S: futures_util::Sink<Message> + Unpin,
{
    let WriterContext {
        service,
        session,
        connection,
        guard,
        closing,
        scoped,
    } = context;
    let limit = if scoped {
        wire::MAX_WIRE_BYTES
    } else {
        MAX_MESSAGE_SIZE
    };
    'commands: loop {
        if !session.is_active() || closing.is_cancelled() || stopped.borrow().is_some() {
            break;
        }
        let command = tokio::select! {biased;_=session.closed()=>break,_=closing.cancelled()=>break,_=stopped.changed()=>break,command=rx.recv()=>match command{Some(command)=>command,None=>break}};
        // Wait for sink capacity before authorizing and serializing a command.
        // A stalled network must not retain an authorized disclosure for later.
        let ready = tokio::select! {biased;_=session.closed()=>break,_=closing.cancelled()=>break,_=stopped.changed()=>break,result=tokio::time::timeout(WRITE_TIMEOUT,futures_util::future::poll_fn(|cx|std::pin::Pin::new(&mut sender).poll_ready(cx)))=>result};
        if !matches!(ready, Ok(Ok(()))) {
            break;
        }
        let commands = if matches!(command, Command::Push) {
            let result = tokio::select! {biased;_=session.closed()=>break,_=closing.cancelled()=>break,_=stopped.changed()=>break,result=async {
                let admission = begin(&service, &session, &guard).await?;
                service.legacy_push_watches_admitted(admission, session.clone(), connection).await
            }=>result};
            let Ok(watches) = result else { break };
            watches
                .into_iter()
                .map(Command::PushWatch)
                .collect::<Vec<_>>()
        } else {
            vec![command]
        };
        for (index, command) in commands.into_iter().enumerate() {
            if index > 0 {
                let ready = tokio::select! {biased;_=session.closed()=>break 'commands,_=closing.cancelled()=>break 'commands,_=stopped.changed()=>break 'commands,result=tokio::time::timeout(WRITE_TIMEOUT,futures_util::future::poll_fn(|cx|std::pin::Pin::new(&mut sender).poll_ready(cx)))=>result};
                if !matches!(ready, Ok(Ok(()))) {
                    return;
                }
            }
            let result = tokio::select! {biased;_=session.closed()=>break 'commands,_=closing.cancelled()=>break 'commands,_=stopped.changed()=>break 'commands,result=process(command,&service,&session,connection,&guard)=>result};
            let Ok(message) = result else { break 'commands };
            let Some(message) = message else { continue };
            if matches!(&message,Message::Text(text) if text.len()>limit) {
                let _ = tokio::time::timeout(
                    WRITE_TIMEOUT,
                    sender.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                        code: 1009,
                        reason: "message too large".into(),
                    }))),
                )
                .await;
                return;
            }
            if !session.is_active() || closing.is_cancelled() || stopped.borrow().is_some() {
                break 'commands;
            }
            if std::pin::Pin::new(&mut sender).start_send(message).is_err() {
                return;
            }
            let sent = tokio::select! {biased;_=session.closed()=>break 'commands,_=closing.cancelled()=>break 'commands,_=stopped.changed()=>break 'commands,result=tokio::time::timeout(WRITE_TIMEOUT,sender.flush())=>result};
            if !matches!(sent, Ok(Ok(()))) {
                return;
            }
        }
    }

    // Closing the application can also drop the authentication manager and
    // revoke its sessions. Preserve the application shutdown close code even
    // when that revocation wins a race with the reader's terminal notification.
    let terminal = if closing.is_cancelled() {
        Some(1001)
    } else if !session.is_active() {
        Some(1008)
    } else {
        *stopped.borrow()
    };
    if let Some(code) = terminal {
        // Teardown does not wait for a blocked sink to become writable. A ready
        // peer receives the terminal frame; a stalled peer loses the transport.
        if !matches!(
            futures_util::future::poll_fn(|cx| std::pin::Pin::new(&mut sender).poll_ready(cx))
                .now_or_never(),
            Some(Ok(()))
        ) {
            return;
        }
        if scoped && code == 1013 && session.is_active() && !closing.is_cancelled() {
            let payload = String::from_utf8(
                wire::encode(&haystack_app::SubscriptionOutcome::Resync(
                    haystack_app::SubscriptionResync::Overflow,
                ))
                .expect("bounded outcome"),
            )
            .expect("UTF8");
            let frame = ScopedFrame {
                profile: wire::PROFILE.into(),
                req_id: "0".into(),
                payload,
            };
            let _ = tokio::time::timeout(
                WRITE_TIMEOUT,
                sender.send(Message::Text(
                    serde_json::to_string(&frame).expect("JSON").into(),
                )),
            )
            .await;
        }
        let _ = tokio::time::timeout(
            WRITE_TIMEOUT,
            sender.send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code,
                reason: "connection ended".into(),
            }))),
        )
        .await;
    }
}
async fn handle_socket(
    socket: WebSocket,
    service: StateSubscriptionService,
    session: SubscriptionSession,
    guard: WorkGuard,
    closing: CancellationToken,
    scoped: bool,
) {
    let connection = rand::random();
    let _attachment = Attachment {
        service: service.clone(),
        connection,
    };
    let (tx, rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
    let (stop, stopped) = tokio::sync::watch::channel(None);
    let (sender, mut receiver) = socket.split();
    let mut writer = tokio::task::JoinSet::new();
    writer.spawn(write_commands(
        sender,
        rx,
        stopped,
        WriterContext {
            service: service.clone(),
            session: session.clone(),
            connection,
            guard: guard.child(),
            closing: closing.clone(),
            scoped,
        },
    ));
    let mut pings = tokio::time::interval(PING_INTERVAL);
    pings.tick().await;
    let mut pushes = tokio::time::interval(Duration::from_millis(500));
    pushes.tick().await;
    let mut last_push = service.read_service().graph().version();
    let mut pong_deadline = None;
    let code = loop {
        tokio::select! {biased;
            _=closing.cancelled()=>break 1001,
            _=session.closed()=>break 1008,
            _=writer.join_next()=>break 1011,
            _=async{if let Some(deadline)=pong_deadline{tokio::time::sleep_until(deadline).await}else{std::future::pending::<()>().await}}=>break 1001,
            message=receiver.next()=>{
                let Some(Ok(message))=message else{break 1002};
                let command=match message{
                    Message::Text(text) if scoped=>{let Ok(frame)=serde_json::from_str::<ScopedFrame>(&text) else{break 1002};if !frame.valid(){break 1002}Command::Scoped(frame)},
                    Message::Text(text)=>{let Ok(request)=serde_json::from_str::<WsRequest>(&text) else{break 1002};if request.req_id.is_empty()||request.req_id.len()>128{break 1002}Command::Legacy(request)},
                    Message::Ping(payload)=>Command::Control(Message::Pong(payload)),
                    Message::Pong(_)=>{pong_deadline=None;continue},
                    Message::Close(_)=>break 1000,
                    Message::Binary(_)=>break 1003,
                };
                if tx.try_send(command).is_err(){break 1013}
            },
            _=pings.tick()=>{if tx.try_send(Command::Control(Message::Ping(Vec::new().into()))).is_err(){break 1013}pong_deadline=Some(tokio::time::Instant::now()+PONG_TIMEOUT);},
            _=pushes.tick(),if !scoped=>{let current=service.read_service().graph().version();if current>last_push{if tx.try_send(Command::Push).is_err(){break 1013}last_push=current;}},
        }
    };
    let _ = stop.send(Some(code));
    drop(tx);
    if tokio::time::timeout(WRITE_TIMEOUT, writer.join_next())
        .await
        .is_err()
    {
        writer.abort_all();
        while writer.join_next().await.is_some() {}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ws_request_deserialization() {
        let json = r#"{
            "op": "watchSub",
            "reqId": "abc-123",
            "ids": ["@ref1", "@ref2"]
        }"#;
        let req: WsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.op, "watchSub");
        assert_eq!(req.req_id, "abc-123");
        assert!(req.watch_id.is_none());
        let ids = req.ids.unwrap();
        assert_eq!(ids, vec!["@ref1", "@ref2"]);
    }

    #[test]
    fn ws_request_deserialization_minimal() {
        let json = r#"{"op": "watchPoll", "reqId": "r-1", "watchId": "w-1"}"#;
        let req: WsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.op, "watchPoll");
        assert_eq!(req.req_id, "r-1");
        assert_eq!(req.watch_id.as_deref(), Some("w-1"));
        assert!(req.ids.is_none());
    }

    #[test]
    fn ws_response_serialization() {
        let resp = WsResponse::ok(
            Some("r-1".into()),
            vec![serde_json::json!({"id": "r:site-1"})],
            Some("w-1".into()),
        );
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["reqId"], "r-1");
        assert_eq!(json["watchId"], "w-1");
        assert!(json["rows"].is_array());
        assert_eq!(json["rows"][0]["id"], "r:site-1");
        assert!(json.get("error").is_none());
    }

    #[test]
    fn ws_response_omits_none_fields() {
        let resp = WsResponse::ok(None, vec![], None);
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("reqId").is_none());
        assert!(json.get("error").is_none());
        assert!(json.get("watchId").is_none());
        assert!(json["rows"].is_array());
    }

    #[test]
    fn ws_response_includes_req_id() {
        let resp = WsResponse::error(Some("req-42".into()), "something went wrong");
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["reqId"], "req-42");
        assert_eq!(json["error"], "something went wrong");
        assert!(json.get("rows").is_none());
        assert!(json.get("watchId").is_none());
    }

    #[test]
    fn ws_error_response_format() {
        let resp = WsResponse::error(None, "bad request");
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["error"], "bad request");
        assert!(json.get("reqId").is_none());
        assert!(json.get("rows").is_none());
        assert!(json.get("watchId").is_none());
    }
    use haystack_app::{
        AllowAll, ApplicationBuilder, ApplicationOwner, EphemeralMutationStore, PolicySnapshot,
        Principal, ReadLimits, ReadOperation, ReadPolicy, SubscriptionCreate, SubscriptionLimits,
        SubscriptionOutcome, SubscriptionRequest,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct SinkState {
        ready: AtomicBool,
        block_after: AtomicUsize,
        entered: tokio::sync::Notify,
        waker: futures_util::task::AtomicWaker,
        sent: Mutex<Vec<Message>>,
        emitted: tokio::sync::Notify,
    }
    impl SinkState {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                ready: AtomicBool::new(false),
                block_after: AtomicUsize::new(usize::MAX),
                entered: tokio::sync::Notify::new(),
                waker: futures_util::task::AtomicWaker::new(),
                sent: Mutex::new(Vec::new()),
                emitted: tokio::sync::Notify::new(),
            })
        }
        fn ready(&self) {
            self.ready.store(true, Ordering::SeqCst);
            self.waker.wake();
        }
    }
    struct ControlledSink(Arc<SinkState>);
    impl futures_util::Sink<Message> for ControlledSink {
        type Error = std::convert::Infallible;
        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.waker.register(cx.waker());
            if self.0.ready.load(Ordering::SeqCst) {
                std::task::Poll::Ready(Ok(()))
            } else {
                self.0.entered.notify_one();
                std::task::Poll::Pending
            }
        }
        fn start_send(self: std::pin::Pin<&mut Self>, message: Message) -> Result<(), Self::Error> {
            let count = {
                let mut sent = self.0.sent.lock().unwrap();
                sent.push(message);
                sent.len()
            };
            if count == self.0.block_after.load(Ordering::SeqCst) {
                self.0.ready.store(false, Ordering::SeqCst);
            }
            self.0.emitted.notify_one();
            Ok(())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    struct TogglePolicy(Arc<AtomicBool>);
    struct Snapshot(bool);
    impl ReadPolicy for TogglePolicy {
        fn snapshot(&self, _: &Principal) -> Result<Arc<dyn PolicySnapshot>, ReadError> {
            Ok(Arc::new(Snapshot(self.0.load(Ordering::SeqCst))))
        }
    }
    impl PolicySnapshot for Snapshot {
        fn function(&self, _: &haystack_app::FunctionIdentity) -> bool {
            true
        }
        fn scope_key(&self) -> &str {
            if self.0 { "visible" } else { "revoked" }
        }
        fn operation(&self, _: ReadOperation) -> bool {
            true
        }
        fn entity(&self, _: &str) -> bool {
            self.0
        }
        fn tag(&self, _: &str, _: &str) -> bool {
            self.0
        }
        fn reference(&self, _: &str) -> bool {
            self.0
        }
        fn reference_display(&self, _: &str) -> bool {
            self.0
        }
        fn catalog(&self, _: haystack_app::CatalogKind, _: &str) -> bool {
            self.0
        }
        fn nominal_provenance(&self, _: &haystack_core::kinds::NominalScalar) -> bool {
            self.0
        }
    }
    async fn owner(policy: Arc<dyn ReadPolicy>) -> (ApplicationOwner, StateSubscriptionService) {
        let graph =
            haystack_core::graph::SharedGraph::new(haystack_core::graph::EntityGraph::new());
        let mut row = HDict::new();
        row.set(
            "id",
            Kind::Ref(haystack_core::kinds::HRef::from_val("private-point")),
        );
        row.set("secret", Kind::Str("retained-value".into()));
        graph.add(row).unwrap();
        let builder =
            ApplicationBuilder::new(graph.clone(), policy, ReadLimits::default()).unwrap();
        let service = StateSubscriptionService::new(
            builder.handle().read_service(),
            EphemeralMutationStore::new(graph),
            SubscriptionLimits::default(),
        )
        .unwrap();
        let owner = builder
            .state_subscriptions(service.clone())
            .unwrap()
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        owner.ready().await.unwrap();
        (owner, service)
    }
    async fn initial(
        service: &StateSubscriptionService,
        session: &SubscriptionSession,
    ) -> Arc<haystack_app::SubscriptionDelivery> {
        let request = SubscriptionRequest::Create(SubscriptionCreate {
            authority: service.authority(),
            key: "cached".into(),
            ids: vec!["private-point".into()],
            lease_ms: 5000,
        });
        let outcome = service
            .execute(
                ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(2)),
                session.clone(),
                request,
            )
            .await
            .unwrap();
        let SubscriptionOutcome::Delivery(delivery) = outcome else {
            panic!("initial")
        };
        delivery
    }
    fn queued(delivery: &haystack_app::SubscriptionDelivery, id: &str) -> Command {
        Command::Scoped(ScopedFrame {
            profile: wire::PROFILE.into(),
            req_id: id.into(),
            payload: String::from_utf8(
                wire::encode(&SubscriptionRequest::Poll {
                    watch: delivery.watch.clone(),
                })
                .unwrap(),
            )
            .unwrap(),
        })
    }
    #[tokio::test]
    async fn queued_cached_disclosure_reauthorizes_after_waiting_for_sink_capacity() {
        let visible = Arc::new(AtomicBool::new(true));
        let (owner, service) = owner(Arc::new(TogglePolicy(visible.clone()))).await;
        let session = SubscriptionSession::trusted("owner", Duration::from_secs(5)).unwrap();
        let delivery = initial(&service, &session).await;
        let sink = SinkState::new();
        let (tx, rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        let (stop, stopped) = tokio::sync::watch::channel(None);
        let writer = tokio::spawn(write_commands(
            ControlledSink(sink.clone()),
            rx,
            stopped,
            WriterContext {
                service: service.clone(),
                session,
                connection: [1; 16],
                guard: owner.handle().admit().unwrap(),
                closing: owner.handle().closing(),
                scoped: true,
            },
        ));
        tx.send(queued(&delivery, "1")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), sink.entered.notified())
            .await
            .unwrap();
        assert_eq!(service.read_service().load().admitted, 0);
        visible.store(false, Ordering::SeqCst);
        sink.ready();
        tokio::time::timeout(Duration::from_secs(1), sink.emitted.notified())
            .await
            .unwrap();
        let Message::Text(text) = sink.sent.lock().unwrap()[0].clone() else {
            panic!()
        };
        assert!(!text.contains("private-point") && !text.contains("retained-value"));
        let frame: ScopedFrame = serde_json::from_str(&text).unwrap();
        assert_eq!(
            wire::decode::<SubscriptionOutcome>(frame.payload.as_bytes()).unwrap(),
            SubscriptionOutcome::Resync(haystack_app::SubscriptionResync::Policy)
        );
        stop.send(Some(1000)).unwrap();
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .unwrap()
            .unwrap();
        owner.close().await.unwrap();
        owner.terminated().await;
    }
    #[tokio::test]
    async fn blocked_queued_writer_observes_revocation_expiry_and_owner_close_independently() {
        for mode in 0..3 {
            let (owner, service) = owner(Arc::new(AllowAll)).await;
            let session = SubscriptionSession::trusted(
                "owner",
                if mode == 1 {
                    Duration::from_millis(100)
                } else {
                    Duration::from_secs(5)
                },
            )
            .unwrap();
            let delivery = initial(&service, &session).await;
            let sink = SinkState::new();
            let (tx, rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
            let (_stop, stopped) = tokio::sync::watch::channel(None);
            let writer = tokio::spawn(write_commands(
                ControlledSink(sink.clone()),
                rx,
                stopped,
                WriterContext {
                    service: service.clone(),
                    session: session.clone(),
                    connection: [2; 16],
                    guard: owner.handle().admit().unwrap(),
                    closing: owner.handle().closing(),
                    scoped: true,
                },
            ));
            tx.send(queued(&delivery, "1")).await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), sink.entered.notified())
                .await
                .unwrap();
            if mode == 0 {
                session.close()
            } else if mode == 2 {
                tokio::time::timeout(Duration::from_secs(1), owner.close())
                    .await
                    .unwrap()
                    .unwrap();
            }
            tokio::time::timeout(Duration::from_secs(1), writer)
                .await
                .unwrap()
                .unwrap();
            assert!(sink.sent.lock().unwrap().is_empty());
            assert_eq!(service.read_service().load().admitted, 0);
            owner.close().await.unwrap();
            owner.terminated().await;
            assert_eq!(service.binding_count(), 0);
        }
    }
    #[tokio::test]
    async fn bounded_queue_overflow_is_explicit_terminal_resync_without_implicit_ack() {
        let (owner, service) = owner(Arc::new(AllowAll)).await;
        let session = SubscriptionSession::trusted("owner", Duration::from_secs(5)).unwrap();
        let delivery = initial(&service, &session).await;
        let sink = SinkState::new();
        let (tx, rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        let (stop, stopped) = tokio::sync::watch::channel(None);
        let writer = tokio::spawn(write_commands(
            ControlledSink(sink.clone()),
            rx,
            stopped,
            WriterContext {
                service: service.clone(),
                session: session.clone(),
                connection: [3; 16],
                guard: owner.handle().admit().unwrap(),
                closing: owner.handle().closing(),
                scoped: true,
            },
        ));
        tx.send(queued(&delivery, "1")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), sink.entered.notified())
            .await
            .unwrap();
        for index in 0..CHANNEL_CAPACITY {
            tx.try_send(queued(&delivery, &(index + 2).to_string()))
                .unwrap();
        }
        assert!(matches!(
            tx.try_send(queued(&delivery, "100")),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));
        stop.send(Some(1013)).unwrap();
        sink.ready();
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .unwrap()
            .unwrap();
        {
            let sent = sink.sent.lock().unwrap();
            let Message::Text(text) = &sent[0] else {
                panic!()
            };
            let frame: ScopedFrame = serde_json::from_str(text).unwrap();
            assert_eq!(frame.req_id, "0");
            assert_eq!(
                wire::decode::<SubscriptionOutcome>(frame.payload.as_bytes()).unwrap(),
                SubscriptionOutcome::Resync(haystack_app::SubscriptionResync::Overflow)
            );
            assert!(matches!(sent.last(),Some(Message::Close(Some(frame))) if frame.code==1013));
        }
        let replay = service
            .execute(
                ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(2)),
                session,
                SubscriptionRequest::Poll {
                    watch: delivery.watch.clone(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(replay,SubscriptionOutcome::Delivery(current) if current.token==delivery.token)
        );
        owner.close().await.unwrap();
        owner.terminated().await;
    }
    #[tokio::test]
    async fn review_second_legacy_push_reauthorizes_after_its_own_sink_wait() {
        let visible = Arc::new(AtomicBool::new(true));
        let (owner, service) = owner(Arc::new(TogglePolicy(visible.clone()))).await;
        let mut second = HDict::new();
        second.set(
            "id",
            Kind::Ref(haystack_core::kinds::HRef::from_val("second-private-point")),
        );
        second.set("secret", Kind::Str("second-retained-value".into()));
        service.read_service().graph().add(second).unwrap();
        let session = SubscriptionSession::trusted("owner", Duration::from_secs(5)).unwrap();
        let connection = [12; 16];
        let guard = owner.handle().admit().unwrap();
        for id in ["private-point", "second-private-point"] {
            let admission = begin(&service, &session, &guard).await.unwrap();
            service
                .legacy_admitted(
                    admission,
                    session.clone(),
                    LegacySubscriptionRequest::Subscribe {
                        watch: None,
                        ids: vec![id.into()],
                    },
                    Some(connection),
                )
                .await
                .unwrap();
        }
        drop(guard);
        let sink = SinkState::new();
        sink.block_after.store(1, Ordering::SeqCst);
        sink.ready();
        let (tx, rx) = tokio::sync::mpsc::channel(CHANNEL_CAPACITY);
        let (_stop, stopped) = tokio::sync::watch::channel(None);
        let writer = tokio::spawn(write_commands(
            ControlledSink(sink.clone()),
            rx,
            stopped,
            WriterContext {
                service: service.clone(),
                session: session.clone(),
                connection,
                guard: owner.handle().admit().unwrap(),
                closing: owner.handle().closing(),
                scoped: false,
            },
        ));
        tx.send(Command::Push).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), sink.entered.notified())
            .await
            .unwrap();
        assert_eq!(sink.sent.lock().unwrap().len(), 1);
        visible.store(false, Ordering::SeqCst);
        assert!(session.is_active());
        sink.ready();
        drop(tx);
        tokio::time::timeout(Duration::from_secs(1), writer)
            .await
            .unwrap()
            .unwrap();
        let sent = sink.sent.lock().unwrap().clone();
        assert!(sent.iter().skip(1).all(|message| !matches!(message, Message::Text(text) if text.contains("private-point") || text.contains("retained-value"))), "a queued legacy push disclosed revoked content: {sent:?}");
        owner.close().await.unwrap();
        owner.terminated().await;
    }
}
