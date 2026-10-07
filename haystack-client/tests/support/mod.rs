//! Synthetic loopback server with no external services or certificate files.
//! A dedicated runtime thread owns all sockets. Drop signals shutdown and joins
//! that thread even if an assertion unwinds the test. Each response closes TLS.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD as OUTER},
};
use haystack_client::tls::TlsConfig;
use hmac::{Hmac, KeyInit, Mac};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
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
    NonExtendedNonce,
    InvalidUtf8,
    MultipleScram,
    CombinedSchemes,
    SeparateSchemes,
    BareSchemeAfterScram,
}
#[derive(Clone, Copy, Default, Debug)]
pub enum Final {
    #[default]
    Valid,
    WrongSignature,
    WrongHash,
    MissingHash,
    ShortSignature,
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
#[derive(Clone, Copy, Default)]
pub enum Tokens {
    #[default]
    Rotate,
    Absent,
    Introduce,
    Remove,
}
impl Tokens {
    fn first(self) -> Option<&'static str> {
        match self {
            Self::Rotate | Self::Remove => Some("discovery-token"),
            _ => None,
        }
    }
    fn final_token(self) -> Option<&'static str> {
        match self {
            Self::Rotate | Self::Introduce => Some("proof-token"),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Default, Debug)]
pub enum EmptyMembers {
    #[default]
    None,
    Leading,
    Interior,
    Trailing,
}
impl EmptyMembers {
    fn apply(self, value: String) -> String {
        match self {
            Self::None => value,
            Self::Leading => format!(", , {}", value.replacen("SCRAM ", "SCRAM , ", 1)),
            Self::Interior => value.replacen(',', ", ,", 1),
            Self::Trailing => format!("{value}, ,"),
        }
    }
}
#[derive(Default, Clone, Copy)]
pub struct Options {
    pub challenge: Challenge,
    pub discovery_empty: EmptyMembers,
    pub challenge_empty: EmptyMembers,
    pub tokens: Tokens,
    pub final_message: Final,
    pub domain: Domain,
    pub auth_delay: Duration,
}
#[derive(Default)]
pub struct State {
    transcript: Option<String>,
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
// Independent Python stdlib derivation: pbkdf2_hmac('sha256', b'password',
// b'test-only-salt', 4096), then HMAC with b'Client Key' / b'Server Key'.
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
        assert_eq!(authorization, "HELLO username=dXNlcg");
        let token = options
            .tokens
            .first()
            .map(|t| format!(", handshakeToken={t}"))
            .unwrap_or_default();
        (
            401,
            format!(
                "WWW-Authenticate: {}\r\n",
                options
                    .discovery_empty
                    .apply(format!("SCRAM hash=SHA-256{token}"))
            ),
        )
    } else if authorization.starts_with("SCRAM ") && state.transcript.is_none() {
        assert_eq!(
            optional_field(authorization, "handshakeToken"),
            options.tokens.first()
        );
        let first = String::from_utf8(OUTER.decode(field(authorization, "data")).unwrap()).unwrap();
        let bare = first.strip_prefix("n,,").unwrap();
        let nonce = bare.strip_prefix("n=user,r=").unwrap();
        let server_first = format!("r={nonce}server-nonce,s=dGVzdC1vbmx5LXNhbHQ=,i=4096");
        state.transcript = Some(format!(
            "{bare},{server_first},c=biws,r={nonce}server-nonce"
        ));
        let mut data = OUTER.encode(server_first);
        data = match options.challenge {
            Challenge::OversizedData => OUTER.encode("r=".to_string() + &"x".repeat(4200)),
            Challenge::DuplicateNonce => {
                OUTER.encode(format!("r={nonce}extra,r={nonce}extra,s=c2FsdA==,i=4096"))
            }
            Challenge::EmptySalt => OUTER.encode(format!("r={nonce}extra,s=,i=4096")),
            Challenge::ZeroIterations => OUTER.encode(format!("r={nonce}extra,s=c2FsdA==,i=0")),
            Challenge::ExcessiveIterations => {
                OUTER.encode(format!("r={nonce}extra,s=c2FsdA==,i=1000001"))
            }
            Challenge::NonExtendedNonce => OUTER.encode(format!("r={nonce},s=c2FsdA==,i=4096")),
            Challenge::InvalidUtf8 => OUTER.encode([255]),
            Challenge::WrongNonce => OUTER.encode("r=wrong,s=c2FsdA==,i=4096"),
            _ => data,
        };
        let token = options
            .tokens
            .final_token()
            .map(|t| format!(", handshakeToken={t}"))
            .unwrap_or_default();
        let valid = format!("SCRAM hash=SHA-256{token}, data={data}");
        let challenge = match options.challenge {
            Challenge::UnsupportedHash => valid.replace("SHA-256", "SHA-512"),
            Challenge::MissingHash => valid.replace("hash=SHA-256, ", ""),
            Challenge::DuplicateHash => format!("{valid}, hash=SHA-256"),
            Challenge::DuplicateToken => format!("{valid}, handshakeToken=duplicate"),
            Challenge::EmptyToken => valid.replace("handshakeToken=proof-token", "handshakeToken="),
            Challenge::Malformed => {
                format!("SCRAM handshakeToken={SENTINEL}, hash=SHA-256, data=???")
            }
            Challenge::OversizedHeader => format!(
                "SCRAM handshakeToken={}, hash=SHA-256, data={data}",
                "x".repeat(8200)
            ),
            Challenge::MultipleScram => format!("{valid}\r\nWWW-Authenticate: {valid}"),
            Challenge::BareSchemeAfterScram => format!("{valid}, Negotiate"),
            Challenge::CombinedSchemes => format!("Basic realm=\"synthetic\", {valid}"),
            _ => valid,
        };
        let challenge = options.challenge_empty.apply(challenge);
        let prefix = if matches!(options.challenge, Challenge::SeparateSchemes) {
            "WWW-Authenticate: Basic realm=synthetic\r\n"
        } else {
            ""
        };
        (401, format!("{prefix}WWW-Authenticate: {challenge}\r\n"))
    } else if authorization.starts_with("SCRAM ") {
        assert_eq!(
            optional_field(authorization, "handshakeToken"),
            options.tokens.final_token()
        );
        let final_text =
            String::from_utf8(OUTER.decode(field(authorization, "data")).unwrap()).unwrap();
        let (without, proof) = final_text.rsplit_once(",p=").unwrap();
        let transcript = state.transcript.as_ref().unwrap();
        assert!(transcript.ends_with(without));
        let proof = BASE64.decode(proof).unwrap();
        let client_key = BASE64.decode(CLIENT_KEY).unwrap();
        let stored = Sha256::digest(&client_key);
        let signature = mac(&stored, transcript.as_bytes());
        let expected: Vec<_> = client_key
            .iter()
            .zip(signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        if proof == expected {
            let sig = match options.final_message {
                Final::WrongSignature => vec![0; 32],
                Final::ShortSignature => vec![0; 1],
                _ => mac(&BASE64.decode(SERVER_KEY).unwrap(), transcript.as_bytes()),
            };
            let data = OUTER.encode(format!("v={}", BASE64.encode(sig)));
            let valid = format!("authToken=session-token, hash=SHA-256, data={data}");
            let info = match options.final_message {
                Final::WrongHash => valid.replace("SHA-256", "SHA-512"),
                Final::MissingHash => valid.replace("hash=SHA-256, ", ""),
                Final::DuplicateToken => format!("{valid}, authToken={SENTINEL}"),
                Final::DuplicateHeader => format!("{valid}\r\nAuthentication-Info: {valid}"),
                Final::EmptyToken => valid.replace("authToken=session-token", "authToken="),
                Final::Malformed => format!(
                    "authToken={SENTINEL}, hash=SHA-256, data={}",
                    OUTER.encode(format!("v={SENTINEL}"))
                ),
                _ => valid,
            };
            (200, format!("Authentication-Info: {info}\r\n"))
        } else {
            body = SENTINEL.into();
            (403, String::new())
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

const CLIENT_KEY: &str = "miBLs3zHo+BuurNPsetfCUPwbVy1xni0Bd32xTiS3z4=";

const SERVER_KEY: &str = "3a7YdMwBdzDbvbjbuV1MmTZ5BgUGyEfjMa4yz/c8e7w=";

fn mac(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256>>::new_from_slice(key).unwrap();
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}
fn optional_field<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split([',', ' ']).find_map(|s| {
        s.split_once('=')
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| v)
    })
}
