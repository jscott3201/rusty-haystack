use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::future::join_all;
use haystack_client::HaystackClient;
use haystack_client::transport::http::HttpTransport;
use haystack_core::data::HDict;
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_core::kinds::{HDateTime, HRef, Kind, Number};
use haystack_core::ontology::DefNamespace;
use haystack_server::HaystackServer;
use haystack_server::auth::AuthManager;
use haystack_server::auth::users::hash_password;
use haystack_server::connector::ConnectorConfig;
use haystack_server::federation::Federation;

/// Entities per remote server.
const ENTITIES_PER_REMOTE: usize = 10_000;

/// Find a free TCP port on localhost.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    listener.local_addr().unwrap().port()
}

/// Get a lazily-initialized static tokio runtime for client operations.
fn get_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

/// Build a test graph with 100 sites and `n` points distributed evenly.
fn build_test_graph(n: usize) -> SharedGraph {
    let mut graph = EntityGraph::new();

    // Create 100 sites
    for i in 0..100 {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(format!("site-{i}"))));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str(format!("Site {i}")));
        d.set(
            "area",
            Kind::Number(Number::new(10000.0, Some("ft\u{00b2}".into()))),
        );
        graph.add(d).unwrap();
    }

    // Create n points, distributed across the 100 sites
    for i in 0..n {
        let site_idx = i % 100;
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(format!("p-{i}"))));
        d.set("point", Kind::Marker);
        d.set("his", Kind::Marker);
        d.set("sensor", Kind::Marker);
        d.set("temp", Kind::Marker);
        d.set("writable", Kind::Marker);
        d.set("dis", Kind::Str(format!("Point {i}")));
        d.set(
            "siteRef",
            Kind::Ref(HRef::from_val(format!("site-{site_idx}"))),
        );
        d.set(
            "curVal",
            Kind::Number(Number::new(
                70.0 + (i as f64) * 0.01,
                Some("\u{00b0}F".into()),
            )),
        );
        d.set("kind", Kind::Str("Number".into()));
        graph.add(d).unwrap();
    }

    SharedGraph::new(graph)
}

/// Bench credential constants (used for SCRAM auth on remote servers).
const BENCH_USER: &str = "bench";
const BENCH_PASS: &str = "bench";

/// Build an AuthManager with a single user for benchmarking.
fn bench_auth() -> AuthManager {
    let hash = hash_password(BENCH_PASS);
    let toml = format!(
        r#"
[users.{BENCH_USER}]
password_hash = "{hash}"
permissions = ["read", "write"]
"#
    );
    AuthManager::from_toml_str(&toml).unwrap()
}

/// A test server that starts a real Haystack server on a free port.
struct TestServer {
    port: u16,
    _thread: std::thread::JoinHandle<()>,
}

