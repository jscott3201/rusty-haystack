use crate::{
    AuthError, JwtVerifier, Limits, McpError, Report,
    common::{encode, strict_json},
    handler::{Invocation, Lease, RequestDeadline, Tools},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use haystack_app::{ApplicationHandle, CancellationToken, Principal, WorkGuard};
use http::{HeaderMap, HeaderValue, Request, Response};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    body::{Bytes, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    future::Future,
    io,
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};
use tokio::{
    net::TcpListener,
    sync::{Notify, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use tower::Service;

type Body = Full<Bytes>;
type Sdk = StreamableHttpService<Tools, NeverSessionManager>;
// Limit individual writes so a large JSON frame cannot bypass kernel
// backpressure by being submitted as one oversized write/writev operation.
struct SocketIo {
    socket: tokio::net::TcpStream,
    slot: Arc<Mutex<Option<Arc<Admitted>>>>,
}
impl tokio::io::AsyncRead for SocketIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let remaining = buf.remaining();
        let filled = buf.filled().len();
        let result = Pin::new(&mut self.socket).poll_read(cx, buf);
        let eof = remaining != 0
            && matches!(&result, Poll::Ready(Ok(())))
            && buf.filled().len() == filled;
        if (eof || matches!(&result, Poll::Ready(Err(_))))
            && let Some(request) = self.slot.lock().as_ref()
        {
            // Hyper may close its write half before ConnectionLease is dropped.
            // Cancel this request before exposing a terminal read to Hyper, but
            // retain the slot/permit until connection and worker cleanup join.
            request.cancellation.cancel();
        }
        result
    }
}
impl tokio::io::AsyncWrite for SocketIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.socket).poll_write(cx, &bytes[..bytes.len().min(16_384)])
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}
const VERSION: &str = "2026-07-28";
const METADATA: &str = "/.well-known/oauth-protected-resource/mcp";

/// Local HTTP/1 profile: numeric loopback authority, one request per connection,
/// JSON responses, no sessions, proxy trust, event replay, or public TLS profile.
#[derive(Clone)]
pub struct HttpConfig {
    pub limits: Limits,
    pub output_profile: crate::OutputProfile,
    pub semantic_resources: bool,
    /// Must equal `http://<bound numeric address>/mcp`, also the token audience.
    pub resource: String,
    /// Exact origins; empty rejects all present Origin headers. Absence is allowed.
    pub allowed_origins: Vec<String>,
    pub max_connections: usize,
    pub max_header_bytes: usize,
}
impl HttpConfig {
    pub fn local(address: std::net::SocketAddr) -> Self {
        Self {
            limits: Limits::default(),
            output_profile: crate::OutputProfile::H4,
            semantic_resources: false,
            resource: format!("http://{address}/mcp"),
            allowed_origins: Vec::new(),
            max_connections: 32,
            max_header_bytes: 16_384,
        }
    }
}
struct Shared {
    application: ApplicationHandle,
    config: HttpConfig,
    verifier: Arc<JwtVerifier>,
    authority: String,
    metadata_url: String,
    permits: Arc<Semaphore>,
    report: Mutex<Report>,
    released: Notify,
    sdk_instances: AtomicUsize,
    stop: CancellationToken,
}
struct Admitted {
    principal: Principal,
    deadline: Arc<RequestDeadline>,
    cancellation: CancellationToken,
    _permit: OwnedSemaphorePermit,
    work: WorkGuard,
    _release: Release,
}
struct SdkRelease(Weak<Shared>);
impl Drop for SdkRelease {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            shared.sdk_instances.fetch_sub(1, Ordering::SeqCst);
            shared.released.notify_one();
        }
    }
}
struct Release(Weak<Shared>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            shared.report.lock().released += 1;
            shared.released.notify_one();
        }
    }
}
impl Invocation for Admitted {
    fn principal(&self) -> &Principal {
        &self.principal
    }
    fn deadline(&self) -> Instant {
        self.deadline.get()
    }
    fn tighten_deadline(&self, deadline: Instant) {
        self.deadline.tighten(deadline);
    }
    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    fn work(&self) -> WorkGuard {
        self.work.child()
    }
}
/// A connection retains native admission until Hyper has completed delivery or
/// the connection is suppressed. Handler extensions independently retain it
/// until read_joined has released the actual worker.
struct ConnectionLease(Arc<Mutex<Option<Arc<Admitted>>>>);
impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if let Some(request) = self.0.lock().take() {
            request.cancellation.cancel();
        }
    }
}

