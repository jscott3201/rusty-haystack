//! Real first-party HTTP authentication plus watch subscribe/poll/unsubscribe.
use haystack_client::HaystackClient;
use haystack_core::{
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, Number},
};
use haystack_server::{
    HaystackServer,
    auth::{
        AuthManager,
        users::{UserRecord, parse_password_hash},
    },
};
use std::{collections::HashMap, sync::mpsc, thread::JoinHandle, time::Duration};
use tokio::sync::oneshot;

struct Server {
    url: String,
    ws_url: String,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}
impl Server {
    fn start(graph: SharedGraph) -> Self {
        let users=HashMap::from([("user".into(),UserRecord { credentials: parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap(), permissions:vec!["read".into()] })]);
        let server = HaystackServer::new(graph)
            .with_auth(AuthManager::new(users, Duration::from_secs(60)))
            .port(0);
        let (stop, mut stopped) = oneshot::channel();
        let (address_tx, address_rx) = mpsc::channel();
        let task = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move { tokio::select! { _=&mut stopped=>{}, result=server.run_reporting_addr(move|a|address_tx.send(a).unwrap())=>result.unwrap(), } });
        });
        let address = address_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        Self {
            url: format!("http://{address}/api"),
            ws_url: format!("ws://{address}/api/ws"),
            stop: Some(stop),
            task: Some(task),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let r = task.join();
            if !std::thread::panicking() {
                r.unwrap()
            }
        }
    }
}

#[tokio::test]
async fn first_party_watch_exchange_preserves_types_and_large_values() {
    let graph = SharedGraph::new(EntityGraph::new());
    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("point-1")));
    entity.set("point", Kind::Marker);
    entity.set("dis", Kind::Str("工程".repeat(300)));
    entity.set("curVal", Kind::Number(Number::unitless(72.0)));
    graph.add(entity).unwrap();
    let server = Server::start(graph.clone());
    let client = HaystackClient::connect_ws(&server.url, &server.ws_url, "user", "pencil")
        .await
        .unwrap();
    let initial = tokio::time::timeout(
        Duration::from_millis(500),
        client.watch_sub(&["point-1"], None),
    )
    .await
    .expect("subscription must settle")
    .unwrap();
    assert_eq!(initial.rows.len(), 1);
    assert_eq!(
        initial.rows[0].get("dis"),
        Some(&Kind::Str("工程".repeat(300)))
    );
    let Some(Kind::Str(watch)) = initial.meta.get("watchId") else {
        panic!("watch ID required")
    };
    let mut change = HDict::new();
    change.set("curVal", Kind::Number(Number::unitless(75.0)));
    graph.update("point-1", change).unwrap();
    let polled = client.watch_poll(watch).await.unwrap();
    assert_eq!(polled.rows.len(), 1);
    assert_eq!(
        polled.rows[0].get("curVal"),
        Some(&Kind::Number(Number::unitless(75.0)))
    );
    let push = tokio::time::timeout(Duration::from_secs(2), client.next_watch_push())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(push.watch_id, *watch);
    assert_eq!(
        push.grid.rows[0].get("curVal"),
        Some(&Kind::Number(Number::unitless(75.0)))
    );
    let (first, second) = tokio::join!(client.watch_poll(watch), client.watch_poll(watch));
    assert!(first.unwrap().rows.is_empty());
    assert!(second.unwrap().rows.is_empty());
    client.watch_unsub(watch, &[]).await.unwrap();
    assert!(client.watch_poll(watch).await.is_err());
    client.close().await.unwrap();
}

async fn raw_client(
    server: &Server,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let client = haystack_client::ClientConfig::default()
        .build_reqwest_client()
        .unwrap();
    let token = haystack_client::auth::authenticate(&client, &server.url, "user", "pencil")
        .await
        .unwrap();
    let mut request = server.ws_url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("BEARER authToken={token}").parse().unwrap(),
    );
    tokio_tungstenite::connect_async(request).await.unwrap().0
}