impl TestServer {
    /// Start a test server with the given shared graph and optional auth/federation.
    fn start_configured(
        graph: SharedGraph,
        auth: Option<AuthManager>,
        federation: Option<Federation>,
    ) -> Self {
        haystack_client::ensure_crypto_provider();
        let port = free_port();
        let ns = DefNamespace::load_standard().unwrap();

        let thread = std::thread::spawn(move || {
            let rt = actix_rt::System::new();
            rt.block_on(async move {
                let mut server = HaystackServer::new(graph).with_namespace(ns).port(port);
                if let Some(a) = auth {
                    server = server.with_auth(a);
                }
                if let Some(f) = federation {
                    server = server.with_federation(f);
                }
                server.run().await.expect("server failed");
            });
        });

        // Wait for the server to be ready by polling TCP connectivity
        for _ in 0..200 {
            if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        Self {
            port,
            _thread: thread,
        }
    }

    /// Start a test server with auth enabled (for remote servers).
    fn start_with_auth(graph: SharedGraph, auth: AuthManager) -> Self {
        Self::start_configured(graph, Some(auth), None)
    }

    /// Start a test server with federation configured (for the lead server).
    fn start_with_federation(graph: SharedGraph, federation: Federation) -> Self {
        Self::start_configured(graph, None, Some(federation))
    }

    /// Get the HTTP API URL for this server.
    fn api_url(&self) -> String {
        format!("http://127.0.0.1:{}/api", self.port)
    }

    /// Connect an HTTP client to this server (no auth).
    fn connect_http(&self) -> HaystackClient<HttpTransport> {
        let transport = HttpTransport::new(&self.api_url(), String::new());
        HaystackClient::from_transport(transport)
    }

    /// Connect an HTTP client using HBF binary format (no auth).
    fn connect_hbf(&self) -> HaystackClient<HttpTransport> {
        let transport = HttpTransport::with_format(
            &self.api_url(),
            String::new(),
            haystack_core::codecs::HBF_MIME,
        );
        HaystackClient::from_transport(transport)
    }
}

/// Build prefixed entities matching what a remote graph contains.
/// This simulates what a real federation sync would produce.
fn build_prefixed_entities(n: usize, prefix: &str) -> Vec<HDict> {
    use haystack_server::connector::prefix_refs;

    let mut entities = Vec::with_capacity(n + 100);

    for i in 0..100 {
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(format!("site-{i}"))));
        d.set("site", Kind::Marker);
        d.set("dis", Kind::Str(format!("Site {i}")));
        d.set(
            "area",
            Kind::Number(Number::new(10000.0, Some("ft\u{00b2}".into()))),
        );
        prefix_refs(&mut d, prefix);
        entities.push(d);
    }

    for i in 0..n {
        let site_idx = i % 100;
        let mut d = HDict::new();
        d.set("id", Kind::Ref(HRef::from_val(format!("p-{i}"))));
        d.set("point", Kind::Marker);
        d.set("his", Kind::Marker);
        d.set("sensor", Kind::Marker);
        d.set("temp", Kind::Marker);
        d.set("dis", Kind::Str(format!("Point {i}")));
        d.set(
            "siteRef",
            Kind::Ref(HRef::from_val(format!("site-{site_idx}"))),
        );
        d.set(
            "curVal",
            Kind::Number(Number::new(
                70.0 + (i as f64) * 0.01,
                Some("\u{00b0}F".into()),
            )),
        );
        d.set("kind", Kind::Str("Number".into()));
        prefix_refs(&mut d, prefix);
        entities.push(d);
    }

    entities
}

/// The full federation cluster: 2 remote servers + 1 lead with federation.
struct FederationCluster {
    remote_a: TestServer,
    remote_b: TestServer,
    lead: TestServer,
}

impl FederationCluster {
    /// Build and start the cluster. Pre-populates federation caches.
    fn start() -> Self {
        // Start two remote servers, each with 10k entities and auth enabled
        // (auth is required for federation proxy operations like hisRead/hisWrite)
        let remote_a =
            TestServer::start_with_auth(build_test_graph(ENTITIES_PER_REMOTE), bench_auth());
        let remote_b =
            TestServer::start_with_auth(build_test_graph(ENTITIES_PER_REMOTE), bench_auth());

        // Build federation config pointing to both remotes.
        // Remote servers have no auth, so we use empty credentials and skip
        // SCRAM-based sync. Instead we pre-populate the connector caches
        // with prefixed entities matching the remote graphs.
        let mut federation = Federation::new();
        federation
            .add(ConnectorConfig {
                name: "Remote A".to_string(),
                url: remote_a.api_url(),
                username: BENCH_USER.to_string(),
                password: BENCH_PASS.to_string(),
                id_prefix: Some("ra-".to_string()),
                ws_url: None,
                sync_interval_secs: Some(3600), // long interval — we sync manually
                client_cert: None,
                client_key: None,
                ca_cert: None,
                domain: None,
            })
            .unwrap();
        federation
            .add(ConnectorConfig {
                name: "Remote B".to_string(),
                url: remote_b.api_url(),
                username: BENCH_USER.to_string(),
                password: BENCH_PASS.to_string(),
                id_prefix: Some("rb-".to_string()),
                ws_url: None,
                sync_interval_secs: Some(3600),
                client_cert: None,
                client_key: None,
                ca_cert: None,
                domain: None,
            })
            .unwrap();

        // Pre-populate connector caches (simulates a completed sync)
        let entities_a = build_prefixed_entities(ENTITIES_PER_REMOTE, "ra-");
        let entities_b = build_prefixed_entities(ENTITIES_PER_REMOTE, "rb-");
        eprintln!(
            "  Populated caches: {} + {} entities",
            entities_a.len(),
            entities_b.len()
        );
        federation.connectors[0].update_cache(entities_a);
        federation.connectors[1].update_cache(entities_b);

        // Start lead server with the pre-populated federation (empty local graph)
        let lead =
            TestServer::start_with_federation(SharedGraph::new(EntityGraph::new()), federation);

        FederationCluster {
            remote_a,
            remote_b,
            lead,
        }
    }
}

