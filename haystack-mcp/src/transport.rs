use crate::{
    Limits, McpError, OutputProfile, Report,
    common::{depth_ok, encode},
    handler::{Invocation, Lease, RequestDeadline, Service},
};
use haystack_app::{ApplicationHandle, CancellationToken, Principal, WorkGuard};
use parking_lot::Mutex;
use rmcp::{
    RoleServer,
    model::{
        ClientJsonRpcMessage, ClientNotification, GetExtensions, JsonRpcMessage, RequestId,
        ServerJsonRpcMessage,
    },
    transport::Transport,
};
use serde::Deserialize;
#[cfg(test)]
use std::time::Duration;
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
    time::Instant,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
};

pub(crate) struct Request {
    pub id: RequestId,
    principal: Principal,
    pub deadline: RequestDeadline,
    pub cancellation: CancellationToken,
    _permit: OwnedSemaphorePermit,
    pub work: WorkGuard,
    // Runs after permit and work drop, so release evidence reflects cleanup.
    _release: Release,
}
struct Release(Weak<Shared>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(shared) = self.0.upgrade() {
            shared.state.lock().report.released += 1;
            shared.changed.notify_waiters();
        }
    }
}
impl Invocation for Request {
    fn principal(&self) -> &Principal {
        &self.principal
    }
    fn deadline(&self) -> Instant {
        self.deadline.get()
    }
    fn tighten_deadline(&self, deadline: Instant) {
        if self.deadline.tighten(deadline)
            && let Some(shared) = self._release.0.upgrade()
        {
            shared.changed.notify_waiters();
        }
    }
    fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    fn work(&self) -> WorkGuard {
        self.work.child()
    }
}

struct State {
    requests: HashMap<RequestId, Arc<Request>>,
    error: Option<McpError>,
    report: Report,
}
pub(crate) struct Shared {
    pub application: ApplicationHandle,
    pub principal: Principal,
    pub limits: Limits,
    pub profile: OutputProfile,
    pub semantic_resources: bool,
    state: Mutex<State>,
    permits: Arc<Semaphore>,
    stop: CancellationToken,
    changed: tokio::sync::Notify,
    #[cfg(test)]
    reader_pause: Mutex<Option<ReaderPause>>,
}
impl Shared {
    fn stop(&self, error: Option<McpError>) {
        let mut state = self.state.lock();
        if state.error.is_none() {
            state.error = error;
        }
        self.stop.cancel();
        // Context extensions retain each admitted request through its actual
        // handler cleanup, even if SDK drain finishes before that cleanup.
        for request in state.requests.values() {
            request.cancellation.cancel();
        }
        let requests = std::mem::take(&mut state.requests);
        drop(state);
        drop(requests);
        self.changed.notify_waiters();
    }
    fn admit(
        self: &Arc<Self>,
        id: RequestId,
        started: Instant,
    ) -> Result<Arc<Request>, &'static str> {
        let mut state = self.state.lock();
        if self.stop.is_cancelled() {
            return Err("closed");
        }
        if state.requests.contains_key(&id) {
            return Err("duplicate_request_id");
        }
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| "capacity")?;
        let work = self.application.admit().map_err(|_| "closed")?;
        let request = Arc::new(Request {
            id: id.clone(),
            principal: self.principal.clone(),
            deadline: RequestDeadline::new(started + self.limits.request_timeout),
            cancellation: self.stop.child_token(),
            _permit: permit,
            work,
            _release: Release(Arc::downgrade(self)),
        });
        state.report.admitted += 1;
        state.report.peak_in_flight = state
            .report
            .peak_in_flight
            .max(self.limits.max_in_flight - self.permits.available_permits());
        state.requests.insert(id, request.clone());
        Ok(request)
    }
    fn request(&self, id: &RequestId) -> Option<Arc<Request>> {
        self.state.lock().requests.get(id).cloned()
    }
    fn finish(&self, request: &Arc<Request>) {
        let mut state = self.state.lock();
        let removed = if state
            .requests
            .get(&request.id)
            .is_some_and(|current| Arc::ptr_eq(current, request))
        {
            state.requests.remove(&request.id)
        } else {
            None
        };
        drop(state);
        drop(removed);
        self.changed.notify_waiters();
    }
    fn deadline(&self, started: Option<Instant>) -> Option<Instant> {
        self.state
            .lock()
            .requests
            .values()
            .map(|request| request.deadline.get())
            .chain(started.map(|time| time + self.limits.request_timeout))
            .min()
    }
}

