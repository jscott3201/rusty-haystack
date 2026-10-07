//! Network qualification of configured TLS, application auth, and replay policy.
mod support;
use haystack_client::{AuthMode, ClientConfig, ClientError, HaystackClient, tls::TlsConfig};
use haystack_core::kinds::{Kind, Number};
use std::time::Duration;
use support::{Certificates, Challenge, Domain, Final, Options, SENTINEL, Server};

fn config(tls: TlsConfig) -> ClientConfig {
    ClientConfig {
        tls: Some(tls),
        ..ClientConfig::default()
    }
}

#[tokio::test]
async fn configured_tls_survives_scram_and_new_operation_connections() {
    let certs = Certificates::new();
    let server = Server::start(&certs, true, Options::default());
    let client = HaystackClient::connect_with_tls(&server.url, "user", "password", &certs.tls)
        .await
        .expect("SCRAM over mandatory mTLS");
    client
        .read("site", None)
        .await
        .expect("POST on a new TLS connection");
    client.about().await.expect("GET on a new TLS connection");
    let state = server.state.lock().unwrap();
    assert_eq!(state.domain, 2);
    assert_eq!(state.requests.len(), 4);
    assert!(state.requests[2].0.starts_with("POST /api/read "));
    assert!(state.requests[3].0.starts_with("GET /api/about "));
    for (_, auth) in &state.requests[2..] {
        assert_eq!(auth, "BEARER authToken=session-token");
    }
}

#[tokio::test]
async fn ca_only_and_mtls_profiles_support_basic_and_scram() {
    let certs = Certificates::new();
    for mtls in [false, true] {
        for auth_mode in [AuthMode::Scram, AuthMode::Basic] {
            let server = Server::start(&certs, mtls, Options::default());
            let tls = if mtls {
                certs.tls.clone()
            } else {
                TlsConfig::with_ca(certs.ca.clone())
            };
            let config = ClientConfig {
                auth_mode,
                ..config(tls)
            };
            let client =
                HaystackClient::connect_with_config(&server.url, "user", "password", &config)
                    .await
                    .unwrap();
            client.about().await.unwrap();
            client.read("site", None).await.unwrap();
            let state = server.state.lock().unwrap();
            assert_eq!(state.domain, 2);
            if auth_mode == AuthMode::Basic {
                assert_eq!(state.requests.len(), 2);
                for (_, auth) in &state.requests {
                    assert_eq!(auth, "Basic dXNlcjpwYXNzd29yZA==");
                }
            }
        }
    }
}

#[tokio::test]
async fn configured_ca_bundle_uses_all_roots() {
    let certs = Certificates::new();
    let other = Certificates::new();
    let server = Server::start(&certs, false, Options::default());
    let mut bundle = other.ca.clone();
    bundle.extend_from_slice(&certs.ca);
    let client = HaystackClient::connect_with_config(
        &server.url,
        "user",
        "password",
        &config(TlsConfig::with_ca(bundle)),
    )
    .await
    .unwrap();
    client.about().await.unwrap();
    assert_eq!(server.state.lock().unwrap().domain, 1);
}

#[tokio::test]
async fn tls_rejections_never_dispatch_http_or_domain_operations() {
    let certs = Certificates::new();
    let other = Certificates::new();
    for case in [
        "wrong-ca",
        "hostname",
        "missing-client",
        "wrong-client",
        "default-trust",
    ] {
        let server = Server::start(&certs, true, Options::default());
        let mut tls = certs.tls.clone();
        let mut url = server.url.clone();
        match case {
            "wrong-ca" => tls.ca_cert_pem = Some(other.ca.clone()),
            "hostname" => url = url.replace("localhost", "127.0.0.1"),
            "missing-client" => tls = TlsConfig::with_ca(certs.ca.clone()),
            "wrong-client" => tls = certs.unauthorized.clone(),
            "default-trust" => tls.ca_cert_pem = None,
            _ => unreachable!(),
        }
        let result =
            HaystackClient::connect_with_config(&url, "user", "password", &config(tls)).await;
        assert!(matches!(result, Err(ClientError::Transport(_))), "{case}");
        let state = server.state.lock().unwrap();
        assert!(state.requests.is_empty(), "{case}");
        assert_eq!(state.domain, 0, "{case}");
    }
}

