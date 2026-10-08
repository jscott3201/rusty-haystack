//! Real loopback fixture with bound-address readiness and owned task lifetime.
use haystack_app::{
    AllowAll, ApplicationBuilder, ApplicationOwner, HisStore, HistoryLimits, HistoryService,
    ReadLimits,
};
use haystack_client::HaystackClient;
use haystack_client::transport::http::HttpTransport;
use haystack_core::graph::SharedGraph;
use haystack_core::ontology::DefNamespace;
use haystack_server::HaystackServer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

pub struct TestServer {
    runtime: Runtime,
    owner: Option<ApplicationOwner>,
    address: SocketAddr,
}

impl TestServer {
    pub fn start(graph: SharedGraph) -> Self {
        haystack_client::ensure_crypto_provider();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("benchmark runtime");
        let builder = ApplicationBuilder::new(
            graph.clone(),
            Arc::new(AllowAll),
            ReadLimits {
                max_rows: 10_000,
                max_output_bytes: 8 * 1024 * 1024,
                ..ReadLimits::default()
            },
        )
        .unwrap();
        let history = HistoryService::new(
            builder.handle().read_service(),
            Arc::new(HisStore::new()),
            HistoryLimits {
                total_rows: 10_000,
                total_bytes: 8 * 1024 * 1024,
                ..HistoryLimits::default()
            },
        )
        .unwrap();
        let builder = builder.owned_history(history).unwrap();
        let server = HaystackServer::new(graph)
            .with_application(builder.handle())
            .with_namespace(DefNamespace::load_standard().expect("standard definitions"))
            .port(0);
        let owner = builder
            .owned_resource(server.into_listener())
            .start(runtime.handle())
            .unwrap();
        let address = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), owner.ready())
                .await
                .expect("server readiness deadline")
                .expect("server ready")
                .listeners[0]
                .address
        });
        let fixture = Self {
            runtime,
            owner: Some(owner),
            address,
        };
        let client = fixture.connect_http();
        fixture.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), client.about())
                .await
                .expect("server readiness deadline")
                .expect("server readiness response");
        });
        fixture
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    pub fn api_url(&self) -> String {
        format!("http://{}/api", self.address)
    }

    pub fn connect_http(&self) -> HaystackClient<HttpTransport> {
        HaystackClient::from_transport(HttpTransport::new(&self.api_url(), String::new()))
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let report = self.runtime.block_on(async {
                let _ = owner.close().await;
                owner.terminated().await
            });
            if !std::thread::panicking() {
                assert!(report.close.is_ok(), "benchmark owner close: {report:?}");
            }
        }
        // Runtime drops only after real application work and provider cleanup.
    }
}