struct Output {
    bytes: Vec<u8>,
    deadline: Instant,
    request: Option<Arc<Request>>,
    shared: Arc<Shared>,
    acknowledgement: Option<oneshot::Sender<Result<(), McpError>>>,
    _permit: OwnedSemaphorePermit,
}
impl Drop for Output {
    fn drop(&mut self) {
        if let Some(request) = &self.request {
            self.shared.finish(request);
        }
    }
}
#[derive(Clone)]
struct Outbox {
    sender: mpsc::Sender<Output>,
    permits: Arc<Semaphore>,
    shared: Arc<Shared>,
}
impl Outbox {
    fn submit(
        &self,
        message: ServerJsonRpcMessage,
        request: Option<Arc<Request>>,
        deadline: Instant,
    ) -> Result<oneshot::Receiver<Result<(), McpError>>, McpError> {
        if self.shared.stop.is_cancelled() {
            return Err(McpError::Io);
        }
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| McpError::Capacity)?;
        let max = self.shared.limits.max_response_bytes.saturating_sub(1);
        let mut bytes = encode(&message, max)?;
        bytes.push(b'\n');
        let (acknowledgement, result) = oneshot::channel();
        self.sender
            .try_send(Output {
                bytes,
                deadline,
                request,
                shared: self.shared.clone(),
                acknowledgement: Some(acknowledgement),
                _permit: permit,
            })
            .map_err(|_| McpError::Capacity)?;
        Ok(result)
    }
    fn rejection(&self, id: Option<RequestId>, reason: &'static str, started: Instant) {
        self.protocol_error(id, rmcp::model::ErrorCode::INVALID_REQUEST, reason, started);
    }
    fn protocol_error(
        &self,
        id: Option<RequestId>,
        code: rmcp::model::ErrorCode,
        reason: &'static str,
        started: Instant,
    ) {
        let error = rmcp::ErrorData::new(
            code,
            "MCP request rejected",
            Some(serde_json::json!({"reason": reason})),
        );
        if let Err(error) = self.submit(
            ServerJsonRpcMessage::error(error, id),
            None,
            started + self.shared.limits.request_timeout,
        ) && !self.shared.stop.is_cancelled()
        {
            self.shared.stop(Some(error));
        }
    }
}

