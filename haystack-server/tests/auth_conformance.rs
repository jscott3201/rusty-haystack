//! Independent raw HTTP client drives the actual router. Proof arithmetic uses
//! fixed Python-stdlib-derived keys, not the production SCRAM helpers.
use base64::{
    Engine,
    engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as OUTER},
};
use haystack_client::HaystackClient;
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_server::{
    HaystackServer,
    auth::{
        AuthLimits, AuthManager,
        users::{UserRecord, parse_password_hash},
    },
};
use hmac::{Hmac, KeyInit, Mac};
use std::{collections::HashMap, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
};

struct Server {
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(limits: AuthLimits) -> Self {
        let users = ["user", "josé,=工"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    UserRecord {
                        credentials: parse_password_hash(HASH).unwrap(),
                        permissions: vec!["read".into()],
                    },
                )
            })
            .collect();
        let manager = AuthManager::new(users, Duration::from_secs(3600)).with_limits(limits);
        let server = HaystackServer::new(SharedGraph::new(EntityGraph::new()))
            .with_auth(manager)
            .port(0);
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(server.run_reporting_addr(move |address| {
            let _ = tx.send(address);
        }));
        Self {
            address: rx.await.unwrap(),
            task,
        }
    }
    fn url(&self) -> String {
        format!("http://{}/api", self.address)
    }
    async fn request(&self, headers: &str) -> Response {
        let mut stream = TcpStream::connect(self.address).await.unwrap();
        let request = format!(
            "GET /api/about HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{headers}\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let (headers, body) = text.split_once("\r\n\r\n").unwrap();
        let status = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = headers
            .lines()
            .skip(1)
            .map(|line| {
                let (k, v) = line.split_once(':').unwrap();
                (k.to_ascii_lowercase(), v.trim().to_string())
            })
            .collect();
        Response {
            status,
            headers,
            body: body.into(),
        }
    }
    async fn auth(&self, value: &str) -> Response {
        self.request(&format!("Authorization: {value}\r\n")).await
    }
}
struct Response {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}
fn fields(value: &str) -> HashMap<&str, &str> {
    value
        .strip_prefix("SCRAM ")
        .unwrap_or(value)
        .split(',')
        .map(|f| f.trim().split_once('=').unwrap())
        .collect()
}
fn mac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut h = <Hmac<sha2::Sha256>>::new_from_slice(key).unwrap();
    h.update(data);
    h.finalize().into_bytes().to_vec()
}

