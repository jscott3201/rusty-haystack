//! Real loopback fixture with bound-address readiness and owned task lifetime.
use haystack_client::HaystackClient;
use haystack_client::transport::http::HttpTransport;
use haystack_core::graph::SharedGraph;
use haystack_core::ontology::DefNamespace;
use haystack_server::HaystackServer;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub struct TestServer {
    runtime: Runtime,
    task: Option<JoinHandle<std::io::Result<()>>>,
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
        let (ready, bound) = oneshot::channel();
        let server = HaystackServer::new(graph)
            .with_namespace(DefNamespace::load_standard().expect("standard definitions"))
            .port(0);
        let task = runtime.spawn(async move {
            server
                .run_reporting_addr(move |address| {
                    ready.send(address).expect("benchmark startup receiver");
                })
                .await
        });
        let address = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), bound)
                .await
                .expect("server bind deadline")
                .expect("server bound successfully")
        });
        let fixture = Self {
            runtime,
            task: Some(task),
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
        if let Some(task) = self.task.take() {
            task.abort();
            let outcome = self.runtime.block_on(task);
            // Preserve an original fixture/test panic while still joining the
            // cancelled task. On normal teardown, an unexpected server exit fails.
            if !std::thread::panicking() {
                assert!(
                    matches!(outcome, Err(ref error) if error.is_cancelled()),
                    "unexpected benchmark server exit: {outcome:?}"
                );
            }
        }
        // Dropping this owned runtime also stops Axum connection tasks. There is
        // no detached server thread or process-global runtime retaining sockets.
    }
}