/// Serve a prebound loopback listener, borrowing the application/runtime. Cancel
/// `stop` and await this future to join all connections and admitted workers.
/// Application close also stops this adapter. Dropping this future is not a
/// shutdown receipt; the embedding owner must retain and join it.
pub async fn serve_http(
    application: ApplicationHandle,
    listener: TcpListener,
    config: HttpConfig,
    verifier: Arc<JwtVerifier>,
    stop: CancellationToken,
) -> Result<Report, McpError> {
    config.limits.validate(config.output_profile)?;
    if config.limits.max_response_bytes
        < config.output_profile.resource_min_response_bytes(
            application.history_service().is_some(),
            config.semantic_resources,
        )
    {
        return Err(McpError::Configuration);
    }
    let address = listener.local_addr().map_err(|_| McpError::Io)?;
    if !address.ip().is_loopback()
        || config.resource != format!("http://{address}/mcp")
        || verifier.resource() != config.resource
        || !(1..=128).contains(&config.max_connections)
        || !(8192..=65_536).contains(&config.max_header_bytes)
        || config.allowed_origins.len() > 16
        || config
            .allowed_origins
            .iter()
            .any(|origin| !valid_origin(origin))
    {
        return Err(McpError::Configuration);
    }
    let endpoint_work = application.admit().map_err(|_| McpError::Configuration)?;
    let closing = application.closing();
    let sdk_stop = CancellationToken::new();
    let sdk_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_stateless_protocol_metadata_required(true)
        .with_allowed_hosts([address.to_string()])
        .with_allowed_origins(config.allowed_origins.clone())
        .enforce_origin_validation()
        .with_sse_keep_alive(None)
        .with_sse_retry(None)
        .with_max_request_body_bytes(config.limits.max_frame_bytes)
        .with_cancellation_token(sdk_stop.clone());
    let shared = Arc::new(Shared {
        application: application.clone(),
        authority: address.to_string(),
        metadata_url: format!("http://{address}{METADATA}"),
        permits: Arc::new(Semaphore::new(config.limits.max_in_flight)),
        report: Mutex::new(Report::default()),
        released: Notify::new(),
        sdk_instances: AtomicUsize::new(0),
        stop: stop.child_token(),
        config,
        verifier,
    });
    let factory_shared = shared.clone();
    let sdk = Sdk::new(
        move || {
            factory_shared.sdk_instances.fetch_add(1, Ordering::SeqCst);
            Ok(Tools::new(
                factory_shared.application.clone(),
                factory_shared.config.limits.clone(),
                factory_shared.config.output_profile,
                factory_shared.config.semantic_resources,
                true,
            )
            .with_lifetime(Arc::new(SdkRelease(Arc::downgrade(&factory_shared)))))
        },
        Arc::new(NeverSessionManager::default()),
        sdk_config,
    );
    let connections = Arc::new(Semaphore::new(shared.config.max_connections));
    let mut tasks = JoinSet::new();
    let mut error = None;
    loop {
        tokio::select! {
            biased;
            _=stop.cancelled()=>break,
            _=closing.cancelled()=>break,
            joined=tasks.join_next(), if !tasks.is_empty()=>{if joined.is_some_and(|r|r.is_err()){error=Some(McpError::Task);break;}},
            accepted=listener.accept()=>{
                let (socket,_)=match accepted {Ok(value)=>value,Err(_)=>{error=Some(McpError::Io);break;}};
                let deadline=Instant::now()+shared.config.limits.request_timeout;
                let Ok(permit)=connections.clone().try_acquire_owned() else {drop(socket);continue;};
                let shared=shared.clone(); let sdk=sdk.clone();
                let slot=Arc::new(Mutex::new(None));
                let guard=ConnectionLease(slot.clone());
                let request_shared=shared.clone(); let request_slot=slot.clone();
                let connection=accepted_connection(
                    socket, slot.clone(), deadline,
                    shared.config.max_header_bytes, shared.stop.clone(),
                    move |deadline| service_fn(move |request| handle(request,request_shared.clone(),sdk.clone(),request_slot.clone(),deadline.clone())),
                );
                tasks.spawn(async move {
                    let _permit=permit;
                    let delivered=connection.await;
                    if slot.lock().is_some() { let mut report=shared.report.lock();if delivered {report.delivered+=1;} else {report.suppressed+=1;} }
                    drop(guard);
                });
            }
        }
    }
    drop(listener);
    shared.stop.cancel();
    while let Some(result) = tasks.join_next().await {
        if result.is_err() {
            error = Some(McpError::Task);
        }
    }
    sdk_stop.cancel();
    // SDK task drainage is not native cleanup evidence. A lease remains alive
    // in every executing handler, so equality requires its joined worker exit.
    loop {
        let notified = shared.released.notified();
        let finished = {
            let report = shared.report.lock();
            report.admitted == report.released && shared.sdk_instances.load(Ordering::SeqCst) == 0
        };
        if finished {
            break;
        }
        notified.await;
    }
    drop(sdk);
    drop(endpoint_work);
    if let Some(error) = error {
        Err(error)
    } else {
        Ok(*shared.report.lock())
    }
}

