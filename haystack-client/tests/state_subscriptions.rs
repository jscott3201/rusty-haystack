//! Independent HTTP peers exercise at-most-once dispatch and strict outcomes.
use haystack_client::{HaystackClient, transport::http::HttpTransport};
use haystack_core::{
    codecs::{codec_for, subscription as wire},
    data::HGrid,
};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn request() -> wire::SubscriptionRequest {
    wire::SubscriptionRequest::Create(wire::SubscriptionCreate {
        authority: [1; 16],
        key: "known-create".into(),
        ids: vec!["point".into()],
        lease_ms: 1000,
    })
}
#[tokio::test]
async fn unreadable_unmatched_oversized_and_redirected_responses_never_replay_http_creation() {
    for bad in 0..7 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                bytes.extend_from_slice(&chunk[..read]);
                assert!(bytes.len() < 65_536);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let header = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let outcome =
                wire::SubscriptionOutcome::Delivery(Arc::new(wire::SubscriptionDelivery {
                    watch: wire::SubscriptionId {
                        authority: [1; 16],
                        watch: [2; 16],
                        key: if bad == 3 {
                            "wrong-create".into()
                        } else {
                            "known-create".into()
                        },
                    },
                    dataset: [3; 16],
                    incarnation: [4; 16],
                    catalog_generation: 1,
                    scope_generation: 1,
                    token: [5; 16],
                    from: 1,
                    through: 1,
                    initial: true,
                    rows: vec![],
                    removed: vec![],
                }));
            let mut grid = if bad == 1 {
                HGrid::new()
            } else {
                wire::to_grid(&outcome).unwrap()
            };
            if bad == 2 {
                grid.meta.set("err", haystack_core::kinds::Kind::Marker);
            }
            let mut body = codec_for("text/zinc").unwrap().encode_grid(&grid).unwrap();
            if bad == 0 {
                body = "truncated".into();
            }
            let length = if bad == 0 {
                body.len() + 100
            } else if bad == 5 {
                wire::MAX_WIRE_BYTES + 1
            } else {
                body.len()
            };
            let mime = if bad == 4 {
                "application/json"
            } else {
                "text/zinc"
            };
            let response = if bad == 6 {
                format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{address}/api/watchSub\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}"
                )
            };
            let _ = socket.write_all(response.as_bytes()).await;
            drop(socket);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "first-party transport must not replay or follow a redirect"
            );
        });
        let client = HaystackClient::from_transport(HttpTransport::new(
            &format!("http://{address}/api"),
            "fixture-token".into(),
        ));
        assert_eq!(
            client.state_subscription(&request()).await.unwrap(),
            wire::SubscriptionOutcome::Unknown,
            "case {bad}"
        );
        peer.await.unwrap();
    }
}
