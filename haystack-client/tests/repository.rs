//! Independent literal wire fixtures for pinned Xeto 873b9224 repository contracts.
mod repository_support;
use haystack_client::repository::*;
use repository_support::{Reply, Server};
use std::{str::FromStr, time::Duration};
use tokio_util::sync::CancellationToken;

const ZIP: &[u8] = include_bytes!("fixtures/repository/downloaded-1.0.0.xetolib");
const DIGEST: &str = "sha256:Qm3aICrF2MHDjamk30dmuQCif1quynjfBEPGq_zMP80";
const ROW: &str = r#"{"lib":"downloaded","version":"1.0.0","maturity":"stable","availability":"available","depends":[{"lib":"sys","versions":"5.0.0"}],"digest":"sha256:Qm3aICrF2MHDjamk30dmuQCif1quynjfBEPGq_zMP80"}"#;
fn config() -> RepositoryConfig {
    RepositoryConfig {
        allow_loopback_http: true,
        ..Default::default()
    }
}
fn client(server: &Server) -> RepositoryClient {
    RepositoryClient::with_config(&server.base, config()).unwrap()
}
fn pin() -> LibraryPin {
    LibraryPin::new("downloaded", "1.0.0".parse().unwrap()).unwrap()
}
fn token() -> CancellationToken {
    CancellationToken::new()
}
fn row(version: &str) -> String {
    format!(
        r#"{{"lib":"downloaded","version":"{version}","maturity":"alpha","availability":"yanked"}}"#
    )
}

