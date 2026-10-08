use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::ontology::DefNamespace;
use haystack_server::HaystackServer;
use haystack_server::auth::AuthManager;
use haystack_server::auth::users::load_users_from_toml;
use haystack_server::cors::CorsPolicy;

pub struct ServeConfig<'a> {
    pub port: u16,
    pub file: Option<&'a str>,
    pub users_file: Option<&'a str>,
    pub host: Option<&'a str>,
    pub demo: bool,
    pub cors_origins: Vec<String>,
}

pub fn run(cfg: ServeConfig<'_>) {
    env_logger::init();

    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("Error: failed to create runtime: {e}");
        std::process::exit(1);
    });
    let result = rt.block_on(async {
        // The graph is the catalog authority shared by application adapters.
        let ns = std::sync::Arc::new(DefNamespace::load_standard().unwrap_or_else(|e| {
            eprintln!("Error loading ontology: {}", e);
            std::process::exit(1);
        }));

        let graph = if let Some(f) = cfg.file {
            eprintln!("Loading entities from: {}", f);

            let content = std::fs::read_to_string(f).unwrap_or_else(|e| {
                eprintln!("Error reading '{}': {}", f, e);
                std::process::exit(1);
            });

            let mime = if f.ends_with(".trio") {
                "text/trio"
            } else if f.ends_with(".json") {
                "application/json"
            } else {
                "text/zinc"
            };

            let codec = haystack_core::codecs::codec_for(mime).unwrap_or_else(|| {
                eprintln!("Error: unsupported format: {}", mime);
                std::process::exit(1);
            });
            let grid = codec.decode_grid(&content).unwrap_or_else(|e| {
                eprintln!("Error decoding: {}", e);
                std::process::exit(1);
            });

            let eg = EntityGraph::from_grid(&grid, Some(std::sync::Arc::clone(&ns)))
                .unwrap_or_else(|e| {
                    eprintln!("Error building graph: {}", e);
                    std::process::exit(1);
                });

            eprintln!("Loaded {} entities", eg.len());
            SharedGraph::new(eg)
        } else if cfg.demo {
            let entities = haystack_server::demo::demo_entities();
            let mut eg = EntityGraph::with_namespace(std::sync::Arc::clone(&ns));
            for e in entities {
                eg.add(e).unwrap_or_else(|e| {
                    eprintln!("Error adding demo entity: {}", e);
                    std::process::exit(1);
                });
            }
            eprintln!("Loaded {} demo entities", eg.len());
            SharedGraph::new(eg)
        } else {
            SharedGraph::new(EntityGraph::with_namespace(std::sync::Arc::clone(&ns)))
        };

        let auth = if let Some(uf) = cfg.users_file {
            let users = load_users_from_toml(uf).unwrap_or_else(|e| {
                eprintln!("Error loading users: {}", e);
                std::process::exit(1);
            });
            eprintln!("Loaded {} users", users.len());
            AuthManager::new(users, std::time::Duration::from_secs(3600))
        } else {
            AuthManager::empty()
        };

        let bind_host = cfg.host.unwrap_or("127.0.0.1");

        // No --cors-origin means no CORS headers at all, not an empty
        // allowlist: a server nobody asked to expose cross-origin should
        // behave exactly as it did before the flag existed.
        let cors = if cfg.cors_origins.is_empty() {
            CorsPolicy::Disabled
        } else {
            CorsPolicy::Allow(cfg.cors_origins)
        };

        let owner = HaystackServer::new(graph)
            .with_namespace((*ns).clone())
            .with_auth(auth)
            .with_cors(cors)
            .host(bind_host)
            .port(cfg.port)
            .start()
            .map_err(|error| error.to_string())?;
        let ready = match owner.ready().await {
            Ok(ready) => ready,
            Err(error) => {
                owner.terminated().await;
                return Err(error.to_string());
            }
        };
        println!("Listening on {}", ready.listeners[0].address);
        let signal = tokio::select! {
            result = shutdown_signal() => result,
            report = owner.terminated() => return report.close.map(|_| ()).map_err(|error| error.to_string()),
        };
        let closed = owner.close().await;
        if let Err(error) = &closed {
            eprintln!("Application close: {error}; waiting for owned cleanup to finish");
        }
        // A timeout reports failure, not completion. Keep the runtime driving
        // until queued/running work and cleanup hooks have actually finished.
        owner.terminated().await;
        signal.map_err(|error| error.to_string())?;
        closed.map(|_| ()).map_err(|error| error.to_string())
    });
    // Runtime destruction is outside the asynchronous context and follows the
    // application's termination receipt, including late cleanup after timeout.
    drop(rt);
    if let Err(error) = result {
        eprintln!("Server error: {error}");
        std::process::exit(1);
    }
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
