#![allow(dead_code)]
mod history_stream_support;
mod support;
use haystack_client::{
    AuthMode, ClientConfig, HaystackClient,
    history::{HistoryStreamEnd, HistoryStreamError},
    tls::TlsConfig,
};
use haystack_core::codecs::history::HistoryTerminal;
use history_stream_support::*;
use support::{Certificates, Domain, Options, Server};

#[tokio::test]
async fn streaming_reuses_ca_mtls_and_auth_on_new_connections() {
    let certs = Certificates::new();
    for auth_mode in [AuthMode::Scram, AuthMode::Basic] {
        let expected = result(4, 2);
        let server = Server::start_with_body(
            &certs,
            true,
            Options::default(),
            Some(String::from_utf8(encoded(&expected)).unwrap()),
        );
        let client = HaystackClient::connect_with_config(
            &server.url,
            "user",
            "password",
            &ClientConfig {
                auth_mode,
                tls: Some(certs.tls.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        for _ in 0..2 {
            let mut session = client
                .his_read_stream(&request(), Default::default())
                .await
                .unwrap();
            assert_eq!(session.header().metadata, expected.metadata);
            let batch = session.next().await;
            assert_eq!(batch.samples, expected.samples);
            assert_eq!(
                batch.end,
                Some(HistoryStreamEnd::Remote(HistoryTerminal::Complete))
            );
            session.close().await;
        }
        let state = server.state.lock().unwrap();
        assert_eq!(state.domain, 2);
        let auth_count = if auth_mode == AuthMode::Scram { 3 } else { 0 };
        assert_eq!(state.requests.len(), auth_count + 2);
        for (line, authorization) in &state.requests[auth_count..] {
            assert!(line.starts_with("POST /api/hisRead "));
            assert!(authorization.starts_with(if auth_mode == AuthMode::Scram {
                "BEARER "
            } else {
                "Basic "
            }));
        }
    }
}
#[tokio::test]
async fn streaming_enforces_ca_hostname_and_mandatory_identity_at_dispatch() {
    let certs = Certificates::new();
    for case in ["missing-client", "wrong-client", "default-ca", "wrong-host"] {
        let server = Server::start_with_body(
            &certs,
            true,
            Options::default(),
            Some(String::from_utf8(encoded(&result(1, 0))).unwrap()),
        );
        let mut tls = certs.tls.clone();
        let mut url = server.url.clone();
        match case {
            "missing-client" => {
                tls.client_cert_pem.clear();
                tls.client_key_pem.clear();
            }
            "wrong-client" => tls = certs.unauthorized.clone(),
            "default-ca" => tls.ca_cert_pem = None,
            "wrong-host" => url = url.replace("localhost", "127.0.0.1"),
            _ => unreachable!(),
        }
        // Basic construction is local, so this exercises the stream request's
        // TLS connection itself, including no preceding SCRAM connection.
        let client = HaystackClient::connect_with_config(
            &url,
            "user",
            "password",
            &ClientConfig {
                auth_mode: AuthMode::Basic,
                tls: Some(tls),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(
                client.his_read_stream(&request(), Default::default()).await,
                Err(HistoryStreamError::Transport)
            ),
            "{case}"
        );
        assert_eq!(server.state.lock().unwrap().domain, 0);
    }
}
#[tokio::test]
async fn streaming_does_not_redirect_reauthenticate_or_replay() {
    let certs = Certificates::new();
    for (domain, error) in [
        (Domain::Redirect, HistoryStreamError::Http(307)),
        (Domain::Expired, HistoryStreamError::Credentials),
        (Domain::DropResponse, HistoryStreamError::Transport),
    ] {
        let server = Server::start(
            &certs,
            false,
            Options {
                domain,
                ..Default::default()
            },
        );
        let client = HaystackClient::connect_with_config(
            &server.url,
            "user",
            "password",
            &ClientConfig {
                tls: Some(TlsConfig::with_ca(certs.ca.clone())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(client.his_read_stream(&request(),Default::default()).await,Err(actual) if actual==error)
        );
        let state = server.state.lock().unwrap();
        assert_eq!(state.domain, 1);
        assert_eq!(state.requests.len(), 4);
    }
}
#[tokio::test]
async fn streaming_rejects_nonidentity_encoding_and_unselected_formats() {
    for encoding in ["gzip", "br", "deflate", "zstd", "unknown"] {
        let server = history_stream_support::Server::bytes(
            encoded(&result(1, 0)),
            vec![64],
            &format!("Content-Encoding: {encoding}\r\n"),
        )
        .await;
        let client = client(&server.url).await;
        assert!(matches!(
            client.his_read_stream(&request(), Default::default()).await,
            Err(HistoryStreamError::Protocol)
        ));
        server.finish().await;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    for format in ["application/json", "application/json;v=3", "text/trio"] {
        let mut config = config();
        config.wire_format = format.into();
        let client = HaystackClient::connect_with_config(&url, "user", "password", &config)
            .await
            .unwrap();
        assert!(matches!(
            client.his_read_stream(&request(), Default::default()).await,
            Err(HistoryStreamError::UnsupportedTransport)
        ));
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn malformed_non_date_ranges_are_rejected_before_dispatch() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = client(&format!("http://{}", listener.local_addr().unwrap())).await;
    for range in [
        "\"\\u雪雪\"",
        "[2024-01-01]",
        "{date:2024-01-01}",
        "X(\"date\")",
    ] {
        let mut request = request();
        request.range = range.into();
        assert!(matches!(
            client.his_read_stream(&request, Default::default()).await,
            Err(HistoryStreamError::Protocol)
        ));
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}
