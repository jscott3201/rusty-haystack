//! Independent loopback peers assert the watch JSON contract and lifecycle.
use futures_util::{SinkExt, StreamExt};
use haystack_client::{
    ClientError, HaystackClient,
    transport::{Transport, ws::WsTransport},
};
use haystack_core::{
    data::{HCol, HDict, HGrid},
    kinds::{HRef, Kind},
};
use serde_json::{Value, json};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};
use tokio::{net::TcpListener, sync::oneshot};
use tokio_tungstenite::{accept_async, tungstenite::Message};

struct Peer {
    url: String,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}
impl Peer {
    fn start<F, Fut>(script: F) -> Self
    where
        F: FnOnce(TcpListener) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("ws://{}/api/ws", listener.local_addr().unwrap());
        let (stop, mut stopped) = oneshot::channel();
        let task = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = TcpListener::from_std(listener).unwrap();
                tokio::select! {
                    _ = &mut stopped => {},
                    result = tokio::time::timeout(Duration::from_secs(8), script(listener)) => result.expect("script bounded by fixture timeout"),
                }
            });
        });
        Self {
            url,
            stop: Some(stop),
            task: Some(task),
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let result = task.join();
            if !std::thread::panicking() {
                result.expect("scripted peer thread");
            }
        }
    }
}
fn subscription(ids: &[&str]) -> HGrid {
    let rows = ids
        .iter()
        .map(|id| {
            let mut row = HDict::new();
            row.set("id", Kind::Ref(HRef::from_val(*id)));
            row
        })
        .collect();
    HGrid::from_parts(HDict::new(), vec![HCol::new("id")], rows)
}

#[tokio::test]
async fn independent_peer_requires_flat_watch_json_and_typed_rows() {
    let peer = Peer::start(|listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let request = ws.next().await.unwrap().unwrap();
        let Message::Text(text) = request else {
            panic!("watch requests must be uncompressed text")
        };
        let request: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(request["op"], "watchSub");
        assert_eq!(request["ids"], json!(["point-1"]));
        assert!(request["reqId"].is_string());
        assert!(request.get("body").is_none());
        ws.send(Message::Text(json!({"reqId":request["reqId"], "watchId":"watch-1", "rows":[{"id":"r:point-1", "curVal":"n:72 °F", "dis":"s:工程", "site":"m:", "enabled":true, "missing":null, "nested":{"names":["工程","😀value"]}}]}).to_string().into())).await.unwrap();
        while let Some(Ok(message)) = ws.next().await {
            if message.is_close() {
                break;
            }
        }
    });
    let transport =
        WsTransport::connect_with_timeout(&peer.url, "synthetic-token", Duration::from_millis(250))
            .await
            .unwrap();
    let response = transport
        .call("watchSub", &subscription(&["point-1"]))
        .await
        .expect("typed watch response");
    assert_eq!(
        response.meta.get("watchId"),
        Some(&Kind::Str("watch-1".into()))
    );
    assert_eq!(response.rows[0].get("dis"), Some(&Kind::Str("工程".into())));
    assert!(
        matches!(response.rows[0].get("curVal"), Some(Kind::Number(n)) if n.val == 72.0 && n.unit.as_deref() == Some("°F"))
    );
    assert_eq!(response.rows[0].get("site"), Some(&Kind::Marker));
    let Some(Kind::Dict(nested)) = response.rows[0].get("nested") else {
        panic!("nested typed value")
    };
    assert_eq!(
        nested.get("names"),
        Some(&Kind::List(vec![
            Kind::Str("工程".into()),
            Kind::Str("😀value".into())
        ]))
    );
    transport.close().await.unwrap();
}