/// Get or initialize the shared federation cluster (started once for all benchmarks).
fn get_cluster() -> &'static FederationCluster {
    static CLUSTER: OnceLock<FederationCluster> = OnceLock::new();
    CLUSTER.get_or_init(|| {
        eprintln!("Starting federation cluster (2 remotes x {ENTITIES_PER_REMOTE} entities)...");
        let cluster = FederationCluster::start();
        eprintln!("Federation cluster ready.");
        cluster
    })
}

// ---------------------------------------------------------------------------
// Sync benchmarks
// ---------------------------------------------------------------------------

fn sync_benchmarks(c: &mut Criterion) {
    let rt = get_runtime();
    let cluster = get_cluster();

    // Benchmark reading all entities from one remote (simulates sync fetch).
    // Uses direct HTTP client to avoid SCRAM overhead — measures the raw
    // read-all + deserialize cost for 10k entities.
    let mut group = c.benchmark_group("federation_sync");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    // Connect authenticated clients to the remotes
    let remote_client = rt.block_on(async {
        HaystackClient::connect(&cluster.remote_a.api_url(), BENCH_USER, BENCH_PASS)
            .await
            .unwrap()
    });

    group.bench_function("read_all_10k_from_remote", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(remote_client.read("*", None).await.unwrap());
            });
        });
    });

    // Benchmark reading all from both remotes concurrently (simulates full sync)
    let client_a = rt.block_on(async {
        HaystackClient::connect(&cluster.remote_a.api_url(), BENCH_USER, BENCH_PASS)
            .await
            .unwrap()
    });
    let client_b = rt.block_on(async {
        HaystackClient::connect(&cluster.remote_b.api_url(), BENCH_USER, BENCH_PASS)
            .await
            .unwrap()
    });

    group.bench_function("read_all_20k_both_remotes", |b| {
        b.iter(|| {
            rt.block_on(async {
                let (a, b) = tokio::join!(client_a.read("*", None), client_b.read("*", None),);
                black_box(a.unwrap());
                black_box(b.unwrap());
            });
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Federated read benchmarks
// ---------------------------------------------------------------------------

fn read_benchmarks(c: &mut Criterion) {
    let rt = get_runtime();
    let cluster = get_cluster();
    let client = cluster.lead.connect_http();
    let hbf_client = cluster.lead.connect_hbf();

    let mut group = c.benchmark_group("federation_read");

    // Read a single federated entity by ID
    group.bench_function("read_by_id", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.read_by_ids(&["ra-p-500"]).await.unwrap());
            });
        });
    });

    // Filter: selective — one site from one remote
    group.bench_function("filter_site", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.read("site and dis==\"Site 5\"", None).await.unwrap());
            });
        });
    });

    // Filter: all points (20k federated entities) — Zinc text
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));
    group.bench_function("filter_all_points_20k", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.read("point", None).await.unwrap());
            });
        });
    });

    // Filter: all points (20k federated entities) — HBF binary
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));
    group.bench_function("filter_all_points_20k_hbf", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(hbf_client.read("point", None).await.unwrap());
            });
        });
    });

    // Read by ID — HBF binary
    group.sample_size(100);
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("read_by_id_hbf", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(hbf_client.read_by_ids(&["ra-p-500"]).await.unwrap());
            });
        });
    });

    // Nav from root
    group.sample_size(10);
    group.bench_function("nav_root", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.nav(None).await.unwrap());
            });
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Federated history benchmarks
// ---------------------------------------------------------------------------

