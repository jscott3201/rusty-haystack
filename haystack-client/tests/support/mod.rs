//! Synthetic loopback server with no external services or certificate files.
//! A dedicated runtime thread owns all sockets. Drop signals shutdown and joins
//! that thread even if an assertion unwinds the test. Each response closes TLS.
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use haystack_client::tls::TlsConfig;
use haystack_core::auth;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};
use tokio_rustls::TlsAcceptor;

pub const SENTINEL: &str = "private-peer-sentinel";
pub const GRID: &str = "ver:\"3.0\"\nempty\n";

pub struct Certificates {
    pub ca: Vec<u8>,
    pub tls: TlsConfig,
    pub unauthorized: TlsConfig,
    client_ca: CertificateDer<'static>,
    server: CertificateDer<'static>,
    server_key: PrivateKeyDer<'static>,
}
impl Certificates {
    pub fn new() -> Self {
        fn params(name: &str) -> CertificateParams {
            let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
            p.distinguished_name.push(DnType::CommonName, name);
            // Match the runtime clock and platform validity policy; never expire
            // a committed fixture or skip actual server/hostname verification.
            let now = time::OffsetDateTime::now_utc();
            p.not_before = now - time::Duration::days(1);
            p.not_after = now + time::Duration::days(7);
            p
        }
        fn issuer(name: &str) -> (rcgen::Certificate, Issuer<'static, KeyPair>) {
            let mut p = params(name);
            p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            let key = KeyPair::generate().unwrap();
            let cert = p.self_signed(&key).unwrap();
            (cert, Issuer::new(p, key))
        }
        fn leaf(issuer: &Issuer<'_, KeyPair>, server: bool) -> (rcgen::Certificate, KeyPair) {
            let mut p = params(if server { "test server" } else { "test client" });
            if server {
                p.subject_alt_names =
                    vec![rcgen::SanType::DnsName("localhost".try_into().unwrap())];
            }
            p.extended_key_usages = vec![if server {
                ExtendedKeyUsagePurpose::ServerAuth
            } else {
                ExtendedKeyUsagePurpose::ClientAuth
            }];
            p.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            let key = KeyPair::generate().unwrap();
            (p.signed_by(&key, issuer).unwrap(), key)
        }
        let (ca, ca_issuer) = issuer("test server CA");
        let (client_ca, client_issuer) = issuer("test client CA");
        let (_, unauthorized_issuer) = issuer("test unauthorized CA");
        let (server, server_key) = leaf(&ca_issuer, true);
        let (client, client_key) = leaf(&client_issuer, false);
        let (unauthorized, unauthorized_key) = leaf(&unauthorized_issuer, false);
        let ca = ca.pem().into_bytes();
        let tls = TlsConfig {
            client_cert_pem: client.pem().into_bytes(),
            client_key_pem: client_key.serialize_pem().into_bytes(),
            ca_cert_pem: Some(ca.clone()),
        };
        let unauthorized = TlsConfig {
            client_cert_pem: unauthorized.pem().into_bytes(),
            client_key_pem: unauthorized_key.serialize_pem().into_bytes(),
            ca_cert_pem: Some(ca.clone()),
        };
        Self {
            ca,
            tls,
            unauthorized,
            client_ca: client_ca.der().clone(),
            server: server.der().clone(),
            server_key: PrivateKeyDer::Pkcs8(server_key.serialize_der().into()),
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub enum Challenge {
    #[default]
    Valid,
    UnsupportedHash,
    MissingHash,
    DuplicateHash,
    DuplicateToken,
    EmptyToken,
    Malformed,
    OversizedHeader,
    OversizedData,
    DuplicateNonce,
    EmptySalt,
    ZeroIterations,
    ExcessiveIterations,
    WrongNonce,
    MultipleScram,
    CombinedSchemes,
    SeparateSchemes,
}
#[derive(Clone, Copy, Default, Debug)]
pub enum Final {
    #[default]
    Valid,
    WrongSignature,
    WrongHash,
    DuplicateToken,
    DuplicateHeader,
    EmptyToken,
    Malformed,
}
#[derive(Clone, Copy, Default, Debug)]
pub enum Domain {
    #[default]
    Good,
    Expired,
    Redirect,
    DropResponse,
    ErrorBody,
    ErrorGrid,
    InvalidGrid,
}
#[derive(Default, Clone, Copy)]
pub struct Options {
    pub challenge: Challenge,
    pub final_message: Final,
    pub domain: Domain,
    pub auth_delay: Duration,
}
#[derive(Default)]
pub struct State {
    handshake: Option<auth::ScramHandshake>,
    pub requests: Vec<(String, String)>,
    pub domain: usize,
}
pub struct Server {
    pub url: String,
    pub state: Arc<Mutex<State>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let result = task.join();
            if !std::thread::panicking() {
                result.expect("loopback server thread");
            }
        }
    }
}
impl Server {
    pub fn start(certs: &Certificates, mtls: bool, options: Options) -> Self {
        haystack_client::ensure_crypto_provider();
        let builder = rustls::ServerConfig::builder();
        let builder = if mtls {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(certs.client_ca.clone()).unwrap();
            builder.with_client_cert_verifier(
                rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                    .build()
                    .unwrap(),
            )
        } else {
            builder.with_no_client_auth()
        };
        let config = builder
            .with_single_cert(vec![certs.server.clone()], certs.server_key.clone_key())
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "https://localhost:{}/api",
            listener.local_addr().unwrap().port()
        );
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let (stop, mut stopped) = oneshot::channel();
        let task = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = TcpListener::from_std(listener).unwrap();
                loop {
                    tokio::select! {
                        _ = &mut stopped => break,
                        accepted = listener.accept() => {
                            let (tcp, _) = accepted.unwrap();
                            let exchange = async {
                                let Ok(mut stream) = acceptor.accept(tcp).await else { return };
                                let mut bytes = Vec::new();
                                let header_end = loop {
                                    let mut buf = [0; 1024];
                                    let Ok(n) = stream.read(&mut buf).await else { return };
                                    if n == 0 { return }
                                    bytes.extend_from_slice(&buf[..n]);
                                    if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break pos + 4 }
                                    assert!(bytes.len() < 32768);
                                };
                                let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                                let content_len = headers.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").map(|n| n.parse::<usize>().unwrap())).unwrap_or(0);
                                while bytes.len() < header_end + content_len {
                                    let mut buf = [0; 1024];
                                    let Ok(n) = stream.read(&mut buf).await else { return };
                                    if n == 0 { return }
                                    bytes.extend_from_slice(&buf[..n]);
                                }
                                let authorization = headers.lines().find_map(|l| l.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case("authorization")).map(|(_, v)| v.trim())).unwrap_or("");
                                let request = headers.lines().next().unwrap();
                                let response = response(&shared, options, request, authorization);
                                if authorization.starts_with("HELLO ") || authorization.starts_with("SCRAM ") { tokio::time::sleep(options.auth_delay).await; }
                                if let Some(response) = response { let _ = stream.write_all(response.as_bytes()).await; }
                                let _ = stream.shutdown().await;
                            };
                            tokio::select! {
                                _ = &mut stopped => break,
                                _ = tokio::time::timeout(Duration::from_secs(5), exchange) => {}
                            }
                        }
                    }
                }
            });
        });
        Self {
            url,
            state,
            stop: Some(stop),
            task: Some(task),
        }
    }
}
fn field<'a>(header: &'a str, name: &str) -> &'a str {
    header
        .split([',', ' '])
        .find_map(|s| {
            s.split_once('=')
                .filter(|(k, _)| *k == name)
                .map(|(_, v)| v)
        })
        .unwrap()
}
fn response(
    shared: &Mutex<State>,
    options: Options,
    request: &str,
    authorization: &str,
) -> Option<String> {
    let mut state = shared.lock().unwrap();
    state.requests.push((request.into(), authorization.into()));
    let mut body = GRID.to_string();
    let (status, extra) = if authorization.starts_with("HELLO ") {
        let nonce = auth::extract_client_nonce(field(authorization, "data")).unwrap();
        let credentials = auth::derive_credentials("password", b"test-only-salt", 4096);
        let (handshake, mut data) = auth::server_first_message("user", &nonce, &credentials);
        state.handshake = Some(handshake);
        data = match options.challenge {
            Challenge::OversizedData => BASE64.encode("r=".to_string() + &"x".repeat(4200)),
            Challenge::DuplicateNonce => {
                BASE64.encode(format!("r={nonce}extra,r={nonce}extra,s=c2FsdA==,i=4096"))
            }
            Challenge::EmptySalt => BASE64.encode(format!("r={nonce}extra,s=,i=4096")),
            Challenge::ZeroIterations => BASE64.encode(format!("r={nonce}extra,s=c2FsdA==,i=0")),
            Challenge::ExcessiveIterations => {
                BASE64.encode(format!("r={nonce}extra,s=c2FsdA==,i=1000001"))
            }
            Challenge::WrongNonce => BASE64.encode("r=wrong,s=c2FsdA==,i=4096"),
            _ => data,
        };
        let valid = auth::format_www_authenticate("handshake", "SHA-256", &data);
        let challenge = match options.challenge {
            Challenge::UnsupportedHash => valid.replace("SHA-256", "SHA-512"),
            Challenge::MissingHash => valid.replace("hash=SHA-256, ", ""),
            Challenge::DuplicateHash => format!("{valid}, hash=SHA-256"),
            Challenge::DuplicateToken => format!("{valid}, handshakeToken=duplicate"),
            Challenge::EmptyToken => valid.replace("handshakeToken=handshake", "handshakeToken="),
            Challenge::Malformed => {
                format!("SCRAM handshakeToken={SENTINEL}, hash=SHA-256, data=???")
            }
            Challenge::OversizedHeader => format!(
                "SCRAM handshakeToken={}, hash=SHA-256, data={data}",
                "x".repeat(8200)
            ),
            Challenge::MultipleScram => format!("{valid}\r\nWWW-Authenticate: {valid}"),
            Challenge::CombinedSchemes => format!("Basic realm=\"synthetic\", {valid}"),
            _ => valid,
        };
        let prefix = if matches!(options.challenge, Challenge::SeparateSchemes) {
            "WWW-Authenticate: Basic realm=synthetic\r\n"
        } else {
            ""
        };
        (401, format!("{prefix}WWW-Authenticate: {challenge}\r\n"))
    } else if authorization.starts_with("SCRAM ") {
        match auth::server_verify_final(
            state.handshake.as_ref().unwrap(),
            field(authorization, "data"),
        ) {
            Ok(sig) => {
                let sig = if matches!(options.final_message, Final::WrongSignature) {
                    vec![0; 32]
                } else {
                    sig
                };
                let data = BASE64.encode(format!("v={}", BASE64.encode(sig)));
                let valid = auth::format_auth_info("session-token", &data);
                let info = match options.final_message {
                    Final::WrongHash => format!("{valid}, hash=SHA-512"),
                    Final::DuplicateToken => format!("{valid}, authToken={SENTINEL}"),
                    Final::DuplicateHeader => format!("{valid}\r\nAuthentication-Info: {valid}"),
                    Final::EmptyToken => valid.replace("authToken=session-token", "authToken="),
                    Final::Malformed => format!(
                        "authToken={SENTINEL}, data={}",
                        BASE64.encode(format!("v={SENTINEL}"))
                    ),
                    _ => valid,
                };
                (200, format!("Authentication-Info: {info}\r\n"))
            }
            Err(_) => {
                body = SENTINEL.into();
                (401, String::new())
            }
        }
    } else {
        state.domain += 1;
        match options.domain {
            Domain::Good => (200, String::new()),
            Domain::Expired => {
                body = SENTINEL.into();
                (401, String::new())
            }
            Domain::Redirect if !request.contains("/redirect-target ") => {
                (307, "Location: /api/redirect-target\r\n".into())
            }
            Domain::Redirect => (200, String::new()),
            Domain::DropResponse => return None,
            Domain::ErrorBody => {
                body = SENTINEL.into();
                (500, String::new())
            }
            Domain::ErrorGrid => {
                body = format!("ver:\"3.0\" err dis:\"{SENTINEL}\"\nempty\n");
                (200, String::new())
            }
            Domain::InvalidGrid => {
                body = format!("{SENTINEL}\n");
                (200, String::new())
            }
        }
    };
    Some(format!(
        "HTTP/1.1 {status} Test\r\n{extra}Content-Type: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
}