enum Delivery {
    Complete,
    Suppressed,
    Stopped,
}
async fn deliver<W: AsyncWrite + Unpin>(
    io: &mut W,
    output: &Output,
    shared: &Shared,
) -> Result<Delivery, McpError> {
    let cancelled = async {
        match &output.request {
            Some(request) => request.cancellation.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(cancelled);
    let mut written = 0;
    loop {
        let complete = written == output.bytes.len();
        let count = tokio::select! {
            biased;
            _ = shared.stop.cancelled() => return Ok(Delivery::Stopped),
            _ = &mut cancelled => return if written == 0 { Ok(Delivery::Suppressed) } else { Err(McpError::CancelledDelivery) },
            _ = tokio::time::sleep_until(output.deadline.into()) => return Err(McpError::Deadline),
            result = async {
                if complete { io.flush().await.map(|()| 0) } else { io.write(&output.bytes[written..]).await }
            } => result.map_err(|_| McpError::Io)?,
        };
        if complete {
            return Ok(Delivery::Complete);
        }
        if count == 0 {
            return Err(McpError::Io);
        }
        written += count;
    }
}

async fn writer<W: AsyncWrite + Unpin>(
    mut io: W,
    mut input: mpsc::Receiver<Output>,
    shared: Arc<Shared>,
) {
    loop {
        let output = tokio::select! {
            biased;
            _ = shared.stop.cancelled() => break,
            output = input.recv() => output,
        };
        let Some(mut output) = output else { break };
        let result = match deliver(&mut io, &output, &shared).await {
            Ok(Delivery::Complete) => {
                shared.state.lock().report.delivered += 1;
                Ok(())
            }
            Ok(Delivery::Suppressed) => {
                shared.state.lock().report.suppressed += 1;
                Ok(())
            }
            Ok(Delivery::Stopped) => Ok(()),
            Err(error) => {
                shared.stop(Some(error.clone()));
                Err(error)
            }
        };
        let acknowledgement = output.acknowledgement.take();
        // Finish the physical send and release queue/request ownership before
        // acknowledging it to the SDK, including cancellation suppression.
        drop(output);
        if let Some(acknowledgement) = acknowledgement {
            let _ = acknowledgement.send(result);
        }
    }
    input.close();
    while input.try_recv().is_ok() {}
    // No unbounded flush/shutdown on a blocked peer. All selected writes are
    // cancellable; dropping the pipe releases the descriptor after task join.
}

// The SDK's untagged message decoder can otherwise consume a request with
// an invalid ID as a notification. Missing IDs are allowed, but every present
// ID (including null) must pass the selected RequestId decoder first.
#[derive(Deserialize)]
struct EnvelopeId {
    #[serde(rename = "id", default, deserialize_with = "present_id")]
    _id: Option<RequestId>,
}
fn present_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<RequestId>, D::Error> {
    RequestId::deserialize(deserializer).map(Some)
}

async fn reader<R: AsyncRead + Unpin>(
    io: R,
    sender: mpsc::Sender<ClientJsonRpcMessage>,
    outbox: Outbox,
    shared: Arc<Shared>,
    cancelled: CancellationToken,
) {
    let mut input = BufReader::with_capacity(4096, io);
    let mut frame = Vec::with_capacity(shared.limits.max_frame_bytes);
    let mut started: Option<Instant> = None;
    let application_closing = shared.application.closing();
    loop {
        // Register before observing the predicate so completion cannot be lost.
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let deadline = shared.deadline(started);
        #[cfg(test)]
        pause_reader(&shared, false, deadline).await;
        #[cfg(test)]
        pause_reader(&shared, true, deadline).await;
        let bytes = tokio::select! {
            biased;
            _ = shared.stop.cancelled() => break,
            _ = cancelled.cancelled() => { shared.stop(None); break; }
            _ = application_closing.cancelled() => { shared.stop(None); break; }
            _ = async { match deadline { Some(deadline) => tokio::time::sleep_until(deadline.into()).await, None => std::future::pending().await } } => {
                // A completed request can leave both this old timer and its
                // notification ready. Only the current predicate is authority.
                if shared.deadline(started).is_some_and(|deadline| deadline <= Instant::now()) {
                    shared.stop(Some(McpError::Deadline)); break;
                }
                continue;
            }
            _ = changed => continue,
            result = input.fill_buf() => match result { Ok(bytes) => bytes, Err(_) => { shared.stop(Some(McpError::Io)); break; } },
        };
        if bytes.is_empty() {
            shared.stop(if frame.is_empty() {
                None
            } else {
                Some(McpError::Framing)
            });
            break;
        }
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let count = newline.unwrap_or(bytes.len());
        started.get_or_insert_with(Instant::now);
        if count > shared.limits.max_frame_bytes.saturating_sub(frame.len()) {
            shared.stop(Some(McpError::Framing));
            break;
        }
        frame.extend_from_slice(&bytes[..count]);
        input.consume(count + usize::from(newline.is_some()));
        if newline.is_none() {
            continue;
        }
        let start = started.take().expect("nonempty frame read");
        if !depth_ok(&frame, shared.limits.max_json_depth) {
            shared.stop(Some(McpError::Framing));
            break;
        }
        let parsed = serde_json::from_slice::<EnvelopeId>(&frame)
            .and_then(|_| serde_json::from_slice::<ClientJsonRpcMessage>(&frame));
        let message = match parsed {
            Ok(message) => {
                frame.clear();
                message
            }
            Err(_) => {
                let (id, code, reason) = match serde_json::from_slice::<serde_json::Value>(&frame) {
                    Ok(value) => {
                        let id = value
                            .get("id")
                            .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok())
                            .filter(|id| !matches!(id, RequestId::String(id) if id.len() > 128));
                        (
                            id,
                            rmcp::model::ErrorCode::INVALID_REQUEST,
                            "invalid_jsonrpc",
                        )
                    }
                    Err(_) => (None, rmcp::model::ErrorCode::PARSE_ERROR, "invalid_json"),
                };
                frame.clear();
                outbox.protocol_error(id, code, reason, start);
                tokio::task::yield_now().await;
                continue;
            }
        };
        match message {
            JsonRpcMessage::Request(mut request) => {
                if matches!(&request.id, RequestId::String(id) if id.len() > 128) {
                    outbox.rejection(None, "request_id_too_large", start);
                    continue;
                }
                match shared.admit(request.id.clone(), start) {
                    Ok(lease) => {
                        request.request.extensions_mut().insert(Lease(lease));
                        if sender.try_send(JsonRpcMessage::Request(request)).is_err() {
                            shared.stop(Some(McpError::Capacity));
                            break;
                        }
                    }
                    Err(reason) => outbox.rejection(Some(request.id), reason, start),
                }
            }
            JsonRpcMessage::Notification(notification) => {
                // Consume cancellation privately. The SDK still retires its
                // request entry when our joined handler's suppressed result
                // reaches send(), preventing its late-response/ID reuse race.
                if let ClientNotification::CancelledNotification(cancelled) =
                    notification.notification
                    && let Some(id) = cancelled.params.request_id
                    && let Some(request) = shared.request(&id)
                {
                    request.cancellation.cancel();
                }
                // initialized needs no SDK callback/gate; all other unsolicited
                // notifications are ignored without spawning SDK handler tasks.
            }
            _ => {
                shared.stop(Some(McpError::Protocol));
                break;
            }
        }
        tokio::task::yield_now().await;
    }
}

struct BoundedTransport {
    input: mpsc::Receiver<ClientJsonRpcMessage>,
    outbox: Outbox,
    shared: Arc<Shared>,
}
impl Drop for BoundedTransport {
    fn drop(&mut self) {
        self.shared.stop(None);
    }
}
impl Transport<RoleServer> for BoundedTransport {
    type Error = McpError;
    fn send(
        &mut self,
        message: ServerJsonRpcMessage,
    ) -> impl Future<Output = Result<(), McpError>> + Send + 'static {
        let shared = self.shared.clone();
        let id = match &message {
            JsonRpcMessage::Response(response) => Some(&response.id),
            JsonRpcMessage::Error(error) => error.id.as_ref(),
            _ => None,
        };
        let request = id.and_then(|id| shared.request(id));
        let deadline = request.as_ref().map_or_else(
            || Instant::now() + shared.limits.request_timeout,
            |request| request.deadline.get(),
        );
        let result = self.outbox.submit(message, request, deadline);
        if let Err(error) = &result
            && !shared.stop.is_cancelled()
        {
            shared.stop(Some(error.clone()));
        }
        async move { result?.await.map_err(|_| McpError::Io)? }
    }
    async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
        tokio::select! {
            biased;
            _ = self.shared.stop.cancelled() => None,
            message = self.input.recv() => message,
        }
    }
    async fn close(&mut self) -> Result<(), McpError> {
        self.shared.stop(None);
        Ok(())
    }
}