#[tokio::test]
async fn requests_larger_than_512_bytes_remain_text_and_round_trip() {
    let observed = Arc::new(Mutex::new(0));
    let shared = observed.clone();
    let peer = Peer::start(move |listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let Message::Text(text) = ws.next().await.unwrap().unwrap() else {
            panic!("large requests must be JSON text")
        };
        assert!(text.len() > 512);
        let request: Value = serde_json::from_str(&text).unwrap();
        *shared.lock().unwrap() = request["ids"].as_array().unwrap().len();
        ws.send(Message::Text(
            json!({"reqId":request["reqId"],"watchId":"large-watch","rows":[]})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        while let Some(Ok(message)) = ws.next().await {
            if message.is_close() {
                break;
            }
        }
    });
    let transport =
        WsTransport::connect_with_timeout(&peer.url, "synthetic-token", Duration::from_millis(250))
            .await
            .unwrap();
    let ids: Vec<_> = (0..100).map(|i| format!("synthetic-point-{i}")).collect();
    let refs: Vec<_> = ids.iter().map(String::as_str).collect();
    transport
        .call("watchSub", &subscription(&refs))
        .await
        .unwrap();
    assert_eq!(*observed.lock().unwrap(), 100);
    transport.close().await.unwrap();
}

async fn request(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> Value {
    let Message::Text(text) = ws.next().await.unwrap().unwrap() else {
        panic!("expected text request")
    };
    serde_json::from_str(&text).unwrap()
}
async fn reply(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>, value: Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn wait_close(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) {
    while let Some(Ok(message)) = ws.next().await {
        if message.is_close() {
            break;
        }
    }
}

#[tokio::test]
async fn overlapping_responses_correlate_out_of_order_and_pushes_are_separate() {
    let peer = Peer::start(|listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let first = request(&mut ws).await;
        let second = request(&mut ws).await;
        reply(
            &mut ws,
            json!({"reqId":"unknown","watchId":"unknown-watch","rows":[]}),
        )
        .await;
        reply(
            &mut ws,
            json!({"type":"push","watchId":"push-watch","rows":[{"value":"n:5"}]}),
        )
        .await;
        reply(&mut ws, json!({"reqId":second["reqId"],"error":"rejected"})).await;
        reply(
            &mut ws,
            json!({"reqId":first["reqId"],"watchId":first["ids"][0],"rows":[]}),
        )
        .await;
        reply(
            &mut ws,
            json!({"reqId":first["reqId"],"watchId":"duplicate","rows":[]}),
        )
        .await;
        wait_close(&mut ws).await;
    });
    let transport = WsTransport::connect_with_timeout(&peer.url, "token", Duration::from_secs(1))
        .await
        .unwrap();
    let first = subscription(&["first"]);
    let second = subscription(&["second"]);
    let (first, second) = tokio::join!(
        transport.call("watchSub", &first),
        transport.call("watchSub", &second)
    );
    assert_eq!(
        first.unwrap().meta.get("watchId"),
        Some(&Kind::Str("first".into()))
    );
    assert!(matches!(second, Err(ClientError::ServerError(_))));
    let push = transport.next_push().await.unwrap();
    assert_eq!(push.watch_id, "push-watch");
    assert_eq!(push.grid.rows.len(), 1);
    assert!(!transport.is_closed());
    transport.close().await.unwrap();
}

#[tokio::test]
async fn malformed_binary_and_oversized_messages_settle_all_pending_calls() {
    use tokio_tungstenite::tungstenite::protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    };
    let cases = vec![
        vec![Message::Text("not JSON".into())],
        vec![Message::Text(json!({"rows":[]}).to_string().into())],
        vec![Message::Text(
            json!({"reqId":"1","rows":[],"error":"ambiguous"})
                .to_string()
                .into(),
        )],
        vec![Message::Text(
            json!({"reqId":"1","watchId":"w","rows":[{"bad":"n:invalid"}]})
                .to_string()
                .into(),
        )],
        vec![Message::Binary(vec![0, 1, 2].into())],
        vec![Message::Text(" ".repeat(1024 * 1024 + 1).into())],
        vec![
            Message::Frame(Frame::message(
                vec![b' '; 600_000],
                OpCode::Data(Data::Text),
                false,
            )),
            Message::Frame(Frame::message(
                vec![b' '; 600_000],
                OpCode::Data(Data::Continue),
                true,
            )),
        ],
    ];
    for messages in cases {
        let peer = Peer::start(move |listener| async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = accept_async(stream).await.unwrap();
            request(&mut ws).await;
            request(&mut ws).await;
            for message in messages {
                if ws.send(message).await.is_err() {
                    break;
                }
            }
            wait_close(&mut ws).await;
        });
        let transport =
            WsTransport::connect_with_timeout(&peer.url, "token", Duration::from_secs(2))
                .await
                .unwrap();
        let grid = subscription(&["p"]);
        let results = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                transport.call("watchSub", &grid),
                transport.call("watchSub", &grid)
            )
        })
        .await
        .expect("every pending call must settle on protocol failure");
        assert!(results.0.is_err());
        assert!(results.1.is_err());
        assert!(transport.is_closed());
        assert!(transport.call("watchSub", &grid).await.is_err());
        transport.close().await.unwrap();
    }
}

#[tokio::test]
async fn abrupt_half_close_releases_all_pending_waiters() {
    use tokio::io::AsyncWriteExt;
    let peer = Peer::start(|listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        for _ in 0..3 {
            request(&mut ws).await;
        }
        ws.get_mut().shutdown().await.unwrap();
        wait_close(&mut ws).await;
    });
    let transport = WsTransport::connect_with_timeout(&peer.url, "token", Duration::from_secs(2))
        .await
        .unwrap();
    let grid = subscription(&["p"]);
    let results = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(
            transport.call("watchSub", &grid),
            transport.call("watchSub", &grid),
            transport.call("watchSub", &grid)
        )
    })
    .await
    .unwrap();
    assert!(results.0.is_err() && results.1.is_err() && results.2.is_err());
    transport.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_and_timed_out_calls_do_not_steal_later_responses() {
    let (seen_tx, seen_rx) = oneshot::channel();
    let peer = Peer::start(move |listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let cancelled = request(&mut ws).await;
        seen_tx.send(()).unwrap();
        let timed_out = request(&mut ws).await;
        let live = request(&mut ws).await;
        for old in [cancelled, timed_out] {
            reply(
                &mut ws,
                json!({"reqId":old["reqId"],"watchId":"late","rows":[]}),
            )
            .await;
        }
        reply(
            &mut ws,
            json!({"reqId":live["reqId"],"watchId":"live","rows":[]}),
        )
        .await;
        wait_close(&mut ws).await;
    });
    let transport = Arc::new(
        WsTransport::connect_with_timeout(&peer.url, "token", Duration::from_millis(100))
            .await
            .unwrap(),
    );
    let cloned = transport.clone();
    let cancelled =
        tokio::spawn(async move { cloned.call("watchSub", &subscription(&["cancel"])).await });
    seen_rx.await.unwrap();
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    assert!(matches!(
        transport
            .call("watchSub", &subscription(&["timeout"]))
            .await,
        Err(ClientError::Timeout(_))
    ));
    assert!(!transport.is_closed());
    let live = transport
        .call("watchSub", &subscription(&["live"]))
        .await
        .unwrap();
    assert_eq!(live.meta.get("watchId"), Some(&Kind::Str("live".into())));
    transport.close().await.unwrap();
}