// This ordinary constructor is called from the successful accept branch, before
// spawn. Its future may receive its first executor poll much later.
fn accepted_connection<F, S>(
    socket: tokio::net::TcpStream,
    slot: Arc<Mutex<Option<Arc<Admitted>>>>,
    deadline: Instant,
    max_header_bytes: usize,
    stop: CancellationToken,
    make_service: F,
) -> impl Future<Output = bool>
where
    F: FnOnce(Arc<RequestDeadline>) -> S,
    S: hyper::service::Service<Request<Incoming>, Response = Response<Body>, Error = Infallible>,
{
    // The deadline was stamped synchronously at accept, before any setup or
    // spawn. Do not restart that budget on the future's first executor poll.
    let deadline = Arc::new(RequestDeadline::new(deadline));
    let configured = {
        let socket_ref = socket2::SockRef::from(&socket);
        // The OS may round/double these requested kernel buffer bounds.
        socket_ref.set_send_buffer_size(16_384).is_ok()
            && socket_ref.set_recv_buffer_size(16_384).is_ok()
            && socket.set_nodelay(true).is_ok()
    };
    async move {
        if !configured {
            return false;
        }
        let service = make_service(deadline.clone());
        let mut builder = http1::Builder::new();
        builder
            .keep_alive(false)
            .half_close(false)
            .max_headers(64)
            .max_buf_size(max_header_bytes);
        let connection = builder.serve_connection(TokioIo::new(SocketIo { socket, slot }), service);
        tokio::select! {
            biased;
            _ = stop.cancelled() => false,
            _ = deadline.elapsed() => false,
            result = connection => result.is_ok(),
        }
    }
}

