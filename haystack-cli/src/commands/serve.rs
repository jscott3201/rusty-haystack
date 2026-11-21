use haystack_app::ApplicationOwner;
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::ontology::DefNamespace;
use haystack_server::HaystackServer;
use haystack_server::auth::AuthManager;
use haystack_server::auth::users::load_users_from_toml;
use haystack_server::cors::CorsPolicy;

#[cfg(feature = "postgres")]
mod postgres;

pub struct ServeConfig<'a> {
    pub port: u16,
    pub file: Option<&'a str>,
    pub users_file: Option<&'a str>,
    pub host: Option<&'a str>,
    pub demo: bool,
    pub cors_origins: Vec<String>,
    pub postgres_config: Option<&'a str>,
}

pub fn run(cfg: ServeConfig<'_>) {
    env_logger::init();
    let result = tokio::runtime::Runtime::new()
        .map_err(|_| "failed to create runtime".to_string())
        .and_then(|rt| {
            let result = rt.block_on(serve(cfg));
            // Drop outside async only after owned work and cleanup terminated.
            drop(rt);
            result
        });
    if let Err(error) = result {
        eprintln!("Server error: {error}");
        std::process::exit(1);
    }
}

async fn serve(cfg: ServeConfig<'_>) -> Result<(), String> {
    serve_with_ready(cfg, |address| println!("Listening on {address}")).await
}

// Keep readiness notification on the coordinator: returning from this callback
// is the boundary before the shutdown wait may first be polled.
async fn serve_with_ready(
    cfg: ServeConfig<'_>,
    announce_ready: impl FnOnce(std::net::SocketAddr),
) -> Result<(), String> {
    #[cfg(not(feature = "postgres"))]
    if cfg.postgres_config.is_some() {
        return Err("PostgreSQL support is not installed; rebuild rusty-haystack-cli with --features postgres".into());
    }
    let auth = if let Some(path) = cfg.users_file {
        // TOML and hash parser diagnostics can include credential material.
        let users = load_users_from_toml(path).map_err(|_| "invalid or unreadable users file")?;
        if cfg.postgres_config.is_some() && users.is_empty() {
            return Err("PostgreSQL serving requires a nonempty --users file".into());
        }
        AuthManager::new(users, std::time::Duration::from_secs(3600))
    } else {
        if cfg.postgres_config.is_some() {
            return Err("PostgreSQL serving requires a nonempty --users file".into());
        }
        AuthManager::empty()
    };
    let cors = if cfg.cors_origins.is_empty() {
        CorsPolicy::Disabled
    } else {
        CorsPolicy::Allow(cfg.cors_origins.clone())
    };
    // Own both Unix handlers before a listener can bind or readiness can escape.
    // Constructing an async ctrl_c future alone would still register lazily.
    let mut shutdown = ShutdownSignals::register().map_err(|error| error.to_string())?;
    let owner = start(&cfg, auth, cors).await?;
    let ready = match owner.ready().await {
        Ok(ready) => ready,
        Err(error) => {
            owner.terminated().await;
            return Err(error.to_string());
        }
    };
    announce_ready(ready.listeners[0].address);
    let signal = tokio::select! {
        result = shutdown.wait() => result,
        report = owner.terminated() => return terminated(report),
    };
    let closed = owner.close().await;
    if let Err(error) = &closed {
        eprintln!("Application close: {error}; waiting for owned cleanup to finish");
    }
    // A close timeout is failure, not completion; keep driving late cleanup.
    let report = owner.terminated().await;
    signal.map_err(|error| error.to_string())?;
    closed.map_err(|error| error.to_string())?;
    terminated(report)
}

fn terminated(report: haystack_app::TerminationReport) -> Result<(), String> {
    if !report.cleanup_errors.is_empty() {
        return Err("application resource cleanup failed".into());
    }
    report.close.map(|_| ()).map_err(|error| error.to_string())
}