/// Create a vector of history items (dicts with "ts" and "val").
fn make_his_items(count: usize, base_hour: u32) -> Vec<HDict> {
    use chrono::{FixedOffset, TimeZone};

    let offset = FixedOffset::east_opt(0).unwrap();
    (0..count)
        .map(|i| {
            let minute = (i % 60) as u32;
            let hour = base_hour + (i / 60) as u32;
            let dt = offset
                .with_ymd_and_hms(2024, 6, 1, hour % 24, minute, 0)
                .unwrap();
            let hdt = HDateTime::new(dt, "UTC");
            let mut d = HDict::new();
            d.set("ts", Kind::DateTime(hdt));
            d.set("val", Kind::Number(Number::unitless(70.0 + i as f64 * 0.1)));
            d
        })
        .collect()
}

fn his_benchmarks(c: &mut Criterion) {
    let rt = get_runtime();
    let cluster = get_cluster();
    let client = cluster.lead.connect_http();

    let mut group = c.benchmark_group("federation_his");

    // Pre-load 1000 history items on remote A's p-0 (accessed as ra-p-0)
    let preload_items = make_his_items(1000, 0);
    rt.block_on(async {
        // Write directly to remote A (with auth) — the lead will proxy reads
        let remote_client =
            HaystackClient::connect(&cluster.remote_a.api_url(), BENCH_USER, BENCH_PASS)
                .await
                .unwrap();
        remote_client.his_write("p-0", preload_items).await.unwrap();
    });

    // hisRead proxied through federation
    group.bench_function("his_read_proxied_1000", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(client.his_read("ra-p-0", "2024-06-01").await.unwrap());
            });
        });
    });

    // hisWrite proxied through federation
    group.bench_function("his_write_proxied_100", |b| {
        b.iter(|| {
            let items = make_his_items(100, 12);
            rt.block_on(async {
                black_box(client.his_write("ra-p-1", items).await.unwrap());
            });
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Federated pointWrite benchmarks
// ---------------------------------------------------------------------------

fn point_write_benchmarks(c: &mut Criterion) {
    let rt = get_runtime();
    let cluster = get_cluster();
    let client = cluster.lead.connect_http();

    let mut group = c.benchmark_group("federation_point_write");

    group.bench_function("point_write_proxied", |b| {
        b.iter(|| {
            rt.block_on(async {
                black_box(
                    client
                        .point_write("ra-p-100", 16, Kind::Number(Number::unitless(72.5)))
                        .await
                        .unwrap(),
                );
            });
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Concurrent federated read benchmarks
// ---------------------------------------------------------------------------

fn concurrent_benchmarks(c: &mut Criterion) {
    let rt = get_runtime();
    let cluster = get_cluster();

    let mut group = c.benchmark_group("federation_concurrent");

    // 50 concurrent reads of federated entities (mix of ra- and rb- prefixes)
    let clients: Vec<_> = (0..50).map(|_| cluster.lead.connect_http()).collect();

    group.bench_function("concurrent_reads_50", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut handles = Vec::new();
                for (i, client) in clients.iter().enumerate() {
                    let prefix = if i % 2 == 0 { "ra" } else { "rb" };
                    let id = format!("{prefix}-p-{}", (i * 200) % ENTITIES_PER_REMOTE);
                    handles.push(async move { client.read_by_ids(&[id.as_str()]).await.unwrap() });
                }
                for result in join_all(handles).await {
                    black_box(result);
                }
            });
        });
    });

    // 50 concurrent filter reads across federation
    let filter_clients: Vec<_> = (0..50).map(|_| cluster.lead.connect_http()).collect();

    group.bench_function("concurrent_filter_50", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut handles = Vec::new();
                for (i, client) in filter_clients.iter().enumerate() {
                    let site_idx = i % 100;
                    let filter = format!("point and siteRef==@ra-site-{site_idx}");
                    handles.push(async move { client.read(filter.as_str(), None).await.unwrap() });
                }
                for result in join_all(handles).await {
                    black_box(result);
                }
            });
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    sync_benchmarks,
    read_benchmarks,
    his_benchmarks,
    point_write_benchmarks,
    concurrent_benchmarks
);
criterion_main!(benches);
