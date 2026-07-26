use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::ontology::DefNamespace;
use haystack_server::HaystackServer;
use haystack_server::auth::AuthManager;
use haystack_server::auth::users::load_users_from_toml;

pub struct ServeConfig<'a> {
    pub port: u16,
    pub file: Option<&'a str>,
    pub users_file: Option<&'a str>,
    pub host: Option<&'a str>,
    pub demo: bool,
}

pub fn run(cfg: ServeConfig<'_>) {
    env_logger::init();

    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("Error: failed to create runtime: {e}");
        std::process::exit(1);
    });
    rt.block_on(async {
        // Shared with every graph below. Without this the graph has no
        // namespace and every spec-match filter (`ph::Point`) is refused, even
        // though the server itself holds the ontology — they are separate
        // owners, and only the graph's copy is consulted when evaluating a
        // filter.
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

        HaystackServer::new(graph)
            // The server keeps its own mutable copy: the lib load/unload
            // endpoints mutate it, and the graphs must not see that shift.
            .with_namespace((*ns).clone())
            .with_auth(auth)
            .host(bind_host)
            .port(cfg.port)
            // Printed after the bind succeeds, so it is a readiness signal as well
            // as an address. The old banner was printed before binding and reported
            // the REQUESTED port, which made it useless for both purposes and left
            // `--port 0` undiscoverable.
            .run_reporting_addr(|addr| println!("Listening on {addr}"))
            .await
            .unwrap_or_else(|e| {
                eprintln!("Server error: {}", e);
                std::process::exit(1);
            });
    });
}