#[tokio::test]
async fn qualified_wire_metadata_versions_and_exact_binary_integrity() {
    let server = Server::new(vec![
        Reply::json(r#"{"dis":"Fixture","custom":"tag"}"#),
        Reply::json(&format!("[{ROW}]")),
        Reply::bytes("application/xetolib", ZIP),
    ]);
    let client = client(&server);
    let cancel = token();
    assert_eq!(client.ping(&cancel).await.unwrap().extra["custom"], "tag");
    let versions = client
        .versions("downloaded", None, None, &cancel)
        .await
        .unwrap();
    let downloaded = client
        .fetch(
            &pin(),
            Some((&versions[0]).into()),
            Some(DIGEST.parse().unwrap()),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(downloaded.bytes, ZIP);
    assert_eq!(downloaded.computed_digest.to_string(), DIGEST);
    assert_eq!(downloaded.integrity, Integrity::AdvertisedAndCaller);
    assert_eq!(downloaded.metadata.as_ref(), Some(versions[0].metadata()));
    assert_eq!(
        downloaded.metadata_source.as_ref(),
        Some(versions[0].source())
    );
    assert_eq!(downloaded.source.redirects, 0);
    assert_eq!(downloaded.source.request_url, downloaded.source.final_url);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /repo/base/sys.repo::repoPing HTTP/1.1\r\n"));
    assert!(
        requests[1]
            .starts_with("GET /repo/base/sys.repo::repoVersions?lib=downloaded HTTP/1.1\r\n")
    );
    assert!(requests[2].starts_with(
        "GET /repo/base/sys.repo::repoFetch?lib=downloaded&version=1.0.0 HTTP/1.1\r\n"
    ));
    for request in &requests {
        let r = request.to_ascii_lowercase();
        assert!(r.contains("xeto-version: 5\r\n"));
        assert!(r.contains("accept-encoding: identity\r\n"));
        assert!(!r.contains("authorization:"));
        assert!(!r.contains("cookie:"));
    }
    assert!(requests[0].contains("accept: application/json;box=none\r\n"));
    assert!(requests[2].contains("accept: application/xetolib\r\n"));
}

#[tokio::test]
async fn search_retains_relevance_and_contextual_optional_types() {
    let body = r#"{"libs":[{"lib":"zeta","latestVersion":"1.2.0","latestMaturity":"beta","latestPublished":"2026-08-03T09:00:00-04:00 New_York","latestStable":null,"deprecated":null},{"lib":"alpha","latestVersion":"3.1.0","latestMaturity":"stable","latestStable":"3.0.0","doc":"Description"}],"more":"✓","total":null}"#;
    let server = Server::new(vec![Reply::json(body)]);
    let found = client(&server)
        .search("a & b/+?", Some(2), &token())
        .await
        .unwrap();
    assert_eq!(
        found
            .libs
            .iter()
            .map(|s| s.lib.as_str())
            .collect::<Vec<_>>(),
        ["zeta", "alpha"]
    );
    assert!(found.more);
    assert_eq!(found.total, None);
    assert_eq!(found.libs[0].latest_stable, None);
    assert_eq!(
        found.libs[0].latest_published.as_ref().unwrap().tz_name,
        "New_York"
    );
    assert!(server.requests()[0].starts_with(
        "GET /repo/base/sys.repo::repoSearch?query=a+%26+b%2F%2B%3F&limit=2 HTTP/1.1"
    ));
    for body in [
        r#"{"libs":[],"more":true}"#,
        r#"{"libs":[],"total":-1}"#,
        r#"{"libs":[],"total":1.5}"#,
        r#"{"libs":[{"lib":"a","latestVersion":"1.0.0","latestMaturity":"stable","latestPublished":"today"}]}"#,
    ] {
        let server = Server::new(vec![Reply::json(body)]);
        assert!(matches!(
            client(&server).search("*", None, &token()).await,
            Err(RepositoryError::Metadata(_))
        ));
    }
}

#[tokio::test]
async fn versions_numeric_order_empty_unknown_and_absent_dependencies_are_distinct() {
    let server = Server::new(vec![
        Reply::json(&format!("[{},{}]", row("1.10.0"), row("1.9.0"))),
        Reply::json("[]"),
        Reply::json(
            r#"[{"lib":"downloaded","version":"1.0.0","maturity":"stable","availability":"available","depends":[]}]"#,
        ),
    ]);
    let client = client(&server);
    let cancel = token();
    let range = "1.x.x".parse().unwrap();
    let rows = client
        .versions("downloaded", Some(&range), Some(2), &cancel)
        .await
        .unwrap();
    assert_eq!(rows[0].pin.version().to_string(), "1.10.0");
    assert_eq!(rows[0].depends, None);
    assert_eq!(rows[0].availability, Availability::Yanked);
    assert!(
        client
            .versions("unknown", None, None, &cancel)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        client
            .versions("downloaded", None, None, &cancel)
            .await
            .unwrap()[0]
            .depends,
        Some(vec![])
    );
    assert!(server.requests()[0].contains("versions=1.x.x&limit=2"));
}

#[test]
fn version_and_dependency_grammar_is_not_semver() {
    assert_eq!(
        LibraryVersion::from_str("01.002.0003").unwrap().to_string(),
        "1.2.3"
    );
    for v in [
        "1",
        "1.2",
        "1.2.3.4",
        "1.2.3-rc1",
        "1.2.3+meta",
        "+1.2.3",
        "1. 2.3",
        "18446744073709551616.0.0",
    ] {
        assert!(LibraryVersion::from_str(v).is_err(), "{v}");
    }
    for range in [
        "^1.2.3",
        ">=1.0.0",
        "1.*.*",
        "1.2",
        "1.2.3-",
        "1.2.3 - 1.3.0-2.0.0",
        "2.0.0-1.0.0",
    ] {
        assert!(range.parse::<DependencyVersions>().is_err(), "{range}");
    }
    for (range, yes, no) in [
        ("1.0.0 - 2.0.0", "2.0.0", "2.0.1"),
        ("1.x.3", "1.8.3", "1.8.4"),
        ("x.2.x", "4.2.8", "4.3.0"),
        ("1.2.x-2.3.x", "2.3.99", "2.4.0"),
    ] {
        let range = range.parse::<DependencyVersions>().unwrap();
        assert!(range.contains(yes.parse().unwrap()));
        assert!(!range.contains(no.parse().unwrap()));
    }
    for name in ["", "Upper", "a..b", "a/b", "a?b", "a b", "a:b"] {
        assert!(LibraryPin::new(name, "1.0.0".parse().unwrap()).is_err());
    }
}

#[tokio::test]
async fn malformed_metadata_identity_duplicates_order_and_enums_fail_closed() {
    let cases=vec!["null".into(),"{}".into(),format!("[{},{}]",row("1.9.0"),row("1.10.0")),format!("[{0},{0}]",row("1.0.0")),format!("[{}]",row("1.0.0").replace("downloaded","other")),format!("[{}]",row("1.0.0").replace("alpha","preview")),format!("[{}]",row("1.0.0").replace("yanked","unknown")),format!("[{}]",row("1.0.0").replace("\"version\":\"1.0.0\"","\"version\":\"1.0.0\",\"version\":\"2.0.0\"")),r#"[{"lib":"downloaded","version":"1.0.0","maturity":"stable","availability":"available","depends":[{"lib":"sys"}]}]"#.into(),r#"[{"lib":"downloaded","version":{"_type":"sys::Version","val":"1.0.0"},"maturity":"stable","availability":"available"}]"#.into()];
    for body in cases {
        let server = Server::new(vec![Reply::json(&body)]);
        assert!(
            client(&server)
                .versions("downloaded", None, None, &token())
                .await
                .is_err(),
            "{body}"
        );
    }
    let server = Server::new(vec![Reply::json(&format!("[{}]", row("2.0.0")))]);
    assert!(matches!(
        client(&server)
            .versions(
                "downloaded",
                Some(&"1.x.x".parse().unwrap()),
                None,
                &token()
            )
            .await,
        Err(RepositoryError::Metadata(_))
    ));
}

#[tokio::test]
async fn digest_expectations_have_four_explicit_states_and_each_mismatch_fails() {
    let server = Server::new(vec![Reply::json(&format!("[{ROW}]"))]);
    let metadata = client(&server)
        .versions("downloaded", None, None, &token())
        .await
        .unwrap()
        .remove(0);
    for (advertised, expected, want) in [
        (None, None, Integrity::Unverified),
        (Some((&metadata).into()), None, Integrity::Advertised),
        (None, Some(DIGEST.parse().unwrap()), Integrity::Caller),
        (
            Some((&metadata).into()),
            Some(DIGEST.parse().unwrap()),
            Integrity::AdvertisedAndCaller,
        ),
    ] {
        let server = Server::new(vec![Reply::bytes("application/xetolib", ZIP)]);
        assert_eq!(
            client(&server)
                .fetch(&pin(), advertised, expected, &token())
                .await
                .unwrap()
                .integrity,
            want
        );
    }
    let wrong = "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        .parse()
        .unwrap();
    let mut incorrect = metadata.metadata().clone();
    incorrect.digest = Some(wrong);
    for (advertised, expected) in [
        (Some((&incorrect).into()), None),
        (Some((&metadata).into()), Some(wrong)),
    ] {
        let server = Server::new(vec![Reply::bytes("application/xetolib", ZIP)]);
        assert_eq!(
            client(&server)
                .fetch(&pin(), advertised, expected, &token())
                .await
                .unwrap_err(),
            RepositoryError::DigestMismatch
        );
    }
    for digest in [
        "md5:AAAA",
        "sha256:AAA",
        "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "not a digest",
    ] {
        let body = format!("[{}]", ROW.replace(DIGEST, digest));
        let server = Server::new(vec![Reply::json(&body)]);
        assert_eq!(
            client(&server)
                .versions("downloaded", None, None, &token())
                .await
                .unwrap_err(),
            RepositoryError::InvalidDigest
        );
    }
    let server = Server::new(vec![]);
    let other = LibraryPin::new("other", "1.0.0".parse().unwrap()).unwrap();
    assert_eq!(
        client(&server)
            .fetch(&other, Some((&metadata).into()), None, &token())
            .await
            .unwrap_err(),
        RepositoryError::Identity
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn api_errors_use_spec_and_keep_actual_status_without_private_diagnostics() {
    for (status, body, kind) in [
        (
            404,
            r#"{"spec":"sys.repo::UnknownLibErr","status":404,"dis":"secret-sentinel","errTrace":"secret-sentinel","lib":"downloaded"}"#,
            RepoErrorKind::UnknownLibrary,
        ),
        (
            404,
            r#"{"spec":"sys.repo::UnknownLibVersionErr","status":410,"dis":"secret-sentinel","lib":"downloaded","version":"1.0.0"}"#,
            RepoErrorKind::UnknownVersion,
        ),
        (
            501,
            r#"{"spec":"sys.api::NotImplementedErr","status":501,"dis":"secret-sentinel"}"#,
            RepoErrorKind::Unsupported,
        ),
        (
            404,
            r#"{"spec":"sys.api::UnknownFuncErr","status":404,"dis":"secret-sentinel","funcName":"repoFetch"}"#,
            RepoErrorKind::Unsupported,
        ),
    ] {
        let mut reply = Reply::json(body);
        reply.status = status;
        let server = Server::new(vec![reply]);
        let error = client(&server)
            .fetch(&pin(), None, None, &token())
            .await
            .unwrap_err();
        assert!(
            matches!(error,RepositoryError::Api { status:actual,kind:actual_kind,.. } if actual==status && actual_kind==kind)
        );
        if body.contains("410") {
            assert!(matches!(
                error,
                RepositoryError::Api {
                    advisory_status: Some(410),
                    ..
                }
            ));
        }
        assert!(!format!("{error:?} {error}").contains("secret-sentinel"));
    }
    let mut html = Reply::bytes("text/html", b"secret-sentinel");
    html.status = 404;
    let server = Server::new(vec![html]);
    assert_eq!(
        client(&server)
            .fetch(&pin(), None, None, &token())
            .await
            .unwrap_err(),
        RepositoryError::Http { status: 404 }
    );
    let mut malformed =
        Reply::json(r#"{"spec":"sys.repo::UnknownLibErr","status":404,"dis":"bad"}"#);
    malformed.status = 404;
    let server = Server::new(vec![malformed]);
    assert!(matches!(
        client(&server).fetch(&pin(), None, None, &token()).await,
        Err(RepositoryError::Metadata(_))
    ));
}

#[tokio::test]
async fn headers_encoding_status_and_truncation_never_become_artifacts() {
    let mut truncated = Reply::bytes("application/xetolib", b"PK");
    truncated.length = Some(99);
    let mut unchanged = Reply::bytes("application/xetolib", b"");
    unchanged.status = 304;
    let mut partial = Reply::bytes("application/xetolib", b"PK");
    partial.status = 206;
    let cases = vec![
        (partial, RepositoryError::Http { status: 206 }),
        (Reply::bytes("text/html", ZIP), RepositoryError::MediaType),
        (
            Reply::bytes("application/xetolib", ZIP).header("Xeto-Version", "4"),
            RepositoryError::ProtocolVersion,
        ),
        (
            Reply::bytes("application/xetolib", ZIP).header("Content-Encoding", "gzip"),
            RepositoryError::ContentEncoding,
        ),
        (truncated, RepositoryError::Transport),
        (unchanged, RepositoryError::Http { status: 304 }),
    ];
    for (reply, want) in cases {
        let server = Server::new(vec![reply]);
        assert_eq!(
            client(&server)
                .fetch(&pin(), None, None, &token())
                .await
                .unwrap_err(),
            want
        );
    }
}

#[tokio::test]
async fn response_and_dependency_limits_bound_declared_and_streamed_work() {
    for chunked in [false, true] {
        let mut reply = Reply::bytes("application/xetolib", ZIP);
        reply.chunked = chunked;
        let server = Server::new(vec![reply]);
        let mut config = config();
        config.limits.download_bytes = 8;
        let client = RepositoryClient::with_config(&server.base, config).unwrap();
        assert_eq!(
            client
                .fetch(&pin(), None, None, &token())
                .await
                .unwrap_err(),
            RepositoryError::Limit("body bytes")
        );
    }
    let deps = r#"[{"lib":"downloaded","version":"1.0.0","maturity":"stable","availability":"available","depends":[{"lib":"sys","versions":"5.0.0"},{"lib":"ph","versions":"5.0.0"}]}]"#;
    let server = Server::new(vec![Reply::json(deps)]);
    let mut cfg = config();
    cfg.limits.dependencies = 1;
    assert_eq!(
        RepositoryClient::with_config(&server.base, cfg)
            .unwrap()
            .versions("downloaded", None, None, &token())
            .await
            .unwrap_err(),
        RepositoryError::Limit("dependencies")
    );
    let server = Server::new(vec![Reply::json(r#"{"dis":"too long"}"#)]);
    let mut cfg = config();
    cfg.limits.string_bytes = 4;
    assert_eq!(
        RepositoryClient::with_config(&server.base, cfg)
            .unwrap()
            .ping(&token())
            .await
            .unwrap_err(),
        RepositoryError::Limit("string bytes")
    );
    let server = Server::new(vec![Reply::json(&format!(
        "[{},{}]",
        row("2.0.0"),
        row("1.0.0")
    ))]);
    assert_eq!(
        client(&server)
            .versions("downloaded", None, Some(1), &token())
            .await
            .unwrap_err(),
        RepositoryError::Limit("records")
    );
}

#[test]
fn invalid_configurations_fail_before_network() {
    for base in [
        "http://example.com/",
        "http://localhost/",
        "http://127.0.0.1/",
        "https://user:secret@example.com/",
        "https://@example.com/",
        "https://example.com/?secret=1",
        "https://example.com/#secret",
        "file:///tmp/repo",
    ] {
        assert!(RepositoryClient::new(base).is_err(), "{base}");
    }
    for base in [
        "http://example.com/",
        "http://localhost/",
        "http://127.0.0.1.example.com/",
    ] {
        assert!(RepositoryClient::with_config(base, config()).is_err());
    }
    assert!(RepositoryClient::with_config("http://[::1]/", config()).is_ok());
    for limits in [
        RepositoryLimits {
            timeout: Duration::ZERO,
            ..Default::default()
        },
        RepositoryLimits {
            timeout: Duration::MAX,
            ..Default::default()
        },
        RepositoryLimits {
            metadata_bytes: usize::MAX,
            ..Default::default()
        },
        RepositoryLimits {
            download_bytes: 0,
            ..Default::default()
        },
        RepositoryLimits {
            redirects: 9,
            ..Default::default()
        },
    ] {
        assert!(
            RepositoryClient::with_config(
                "https://example.com/",
                RepositoryConfig {
                    limits,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
}

#[tokio::test]
async fn redirects_are_opt_in_bounded_same_origin_and_never_forward_credentials() {
    let other = Server::new(vec![]);
    for location in [
        &*other.base,
        "http://user:secret@127.0.0.1/private",
        "//user:secret@127.0.0.1/private",
        "/download#private",
    ] {
        let server = Server::new(vec![Reply::redirect(location)]);
        let mut cfg = config();
        cfg.limits.redirects = 2;
        let error = RepositoryClient::with_config(&server.base, cfg)
            .unwrap()
            .fetch(&pin(), None, None, &token())
            .await
            .unwrap_err();
        assert_eq!(error, RepositoryError::Redirect);
        assert!(!format!("{error:?}").contains("secret"));
    }
    assert!(other.requests().is_empty());
    let server = Server::new(vec![Reply::redirect("/download")]);
    assert_eq!(
        client(&server)
            .fetch(&pin(), None, None, &token())
            .await
            .unwrap_err(),
        RepositoryError::Redirect
    );
    for status in [301, 302, 303, 307, 308] {
        let mut redirect =
            Reply::redirect("/download").header("Set-Cookie", "private-cookie=sentinel");
        redirect.status = status;
        let server = Server::new(vec![redirect, Reply::bytes("application/xetolib", ZIP)]);
        let mut cfg = config();
        cfg.limits.redirects = 1;
        let downloaded = RepositoryClient::with_config(&server.base, cfg)
            .unwrap()
            .fetch(&pin(), None, None, &token())
            .await
            .unwrap();
        assert_eq!(downloaded.bytes, ZIP);
        assert_eq!(downloaded.source.redirects, 1);
        assert_eq!(downloaded.source.final_url.path(), "/download");
        assert!(server.requests()[1].starts_with("GET /download HTTP/1.1"));
        assert!(
            !server.requests()[1]
                .to_ascii_lowercase()
                .contains("cookie:")
        );
    }
    let server = Server::new(vec![
        Reply::redirect("/one"),
        Reply::redirect("/two"),
        Reply::bytes("application/xetolib", ZIP),
    ]);
    let mut cfg = config();
    cfg.limits.redirects = 1;
    assert_eq!(
        RepositoryClient::with_config(&server.base, cfg)
            .unwrap()
            .fetch(&pin(), None, None, &token())
            .await
            .unwrap_err(),
        RepositoryError::Redirect
    );
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn one_deadline_covers_redirects_headers_and_body() {
    let mut first = Reply::redirect("/download");
    first.delay_headers = Duration::from_millis(100);
    let mut second = Reply::bytes("application/xetolib", ZIP);
    second.delay_body = Duration::from_millis(250);
    let server = Server::new(vec![first, second]);
    let mut cfg = config();
    cfg.limits.timeout = Duration::from_millis(300);
    cfg.limits.redirects = 1;
    assert_eq!(
        RepositoryClient::with_config(&server.base, cfg)
            .unwrap()
            .fetch(&pin(), None, None, &token())
            .await
            .unwrap_err(),
        RepositoryError::Timeout
    );
    assert_eq!(server.requests().len(), 2);
    let mut stalled = Reply::json(r#"{"dis":"Fixture"}"#);
    stalled.delay_headers = Duration::from_secs(5);
    let server = Server::new(vec![stalled]);
    let mut cfg = config();
    cfg.limits.timeout = Duration::from_millis(50);
    assert_eq!(
        RepositoryClient::with_config(&server.base, cfg)
            .unwrap()
            .ping(&token())
            .await
            .unwrap_err(),
        RepositoryError::Timeout
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn cancellation_and_dropped_future_release_stream_without_retry() {
    let server = Server::new(vec![]);
    let cancelled = token();
    cancelled.cancel();
    assert_eq!(
        client(&server)
            .fetch(&pin(), None, None, &cancelled)
            .await
            .unwrap_err(),
        RepositoryError::Cancelled
    );
    assert!(server.requests().is_empty());
    for drop_future in [false, true] {
        let mut reply = Reply::bytes("application/xetolib", b"PK");
        reply.chunked = true;
        reply.stall = true;
        let server = Server::new(vec![reply]);
        let client = client(&server);
        let pin = pin();
        let cancel = token();
        let mut future = Box::pin(client.fetch(&pin, None, None, &cancel));
        tokio::select! { result=&mut future=>panic!("unexpected completion: {result:?}"),_=server.body_sent()=>{} }
        if !drop_future {
            cancel.cancel();
            assert_eq!(future.await.unwrap_err(), RepositoryError::Cancelled);
        } else {
            drop(future);
        }
        server.peer_closed().await;
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn cancellation_during_headers_stops_without_followup() {
    let mut reply = Reply::json(r#"{"dis":"Fixture"}"#);
    reply.delay_headers = Duration::from_secs(5);
    let server = Server::new(vec![reply]);
    let client = client(&server);
    let cancel = token();
    let mut future = Box::pin(client.ping(&cancel));
    tokio::select! { result=&mut future=>panic!("unexpected completion: {result:?}"),_=server.requested(1)=>{} }
    cancel.cancel();
    assert_eq!(future.await.unwrap_err(), RepositoryError::Cancelled);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn https_trust_hostname_and_downgrade_policy_are_enforced() {
    let (server, ca) = Server::tls(vec![Reply::json(r#"{"dis":"TLS fixture"}"#)]);
    let client = RepositoryClient::with_config(
        &server.base,
        RepositoryConfig {
            ca_cert_pem: Some(ca),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(client.ping(&token()).await.unwrap().dis, "TLS fixture");
    let (server, _) = Server::tls(vec![Reply::json(r#"{"dis":"Untrusted"}"#)]);
    assert_eq!(
        RepositoryClient::new(&server.base)
            .unwrap()
            .ping(&token())
            .await
            .unwrap_err(),
        RepositoryError::Transport
    );
    assert!(server.requests().is_empty());
    let (server, ca) = Server::tls(vec![Reply::json(r#"{"dis":"Wrong hostname"}"#)]);
    let bad_host = server.base.replace("localhost", "127.0.0.1");
    assert_eq!(
        RepositoryClient::with_config(
            &bad_host,
            RepositoryConfig {
                ca_cert_pem: Some(ca),
                ..Default::default()
            }
        )
        .unwrap()
        .ping(&token())
        .await
        .unwrap_err(),
        RepositoryError::Transport
    );
    assert!(server.requests().is_empty());
    let (server, ca) = Server::tls(vec![Reply::redirect("http://localhost/download")]);
    let mut cfg = RepositoryConfig {
        ca_cert_pem: Some(ca),
        ..Default::default()
    };
    cfg.limits.redirects = 1;
    assert_eq!(
        RepositoryClient::with_config(&server.base, cfg)
            .unwrap()
            .ping(&token())
            .await
            .unwrap_err(),
        RepositoryError::Redirect
    );
}

#[tokio::test]
async fn bounded_json_rejects_depth_nodes_duplicates_and_byte_overflow() {
    let cases = [
        (
            r#"{"dis":"a","extra":[[[]]]}"#,
            RepositoryLimits {
                metadata_depth: 2,
                ..Default::default()
            },
            RepositoryError::Limit("JSON depth"),
        ),
        (
            r#"{"dis":"a","extra":[0,1,2]}"#,
            RepositoryLimits {
                metadata_nodes: 4,
                ..Default::default()
            },
            RepositoryError::Limit("JSON nodes"),
        ),
        (
            r#"{"dis":"a","dis":"b"}"#,
            RepositoryLimits::default(),
            RepositoryError::Metadata("JSON syntax"),
        ),
        (
            r#"{"dis":"abcdef"}"#,
            RepositoryLimits {
                metadata_bytes: 8,
                ..Default::default()
            },
            RepositoryError::Limit("body bytes"),
        ),
    ];
    for (body, limits, want) in cases {
        let server = Server::new(vec![Reply::json(body)]);
        assert_eq!(
            RepositoryClient::with_config(&server.base, RepositoryConfig { limits, ..config() })
                .unwrap()
                .ping(&token())
                .await
                .unwrap_err(),
            want
        );
    }
    // A record limit does not accidentally cap that record's required fields.
    let server = Server::new(vec![Reply::json(&format!("[{ROW}]"))]);
    assert_eq!(
        RepositoryClient::with_config(
            &server.base,
            RepositoryConfig {
                limits: RepositoryLimits {
                    records: 1,
                    ..Default::default()
                },
                ..config()
            }
        )
        .unwrap()
        .versions("downloaded", None, None, &token())
        .await
        .unwrap()
        .len(),
        1
    );
}

#[tokio::test]
async fn regression_pr06_contextual_integer_lexemes_are_exact() {
    for (wire, expected) in [
        ("1.0", 1),
        ("1e0", 1),
        ("0e1000", 0),
        ("9.007199254740993e15", 9007199254740993),
        ("9007199254740993.0", 9007199254740993),
        ("9223372036854775807.0", i64::MAX as u64),
        ("\"42\"", 42),
    ] {
        let server = Server::new(vec![Reply::json(&format!(
            r#"{{"libs":[],"total":{wire}}}"#
        ))]);
        assert_eq!(
            client(&server)
                .search("*", None, &token())
                .await
                .unwrap()
                .total,
            Some(expected)
        );
    }
}

#[test]
fn regression_pr06_library_separator_grammar_is_pinned() {
    let version = "1.0.0".parse().unwrap();
    for name in ["fooBar", "foo_", "foo__bar", "foo_.bar"] {
        assert!(LibraryPin::new(name, version).is_err(), "{name}");
    }
    assert!(LibraryPin::new("foo_bar1.baz2", version).is_ok());
}

#[tokio::test]
async fn regression_pr06_mirror_receipt_retains_advertisement_origin() {
    let metadata_server = Server::new(vec![Reply::json(&format!("[{ROW}]"))]);
    let metadata = client(&metadata_server)
        .versions("downloaded", None, None, &token())
        .await
        .unwrap()
        .remove(0);
    let mirror = Server::new(vec![Reply::bytes("application/xetolib", ZIP)]);
    let artifact = client(&mirror)
        .fetch(&pin(), Some((&metadata).into()), None, &token())
        .await
        .unwrap();
    let observed = artifact.metadata_source.as_ref().unwrap();
    assert!(
        observed
            .request_url
            .as_str()
            .starts_with(&metadata_server.base)
    );
    assert!(
        observed
            .final_url
            .as_str()
            .starts_with(&metadata_server.base)
    );
    assert!(artifact.source.final_url.as_str().starts_with(&mirror.base));
    assert_ne!(
        observed.final_url.origin(),
        artifact.source.final_url.origin()
    );
    assert_eq!(artifact.integrity, Integrity::Advertised);
}

#[tokio::test]
async fn contextual_integer_domains_and_typed_errors_use_exact_core_semantics() {
    for wire in [
        "1.1",
        "9007199254740993.1",
        "9223372036854775808",
        "9223372036854775808.0",
        "1e1000",
        "-1",
        "\"1.0\"",
        "\"1e0\"",
        "true",
    ] {
        let server = Server::new(vec![Reply::json(&format!(
            r#"{{"libs":[],"total":{wire}}}"#
        ))]);
        assert!(
            matches!(
                client(&server).search("*", None, &token()).await,
                Err(RepositoryError::Metadata(_))
            ),
            "{wire}"
        );
    }
    for status in ["4.04e2", "404.0", "\"404\""] {
        let mut reply = Reply::json(&format!(
            r#"{{"spec":"sys.repo::UnknownLibErr","status":{status},"dis":"absent","lib":"downloaded"}}"#
        ));
        reply.status = 410;
        let server = Server::new(vec![reply]);
        assert_eq!(
            client(&server)
                .fetch(&pin(), None, None, &token())
                .await
                .unwrap_err(),
            RepositoryError::Api {
                status: 410,
                advisory_status: Some(404),
                kind: RepoErrorKind::UnknownLibrary
            }
        );
    }
    for status in [
        "403.9",
        "99.0",
        "600e0",
        "9223372036854775807.0",
        "\"404.0\"",
    ] {
        let mut reply = Reply::json(&format!(
            r#"{{"spec":"sys.repo::UnknownLibErr","status":{status},"dis":"absent","lib":"downloaded"}}"#
        ));
        reply.status = 404;
        let server = Server::new(vec![reply]);
        assert!(
            matches!(
                client(&server).fetch(&pin(), None, None, &token()).await,
                Err(RepositoryError::Metadata(_))
            ),
            "{status}"
        );
    }
}

#[tokio::test]
async fn lowercase_separator_names_are_checked_at_every_metadata_boundary() {
    for name in ["fooBar", "foo_", "foo__bar", "foo_.bar"] {
        let version = ROW.replace("downloaded", name);
        let dependency = ROW.replace("\"sys\"", &format!("\"{name}\""));
        let search = format!(
            r#"{{"libs":[{{"lib":"{name}","latestVersion":"1.0.0","latestMaturity":"stable"}}]}}"#
        );
        let server = Server::new(vec![
            Reply::json(&format!("[{version}]")),
            Reply::json(&format!("[{dependency}]")),
            Reply::json(&search),
        ]);
        let client = client(&server);
        assert!(matches!(
            client.versions("downloaded", None, None, &token()).await,
            Err(RepositoryError::Metadata(_))
        ));
        assert!(matches!(
            client.versions("downloaded", None, None, &token()).await,
            Err(RepositoryError::Metadata(_))
        ));
        assert!(matches!(
            client.search("*", None, &token()).await,
            Err(RepositoryError::Metadata(_))
        ));
        let mut error = Reply::json(&format!(
            r#"{{"spec":"sys.repo::UnknownLibErr","status":404,"dis":"absent","lib":"{name}"}}"#
        ));
        error.status = 404;
        let server = Server::new(vec![error]);
        assert!(matches!(
            RepositoryClient::with_config(&server.base, config())
                .unwrap()
                .fetch(&pin(), None, None, &token())
                .await,
            Err(RepositoryError::Metadata(_))
        ));
    }
}

#[tokio::test]
async fn caller_metadata_stays_distinct_from_an_observed_advertisement() {
    let metadata = RepoLibVersion {
        pin: pin(),
        maturity: Maturity::Stable,
        availability: Availability::Available,
        availability_msg: None,
        doc: None,
        depends: None,
        digest: Some(DIGEST.parse().unwrap()),
    };
    for expected in [None, Some(DIGEST.parse().unwrap())] {
        let server = Server::new(vec![Reply::bytes("application/xetolib", ZIP)]);
        let artifact = client(&server)
            .fetch(
                &pin(),
                Some(MetadataEvidence::Supplied(&metadata)),
                expected,
                &token(),
            )
            .await
            .unwrap();
        assert_eq!(artifact.metadata.as_ref(), Some(&metadata));
        assert_eq!(artifact.metadata_source, None);
        assert_eq!(
            artifact.integrity,
            if expected.is_some() {
                Integrity::SuppliedMetadataAndCaller
            } else {
                Integrity::SuppliedMetadata
            }
        );
    }
    let metadata_server = Server::new(vec![
        Reply::redirect("/catalog"),
        Reply::json(&format!("[{ROW}]")),
    ]);
    let mut cfg = config();
    cfg.limits.redirects = 1;
    let advertisement = RepositoryClient::with_config(&metadata_server.base, cfg)
        .unwrap()
        .versions("downloaded", None, None, &token())
        .await
        .unwrap()
        .remove(0);
    assert_eq!(advertisement.source().redirects, 1);
    assert_eq!(advertisement.source().final_url.path(), "/catalog");
    let mut changed = advertisement.metadata().clone();
    changed.doc = Some("caller changed metadata".into());
    let server = Server::new(vec![Reply::bytes("application/xetolib", ZIP)]);
    let artifact = client(&server)
        .fetch(&pin(), Some((&changed).into()), None, &token())
        .await
        .unwrap();
    assert_eq!(artifact.metadata_source, None);
    assert_eq!(artifact.integrity, Integrity::SuppliedMetadata);
}

#[tokio::test]
async fn lexical_json_preserves_strings_and_rejects_invalid_syntax() {
    let server = Server::new(vec![Reply::json(
        r#"{"dis":"quote\" slash\\ \u2713","extra":[true,null,1.5,{"n":2}]}"#,
    )]);
    let ping = client(&server).ping(&token()).await.unwrap();
    assert_eq!(ping.dis, "quote\" slash\\ ✓");
    assert_eq!(ping.extra["extra"][2], 1.5);
    for body in [
        r#"{"dis":"a",}"#,
        r#"{"dis":"a","extra":[1,]}"#,
        r#"{"dis":"a","extra":01}"#,
        r#"{"dis":"a","extra":1.}"#,
        r#"{"dis":"a","extra":1e}"#,
        r#"{"dis":"a","extra":truex}"#,
        r#"{"dis":"a","d\u0069s":"b"}"#,
        r#"{"dis":"a"}[]"#,
    ] {
        let server = Server::new(vec![Reply::json(body)]);
        assert_eq!(
            client(&server).ping(&token()).await.unwrap_err(),
            RepositoryError::Metadata("JSON syntax"),
            "{body}"
        );
    }
}
