//! Actual typed-watch HTTP state transitions, over the selected subscription owner.
use haystack_app::*;
use haystack_core::{
    codecs::{codec_for, jeto},
    data::{HCol, HDict, HGrid},
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

fn row(id: &str, value: f64) -> HDict {
    let mut row = HDict::new();
    row.set("id", Kind::Ref(HRef::from_val(id)));
    row.set("curVal", Kind::Number(Number::unitless(value)));
    row.set("curStatus", Kind::Str("ok".into()));
    row.set("dis", Kind::Str(format!("Point {id}")));
    row
}
struct Running {
    url: String,
    owner: ApplicationOwner,
    service: StateSubscriptionService,
    graph: SharedGraph,
    session: SubscriptionSession,
    http: reqwest::Client,
}
impl Running {
    async fn start() -> Self {
        let graph = SharedGraph::new(EntityGraph::with_changelog_capacity(64));
        for (id, val) in [("a", 1.0), ("b", 2.0), ("c", 3.0)] {
            graph.add(row(id, val)).unwrap();
        }
        let builder =
            ApplicationBuilder::new(graph.clone(), Arc::new(AllowAll), ReadLimits::default())
                .unwrap();
        let service = StateSubscriptionService::new(
            builder.handle().read_service(),
            EntityMutationStore::new(graph.clone()),
            SubscriptionLimits::default(),
        )
        .unwrap();
        let builder = builder.state_subscriptions(service.clone()).unwrap();
        let auth = AuthManager::new(HashMap::from([("reader".into(), UserRecord { credentials: parse_password_hash("W22ZaJ0SNY7soEsUEjb6gQ==:4096:WG5d8oPm3OtcPnkdi4Uo7BkeZkBFzpcXkuLmtbsT4qY=:wfPLwcE6nTWhTAmQ7tl2KeoiWGPlZqQxSrmfPwDl2dU=").unwrap(), permissions: vec!["read".into()] })]), Duration::from_secs(60));
        for token in ["login-a", "login-b"] {
            auth.inject_token(
                token.into(),
                AuthUser {
                    username: "reader".into(),
                    permissions: vec!["read".into()],
                },
            );
        }
        let session = auth.validate_session("login-a").unwrap().1;
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
            owner,
            service,
            graph,
            session,
            http: haystack_client::ClientConfig::default()
                .build_reqwest_client()
                .unwrap(),
        }
    }
    fn post(&self, operation: &str) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}/{operation}", self.url))
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=login-a")
            .header("Accept", "application/json")
    }
    async fn grid(&self, operation: &str, grid: HGrid) -> reqwest::Response {
        self.post(operation)
            .header("Content-Type", "text/zinc")
            .body(codec_for("text/zinc").unwrap().encode_grid(&grid).unwrap())
            .send()
            .await
            .unwrap()
    }
    async fn poll(&self, id: &str, refresh: bool, cur: bool) -> reqwest::Response {
        let mut args = serde_json::json!({"watchId":id});
        if refresh {
            args["refresh"] = serde_json::json!("✓");
        }
        if cur {
            args["curValSub"] = serde_json::json!("✓");
        }
        self.post("ph.api::watchPoll")
            .header("Content-Type", "application/json")
            .body(args.to_string())
            .send()
            .await
            .unwrap()
    }
    async fn close(self) {
        self.owner.close().await.unwrap();
        self.owner.terminated().await;
    }
}
fn sub(watch: Option<&str>, ids: &[&str]) -> HGrid {
    let mut meta = HDict::new();
    if let Some(watch) = watch {
        meta.set("watchId", Kind::Str(watch.into()));
    } else {
        meta.set("watchDis", Kind::Str("fixture".into()));
    }
    HGrid::from_parts(
        meta,
        vec![HCol::new("id")],
        ids.iter()
            .map(|id| {
                let mut r = HDict::new();
                r.set("id", Kind::Ref(HRef::from_val(*id)));
                r
            })
            .collect(),
    )
}
async fn grid(response: reqwest::Response) -> HGrid {
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    match jeto::decode(
        text.as_bytes(),
        &jeto::Context::standard(),
        Some("sys::Grid"),
        jeto::Limits::default(),
    )
    .unwrap()
    {
        Kind::Grid(grid) => *grid,
        other => panic!("{other:?}"),
    }
}
fn watch(grid: &HGrid) -> String {
    match grid.meta.get("watchId").unwrap() {
        Kind::Str(id) => id.clone(),
        _ => panic!(),
    }
}
fn ids(grid: &HGrid) -> Vec<&str> {
    grid.rows
        .iter()
        .filter_map(|r| match r.get("id") {
            Some(Kind::Ref(id)) => Some(id.val.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn ordered_subscribe_first_poll_and_membership_edits_preserve_pending_changes() {
    let r = Running::start().await;
    let created = grid(
        r.grid("ph.api::watchSub", sub(None, &["c", "a", "missing", "a"]))
            .await,
    )
    .await;
    assert_eq!(created.rows.len(), 4);
    assert_eq!(ids(&created), ["c", "a", "a"]);
    assert!(created.rows[2].is_empty());
    assert!(
        matches!(created.meta.get("lease"),Some(Kind::Number(n)) if n.unit.is_some() && n.val > 0.0)
    );
    let id = watch(&created);
    let first = grid(r.poll(&id, false, false).await).await;
    assert_eq!(ids(&first), ["a", "c"]);
    assert_eq!(watch(&first), id);
    r.graph.update("a", row("a", 10.0)).unwrap();
    let added = grid(r.grid("ph.api::watchSub", sub(Some(&id), &["b"])).await).await;
    assert_eq!(ids(&added), ["b"]);
    grid(r.grid("ph.api::watchUnsub", sub(Some(&id), &["c"])).await).await;
    let changed = grid(r.poll(&id, false, false).await).await;
    assert_eq!(ids(&changed), ["a"]);
    assert_eq!(
        changed.rows[0].get("curVal"),
        Some(&Kind::Number(Number::unitless(10.0)))
    );
    grid(r.grid("ph.api::watchUnsub", sub(Some(&id), &[])).await).await;
    let refreshed = grid(r.poll(&id, true, true).await).await;
    assert_eq!(ids(&refreshed), ["a", "b"]);
    assert_eq!(
        refreshed
            .cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "curVal", "curStatus"]
    );
    let mut close = sub(Some(&id), &[]);
    close.meta.set("close", Kind::Marker);
    for _ in 0..2 {
        assert!(
            grid(r.grid("ph.api::watchUnsub", close.clone()).await)
                .await
                .rows
                .is_empty()
        );
    }
    assert_eq!(r.service.active_watches(), 0);
    r.close().await;
}

#[tokio::test]
async fn missing_members_stay_inactive_until_remove_readd_but_sub_returns_current_rows() {
    let r = Running::start().await;
    let created = grid(
        r.grid("ph.api::watchSub", sub(None, &["missing", "missing"]))
            .await,
    )
    .await;
    assert_eq!(created.rows.len(), 2);
    assert!(created.rows.iter().all(HDict::is_empty));
    assert!(created.cols.iter().any(|col| col.name == "id"));
    let id = watch(&created);
    assert!(grid(r.poll(&id, false, false).await).await.rows.is_empty());
    r.graph.add(row("missing", 7.0)).unwrap();
    let added = grid(
        r.grid("ph.api::watchSub", sub(Some(&id), &["missing"]))
            .await,
    )
    .await;
    assert_eq!(ids(&added), ["missing"]);
    assert!(grid(r.poll(&id, true, false).await).await.rows.is_empty());
    grid(
        r.grid("ph.api::watchUnsub", sub(Some(&id), &["missing"]))
            .await,
    )
    .await;
    grid(
        r.grid("ph.api::watchSub", sub(Some(&id), &["missing"]))
            .await,
    )
    .await;
    assert_eq!(
        ids(&grid(r.poll(&id, true, false).await).await),
        ["missing"]
    );
    r.close().await;
}

#[tokio::test]
async fn enrollment_only_fences_historical_deletions_and_repeated_add_does_not_consume_updates() {
    let r = Running::start().await;
    let id = watch(&grid(r.grid("ph.api::watchSub", sub(None, &["a"])).await).await);
    grid(r.poll(&id, false, false).await).await;
    r.graph.update("a", row("a", 8.0)).unwrap();
    r.graph.remove("b").unwrap();
    r.graph.add(row("b", 9.0)).unwrap();
    grid(
        r.grid("ph.api::watchSub", sub(Some(&id), &["b", "a"]))
            .await,
    )
    .await;
    let changed = grid(r.poll(&id, false, false).await).await;
    assert_eq!(ids(&changed), ["a", "b"]);
    r.graph.remove("a").unwrap();
    r.graph.add(row("a", 10.0)).unwrap();
    assert_eq!(r.poll(&id, false, false).await.status(), 503);
    assert_eq!(r.poll(&id, true, false).await.status(), 503);
    r.close().await;
}

#[tokio::test]
async fn method_shapes_and_markers_reject_before_creating_or_consuming_a_watch() {
    let r = Running::start().await;
    for path in [
        "watchSub",
        "ph.api::watchSub",
        "ph.api%3A%3AwatchSub",
        "watchUnsub",
        "ph.api::watchUnsub",
        "watchPoll",
        "ph.api::watchPoll",
    ] {
        let response = r
            .http
            .get(format!("{}/{path}", r.url))
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=login-a")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 405, "{path}");
        assert_eq!(response.headers()["allow"], "POST");
    }
    assert_eq!(r.service.active_watches(), 0);
    for lease in [
        Kind::Number(Number::unitless(1.0)),
        Kind::Number(Number::new(-1.0, Some("s".into()))),
        Kind::Number(Number::new(f64::NAN, Some("s".into()))),
        Kind::Str("1min".into()),
        Kind::Number(Number::new(1.0, Some("m".into()))),
    ] {
        let mut request = sub(None, &["a"]);
        request.meta.set("lease", lease);
        assert_eq!(r.grid("ph.api::watchSub", request).await.status(), 400);
    }
    let mut request = sub(None, &["a"]);
    request.meta.remove_tag("watchDis");
    assert_eq!(r.grid("ph.api::watchSub", request).await.status(), 400);
    assert_eq!(r.service.active_watches(), 0);
    let id = watch(&grid(r.grid("watchSub", sub(None, &["a"])).await).await);
    for key in ["refresh", "curValSub"] {
        let args = serde_json::json!({"watchId":id,key:true});
        let response = r
            .post("watchPoll")
            .header("Content-Type", "application/json")
            .body(args.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    assert_eq!(ids(&grid(r.poll(&id, false, false).await).await), ["a"]);
    r.close().await;
}

#[tokio::test]
async fn exact_login_revocation_discovery_and_idempotent_cleanup_do_not_alias_sessions() {
    let r = Running::start().await;
    let ops = r
        .http
        .get(format!("{}/ops", r.url))
        .header("Xeto-Version", "5")
        .header("Authorization", "BEARER authToken=login-a")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for op in ["watchSub", "watchUnsub", "watchPoll"] {
        assert!(ops.contains(&format!("ph.api::{op}")));
    }
    let id = watch(&grid(r.grid("ph.api::watchSub", sub(None, &["a"])).await).await);
    for operation in ["ph.api::watchSub", "ph.api::watchUnsub"] {
        let mut req = sub(Some(&id), &[]);
        if operation.ends_with("watchUnsub") {
            req.meta.set("close", Kind::Marker);
        }
        let response = r
            .http
            .post(format!("{}/{operation}", r.url))
            .header("Xeto-Version", "5")
            .header("Authorization", "BEARER authToken=login-b")
            .header("Content-Type", "text/zinc")
            .body(codec_for("text/zinc").unwrap().encode_grid(&req).unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
    }
    let response = r
        .http
        .post(format!("{}/ph.api::watchPoll", r.url))
        .header("Xeto-Version", "5")
        .header("Authorization", "BEARER authToken=login-b")
        .header("Content-Type", "application/json")
        .body(serde_json::json!({"watchId":id}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert_eq!(r.service.active_watches(), 1);
    r.session.close();
    assert_eq!(r.poll(&id, false, false).await.status(), 403);
    let unknown = sub(Some("00000000000000000000000000000000"), &[]);
    let response = r
        .http
        .post(format!("{}/ph.api::watchUnsub", r.url))
        .header("Xeto-Version", "5")
        .header("Authorization", "BEARER authToken=login-b")
        .header("Content-Type", "text/zinc")
        .body(
            codec_for("text/zinc")
                .unwrap()
                .encode_grid(&unknown)
                .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert!(grid(response).await.rows.is_empty());
    r.close().await;
}

#[tokio::test]
async fn qualified_v4_poll_uses_metadata_and_retains_watch_id() {
    let r = Running::start().await;
    let id = watch(&grid(r.grid("ph.api::watchSub", sub(None, &["a"])).await).await);
    let body = format!("ver:\"3.0\" watchId:\"{id}\" refresh curValSub\nempty\n");
    let response = r
        .http
        .post(format!("{}/ph.api::watchPoll", r.url))
        .header("Xeto-Version", "4")
        .header("Authorization", "BEARER authToken=login-a")
        .header("Content-Type", "text/zinc")
        .header("Accept", "text/zinc")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let result = codec_for("text/zinc")
        .unwrap()
        .decode_grid(&response.text().await.unwrap())
        .unwrap();
    assert_eq!(watch(&result), id);
    assert_eq!(ids(&result), ["a"]);
    assert_eq!(
        result
            .cols
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "curVal", "curStatus"]
    );
    let body = format!("ver:\"3.0\"\nwatchId,refresh\n\"{id}\",M\n");
    let response = r
        .http
        .post(format!("{}/ph.api::watchPoll", r.url))
        .header("Xeto-Version", "4")
        .header("Authorization", "BEARER authToken=login-a")
        .header("Content-Type", "text/zinc")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    r.close().await;
}

#[tokio::test]
async fn lost_create_and_poll_are_not_retried_and_refresh_restores_active_view() {
    let r = Running::start().await;
    // Dropping a received creation response loses its ID. A separate explicit
    // create is a distinct operation, not a hidden deduplicated retry.
    let response = r.grid("ph.api::watchSub", sub(None, &["a"])).await;
    assert_eq!(response.status(), 200);
    drop(response);
    let id = watch(&grid(r.grid("ph.api::watchSub", sub(None, &["a"])).await).await);
    assert_eq!(r.service.active_watches(), 2);
    let response = r.poll(&id, false, false).await;
    assert_eq!(response.status(), 200);
    drop(response);
    assert!(grid(r.poll(&id, false, false).await).await.rows.is_empty());
    assert_eq!(ids(&grid(r.poll(&id, true, false).await).await), ["a"]);
    r.close().await;
}

#[tokio::test]
async fn malformed_zinc_poll_aliases_cannot_consume_pending_changes() {
    let r = Running::start().await;
    let watch = watch(&grid(r.grid("watchSub", sub(None, &["a"])).await).await);
    grid(r.poll(&watch, false, false).await).await;
    let mut value = 10.0;
    for alias in ["watchPoll", "ph.api::watchPoll", "ph.api%3A%3AwatchPoll"] {
        for tail in [",999", " garbage"] {
            r.graph.update("a", row("a", value)).unwrap();
            let response = r
                .post(alias)
                .header("Content-Type", "text/zinc")
                .body(format!("ver:\"3.0\"\nwatchId\n\"{watch}\"{tail}\n"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 400, "{alias} {tail}");
            assert!(response.text().await.unwrap().contains("InvalidArgsErr"));
            let pending = grid(r.poll(&watch, false, false).await).await;
            assert_eq!(pending.rows.len(), 1);
            assert_eq!(
                pending.rows[0].get("curVal"),
                Some(&Kind::Number(Number::unitless(value)))
            );
            value += 1.0;
        }
    }
    r.close().await;
}