#[tokio::test]
async fn independent_raw_peer_observes_three_requests_and_exact_headers() {
    let server = Server::start(AuthLimits::default()).await;
    assert_eq!(server.request("").await.status, 401);
    for name in ["user", "josé,=工"] {
        let hello = server
            .auth(&format!("HELLO username={}", OUTER.encode(name)))
            .await;
        assert_eq!(hello.status, 401);
        let discovery = fields(&hello.headers["www-authenticate"]);
        assert_eq!(discovery["hash"], "SHA-256");
        assert!(!discovery.contains_key("data"));
        let bare = format!(
            "n={},r=fixed-independent-client-nonce",
            name.replace('=', "=3D").replace(',', "=2C")
        );
        let data = OUTER.encode(format!("n,,{bare}"));
        let challenge = server
            .auth(&format!(
                "SCRAM handshakeToken={}, data={data}",
                discovery["handshakeToken"]
            ))
            .await;
        assert_eq!(challenge.status, 401);
        let challenge = fields(&challenge.headers["www-authenticate"]);
        assert_eq!(challenge["hash"], "SHA-256");
        assert_ne!(challenge["handshakeToken"], discovery["handshakeToken"]);
        let first = String::from_utf8(OUTER.decode(challenge["data"]).unwrap()).unwrap();
        let parts = fields(&first);
        assert_eq!(parts["s"], "W22ZaJ0SNY7soEsUEjb6gQ==");
        assert_eq!(parts["i"], "4096");
        assert!(parts["r"].starts_with("fixed-independent-client-nonce"));
        assert!(parts["r"].len() > "fixed-independent-client-nonce".len());
        let final_without = format!("c=biws,r={}", parts["r"]);
        let transcript = format!("{bare},{first},{final_without}");
        let sig = mac(&B64.decode(STORED_KEY).unwrap(), transcript.as_bytes());
        let key = B64.decode(CLIENT_KEY).unwrap();
        let proof: Vec<_> = key.iter().zip(sig.iter()).map(|(a, b)| a ^ b).collect();
        let data = OUTER.encode(format!("{final_without},p={}", B64.encode(proof)));
        let header = format!(
            "SCRAM handshakeToken={}, data={data}",
            challenge["handshakeToken"]
        );
        let final_response = server.auth(&header).await;
        assert_eq!(final_response.status, 200);
        assert!(!final_response.headers.contains_key("www-authenticate"));
        let info = fields(&final_response.headers["authentication-info"]);
        assert_eq!(info["hash"], "SHA-256");
        let expected = format!(
            "v={}",
            B64.encode(mac(&B64.decode(SERVER_KEY).unwrap(), transcript.as_bytes()))
        );
        assert_eq!(OUTER.decode(info["data"]).unwrap(), expected.as_bytes());
        assert_eq!(
            server
                .auth(&format!("BEARER authToken={}", info["authToken"]))
                .await
                .status,
            200
        );
        assert_eq!(server.auth(&header).await.status, 403);
    }
}

#[tokio::test]
async fn server_rejects_malformed_auth_and_reports_capacity_without_secrets() {
    let server = Server::start(AuthLimits {
        max_handshakes: 1,
        ..AuthLimits::default()
    })
    .await;
    for value in [
        "HELLO username=dXNlcg==",
        "HELLO username=dXNlcg, data=bg",
        "HELLO username=dXNlcg, username=dXNlcg",
        "SCRAM data=_w",
        "SCRAM hash=SHA-512, data=bg",
        "SCRAM handshakeToken=private-sentinel, data=bg",
    ] {
        let response = server.auth(value).await;
        assert_eq!(response.status, 403, "{value}");
        assert!(!response.body.contains("private-sentinel"));
    }
    assert_eq!(
        server
            .request(
                "Authorization: HELLO username=dXNlcg\r\nAuthorization: HELLO username=dXNlcg\r\n"
            )
            .await
            .status,
        403
    );
    assert_eq!(
        server
            .auth(&format!("SCRAM data={}", "a".repeat(9000)))
            .await
            .status,
        403
    );
    assert_eq!(server.auth("HELLO username=dXNlcg").await.status, 401);
    assert_eq!(server.auth("HELLO username=dXNlcg").await.status, 503);
}

#[tokio::test]
async fn first_party_http_and_websocket_upgrade_share_conformant_authentication() {
    let server = Server::start(AuthLimits::default()).await;
    let client = HaystackClient::connect(&server.url(), "user", "pencil")
        .await
        .unwrap();
    client
        .read("site", None)
        .await
        .expect("protected HTTP operation after three request SCRAM");
    let ws_url = format!("ws://{}/api/ws", server.address);
    // The real router requires a valid bearer for upgrade. Returning from this
    // constructor proves HTTP authentication supplied that bearer to WS.
    let websocket = HaystackClient::connect_ws(&server.url(), &ws_url, "user", "pencil")
        .await
        .expect("protected WS upgrade after shared HTTP SCRAM");
    drop(websocket);
}

// Fixed keys derived with Python hashlib.pbkdf2_hmac and hmac.digest.
const HASH: &str = "W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=";
const CLIENT_KEY: &str = "pg/JI9Z+hkSpLRa5btpe9GVrDHJcSEN0viVTVXaZbos=";
const STORED_KEY: &str = "WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=";
const SERVER_KEY: &str = "wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=";
