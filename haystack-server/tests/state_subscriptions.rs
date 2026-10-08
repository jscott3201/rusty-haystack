//! Real scoped HTTP/WS/native sharing and independent session lifetime evidence.
use futures_util::{SinkExt, StreamExt};
use haystack_app::*;
use haystack_client::{HaystackClient, transport::http::HttpTransport};
use haystack_core::{
    codecs::subscription as wire,
    data::HDict,
    graph::{EntityGraph, SharedGraph},
    kinds::{HRef, Kind, Number},
};
use haystack_server::{
    HaystackServer,
    auth::{
        AuthManager, AuthUser,
        users::{UserRecord, parse_password_hash},
    },
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
fn entity(value: f64) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val("point")));
    row.set("value", Kind::Number(Number::unitless(value)));
    row
}
fn user() -> AuthUser {
    AuthUser {
        username: "user".into(),
        permissions: vec!["read".into()],
    }
}
struct Running {
    url: String,
    ws: String,
    owner: ApplicationOwner,
    service: StateSubscriptionService,
    session: SubscriptionSession,
    graph: SharedGraph,
}
impl Running {
    async fn start(ttl: Duration, policy: Arc<dyn ReadPolicy>) -> Self {
        let graph = SharedGraph::new(EntityGraph::with_changelog_capacity(8));
        graph.add(entity(1.0)).unwrap();
        let builder =
            ApplicationBuilder::new(graph.clone(), policy, ReadLimits::default()).unwrap();
        let service = StateSubscriptionService::new(
            builder.handle().read_service(),
            EphemeralMutationStore::new(graph.clone()),
            SubscriptionLimits::default(),
        )
        .unwrap();
        let builder = builder.state_subscriptions(service.clone()).unwrap();
        let credentials=parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap();
        let auth = AuthManager::new(
            HashMap::from([(
                "user".into(),
                UserRecord {
                    credentials,
                    permissions: vec!["read".into()],
                },
            )]),
            ttl,
        );
        auth.inject_token("session-a".into(), user());
        auth.inject_token("session-b".into(), user());
        let session = auth.validate_session("session-a").unwrap().1;
        let listener = HaystackServer::new(graph.clone())
            .with_scoped_reads(builder.handle())
            .with_auth(auth)
            .port(0)
            .into_listener();
        let owner = builder
            .owned_resource(listener)
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let address = owner.ready().await.unwrap().listeners[0].address;
        Self {
            url: format!("http://{address}/api"),
            ws: format!("ws://{address}/api/ws"),
            owner,
            service,
            session,
            graph,
        }
    }
    fn http(&self, token: &str, format: &str) -> HaystackClient<HttpTransport> {
        HaystackClient::from_transport(HttpTransport::with_format(&self.url, token.into(), format))
    }
    async fn stop(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
        assert_eq!(self.service.active_watches(), 0);
        assert_eq!(self.service.binding_count(), 0);
    }
}
fn delivery(outcome: SubscriptionOutcome) -> Arc<SubscriptionDelivery> {
    match outcome {
        SubscriptionOutcome::Delivery(delivery) => delivery,
        other => panic!("expected delivery: {other:?}"),
    }
}
fn create(authority: [u8; 16], key: &str) -> SubscriptionRequest {
    SubscriptionRequest::Create(SubscriptionCreate {
        authority,
        key: key.into(),
        ids: vec!["point".into()],
        lease_ms: 5000,
    })
}
fn ack(delivery: &SubscriptionDelivery) -> SubscriptionRequest {
    SubscriptionRequest::Acknowledge {
        watch: delivery.watch.clone(),
        scope_generation: delivery.scope_generation,
        token: delivery.token,
        through: delivery.through,
    }
}
type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn raw(running: &Running) -> Socket {
    let mut request = running.ws.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "Authorization",
        "BEARER authToken=session-a".parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", wire::WS_PROTOCOL.parse().unwrap());
    let (socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(
        response.headers().get("Sec-WebSocket-Protocol").unwrap(),
        wire::WS_PROTOCOL
    );
    socket
}
async fn send(socket: &mut Socket, id: &str, request: &SubscriptionRequest) {
    socket.send(tungstenite::Message::Text(serde_json::json!({"profile":wire::PROFILE,"reqId":id,"payload":String::from_utf8(wire::encode(request).unwrap()).unwrap()}).to_string().into())).await.unwrap();
}
async fn outcome(socket: &mut Socket) -> SubscriptionOutcome {
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let tungstenite::Message::Text(text) = message else {
        panic!("expected typed response")
    };
    let envelope: serde_json::Value = serde_json::from_str(&text).unwrap();
    wire::decode(envelope["payload"].as_str().unwrap().as_bytes()).unwrap()
}
#[tokio::test]
async fn three_http_codecs_ws_and_native_share_prepared_delivery_and_acknowledgement() {
    let running = Running::start(Duration::from_secs(60), Arc::new(AllowAll)).await;
    for (index, format) in ["text/zinc", "application/json;v=3", "application/json"]
        .into_iter()
        .enumerate()
    {
        let http = running.http("session-a", format);
        let SubscriptionOutcome::Authority { authority, dataset } = http
            .state_subscription(&SubscriptionRequest::Describe)
            .await
            .unwrap()
        else {
            panic!("authority")
        };
        assert_eq!(dataset, running.service.entity_store().dataset());
        let request = create(authority, &format!("known-{index}"));
        let first = delivery(http.state_subscription(&request).await.unwrap());
        running
            .graph
            .update("point", entity(10.0 + index as f64))
            .unwrap();
        let ws = http.attach_subscription_ws(&running.ws).await.unwrap();
        let resume = SubscriptionRequest::Resume {
            watch: first.watch.clone(),
            scope_generation: first.scope_generation,
            acknowledged: first.through,
        };
        let repeated = delivery(ws.state_subscription(&resume).await.unwrap());
        assert_eq!(*first, *repeated);
        assert!(matches!(
            http.state_subscription(&ack(&first)).await.unwrap(),
            SubscriptionOutcome::Acknowledged { .. }
        ));
        // Attach catches retained commits without needing another graph wake.
        let changed = delivery(ws.state_subscription(&resume).await.unwrap());
        assert!(!changed.initial);
        assert_eq!(changed.through, running.graph.version());
        assert_eq!(
            changed.rows[0].get("value"),
            Some(&Kind::Number(Number::unitless(10.0 + index as f64)))
        );
        ws.close().await.unwrap();
        let native = running
            .service
            .execute(
                ReadContext::with_timeout(
                    running.session.principal().clone(),
                    Duration::from_secs(2),
                ),
                running.session.clone(),
                SubscriptionRequest::Poll {
                    watch: first.watch.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(delivery(native).token, changed.token);
        let reattached = http.attach_subscription_ws(&running.ws).await.unwrap();
        assert_eq!(
            delivery(reattached.state_subscription(&resume).await.unwrap()).token,
            changed.token
        );
        assert!(matches!(
            reattached.state_subscription(&ack(&changed)).await.unwrap(),
            SubscriptionOutcome::Acknowledged { .. }
        ));
        assert!(matches!(
            http.state_subscription(&SubscriptionRequest::Poll {
                watch: first.watch.clone()
            })
            .await
            .unwrap(),
            SubscriptionOutcome::Idle { .. }
        ));
        let other = running.http("session-b", format);
        assert_eq!(
            other
                .state_subscription(&SubscriptionRequest::Poll {
                    watch: first.watch.clone()
                })
                .await
                .unwrap(),
            SubscriptionOutcome::Rejected(SubscriptionRejection::Forbidden)
        );
        reattached.close().await.unwrap();
        http.state_subscription(&SubscriptionRequest::Unsubscribe {
            watch: first.watch.clone(),
        })
        .await
        .unwrap();
    }
    assert_eq!(running.service.read_service().load().admitted, 0);
    assert_eq!(running.service.binding_count(), 3);
    running.stop().await;
}
#[tokio::test]
async fn a_fresh_login_cannot_resume_the_old_session_but_private_bearer_attachment_can() {
    let running = Running::start(Duration::from_secs(60), Arc::new(AllowAll)).await;
    let http = HaystackClient::connect(&running.url, "user", "pencil")
        .await
        .unwrap();
    let first = delivery(
        http.state_subscription(&create(running.service.authority(), "actual-login"))
            .await
            .unwrap(),
    );
    let ws = http.attach_subscription_ws(&running.ws).await.unwrap();
    assert_eq!(
        delivery(
            ws.state_subscription(&SubscriptionRequest::Poll {
                watch: first.watch.clone()
            })
            .await
            .unwrap()
        )
        .token,
        first.token
    );
    let second_login = HaystackClient::connect(&running.url, "user", "pencil")
        .await
        .unwrap();
    assert_eq!(
        second_login
            .state_subscription(&SubscriptionRequest::Poll {
                watch: first.watch.clone()
            })
            .await
            .unwrap(),
        SubscriptionOutcome::Rejected(SubscriptionRejection::Forbidden)
    );
    ws.close().await.unwrap();
    running.stop().await;
}
#[tokio::test]
async fn idle_websocket_ends_at_fixed_expiry_or_logout_without_another_message() {
    for logout in [false, true] {
        let running = Running::start(
            if logout {
                Duration::from_secs(60)
            } else {
                Duration::from_millis(350)
            },
            Arc::new(AllowAll),
        )
        .await;
        let mut socket = raw(&running).await;
        send(
            &mut socket,
            "1",
            &create(running.service.authority(), "idle"),
        )
        .await;
        delivery(outcome(&mut socket).await);
        let http = running.http("session-a", "text/zinc");
        if logout {
            http.close_session().await.unwrap();
        }
        // No control/poll message is sent after revocation/expiry.
        let terminal = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap();
        assert!(matches!(
            terminal,
            None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_)))
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while running.service.binding_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!running.session.is_active());
        assert_eq!(running.service.read_service().load().admitted, 0);
        running.stop().await;
    }
}
#[tokio::test]
async fn application_close_drains_live_socket_and_subscription_maintenance() {
    let running = Running::start(Duration::from_secs(60), Arc::new(AllowAll)).await;
    let mut socket = raw(&running).await;
    send(
        &mut socket,
        "1",
        &create(running.service.authority(), "shutdown"),
    )
    .await;
    delivery(outcome(&mut socket).await);
    tokio::time::timeout(Duration::from_secs(2), running.owner.close())
        .await
        .unwrap()
        .unwrap();
    running.owner.terminated().await;
    assert_eq!(running.service.binding_count(), 0);
    assert_eq!(running.service.read_service().load().admitted, 0);
    let terminal = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap();
    assert!(matches!(
        terminal,
        None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_)))
    ));
}