#[tokio::test]
async fn invalid_credentials_and_server_signatures_are_rejected() {
    let certs = Certificates::new();
    for (password, final_message) in [
        ("wrong", Final::Valid),
        ("password", Final::WrongSignature),
        ("password", Final::WrongHash),
        ("password", Final::DuplicateToken),
        ("password", Final::DuplicateHeader),
        ("password", Final::EmptyToken),
        ("password", Final::Malformed),
    ] {
        let server = Server::start(
            &certs,
            false,
            Options {
                final_message,
                ..Options::default()
            },
        );
        let result = HaystackClient::connect_with_config(
            &server.url,
            "user",
            password,
            &config(TlsConfig::with_ca(certs.ca.clone())),
        )
        .await;
        let Err(ClientError::AuthFailed(message)) = result else {
            panic!("expected typed authentication rejection")
        };
        assert!(!message.contains(SENTINEL));
        assert_eq!(server.state.lock().unwrap().domain, 0);
    }
}

#[tokio::test]
async fn malformed_negotiation_is_rejected_before_sending_a_proof() {
    let certs = Certificates::new();
    for challenge in [
        Challenge::UnsupportedHash,
        Challenge::MissingHash,
        Challenge::DuplicateHash,
        Challenge::DuplicateToken,
        Challenge::EmptyToken,
        Challenge::Malformed,
        Challenge::OversizedHeader,
        Challenge::OversizedData,
        Challenge::DuplicateNonce,
        Challenge::EmptySalt,
        Challenge::ZeroIterations,
        Challenge::ExcessiveIterations,
        Challenge::WrongNonce,
        Challenge::MultipleScram,
    ] {
        let server = Server::start(
            &certs,
            false,
            Options {
                challenge,
                ..Options::default()
            },
        );
        let result = HaystackClient::connect_with_config(
            &server.url,
            "user",
            "password",
            &config(TlsConfig::with_ca(certs.ca.clone())),
        )
        .await;
        let Err(ClientError::AuthFailed(message)) = result else {
            panic!("expected typed authentication rejection for {challenge:?}")
        };
        assert!(!message.contains(SENTINEL));
        let state = server.state.lock().unwrap();
        assert_eq!(state.requests.len(), 1, "{challenge:?}");
        assert_eq!(state.domain, 0);
    }
}

#[tokio::test]
async fn scram_can_be_selected_from_multiple_auth_schemes() {
    let certs = Certificates::new();
    for challenge in [Challenge::CombinedSchemes, Challenge::SeparateSchemes] {
        let server = Server::start(
            &certs,
            false,
            Options {
                challenge,
                ..Options::default()
            },
        );
        HaystackClient::connect_with_config(
            &server.url,
            "user",
            "password",
            &config(TlsConfig::with_ca(certs.ca.clone())),
        )
        .await
        .unwrap();
        assert_eq!(server.state.lock().unwrap().requests.len(), 2);
    }
}

#[tokio::test]
async fn one_deadline_covers_both_handshake_requests() {
    let certs = Certificates::new();
    let server = Server::start(
        &certs,
        false,
        Options {
            auth_delay: Duration::from_millis(250),
            ..Options::default()
        },
    );
    let budget = Duration::from_millis(450);
    let config = ClientConfig {
        auth_timeout: budget,
        timeout: Duration::from_secs(5),
        ..config(TlsConfig::with_ca(certs.ca.clone()))
    };
    let result =
        HaystackClient::connect_with_config(&server.url, "user", "password", &config).await;
    assert!(matches!(result, Err(ClientError::Timeout(value)) if value == budget));
    let state = server.state.lock().unwrap();
    assert_eq!(
        state.requests.len(),
        2,
        "both individual phases fit; their sum exceeds the budget"
    );
    assert_eq!(state.domain, 0);
}

