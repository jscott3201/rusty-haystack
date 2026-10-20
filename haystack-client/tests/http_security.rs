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
    assert_eq!(state.requests.len(), 5);
    assert!(state.requests[3].0.starts_with("POST /api/read "));
    assert!(state.requests[4].0.starts_with("GET /api/about "));
    for (_, auth) in &state.requests[3..] {
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
        ("password", Final::MissingHash),
        ("password", Final::ShortSignature),
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
        Challenge::NonExtendedNonce,
        Challenge::InvalidUtf8,
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
        assert_eq!(state.requests.len(), 2, "{challenge:?}");
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
        assert_eq!(server.state.lock().unwrap().requests.len(), 3);
    }
}

#[tokio::test]
async fn bare_alternative_after_scram_authenticates() {
    let certs = Certificates::new();
    let server = Server::start(
        &certs,
        false,
        Options {
            challenge: Challenge::BareSchemeAfterScram,
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
    .expect("a bare alternative after SCRAM must not invalidate the supported challenge");
    client.about().await.unwrap();
    let state = server.state.lock().unwrap();
    assert_eq!(state.requests.len(), 4);
    assert_eq!(state.domain, 1);
    assert_eq!(state.requests[3].1, "BEARER authToken=session-token");
}

/// A running blocking task inhibits paused-clock auto-advance in Tokio. The
/// acknowledgement proves it is active before real TLS and PBKDF2 work begins.
/// Drop opens the channel even on unwind; ordinary completion also joins it.
struct ClockHold {
    release: Option<std::sync::mpsc::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl ClockHold {
    fn start() -> (Self, tokio::sync::oneshot::Receiver<()>) {
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::task::spawn_blocking(move || {
            let _ = started.send(());
            let _ = wait.recv();
        });
        (
            Self {
                release: Some(release),
                task: Some(task),
            },
            ready,
        )
    }
    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
    async fn join(&mut self) -> Result<(), String> {
        self.release();
        // Keep ownership across a cancelled join wait so outer cleanup can
        // still await the actual blocking task.
        let result = match self.task.as_mut() {
            Some(task) => task.await.map_err(|error| format!("clock hold: {error}")),
            None => Ok(()),
        };
        self.task = None;
        result
    }
}
impl Drop for ClockHold {
    fn drop(&mut self) {
        self.release();
    }
}

/// Real time only detects a hung fixture; it never decides whether the 700 ms
/// authentication budget was honored. A native thread does not inhibit Tokio's
/// virtual clock, and shutdown wakes and joins the thread without waiting.
struct RealDeadman {
    expired: tokio::sync::oneshot::Receiver<()>,
    stop: Option<std::sync::mpsc::Sender<()>>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl RealDeadman {
    fn start() -> Self {
        let (stop, wait) = std::sync::mpsc::channel();
        let (expired, notification) = tokio::sync::oneshot::channel();
        let task = std::thread::spawn(move || {
            if matches!(
                wait.recv_timeout(Duration::from_secs(10)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                let _ = expired.send(());
            }
        });
        Self {
            expired: notification,
            stop: Some(stop),
            task: Some(task),
        }
    }
}
impl Drop for RealDeadman {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let joined = task.join();
            if !std::thread::panicking() {
                joined.expect("real fixture deadman");
            }
        }
    }
}

#[derive(Debug)]
enum AuthCompletion {
    Finished {
        result: Result<(), ClientError>,
        elapsed: Duration,
    },
    Checkpoint {
        elapsed: Duration,
    },
}
#[derive(Debug)]
struct GatedAuthObservation {
    phases: Vec<(usize, bool, Duration)>,
    completion: Result<AuthCompletion, String>,
    requests: usize,
    domain: usize,
}

async fn gated_authentication(
    phase_delay: Duration,
    withhold_final_response: bool,
) -> GatedAuthObservation {
    let certs = Certificates::new();
    let (server, mut events) = Server::start_gated_auth(&certs);
    let state = server.state.clone();
    let (mut clock, ready) = ClockHold::start();
    let mut deadman = RealDeadman::start();
    let budget = Duration::from_millis(700);
    let config = ClientConfig {
        auth_timeout: budget,
        timeout: Duration::from_secs(5),
        ..config(TlsConfig::with_ca(certs.ca.clone()))
    };
    let start = tokio::time::Instant::now();
    let mut authentication = Box::pin(HaystackClient::connect_with_config(
        &server.url,
        "user",
        "password",
        &config,
    ));
    let mut phases = Vec::with_capacity(3);
    // Own the gate outside the driven future so cancellation/deadman cleanup
    // can release it after dropping the pending authentication request.
    let mut held: Option<support::AuthPhase> = None;
    let exchange = async {
        ready
            .await
            .map_err(|_| "clock hold did not start".to_owned())?;
        for number in 1..=3 {
            let phase = tokio::select! {
                phase = events.recv() => phase.ok_or_else(|| "phase channel closed".to_owned())?,
                result = &mut authentication => return Err(format!(
                    "authentication ended before phase {number}: {:?}", result.map(|_| ())
                )),
            };
            phases.push((phase.number, phase.valid_response, start.elapsed()));
            held = Some(phase);
            if number < 3 || !withhold_final_response {
                // Advance only between observed phases, below the deadline.
                // Timer expiry itself is tested with natural auto-advance.
                tokio::time::advance(phase_delay).await;
                if let Some(phase) = held.take() {
                    phase
                        .release
                        .send(())
                        .map_err(|_| "response gate closed".to_owned())?;
                }
            }
        }
        if withhold_final_response {
            // The actual validated proof is waiting at virtual 500 ms.
            // A fresh 700 ms per-request timeout would still be pending at
            // 750 ms; the shared deadline must finish before this checkpoint.
            // Register both competing futures immediately after release.
            // Joining the blocking task here would introduce an unrelated
            // await before the deadline/checkpoint race; cleanup joins it.
            clock.release();
            tokio::select! {
                result = &mut authentication => Ok(AuthCompletion::Finished {
                    result: result.map(|_| ()),
                    elapsed: start.elapsed(),
                }),
                _ = tokio::time::sleep_until(start + Duration::from_millis(750)) => {
                    Ok(AuthCompletion::Checkpoint { elapsed: start.elapsed() })
                },
            }
        } else {
            let result = (&mut authentication).await.map(|_| ());
            Ok(AuthCompletion::Finished {
                result,
                elapsed: start.elapsed(),
            })
        }
    };
    let mut completion = tokio::select! {
        result = exchange => result,
        _ = &mut deadman.expired => Err("real fixture deadman expired".to_owned()),
    };

    // Retire every owned resource before returning evidence to assertions.
    drop(authentication);
    if let Some(phase) = held.take() {
        let _ = phase.release.send(());
    }
    drop(events);
    if let Err(error) = clock.join().await {
        completion = Err(error);
    }
    drop(server);
    drop(deadman);
    let (requests, domain) = {
        let state = state.lock().unwrap();
        (state.requests.len(), state.domain)
    };
    GatedAuthObservation {
        phases,
        completion,
        requests,
        domain,
    }
}

#[tokio::test(start_paused = true)]
async fn one_deadline_covers_all_three_handshake_requests() {
    let observed = gated_authentication(Duration::from_millis(250), true).await;
    assert_eq!(
        observed.phases,
        vec![
            (1, true, Duration::ZERO),
            (2, true, Duration::from_millis(250)),
            (3, true, Duration::from_millis(500)),
        ]
    );
    assert_eq!(
        observed.requests, 3,
        "individual phases fit; all three exceed the total budget"
    );
    assert_eq!(observed.domain, 0);
    match observed.completion {
        Ok(AuthCompletion::Finished { result, elapsed }) => {
            assert!(
                matches!(result, Err(ClientError::Timeout(value)) if value == Duration::from_millis(700))
            );
            assert!(
                (Duration::from_millis(700)..Duration::from_millis(750)).contains(&elapsed),
                "the 700 ms deadline must precede response three's 750 ms checkpoint: {elapsed:?}"
            );
        }
        Ok(AuthCompletion::Checkpoint { elapsed }) => {
            panic!("authentication still pending at the {elapsed:?} checkpoint")
        }
        Err(error) => panic!("{error}"),
    }
}

#[tokio::test(start_paused = true)]
async fn three_handshake_phases_complete_within_one_deadline() {
    let observed = gated_authentication(Duration::from_millis(200), false).await;
    assert_eq!(
        observed.phases,
        vec![
            (1, true, Duration::ZERO),
            (2, true, Duration::from_millis(200)),
            (3, true, Duration::from_millis(400)),
        ]
    );
    assert_eq!(observed.requests, 3);
    assert_eq!(observed.domain, 0);
    match observed.completion {
        Ok(AuthCompletion::Finished { result, elapsed }) => {
            assert!(result.is_ok(), "all three phases fit: {result:?}");
            assert_eq!(elapsed, Duration::from_millis(600));
        }
        other => panic!("expected authentication within the shared budget: {other:?}"),
    }
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
    assert_eq!(state.requests.len(), 4);
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
    assert_eq!(state.requests.len(), 4);
    assert!(state.requests[3].0.starts_with("POST /api/pointWrite "));
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

#[tokio::test]
async fn handshake_tokens_are_response_scoped() {
    let certs = Certificates::new();
    for tokens in [
        support::Tokens::Rotate,
        support::Tokens::Absent,
        support::Tokens::Introduce,
        support::Tokens::Remove,
    ] {
        let server = Server::start(
            &certs,
            false,
            Options {
                tokens,
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
        client.read("site", None).await.unwrap();
        assert_eq!(server.state.lock().unwrap().requests.len(), 4);
    }
}

#[tokio::test]
async fn empty_http_auth_list_members_are_tolerated_in_each_challenge() {
    use support::EmptyMembers;
    let certs = Certificates::new();
    for empty in [
        EmptyMembers::Leading,
        EmptyMembers::Interior,
        EmptyMembers::Trailing,
    ] {
        for discovery in [true, false] {
            let options = if discovery {
                Options {
                    discovery_empty: empty,
                    ..Options::default()
                }
            } else {
                Options {
                    challenge_empty: empty,
                    ..Options::default()
                }
            };
            let server = Server::start(&certs, false, options);
            let client = HaystackClient::connect_with_config(
                &server.url,
                "user",
                "password",
                &config(TlsConfig::with_ca(certs.ca.clone())),
            )
            .await
            .unwrap_or_else(|error| panic!("empty={empty:?}, discovery={discovery}: {error}"));
            client.read("site", None).await.unwrap();
            assert_eq!(server.state.lock().unwrap().requests.len(), 4);
        }
    }
}