#[tokio::test]
async fn reconnect_reports_retention_gap_or_expired_lease_without_reviving_the_creation_key() {
    for gap in [false, true] {
        let running = Running::start(Duration::from_secs(60), Arc::new(AllowAll)).await;
        let http = running.http("session-a", "text/zinc");
        let mut request = create(running.service.authority(), "resume-terminal");
        if !gap && let SubscriptionRequest::Create(create) = &mut request {
            create.lease_ms = 80;
        }
        let first = delivery(http.state_subscription(&request).await.unwrap());
        http.state_subscription(&ack(&first)).await.unwrap();
        let ws = http.attach_subscription_ws(&running.ws).await.unwrap();
        ws.close().await.unwrap();
        if gap {
            for value in 0..12 {
                running.graph.update("point", entity(value as f64)).unwrap();
            }
        } else {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let ws = http.attach_subscription_ws(&running.ws).await.unwrap();
        let reason = if gap {
            SubscriptionResync::Gap
        } else {
            SubscriptionResync::LeaseExpired
        };
        assert_eq!(
            ws.state_subscription(&SubscriptionRequest::Resume {
                watch: first.watch.clone(),
                scope_generation: 1,
                acknowledged: first.through
            })
            .await
            .unwrap(),
            SubscriptionOutcome::Resync(reason)
        );
        assert_eq!(
            http.state_subscription(&request).await.unwrap(),
            SubscriptionOutcome::Resync(reason)
        );
        assert_eq!(running.service.active_watches(), 0);
        assert_eq!(running.service.binding_count(), 1);
        ws.close().await.unwrap();
        running.stop().await;
    }
}
#[tokio::test]
async fn unsupported_output_codec_is_rejected_before_creating_a_scoped_watch() {
    let running = Running::start(Duration::from_secs(60), Arc::new(AllowAll)).await;
    let request = create(running.service.authority(), "codec-preflight");
    let body = wire::encode_grid(
        &request,
        haystack_core::codecs::codec_for("text/zinc").unwrap(),
    )
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{}/watchSub", running.url))
        .header("Authorization", "BEARER authToken=session-a")
        .header("Content-Type", "text/zinc")
        .header("Accept", "text/trio")
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert_eq!(running.service.binding_count(), 0);
    running.stop().await;
}
#[tokio::test]
async fn subscriptions_coexist_with_history_writes_and_optional_entity_mutation_capability() {
    for entity_writes in [false, true] {
        let graph = SharedGraph::new(EntityGraph::new());
        let mut row = entity(1.0);
        row.set("his", Kind::Marker);
        row.set("kind", Kind::Str("Number".into()));
        row.set("tz", Kind::Str("UTC".into()));
        row.set("unit", Kind::Str("°F".into()));
        graph.add(row).unwrap();
        let builder =
            ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
                .unwrap();
        let reads = builder.handle().read_service();
        let entity_store = EphemeralMutationStore::new(graph.clone());
        let subscriptions = StateSubscriptionService::new(
            reads.clone(),
            entity_store.clone(),
            SubscriptionLimits::default(),
        )
        .unwrap();
        let builder = builder.state_subscriptions(subscriptions.clone()).unwrap();
        let builder = if entity_writes {
            builder
                .entity_mutations(
                    MutationService::new(
                        reads.clone(),
                        entity_store.clone(),
                        Arc::new(AllowAllMutations),
                        MutationLimits::default(),
                    )
                    .unwrap(),
                )
                .unwrap()
        } else {
            builder
        };
        let history_store = HisStore::new();
        let history = HistoryService::new(
            reads,
            Arc::new(history_store.clone()),
            HistoryLimits::default(),
        )
        .unwrap();
        let history_writes = HistoryMutationService::new(
            history.clone(),
            Arc::new(AllowAllHistoryMutations),
            HistoryMutationLimits::default(),
        )
        .unwrap();
        let builder = builder
            .owned_history(history)
            .unwrap()
            .history_mutations(history_writes.clone())
            .unwrap();
        let handle = builder.handle();
        let listener = HaystackServer::new(graph.clone())
            .with_scoped_reads(handle.clone())
            .port(0)
            .into_listener();
        let owner = builder
            .owned_resource(listener)
            .start(&tokio::runtime::Handle::current())
            .unwrap();
        let address = owner.ready().await.unwrap().listeners[0].address;
        let client = HaystackClient::from_transport(HttpTransport::new(
            &format!("http://{address}/api"),
            "".into(),
        ));
        let names = client.ops().await.unwrap().rows;
        for expected in [
            "hisRead",
            "hisWrite",
            "hisReceipt",
            "watchSub",
            "watchPoll",
            "watchAck",
            "watchRenew",
            "watchInfo",
            "ws",
        ] {
            assert!(
                names
                    .iter()
                    .any(|row| row.get("name") == Some(&Kind::Str(expected.into()))),
                "missing {expected}"
            );
        }
        assert_eq!(
            names
                .iter()
                .any(|row| row.get("name") == Some(&Kind::Str("entityBatch".into()))),
            entity_writes
        );
        let session = SubscriptionSession::trusted("embedding", Duration::from_secs(5)).unwrap();
        let first = delivery(
            subscriptions
                .execute(
                    ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(2)),
                    session.clone(),
                    create(subscriptions.authority(), "history-independent"),
                )
                .await
                .unwrap(),
        );
        assert_eq!(first.dataset, entity_store.dataset());
        let state = history_store.state("point").unwrap();
        assert_ne!(first.dataset, state.authority);
        let submitted = HistoryWriteRequest {
            identity: HistoryOperationIdentity {
                authority: state.authority,
                point: "point".into(),
                incarnation: state.incarnation,
                operation_id: "history-only".into(),
            },
            expected_generation: state.generation,
            samples: vec![HistorySample {
                ts: haystack_core::kinds::HDateTime::new(
                    chrono::DateTime::from_timestamp(1_717_200_000, 0)
                        .unwrap()
                        .fixed_offset(),
                    "UTC",
                ),
                val: Kind::Number(Number::new(2.0, Some("°F".into()))),
            }],
        };
        assert!(matches!(
            client.his_write_scoped(&submitted).await.unwrap(),
            HistoryWriteOutcome::Committed(_)
        ));
        assert_eq!(history_store.state("point").unwrap().generation, 1);
        assert_eq!(graph.version(), first.through);
        assert_eq!(entity_store.receipt_count(), 0);
        assert!(handle.history_service().is_some());
        assert!(handle.history_mutation_service().is_some());
        owner.close().await.unwrap();
        owner.terminated().await;
    }
}