async fn handle(
    request: Request<Incoming>,
    shared: Arc<Shared>,
    mut sdk: Sdk,
    slot: Arc<Mutex<Option<Arc<Admitted>>>>,
    deadline: Arc<RequestDeadline>,
) -> Result<Response<Body>, Infallible> {
    let response = handle_inner(request, &shared, &mut sdk, &slot, deadline.clone()).await;
    // CPU-bounded decoding/encoding may finish after the timer became ready.
    // Yield to the connection's deadline/stop branch without publishing bytes.
    if Instant::now() >= deadline.get() || shared.stop.is_cancelled() {
        std::future::pending::<()>().await;
    }
    Ok(response)
}
async fn handle_inner(
    request: Request<Incoming>,
    shared: &Arc<Shared>,
    sdk: &mut Sdk,
    slot: &Arc<Mutex<Option<Arc<Admitted>>>>,
    deadline: Arc<RequestDeadline>,
) -> Response<Body> {
    let headers = request.headers();
    if headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len() + 4)
        .sum::<usize>()
        > shared.config.max_header_bytes
    {
        return empty(431);
    }
    for name in [
        "host",
        "origin",
        "authorization",
        "mcp-protocol-version",
        "mcp-method",
        "mcp-name",
        "content-type",
        "content-length",
    ] {
        if headers.get_all(name).iter().count() > 1 {
            return if name == "authorization" {
                auth_error(shared, 400, Some("invalid_request"))
            } else {
                rpc(400, -32020, "Duplicate singleton header", None, None)
            };
        }
    }
    if header(headers, "host") != Some(&shared.authority)
        || request.uri().authority().is_some()
        || headers
            .keys()
            .any(|name| name == "forwarded" || name.as_str().starts_with("x-forwarded-"))
    {
        return empty(403);
    }
    if headers.contains_key("origin")
        && !header(headers, "origin").is_some_and(|origin| {
            shared
                .config
                .allowed_origins
                .iter()
                .any(|allowed| allowed == origin)
        })
    {
        return empty(403);
    }
    if request.uri().query().is_some() {
        return auth_error(shared, 400, Some("invalid_request"));
    }
    if request.uri().path() == METADATA {
        if request.method() != http::Method::GET {
            return allowed("GET");
        }
        return json_response(
            200,
            &json!({"resource":shared.config.resource,"authorization_servers":[shared.verifier.issuer()],"scopes_supported":[shared.verifier.required_scope()],"bearer_methods_supported":["header"]}),
        );
    }
    if request.uri().path() != "/mcp" {
        return empty(404);
    }
    if request.method() != http::Method::POST {
        return allowed("POST");
    }
    let Some(authorization) = header(headers, "authorization") else {
        return auth_error(shared, 401, None);
    };
    let Some((scheme, token)) = authorization.split_once(' ') else {
        return auth_error(shared, 400, Some("invalid_request"));
    };
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.is_empty()
        || token.bytes().any(|b| b.is_ascii_whitespace() || b == b',')
    {
        return auth_error(shared, 400, Some("invalid_request"));
    }
    let principal = match shared.verifier.verify(token) {
        Ok(p) => p,
        Err(AuthError::InvalidToken) => return auth_error(shared, 401, Some("invalid_token")),
        Err(AuthError::InsufficientScope) => {
            return auth_error(shared, 403, Some("insufficient_scope"));
        }
    };
    if !header(headers, "content-type").is_some_and(|v| {
        v.split(';')
            .next()
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
    }) {
        return empty(415);
    }
    if !accepts(headers, "application/json") || !accepts(headers, "text/event-stream") {
        return empty(406);
    }
    if header(headers, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|n| n > shared.config.limits.max_frame_bytes)
    {
        return empty(413);
    }
    let (mut parts, body) = request.into_parts();
    let bytes = match Limited::new(body, shared.config.limits.max_frame_bytes)
        .collect()
        .await
    {
        Ok(body) => body.to_bytes(),
        Err(_) => return empty(413),
    };
    let value = match strict_json(&bytes, shared.config.limits.max_json_depth) {
        Ok(value) => value,
        Err(_) => return rpc(400, -32700, "Invalid or over-depth JSON", None, None),
    };
    let id = value
        .get("id")
        .filter(|id| {
            serde_json::from_value::<rmcp::model::RequestId>((*id).clone()).is_ok()
                && !id.as_str().is_some_and(|s| s.len() > 128)
        })
        .cloned();
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return rpc(400, -32600, "Invalid request", id, None);
    };
    if value.get("jsonrpc") != Some(&json!("2.0"))
        || id.is_none()
        || value.get("result").is_some()
        || value.get("error").is_some()
        || value.as_object().is_none_or(|object| {
            object
                .keys()
                .any(|key| !matches!(key.as_str(), "jsonrpc" | "id" | "method" | "params"))
        })
    {
        return rpc(400, -32600, "Invalid request", id, None);
    }
    let params = value.get("params");
    let meta = params.and_then(|p| p.get("_meta"));
    let Some(version) = meta
        .and_then(|m| m.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(Value::as_str)
    else {
        return rpc(400, -32602, "Missing protocol metadata", id, None);
    };
    if version.len() > 32 {
        return rpc(400, -32602, "Protocol version exceeds its limit", id, None);
    }
    if !meta
        .and_then(|m| m.get("io.modelcontextprotocol/clientCapabilities"))
        .is_some_and(Value::is_object)
    {
        return rpc(400, -32602, "Missing client capabilities", id, None);
    }
    if header(&parts.headers, "mcp-protocol-version") != Some(version)
        || header(&parts.headers, "mcp-method") != Some(method)
    {
        return rpc(400, -32020, "Header mismatch", id, None);
    }
    if version != VERSION {
        return rpc(
            400,
            -32022,
            "Unsupported protocol version",
            id,
            Some(json!({"requested":version,"supported":[VERSION]})),
        );
    }
    if !matches!(
        method,
        "server/discover"
            | "ping"
            | "tools/list"
            | "tools/call"
            | "resources/list"
            | "resources/read"
            | "resources/templates/list"
    ) {
        return rpc(404, -32601, "Method is not available", id, None);
    }
    if method.starts_with("resources/") && !shared.config.semantic_resources {
        return rpc(404, -32601, "Method is not available", id, None);
    }
    if method == "tools/call" || method == "resources/read" {
        let field = if method == "tools/call" {
            "name"
        } else {
            "uri"
        };
        let name = params.and_then(|p| p.get(field)).and_then(Value::as_str);
        if name.is_none() {
            return rpc(400, -32602, "Invalid tool parameters", id, None);
        }
        let decoded = header(&parts.headers, "mcp-name").and_then(decode_name);
        if decoded.as_deref() != name {
            return rpc(400, -32020, "Header mismatch", id, None);
        }
        if method == "tools/call"
            && !matches!(name, Some("haystack_read" | "haystack_query"))
            && !(name == Some("haystack_history") && shared.application.history_service().is_some())
        {
            return rpc(400, -32602, "Unknown tool", id, None);
        }
    }
    // Reject known-method decoding failures before the SDK catch-all can turn
    // them into defaults, and before any native read/work admission.
    if !matches!(serde_json::from_value::<rmcp::model::ClientJsonRpcMessage>(value),Ok(rmcp::model::JsonRpcMessage::Request(ref r)) if !matches!(r.request,rmcp::model::ClientRequest::CustomRequest(_)))
    {
        return rpc(400, -32602, "Invalid parameters", id, None);
    }
    let Ok(permit) = shared.permits.clone().try_acquire_owned() else {
        return empty(503);
    };
    let Ok(work) = shared.application.admit() else {
        return empty(503);
    };
    let admitted = Arc::new(Admitted {
        principal,
        deadline,
        cancellation: shared.stop.child_token(),
        _permit: permit,
        work,
        _release: Release(Arc::downgrade(shared)),
    });
    {
        let mut report = shared.report.lock();
        report.admitted += 1;
        report.peak_in_flight = report
            .peak_in_flight
            .max(shared.config.limits.max_in_flight - shared.permits.available_permits());
    }
    *slot.lock() = Some(admitted.clone());
    parts.extensions.insert(Lease(admitted));
    // The wrapper has validated every Accept field and the JSON media type.
    // rmcp 3.8.0 reads only the first field with case-sensitive comparisons;
    // pass the canonical selected media values without losing legal HTTP forms.
    parts.headers.insert(
        "accept",
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    parts
        .headers
        .insert("content-type", HeaderValue::from_static("application/json"));
    let result = sdk.call(Request::from_parts(parts, Full::new(bytes))).await;
    let response = match result {
        Ok(response) => response,
        Err(never) => match never {},
    };
    let (parts, body) = response.into_parts();
    match Limited::new(body, shared.config.limits.max_response_bytes)
        .collect()
        .await
    {
        Ok(body) => Response::from_parts(parts, Full::new(body.to_bytes())),
        Err(_) => rpc(500, -32603, "Response budget exceeded", id, None),
    }
}
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}
fn empty(status: u16) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::new()))
        .expect("fixed response")
}
fn allowed(method: &str) -> Response<Body> {
    let mut response = empty(405);
    response
        .headers_mut()
        .insert("allow", method.parse().expect("fixed method"));
    response
}
fn json_response(status: u16, value: &Value) -> Response<Body> {
    let (status, bytes) = match encode(value, 8192) {
        Ok(bytes) => (status, bytes),
        Err(_) => (
            500,
            br#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"Response budget exceeded"}}"#
                .to_vec(),
        ),
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Full::new(bytes.into()))
        .expect("fixed response")
}
fn rpc(
    status: u16,
    code: i64,
    message: &str,
    id: Option<Value>,
    data: Option<Value>,
) -> Response<Body> {
    let mut value = json!({"jsonrpc":"2.0","error":{"code":code,"message":message}});
    if let Some(id) = id {
        value["id"] = id;
    }
    if let Some(data) = data {
        value["error"]["data"] = data;
    }
    json_response(status, &value)
}
fn auth_error(shared: &Shared, status: u16, error: Option<&str>) -> Response<Body> {
    let mut response = empty(status);
    let mut challenge = format!(
        "Bearer resource_metadata=\"{}\", scope=\"{}\"",
        shared.metadata_url,
        shared.verifier.required_scope()
    );
    if let Some(error) = error {
        challenge.push_str(&format!(", error=\"{error}\""));
    }
    response.headers_mut().insert(
        "www-authenticate",
        challenge.parse().expect("validated configuration"),
    );
    response
}
fn accepts(headers: &HeaderMap, wanted: &str) -> bool {
    headers
        .get_all("accept")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|item| {
            let mut parts = item.trim().split(';');
            parts
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case(wanted))
                && match (parts.next(), parts.next()) {
                    (None, None) => true,
                    (Some(parameter), None) => {
                        parameter
                            .trim()
                            .split_once('=')
                            .is_some_and(|(name, value)| {
                                name.eq_ignore_ascii_case("q") && quality(value)
                            })
                    }
                    _ => false,
                }
        })
}
fn quality(value: &str) -> bool {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    matches!(whole, "0" | "1")
        && fraction.len() <= 3
        && fraction.bytes().all(|b| b.is_ascii_digit())
        && value
            .parse::<f64>()
            .is_ok_and(|value| value > 0.0 && value <= 1.0)
}
fn decode_name(value: &str) -> Option<String> {
    match value
        .strip_prefix("=?base64?")
        .and_then(|value| value.strip_suffix("?="))
    {
        Some(value) => String::from_utf8(STANDARD.decode(value).ok()?).ok(),
        None => Some(value.into()),
    }
}
fn valid_origin(origin: &str) -> bool {
    origin.len() <= 2048
        && url::Url::parse(origin).is_ok_and(|url| {
            matches!(url.scheme(), "https" | "http")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && !origin.ends_with('/')
                && !origin.contains('*')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_app::{AllowAll, ApplicationBuilder, ReadLimits};
    use haystack_core::graph::{EntityGraph, SharedGraph};
    use std::time::Duration;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};

    #[tokio::test]
    async fn socket_read_eof_cancels_only_its_request_before_returning_without_releasing_quota() {
        let owner = ApplicationBuilder::new(
            SharedGraph::new(EntityGraph::new()),
            Arc::new(AllowAll),
            ReadLimits::default(),
        )
        .unwrap()
        .start(&tokio::runtime::Handle::current())
        .unwrap();
        owner.ready().await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (peer, accepted) = tokio::join!(
            tokio::net::TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept(),
        );
        let mut peer = peer.unwrap();
        let (socket, _) = accepted.unwrap();
        let parent = CancellationToken::new();
        let sibling = parent.child_token();
        let cancel = parent.child_token();
        let quota = Arc::new(Semaphore::new(1));
        let request = Arc::new(Admitted {
            principal: Principal::Anonymous,
            deadline: Arc::new(RequestDeadline::new(
                Instant::now() + Duration::from_secs(30),
            )),
            cancellation: cancel.clone(),
            _permit: quota.clone().try_acquire_owned().unwrap(),
            work: owner.handle().admit().unwrap(),
            _release: Release(Weak::new()),
        });
        let slot = Arc::new(Mutex::new(Some(request.clone())));
        let guard = ConnectionLease(slot.clone());
        let mut io = SocketIo {
            socket,
            slot: slot.clone(),
        };
        peer.write_all(b"x").await.unwrap();

        // A successful zero-capacity read has no new bytes without being EOF.
        let mut empty = ReadBuf::new(&mut []);
        tokio::time::timeout(
            Duration::from_secs(1),
            std::future::poll_fn(|cx| Pin::new(&mut io).poll_read(cx, &mut empty)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!cancel.is_cancelled());

        // Compare the newly filled length, not whether the caller's buffer
        // already contains bytes, and keep live requests admitted.
        let mut storage = [0; 8];
        let mut data = ReadBuf::new(&mut storage);
        data.put_slice(b"old");
        tokio::time::timeout(
            Duration::from_secs(1),
            std::future::poll_fn(|cx| Pin::new(&mut io).poll_read(cx, &mut data)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(data.filled(), b"oldx");
        assert!(!cancel.is_cancelled());
        assert_eq!(quota.available_permits(), 0);

        peer.shutdown().await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            std::future::poll_fn(|cx| Pin::new(&mut io).poll_read(cx, &mut data)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(data.filled(), b"oldx");
        assert!(
            cancel.is_cancelled(),
            "observed EOF must cancel before Hyper can see it"
        );
        assert!(!parent.is_cancelled() && !sibling.is_cancelled());
        assert!(slot.lock().is_some());
        assert_eq!(quota.available_permits(), 0);
        drop(io);
        drop(guard);
        // The independently held handler/worker lease still owns native quota.
        assert_eq!(quota.available_permits(), 0);
        drop(request);
        assert_eq!(quota.available_permits(), 1);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn delayed_first_poll_cannot_renew_an_accepted_connection_budget() {
        for delayed in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (peer, accepted) = tokio::join!(
                tokio::net::TcpStream::connect(listener.local_addr().unwrap()),
                listener.accept(),
            );
            let mut peer = peer.unwrap();
            let (socket, _) = accepted.unwrap();
            let deadline = Instant::now() + Duration::from_millis(40);
            let admitted = Arc::new(AtomicUsize::new(0));
            let observed = admitted.clone();
            // Construct the actual production connection future at accept, but
            // retain it locally so no executor can poll it before our schedule.
            let connection = accepted_connection(
                socket,
                Arc::new(Mutex::new(None)),
                deadline,
                16_384,
                CancellationToken::new(),
                move |_| {
                    service_fn(move |_| {
                        observed.fetch_add(1, Ordering::SeqCst);
                        async {
                            Ok::<_, Infallible>(Response::new(Full::new(Bytes::from_static(
                                b"admitted",
                            ))))
                        }
                    })
                },
            );
            peer.write_all(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            if delayed {
                // A complete request is already buffered. Delay only the first
                // poll of the accepted future, beyond its entire budget.
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
            let delivered = tokio::time::timeout(Duration::from_secs(1), connection)
                .await
                .unwrap();
            let mut bytes = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut bytes))
                .await
                .unwrap();
            if delayed {
                assert!(
                    !delivered,
                    "an expired accepted connection delivered a response"
                );
                assert_eq!(
                    admitted.load(Ordering::SeqCst),
                    0,
                    "expired connection reached request admission"
                );
                assert!(
                    bytes.is_empty(),
                    "expired connection published response bytes"
                );
                assert!(
                    read.is_ok()
                        || read.is_err_and(|error| error.kind() == io::ErrorKind::ConnectionReset)
                );
            } else {
                assert!(delivered);
                assert_eq!(admitted.load(Ordering::SeqCst), 1);
                assert!(read.is_ok());
                assert!(bytes.ends_with(b"admitted"));
            }
        }
    }
}
