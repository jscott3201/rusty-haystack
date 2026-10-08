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

#[tokio::test]
async fn review_malformed_raw_json_creation_and_acknowledgement_never_replay() {
    for v3 in [false, true] {
        for acknowledgement in [false, true] {
            for bad in 0..if v3 { 3 } else { 4 } {
                let watch = wire::SubscriptionId {
                    authority: [1; 16],
                    watch: [2; 16],
                    key: "known-create".into(),
                };
                let submitted = if acknowledgement {
                    wire::SubscriptionRequest::Acknowledge {
                        watch: watch.clone(),
                        scope_generation: 1,
                        token: [5; 16],
                        through: 7,
                    }
                } else {
                    request()
                };
                let outcome = if acknowledgement {
                    wire::SubscriptionOutcome::Acknowledged {
                        watch,
                        scope_generation: 1,
                        token: [5; 16],
                        through: 7,
                    }
                } else {
                    wire::SubscriptionOutcome::Delivery(Arc::new(wire::SubscriptionDelivery {
                        watch,
                        dataset: [3; 16],
                        incarnation: [4; 16],
                        catalog_generation: 1,
                        scope_generation: 1,
                        token: [5; 16],
                        from: 7,
                        through: 7,
                        initial: true,
                        rows: vec![],
                        removed: vec![],
                    }))
                };
                let payload = String::from_utf8(wire::encode(&outcome).unwrap()).unwrap();
                let payload =
                    serde_json::to_string(&format!("{}{}", if v3 { "s:" } else { "" }, payload))
                        .unwrap();
                let meta = if v3 {
                    r#""meta":{"ver":"3.0"},"#
                } else {
                    r#""_kind":"grid","meta":{},"#
                };
                let valid = format!(
                    r#"{{{meta}"cols":[{{"name":"payload"}}],"rows":[{{"payload":{payload}}}]}}"#
                );
                let body = match bad {
                    0 => valid.replace(r#""payload":"#, r#""payload":null,"payload":"#),
                    1 => valid.replace(r#""meta":{"#, r#""meta":null,"meta":{"#),
                    2 => valid.replace(
                        if v3 {
                            r#""meta":{"ver":"3.0"}"#
                        } else {
                            r#""meta":{}"#
                        },
                        r#""meta":null"#,
                    ),
                    _ => valid.replace(r#""name":"payload""#, r#""name":"payload","meta":null"#),
                };
                let mime = if v3 {
                    "application/json;v=3"
                } else {
                    "application/json"
                };
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let peer = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 4096];
                    loop {
                        let count = socket.read(&mut chunk).await.unwrap();
                        assert_ne!(count, 0);
                        bytes.extend_from_slice(&chunk[..count]);
                        assert!(bytes.len() < 65_536);
                        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
                        {
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
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(response.as_bytes()).await.unwrap();
                    drop(socket);
                    assert!(
                        tokio::time::timeout(Duration::from_millis(100), listener.accept())
                            .await
                            .is_err(),
                        "malformed JSON response must not cause replay"
                    );
                });
                let client = HaystackClient::from_transport(HttpTransport::with_format(
                    &format!("http://{address}/api"),
                    "fixture-token".into(),
                    mime,
                ));
                assert_eq!(
                    client.state_subscription(&submitted).await.unwrap(),
                    wire::SubscriptionOutcome::Unknown,
                    "v3={v3} ack={acknowledgement} case={bad}"
                );
                peer.await.unwrap();
            }
        }
    }
}