/// Serve one bounded MCP connection, borrowing the application and runtime.
/// Explicit stdio transport bounds and selected presentation/capabilities.
#[derive(Debug, Clone, Default)]
pub struct StdioConfig {
    pub limits: Limits,
    pub output_profile: OutputProfile,
    /// Explicitly expose policy-filtered H4 definition/library resources.
    pub semantic_resources: bool,
}

/// Cancellation/EOF stops only this connection. Success proves its reader,
/// writer and request leases were released; an SDK quit reason alone does not.
/// The supplied I/O must itself be cancellation safe (the CLI selects Unix pipes).
pub async fn serve<R, W>(
    application: ApplicationHandle,
    principal: Principal,
    input: R,
    output: W,
    config: StdioConfig,
    cancelled: CancellationToken,
) -> Result<Report, McpError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let StdioConfig {
        limits,
        output_profile: profile,
        semantic_resources,
    } = config;
    limits.validate(profile)?;
    if limits.max_response_bytes
        < profile.resource_min_response_bytes(
            application.history_service().is_some(),
            semantic_resources,
        )
    {
        return Err(McpError::Configuration);
    }
    application
        .ready()
        .await
        .map_err(|_| McpError::Configuration)?;
    let shared = Arc::new(Shared {
        application,
        principal,
        permits: Arc::new(Semaphore::new(limits.max_in_flight)),
        state: Mutex::new(State {
            requests: HashMap::new(),
            error: None,
            report: Report::default(),
        }),
        stop: CancellationToken::new(),
        changed: tokio::sync::Notify::new(),
        #[cfg(test)]
        reader_pause: Mutex::new(None),
        limits,
        profile,
        semantic_resources,
    });
    let _cancel_on_drop = shared.stop.clone().drop_guard();
    let (sender, receiver) = mpsc::channel(shared.limits.max_pending_responses);
    let outbox = Outbox {
        sender,
        permits: Arc::new(Semaphore::new(shared.limits.max_pending_responses)),
        shared: shared.clone(),
    };
    let (incoming, input_receiver) = mpsc::channel(shared.limits.max_in_flight);
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(writer(output, receiver, shared.clone()));
    tasks.spawn(reader(
        input,
        incoming,
        outbox.clone(),
        shared.clone(),
        cancelled,
    ));
    let transport = BoundedTransport {
        input: input_receiver,
        outbox,
        shared: shared.clone(),
    };
    let service = rmcp::serve_server(Service::new(shared.clone()), transport).await;
    match service {
        Ok(service) => {
            if service.waiting().await.is_err() {
                shared.stop(Some(McpError::Task));
            }
        }
        Err(_) if !shared.stop.is_cancelled() => shared.stop(Some(McpError::Protocol)),
        Err(_) => {}
    }
    shared.stop(None);
    while let Some(result) = tasks.join_next().await {
        if result.is_err() {
            shared.stop(Some(McpError::Task));
        }
    }
    // Detached SDK contexts carry generation leases. Waiting for every permit
    // proves their joined application work ended, including cancelled handlers.
    let _all = shared
        .permits
        .clone()
        .acquire_many_owned(shared.limits.max_in_flight as u32)
        .await
        .map_err(|_| McpError::Task)?;
    // A semaphore wake can run on another thread before the releasing
    // Request finishes dropping its later work/release fields. Join that
    // final release signal too before publishing the cleanup report.
    loop {
        let changed = shared.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let complete = {
            let state = shared.state.lock();
            state.report.admitted == state.report.released
        };
        if complete {
            break;
        }
        changed.await;
    }
    let state = shared.state.lock();
    match &state.error {
        Some(error) => Err(error.clone()),
        None => Ok(state.report),
    }
}