async fn start(
    cfg: &ServeConfig<'_>,
    auth: AuthManager,
    cors: CorsPolicy,
) -> Result<ApplicationOwner, String> {
    #[cfg(feature = "postgres")]
    if let Some(path) = cfg.postgres_config {
        return postgres::start(path, cfg.host.unwrap_or("127.0.0.1"), cfg.port, auth, cors).await;
    }
    let ns = std::sync::Arc::new(
        DefNamespace::load_standard().map_err(|e| format!("loading ontology: {e}"))?,
    );
    let graph = if let Some(path) = cfg.file {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("reading entity file: {e}"))?;
        let mime = if path.ends_with(".trio") {
            "text/trio"
        } else if path.ends_with(".json") {
            "application/json"
        } else {
            "text/zinc"
        };
        let codec = haystack_core::codecs::codec_for(mime).ok_or("unsupported entity format")?;
        let grid = codec
            .decode_grid(&content)
            .map_err(|e| format!("decoding entities: {e}"))?;
        EntityGraph::from_grid(&grid, Some(ns.clone()))
            .map_err(|e| format!("building graph: {e}"))?
    } else {
        let mut graph = EntityGraph::with_namespace(ns.clone());
        if cfg.demo {
            for entity in haystack_server::demo::demo_entities() {
                graph
                    .add(entity)
                    .map_err(|e| format!("adding demo entity: {e}"))?;
            }
        }
        graph
    };
    HaystackServer::new(SharedGraph::new(graph))
        .with_namespace((*ns).clone())
        .with_auth(auth)
        .with_cors(cors)
        .host(cfg.host.unwrap_or("127.0.0.1"))
        .port(cfg.port)
        .start()
        .map_err(|error| error.to_string())
}