#[tokio::test]
async fn lost_http_creation_and_acknowledgement_recover_only_by_explicit_same_key_retry() {
    use axum::{
        body::{Body, to_bytes},
        extract::Request,
        middleware::{self, Next},
        response::Response,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let graph = SharedGraph::new(EntityGraph::new());
    graph.add(entity(1.0)).unwrap();
    let builder =
        ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default()).unwrap();
    let service = StateSubscriptionService::new(
        builder.handle().read_service(),
        EphemeralMutationStore::new(graph.clone()),
        SubscriptionLimits::default(),
    )
    .unwrap();
    let builder = builder.state_subscriptions(service.clone()).unwrap();
    let credentials=parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap();
    let auth = AuthManager::new(
        HashMap::from([(
            "user".into(),
            UserRecord {
                credentials,
                permissions: vec!["read".into()],
            },
        )]),
        Duration::from_secs(60),
    );
    auth.inject_token("session-a".into(), user());
    let session = auth.validate_session("session-a").unwrap().1;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let router = HaystackServer::new(graph)
        .with_scoped_reads(builder.handle())
        .with_auth(auth)
        .into_external_router()
        .unwrap()
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let observed = observed.clone();
            async move {
                let watched = matches!(request.uri().path(), "/api/watchSub" | "/api/watchAck");
                let index = if watched {
                    observed.fetch_add(1, Ordering::SeqCst)
                } else {
                    usize::MAX
                };
                let response = next.run(request).await;
                if index != 0 && index != 2 {
                    return response;
                }
                // Consume the real response, proving execution completed, then lose
                // its body in transit. Neither helper may automatically replay it.
                let (mut parts, body) = response.into_parts();
                let bytes = to_bytes(body, wire::MAX_WIRE_BYTES).await.unwrap();
                assert!(!bytes.is_empty());
                parts
                    .headers
                    .insert("Content-Length", "10000".parse().unwrap());
                Response::from_parts(
                    parts,
                    Body::from_stream(futures_util::stream::once(async {
                        Err::<String, _>(std::io::Error::other("injected lost subscription body"))
                    })),
                )
            }
        }));
    let owner = builder.start(&tokio::runtime::Handle::current()).unwrap();
    owner.ready().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap()
    });
    let http = HaystackClient::from_transport(HttpTransport::new(
        &format!("http://{address}/api"),
        "session-a".into(),
    ));
    let request = create(service.authority(), "recoverable-initial");
    assert_eq!(
        http.state_subscription(&request).await.unwrap(),
        SubscriptionOutcome::Unknown
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(service.binding_count(), 1);
    let retained = delivery(
        service
            .execute(
                ReadContext::with_timeout(session.principal().clone(), Duration::from_secs(2)),
                session,
                request.clone(),
            )
            .await
            .unwrap(),
    );
    let retried = delivery(http.state_subscription(&request).await.unwrap());
    assert_eq!(*retained, *retried);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(service.binding_count(), 1);
    assert_eq!(
        http.state_subscription(&ack(&retained)).await.unwrap(),
        SubscriptionOutcome::Unknown
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(matches!(
        http.state_subscription(&ack(&retained)).await.unwrap(),
        SubscriptionOutcome::Acknowledged { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(matches!(
        http.state_subscription(&SubscriptionRequest::Poll {
            watch: retained.watch.clone()
        })
        .await
        .unwrap(),
        SubscriptionOutcome::Idle { .. }
    ));
    owner.close().await.unwrap();
    owner.terminated().await;
    stop.send(()).unwrap();
    task.await.unwrap();
}

fn review_raw_json_request(request: &SubscriptionRequest, v3: bool, bad: usize) -> String {
    let payload = String::from_utf8(wire::encode(request).unwrap()).unwrap();
    let payload =
        serde_json::to_string(&format!("{}{}", if v3 { "s:" } else { "" }, payload)).unwrap();
    let meta = if v3 {
        r#""meta":{"ver":"3.0"},"#
    } else {
        r#""_kind":"grid","meta":{},"#
    };
    let valid =
        format!(r#"{{{meta}"cols":[{{"name":"payload"}}],"rows":[{{"payload":{payload}}}]}}"#);
    match bad {
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
        3 => valid.replace(r#""name":"payload""#, r#""name":"payload","meta":null"#),
        _ => valid,
    }
}
#[tokio::test]
async fn review_malformed_raw_json_cannot_create_or_acknowledge_a_watch() {
    for v3 in [false, true] {
        let app = Running::start(Duration::from_secs(60), Arc::new(AllowAll)).await;
        let mime = if v3 {
            "application/json;v=3"
        } else {
            "application/json"
        };
        let raw = haystack_client::ClientConfig::default()
            .build_reqwest_client()
            .unwrap();
        let request = create(app.service.authority(), "raw-json-input");
        for bad in 0..if v3 { 3 } else { 4 } {
            let response = raw
                .post(format!("{}/watchSub", app.url))
                .header("Authorization", "BEARER authToken=session-a")
                .header("Content-Type", mime)
                .header("Accept", mime)
                .body(review_raw_json_request(&request, v3, bad))
                .send()
                .await
                .unwrap();
            assert!(response.status().is_client_error(), "v3={v3} case={bad}");
            assert_eq!(app.service.binding_count(), 0);
        }
        let client = app.http("session-a", mime);
        let initial = delivery(client.state_subscription(&request).await.unwrap());
        for bad in 0..if v3 { 3 } else { 4 } {
            let response = raw
                .post(format!("{}/watchAck", app.url))
                .header("Authorization", "BEARER authToken=session-a")
                .header("Content-Type", mime)
                .header("Accept", mime)
                .body(review_raw_json_request(&ack(&initial), v3, bad))
                .send()
                .await
                .unwrap();
            assert!(
                response.status().is_client_error(),
                "v3={v3} ack case={bad}"
            );
            assert_eq!(
                *delivery(
                    client
                        .state_subscription(&SubscriptionRequest::Poll {
                            watch: initial.watch.clone()
                        })
                        .await
                        .unwrap()
                ),
                *initial
            );
        }
        app.stop().await;
    }
}