#[tokio::test]
async fn push_queue_overflow_is_terminal() {
    let peer = Peer::start(|listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        request(&mut ws).await;
        for _ in 0..65 {
            reply(&mut ws, json!({"type":"push","watchId":"w","rows":[]})).await;
        }
        wait_close(&mut ws).await;
    });
    let transport = WsTransport::connect_with_timeout(&peer.url, "token", Duration::from_secs(1))
        .await
        .unwrap();
    assert!(matches!(
        transport.call("watchSub", &subscription(&["p"])).await,
        Err(ClientError::Transport(_))
    ));
    assert!(transport.next_push().await.is_err());
    transport.close().await.unwrap();
}

#[tokio::test]
async fn local_unsupported_arguments_never_reach_peer() {
    let peer = Peer::start(|listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        assert!(ws.next().await.unwrap().unwrap().is_close());
    });
    let transport = WsTransport::connect(&peer.url, "token").await.unwrap();
    let client = HaystackClient::from_transport(transport);
    assert!(client.call("read", &HGrid::new()).await.is_err());
    assert!(client.watch_sub(&["p"], Some("1min")).await.is_err());
    assert!(client.watch_sub(&[], None).await.is_err());
    assert!(client.watch_poll("").await.is_err());
    let mut req = subscription(&["p"]);
    req.meta.set("watchDis", Kind::Str("display".into()));
    assert!(client.call("watchSub", &req).await.is_err());
    client.close().await.unwrap();
}

#[tokio::test]
async fn reconnect_does_not_replay_an_uncertain_operation_and_close_is_permanent() {
    use haystack_client::transport::ws::ReconnectingWsTransport;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let peer = Peer::start(move |listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        request(&mut ws).await;
        observed.fetch_add(1, Ordering::SeqCst);
        drop(ws);
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let next = request(&mut ws).await;
        observed.fetch_add(1, Ordering::SeqCst);
        reply(
            &mut ws,
            json!({"reqId":next["reqId"],"watchId":"new","rows":[]}),
        )
        .await;
        wait_close(&mut ws).await;
    });
    let transport =
        ReconnectingWsTransport::connect_with_timeout(&peer.url, "token", Duration::from_secs(1))
            .await
            .unwrap();
    assert!(
        transport
            .call("watchSub", &subscription(&["uncertain"]))
            .await
            .is_err()
    );
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "the first operation must not be replayed"
    );
    transport
        .call("watchSub", &subscription(&["explicit-new-call"]))
        .await
        .unwrap();
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    transport.close().await.unwrap();
    assert!(matches!(
        transport.call("watchSub", &subscription(&["closed"])).await,
        Err(ClientError::ConnectionClosed)
    ));
    assert_eq!(effects.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn close_settles_pending_calls_and_drop_releases_the_socket() {
    let (seen_tx, seen_rx) = oneshot::channel();
    let (closed_tx, closed_rx) = oneshot::channel();
    let peer = Peer::start(move |listener| async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        request(&mut ws).await;
        request(&mut ws).await;
        seen_tx.send(()).unwrap();
        wait_close(&mut ws).await;
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        wait_close(&mut ws).await;
        closed_tx.send(()).unwrap();
    });
    let transport = Arc::new(WsTransport::connect(&peer.url, "token").await.unwrap());
    let first = transport.clone();
    let first = tokio::spawn(async move { first.call("watchSub", &subscription(&["p"])).await });
    let second = transport.clone();
    let second = tokio::spawn(async move { second.call("watchSub", &subscription(&["q"])).await });
    seen_rx.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), transport.close())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first.await.unwrap(),
        Err(ClientError::ConnectionClosed)
    ));
    assert!(matches!(
        second.await.unwrap(),
        Err(ClientError::ConnectionClosed)
    ));
    let dropped = WsTransport::connect(&peer.url, "token").await.unwrap();
    drop(dropped);
    tokio::time::timeout(Duration::from_secs(1), closed_rx)
        .await
        .unwrap()
        .unwrap();
}
