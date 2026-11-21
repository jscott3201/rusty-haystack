//! Synthetic HTTP(S) server. One joined runtime thread owns every socket/task;
//! drop shuts down even on assertion unwind. No external services or credentials.
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, oneshot},
    task::JoinSet,
};

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay_headers: Duration,
    pub delay_body: Duration,
    pub length: Option<usize>,
    pub chunked: bool,
    pub stall: bool,
}
impl Reply {
    pub fn json(body: &str) -> Self {
        Self::bytes("application/json;box=none", body.as_bytes())
    }
    pub fn bytes(media: &str, body: &[u8]) -> Self {
        Self {
            status: 200,
            headers: vec![
                ("Content-Type".into(), media.into()),
                ("Xeto-Version".into(), "5".into()),
            ],
            body: body.to_vec(),
            delay_headers: Duration::ZERO,
            delay_body: Duration::ZERO,
            length: None,
            chunked: false,
            stall: false,
        }
    }
    pub fn redirect(location: &str) -> Self {
        let mut reply = Self::json("{}");
        reply.status = 302;
        reply.headers.push(("Location".into(), location.into()));
        reply
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.headers.push((name.into(), value.into()));
        self
    }
}
#[derive(Default)]
struct State {
    requests: Mutex<Vec<String>>,
    ready: Notify,
    closed: AtomicUsize,
    closed_notify: Notify,
    body_sent: AtomicUsize,
    body_notify: Notify,
}
struct Closed(Arc<State>);
impl Drop for Closed {
    fn drop(&mut self) {
        self.0.closed.fetch_add(1, Ordering::SeqCst);
        self.0.closed_notify.notify_waiters();
    }
}
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut socket: S, reply: Reply, state: Arc<State>) {
    let _closed = Closed(state.clone());
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let n = match socket.read(&mut buffer).await {
            Ok(n) if n > 0 => n,
            _ => return,
        };
        request.extend_from_slice(&buffer[..n]);
        if request.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if request.len() > 16384 {
            return;
        }
    }
    state
        .requests
        .lock()
        .unwrap()
        .push(String::from_utf8(request).unwrap());
    state.ready.notify_waiters();
    tokio::time::sleep(reply.delay_headers).await;
    let mut head = format!("HTTP/1.1 {} Fixture\r\nConnection: close\r\n", reply.status);
    for (k, v) in reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if reply.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n");
    } else {
        head.push_str(&format!(
            "Content-Length: {}\r\n",
            reply.length.unwrap_or(reply.body.len())
        ));
    }
    head.push_str("\r\n");
    if socket.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    tokio::time::sleep(reply.delay_body).await;
    if reply.chunked
        && socket
            .write_all(format!("{:x}\r\n", reply.body.len()).as_bytes())
            .await
            .is_err()
    {
        return;
    }
    if socket.write_all(&reply.body).await.is_err() {
        return;
    }
    if reply.chunked && socket.write_all(b"\r\n").await.is_err() {
        return;
    }
    state.body_sent.fetch_add(1, Ordering::SeqCst);
    state.body_notify.notify_waiters();
    if reply.stall {
        let _ = socket.read(&mut buffer).await;
    } else if reply.chunked {
        let _ = socket.write_all(b"0\r\n\r\n").await;
    }
}

pub struct Server {
    pub base: String,
    state: Arc<State>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    pub fn new(replies: Vec<Reply>) -> Self {
        Self::start(replies, None)
    }
    pub fn tls(replies: Vec<Reply>) -> (Self, Vec<u8>) {
        use rcgen::{
            BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
            KeyPair, KeyUsagePurpose,
        };
        haystack_client::ensure_crypto_provider();
        let now = time::OffsetDateTime::now_utc();
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        // rcgen gives every certificate the same default subject. With the leaf's
        // subject equal to its issuer, Windows chain building treats the leaf as
        // self-signed and rejects it, so name the two certificates apart.
        ca.distinguished_name
            .push(DnType::CommonName, "repository fixture CA");
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        ca.not_before = now - time::Duration::days(1);
        ca.not_after = now + time::Duration::days(7);
        let ca_key = KeyPair::generate().unwrap();
        let ca_pem = ca.self_signed(&ca_key).unwrap().pem().into_bytes();
        let issuer = Issuer::new(ca, ca_key);
        let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, "repository fixture server");
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(7);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, &issuer).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
            )
            .unwrap();
        (Self::start(replies, Some(Arc::new(config))), ca_pem)
    }
    fn start(replies: Vec<Reply>, tls: Option<Arc<rustls::ServerConfig>>) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = if tls.is_some() {
            format!("https://localhost:{port}/repo/base")
        } else {
            format!("http://127.0.0.1:{port}/repo/base")
        };
        listener.set_nonblocking(true).unwrap();
        let state = Arc::new(State::default());
        let server_state = state.clone();
        let (stop, mut shutdown) = oneshot::channel();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
                let listener = TcpListener::from_std(listener).unwrap();
                let mut replies: VecDeque<_> = replies.into();
                let mut tasks = JoinSet::new();
                loop {
                    tokio::select! { biased;
                        _ = &mut shutdown => break,
                        accepted = listener.accept() => {
                            let (socket, _) = accepted.unwrap();
                            let state = server_state.clone();
                            let tls = tls.clone();
                            let reply = replies.pop_front().unwrap_or_else(|| {
                                let mut reply = Reply::json("{}"); reply.status = 500; reply
                            });
                            tasks.spawn(async move {
                                if let Some(config) = tls {
                                    if let Ok(socket) = tokio_rustls::TlsAcceptor::from(config).accept(socket).await {
                                        serve(socket, reply, state).await;
                                    }
                                } else { serve(socket, reply, state).await; }
                            });
                        }
                        Some(result) = tasks.join_next() => result.unwrap(),
                    }
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            });
        });
        Self {
            base,
            state,
            stop: Some(stop),
            thread: Some(thread),
        }
    }
    pub fn requests(&self) -> Vec<String> {
        self.state.requests.lock().unwrap().clone()
    }
    pub async fn requested(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = self.state.ready.notified();
                if self.state.requests.lock().unwrap().len() >= count {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
    pub async fn body_sent(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = self.state.body_notify.notified();
                if self.state.body_sent.load(Ordering::SeqCst) > 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
    pub async fn peer_closed(&self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = self.state.closed_notify.notified();
                if self.state.closed.load(Ordering::SeqCst) > 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
