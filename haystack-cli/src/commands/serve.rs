use haystack_core::graph::{EntityGraph, SharedGraph, SnapshotReader};
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
    pub federation_file: Option<&'a str>,
    pub snapshot_dir: Option<&'a str>,
    pub _snapshot_interval: u64,
}

pub fn run(cfg: ServeConfig<'_>) {
    env_logger::init();

    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| {
        eprintln!("Error: failed to create runtime: {e}");
        std::process::exit(1);
    });
    rt.block_on(async {
        let ns = DefNamespace::load_standard().unwrap_or_else(|e| {
            eprintln!("Error loading ontology: {}", e);
            std::process::exit(1);
        });

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

            let eg = EntityGraph::from_grid(&grid, None).unwrap_or_else(|e| {
                eprintln!("Error building graph: {}", e);
                std::process::exit(1);
            });

            eprintln!("Loaded {} entities", eg.len());
            SharedGraph::new(eg)
        } else if cfg.demo {
            let entities = haystack_server::demo::demo_entities();
            let mut eg = EntityGraph::new();
            for e in entities {
                eg.add(e).unwrap_or_else(|e| {
                    eprintln!("Error adding demo entity: {}", e);
                    std::process::exit(1);
                });
            }
            eprintln!("Loaded {} demo entities", eg.len());
            SharedGraph::new(eg)
        } else {
            SharedGraph::new(EntityGraph::new())
        };

        // Auto-restore from latest snapshot if snapshot-dir is specified and graph is empty
        if let Some(snap_dir) = cfg.snapshot_dir
            && graph.is_empty()
        {
            match SnapshotReader::find_latest(std::path::Path::new(snap_dir)) {
                Ok(Some(snap_path)) => match SnapshotReader::load(&snap_path, &graph) {
                    Ok(meta) => {
                        eprintln!(
                            "Restored {} entities from snapshot: {}",
                            meta.entity_count,
                            snap_path.display()
                        );
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to restore snapshot: {}", e);
                    }
                },
                Ok(None) => {
                    eprintln!("No snapshots found in '{}'", snap_dir);
                }
                Err(e) => {
                    eprintln!("Warning: error scanning snapshot dir: {}", e);
                }
            }
        }

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

        let federation = if let Some(ff) = cfg.federation_file {
            let fed = haystack_server::Federation::from_toml_file(ff).unwrap_or_else(|e| {
                eprintln!("Error loading federation config: {}", e);
                std::process::exit(1);
            });
            eprintln!("Loaded {} federation connectors", fed.connector_count());
            fed
        } else {
            haystack_server::Federation::new()
        };

        let bind_host = cfg.host.unwrap_or("0.0.0.0");
        eprintln!(
            "Starting Haystack HTTP server on {}:{}",
            bind_host, cfg.port
        );

        HaystackServer::new(graph)
            .with_namespace(ns)
            .with_auth(auth)
            .with_federation(federation)
            .host(bind_host)
            .port(cfg.port)
            .run()
            .await
            .unwrap_or_else(|e| {
                eprintln!("Server error: {}", e);
                std::process::exit(1);
            });
    });
}