#[cfg(test)]
struct ReaderPause {
    after_waiter: bool,
    captured: oneshot::Sender<Option<Instant>>,
    resume: oneshot::Receiver<()>,
}
#[cfg(test)]
async fn pause_reader(shared: &Shared, after_waiter: bool, deadline: Option<Instant>) {
    let pause = {
        let mut pause = shared.reader_pause.lock();
        if pause
            .as_ref()
            .is_some_and(|pause| pause.after_waiter == after_waiter)
        {
            pause.take()
        } else {
            None
        }
    };
    if let Some(pause) = pause {
        let _ = pause.captured.send(deadline);
        let _ = pause.resume.await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use haystack_app::{AllowAll, ApplicationBuilder, ReadLimits};
    use haystack_core::graph::{EntityGraph, SharedGraph};

    #[tokio::test]
    async fn changed_request_deadline_is_revalidated_after_both_reader_races() {
        let mut failures = Vec::new();
        for (after_waiter, tightened) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let owner = ApplicationBuilder::new(
                SharedGraph::new(EntityGraph::new()),
                Arc::new(AllowAll),
                ReadLimits::default(),
            )
            .unwrap()
            .start(&tokio::runtime::Handle::current())
            .unwrap();
            owner.ready().await.unwrap();
            let (captured, snapshot) = oneshot::channel();
            let (resume, paused) = oneshot::channel();
            let shared = Arc::new(Shared {
                application: owner.handle(),
                principal: Principal::TrustedEmbedding {
                    subject: "deadline-fixture".into(),
                },
                limits: Limits {
                    request_timeout: Duration::from_millis(if tightened { 3000 } else { 100 }),
                    ..Limits::default()
                },
                profile: OutputProfile::H4,
                semantic_resources: false,
                state: Mutex::new(State {
                    requests: HashMap::new(),
                    error: None,
                    report: Report::default(),
                }),
                permits: Arc::new(Semaphore::new(16)),
                stop: CancellationToken::new(),
                changed: tokio::sync::Notify::new(),
                reader_pause: Mutex::new(Some(ReaderPause {
                    after_waiter,
                    captured,
                    resume: paused,
                })),
            });
            let request = shared.admit(RequestId::Number(1), Instant::now()).unwrap();
            let (outgoing, _output) = mpsc::channel(16);
            let outbox = Outbox {
                sender: outgoing,
                permits: Arc::new(Semaphore::new(16)),
                shared: shared.clone(),
            };
            let (incoming, mut messages) = mpsc::channel(16);
            let (mut peer, input) = tokio::io::duplex(1024);
            let cancelled = CancellationToken::new();
            let task = tokio::spawn(reader(
                input,
                incoming,
                outbox,
                shared.clone(),
                cancelled.clone(),
            ));
            let captured_deadline = snapshot.await.unwrap().unwrap();
            if tightened {
                request.tighten_deadline(Instant::now());
                request.tighten_deadline(captured_deadline + Duration::from_secs(5));
            } else {
                shared.finish(&request);
            }
            drop(request);
            if after_waiter && !tightened {
                // Both the completion notification and the old timer are ready.
                tokio::time::sleep_until((captured_deadline + Duration::from_millis(20)).into())
                    .await;
            }
            resume.send(()).unwrap();
            if tightened {
                let _ =
                    tokio::time::timeout(Duration::from_millis(100), shared.stop.cancelled()).await;
            } else {
                tokio::time::sleep_until((captured_deadline + Duration::from_millis(40)).into())
                    .await;
            }
            let closed = shared.stop.is_cancelled();
            if !closed && !tightened {
                peer.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n")
                    .await
                    .unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_secs(1), messages.recv())
                        .await
                        .unwrap()
                        .is_some()
                );
            }
            cancelled.cancel();
            task.await.unwrap();
            drop(messages);
            assert_eq!(owner.handle().outstanding_tasks(), 0);
            owner.close().await.unwrap();
            assert!(owner.terminated().await.cleanup_errors.is_empty());
            if closed != tightened {
                failures.push((after_waiter, tightened));
            }
        }
        assert!(
            failures.is_empty(),
            "deadline changes were missed or stale deadlines fired (after_waiter, tightened): {failures:?}"
        );
    }
}