#[tokio::test]
async fn server_rejects_malformed_binary_and_oversized_messages() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{
        Message,
        protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        },
    };
    let server = Server::start(SharedGraph::new(EntityGraph::new()));
    let cases = vec![
        vec![Message::Text("{".into())],
        vec![Message::Text(r#"{"op":"watchSub","ids":["p"]}"#.into())],
        vec![Message::Text(
            r#"{"op":"watchSub","reqId":1,"ids":["p"]}"#.into(),
        )],
        vec![Message::Text(
            r#"{"op":"watchSub","reqId":"","ids":["p"]}"#.into(),
        )],
        vec![Message::Text(
            r#"{"op":"watchSub","reqId":"1","ids":["p"],"watchDis":"ignored?"}"#.into(),
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
        let mut ws = raw_client(&server).await;
        for message in messages {
            if ws.send(message).await.is_err() {
                break;
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .expect("invalid input must close promptly");
        assert!(
            matches!(result, None | Some(Err(_)) | Some(Ok(Message::Close(_)))),
            "unexpected live response: {result:?}"
        );
    }
}

#[tokio::test]
async fn server_correlates_unsupported_operations_and_invalid_arguments() {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::Message;
    let server = Server::start(SharedGraph::new(EntityGraph::new()));
    let mut ws = raw_client(&server).await;
    let requests = vec![
        json!({"reqId":"1","op":"read"}),
        json!({"reqId":"2","op":"watchSub","ids":["@"]}),
        json!({"reqId":"3","op":"watchSub","watchId":"existing","ids":["p"]}),
        json!({"reqId":"4","op":"watchPoll","watchId":"w","ids":[]}),
        json!({"reqId":"5","op":"watchSub","ids":vec!["p";1001]}),
    ];
    for request in requests {
        ws.send(Message::Text(request.to_string().into()))
            .await
            .unwrap();
        let Message::Text(text) = tokio::time::timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
        else {
            panic!("expected correlated error")
        };
        let response: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(response["reqId"], request["reqId"]);
        assert!(response["error"].is_string());
        assert!(response.get("rows").is_none());
    }
    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn server_oversized_response_closes_without_sending_rows() {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message;
    let graph = SharedGraph::new(EntityGraph::new());
    let mut entity = HDict::new();
    entity.set("id", Kind::Ref(HRef::from_val("large")));
    entity.set("dis", Kind::Str("x".repeat(1024 * 1024)));
    graph.add(entity).unwrap();
    let server = Server::start(graph);
    let mut ws = raw_client(&server).await;
    ws.send(Message::Text(
        json!({"reqId":"1","op":"watchSub","ids":["large"]})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), ws.next())
        .await
        .unwrap();
    assert!(matches!(
        result,
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None
    ));
}

fn graph_with_prefixed_ids() -> SharedGraph {
    let graph = SharedGraph::new(EntityGraph::new());
    for id in ["@point-1", "point-1"] {
        let mut entity = HDict::new();
        entity.set("id", Kind::Ref(HRef::from_val(id)));
        entity.set("curVal", Kind::Number(Number::unitless(1.0)));
        graph.add(entity).unwrap();
    }
    graph
}
#[tokio::test]
async fn first_party_repeated_prefix_subscription_normalizes_once() {
    let server = Server::start(graph_with_prefixed_ids());
    let client = HaystackClient::connect_ws(&server.url, &server.ws_url, "user", "pencil")
        .await
        .unwrap();
    let grid = client.watch_sub(&["@@point-1"], None).await.unwrap();
    assert_eq!(grid.rows.len(), 1);
    assert_eq!(
        grid.rows[0].get("id"),
        Some(&Kind::Ref(HRef::from_val("@point-1")))
    );
    client.close().await.unwrap();
}
#[tokio::test]
async fn first_party_repeated_prefix_selective_unsubscribe_normalizes_once() {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio_tungstenite::tungstenite::Message;
    let graph = graph_with_prefixed_ids();
    let server = Server::start(graph.clone());
    // An independent request establishes both IDs so the unsubscribe assertion
    // cannot be masked by a matching bug in first-party subscription encoding.
    let mut raw = raw_client(&server).await;
    raw.send(Message::Text(
        json!({"op":"watchSub","reqId":"setup","ids":["@@point-1","point-1"]})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let Message::Text(text) = raw.next().await.unwrap().unwrap() else {
        panic!("subscription response")
    };
    let subscribed: Value = serde_json::from_str(&text).unwrap();
    let watch = subscribed["watchId"].as_str().unwrap();
    let client = HaystackClient::connect_ws(&server.url, &server.ws_url, "user", "pencil")
        .await
        .unwrap();
    client.watch_unsub(watch, &["@@point-1"]).await.unwrap();
    for id in ["@point-1", "point-1"] {
        let mut changes = HDict::new();
        changes.set("curVal", Kind::Number(Number::unitless(2.0)));
        graph.update(id, changes).unwrap();
    }
    let remaining = client.watch_poll(watch).await.unwrap();
    assert_eq!(remaining.rows.len(), 1);
    assert_eq!(
        remaining.rows[0].get("id"),
        Some(&Kind::Ref(HRef::from_val("point-1")))
    );
    client.close().await.unwrap();
}