struct ShutdownSignals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}
impl ShutdownSignals {
    fn register() -> std::io::Result<Self> {
        #[cfg(unix)]
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            #[cfg(unix)]
            interrupt: signal(SignalKind::interrupt())?,
            #[cfg(unix)]
            terminate: signal(SignalKind::terminate())?,
        })
    }
    async fn wait(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.interrupt.recv() => {},
                _ = self.terminate.recv() => {},
            }
            Ok(())
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
    const CHILD_MARKER: &str = "HAYSTACK_TEST_HELD_SERVE";
    const CHILD_TEST: &str = "commands::serve::tests::held_serve_child";

    struct HeldChild {
        child: Child,
        control: BufReader<UnixStream>,
    }
    impl Drop for HeldChild {
        fn drop(&mut self) {
            // Kill and reap only this test's child, including assertion failures
            // and a stalled control exchange. Never leave a detached waiter.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    impl HeldChild {
        fn spawn() -> Self {
            let (parent, child) = UnixStream::pair().expect("private control socket");
            for socket in [&parent, &child] {
                socket.set_read_timeout(Some(CONTROL_TIMEOUT)).unwrap();
                socket.set_write_timeout(Some(CONTROL_TIMEOUT)).unwrap();
            }
            let process = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
                .env(CHILD_MARKER, "1")
                .stdin(Stdio::from(OwnedFd::from(child.try_clone().unwrap())))
                .stdout(Stdio::from(OwnedFd::from(child)))
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn private serve child");
            Self {
                child: process,
                control: BufReader::new(parent),
            }
        }
        fn held_address(&mut self) -> SocketAddr {
            let deadline = Instant::now() + CONTROL_TIMEOUT;
            for _ in 0..32 {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "child did not reach held readiness");
                self.control
                    .get_ref()
                    .set_read_timeout(Some(remaining))
                    .unwrap();
                let mut line = String::new();
                assert_ne!(
                    self.control
                        .by_ref()
                        .take(4097)
                        .read_line(&mut line)
                        .expect("read held readiness"),
                    0,
                    "child exited before held readiness"
                );
                assert!(line.len() <= 4096, "unexpected child output size");
                if let Some(address) = line.trim().strip_prefix("HELD ") {
                    return address.parse().expect("child-owned bound address");
                }
            }
            panic!("child did not publish its bound address");
        }
        fn release_and_join(&mut self) -> std::process::ExitStatus {
            // A signal-terminated old implementation may have already closed
            // the socket. Preserve its exit status as the causal failure.
            let _ = self.control.get_mut().write_all(b"R");
            self.control
                .get_ref()
                .set_read_timeout(Some(CONTROL_TIMEOUT))
                .unwrap();
            let mut output = String::new();
            self.control
                .by_ref()
                .take(65536)
                .read_to_string(&mut output)
                .expect("bounded child completion");
            assert!(
                output.len() < 65536,
                "unexpected child completion output size"
            );
            // The fixture keeps stdout open through runtime destruction. EOF
            // precedes this wait only when that child has exited.
            let status = self.child.wait().expect("reap completed serve child");
            let mut stderr = String::new();
            self.child
                .stderr
                .take()
                .unwrap()
                .take(16384)
                .read_to_string(&mut stderr)
                .unwrap();
            assert!(
                status.success(),
                "signal caused abrupt exit while coordinator held: {status}; {stderr}"
            );
            assert!(
                output.lines().any(|line| line == "DRAINED"),
                "no owned-runtime completion receipt: {output}"
            );
            status
        }
    }

    fn held_coordinator_signal(signal: &str) {
        let mut child = HeldChild::spawn();
        let address = child.held_address();
        assert!(address.ip().is_loopback() && address.port() != 0);
        // The coordinator is blocked inside its readiness callback, before any
        // shutdown-wait poll. Real HTTP must run on the other runtime workers.
        let mut http = TcpStream::connect_timeout(&address, CONTROL_TIMEOUT).unwrap();
        http.set_read_timeout(Some(CONTROL_TIMEOUT)).unwrap();
        http.set_write_timeout(Some(CONTROL_TIMEOUT)).unwrap();
        let body = "ver:\"3.0\"\nfilter\n\"site\"\n";
        write!(http, "POST /api/read HTTP/1.1\r\nHost: {address}\r\nContent-Type: text/zinc\r\nAccept: text/zinc\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        http.take(16384).read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "real held-coordinator HTTP failed: {response}"
        );
        assert!(
            Command::new("kill")
                .args([signal, &child.child.id().to_string()])
                .status()
                .expect("signal exact test-owned child")
                .success()
        );
        assert!(child.release_and_join().success());
        let rebound = TcpListener::bind(address).expect("owned shutdown released exact listener");
        assert_eq!(rebound.local_addr().unwrap(), address);
    }

    #[test]
    fn interrupt_while_ready_coordinator_is_held_drains_owned_runtime() {
        held_coordinator_signal("-INT");
    }
    #[test]
    fn terminate_while_ready_coordinator_is_held_drains_owned_runtime() {
        held_coordinator_signal("-TERM");
    }
    #[test]
    #[ignore = "private subprocess fixture, invoked by the two held-coordinator tests"]
    fn held_serve_child() {
        assert_eq!(std::env::var(CHILD_MARKER).as_deref(), Ok("1"));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(serve_with_ready(
            ServeConfig {
                port: 0,
                file: None,
                users_file: None,
                host: Some("127.0.0.1"),
                demo: false,
                cors_origins: vec![],
                postgres_config: None,
            },
            |address| {
                // The parent alone can release this callback. No signal future
                // has been polled, even though the real listener is ready.
                println!("\nHELD {address}");
                std::io::stdout().flush().unwrap();
                let mut release = [0];
                std::io::stdin()
                    .read_exact(&mut release)
                    .expect("bounded coordinator release");
                assert_eq!(release, [b'R']);
            },
        ));
        drop(runtime);
        assert!(result.is_ok(), "serve cleanup failed: {result:?}");
        println!("DRAINED");
    }
}