#[tokio::test]
async fn rejected_bearer_is_not_refreshed_or_retried() {
    let certs = Certificates::new();
    let server = Server::start(
        &certs,
        false,
        Options {
            domain: Domain::Expired,
            ..Options::default()
        },
    );
    let client = HaystackClient::connect_with_config(
        &server.url,
        "user",
        "password",
        &config(TlsConfig::with_ca(certs.ca.clone())),
    )
    .await
    .unwrap();
    assert!(matches!(
        client.about().await,
        Err(ClientError::AuthFailed(_))
    ));
    let state = server.state.lock().unwrap();
    assert_eq!(state.domain, 1);
    assert_eq!(state.requests.len(), 3);
}

#[tokio::test]
async fn authenticated_redirects_are_not_followed() {
    let certs = Certificates::new();
    let server = Server::start(
        &certs,
        false,
        Options {
            domain: Domain::Redirect,
            ..Options::default()
        },
    );
    let client = HaystackClient::connect_with_config(
        &server.url,
        "user",
        "password",
        &config(TlsConfig::with_ca(certs.ca.clone())),
    )
    .await
    .unwrap();
    assert!(
        matches!(client.about().await, Err(ClientError::ServerError(message)) if message.contains("307"))
    );
    assert_eq!(server.state.lock().unwrap().domain, 1);
}

#[tokio::test]
async fn lost_response_does_not_replay_an_effectful_post() {
    let certs = Certificates::new();
    let server = Server::start(
        &certs,
        true,
        Options {
            domain: Domain::DropResponse,
            ..Options::default()
        },
    );
    let client = HaystackClient::connect_with_tls(&server.url, "user", "password", &certs.tls)
        .await
        .unwrap();
    assert!(matches!(
        client
            .point_write("point", 8, Kind::Number(Number::unitless(42.0)))
            .await,
        Err(ClientError::Transport(_))
    ));
    let state = server.state.lock().unwrap();
    assert_eq!(state.domain, 1, "server observed the effect exactly once");
    assert_eq!(state.requests.len(), 3);
    assert!(state.requests[2].0.starts_with("POST /api/pointWrite "));
}

#[tokio::test]
async fn diagnostics_do_not_echo_peer_bodies_or_url_credentials() {
    let certs = Certificates::new();
    for domain in [Domain::ErrorBody, Domain::ErrorGrid, Domain::InvalidGrid] {
        let server = Server::start(
            &certs,
            false,
            Options {
                domain,
                ..Options::default()
            },
        );
        let config = ClientConfig {
            auth_mode: AuthMode::Basic,
            ..config(TlsConfig::with_ca(certs.ca.clone()))
        };
        let client = HaystackClient::connect_with_config(&server.url, "user", SENTINEL, &config)
            .await
            .unwrap();
        let error = client.about().await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains(SENTINEL));
    }
    for url in [
        format!("https://user:{SENTINEL}@localhost/api"),
        format!("ftp://localhost/{SENTINEL}"),
        format!("https://localhost/api?token={SENTINEL}"),
        "https://@localhost/api".into(),
    ] {
        let result =
            HaystackClient::connect_with_config(&url, "user", SENTINEL, &ClientConfig::default())
                .await;
        let Err(error @ ClientError::Connection(_)) = result else {
            panic!("expected URL rejection")
        };
        assert!(!format!("{error:?} {error}").contains(SENTINEL));
    }
    let tls = TlsConfig {
        client_cert_pem: SENTINEL.as_bytes().to_vec(),
        client_key_pem: SENTINEL.as_bytes().to_vec(),
        ca_cert_pem: Some(SENTINEL.as_bytes().to_vec()),
    };
    let config = config(tls);
    assert!(!format!("{config:?}").contains(SENTINEL));
    let error = config.build_reqwest_client().unwrap_err();
    assert!(!format!("{error:?} {error}").contains(SENTINEL));
}

#[test]
fn incomplete_identity_and_empty_ca_are_rejected_locally() {
    for tls in [
        TlsConfig {
            client_cert_pem: vec![1],
            client_key_pem: vec![],
            ca_cert_pem: None,
        },
        TlsConfig {
            client_cert_pem: vec![],
            client_key_pem: vec![1],
            ca_cert_pem: None,
        },
        TlsConfig::with_ca(Vec::new()),
    ] {
        assert!(matches!(
            config(tls).build_reqwest_client(),
            Err(ClientError::Connection(_))
        ));
    }
}
