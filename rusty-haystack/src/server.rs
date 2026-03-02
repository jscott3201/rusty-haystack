// Python bindings for HaystackServer — embedded server, auth, history, federation.
// Uses a shared tokio runtime; `run()` blocks the calling Python thread.
// GIL is released during all blocking I/O via py.detach().

use std::sync::{Arc, Mutex};

use pyo3::prelude::*;

use haystack_server::app::HaystackServer;
use haystack_server::auth::AuthManager;
use haystack_server::connector::ConnectorConfig;
use haystack_server::federation::Federation;
use haystack_server::his_store::HisStore;

use crate::data::{PyHDict, PyHGrid};
use crate::exceptions;
use crate::graph::PySharedGraph;
use crate::ontology::PyDefNamespace;

fn get_runtime() -> PyResult<&'static tokio::runtime::Runtime> {
    use std::sync::OnceLock;
    static RT: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| {
        PyErr::new::<exceptions::HaystackError, _>(format!("failed to create tokio runtime: {}", e))
    })
}

// ── AuthManager ──

/// SCRAM SHA-256 authentication manager.
///
/// Manages user credentials and token-based session authentication.
/// Load from TOML config or create programmatically.
#[pyclass(name = "AuthManager")]
pub struct PyAuthManager {
    pub(crate) inner: AuthManager,
}

#[pymethods]
impl PyAuthManager {
    /// Create a disabled (no-auth) manager.
    #[staticmethod]
    fn empty() -> Self {
        Self {
            inner: AuthManager::empty(),
        }
    }

    /// Load auth configuration from a TOML file path.
    #[staticmethod]
    fn from_toml(path: &str) -> PyResult<Self> {
        AuthManager::from_toml(path)
            .map(|inner| Self { inner })
            .map_err(|e| PyErr::new::<exceptions::AuthError, _>(e))
    }

    /// Load auth configuration from a TOML string.
    #[staticmethod]
    fn from_toml_str(content: &str) -> PyResult<Self> {
        AuthManager::from_toml_str(content)
            .map(|inner| Self { inner })
            .map_err(|e| PyErr::new::<exceptions::AuthError, _>(e))
    }

    /// Whether authentication is enabled (has users configured).
    fn is_enabled(&self) -> bool {
        self.inner.is_enabled()
    }

    fn __repr__(&self) -> String {
        format!("AuthManager(enabled={})", self.inner.is_enabled())
    }
}

// ── ConnectorConfig ──

/// Configuration for a federation connector to a remote Haystack server.
#[pyclass(name = "ConnectorConfig", from_py_object)]
#[derive(Clone)]
pub struct PyConnectorConfig {
    pub(crate) inner: ConnectorConfig,
}

#[pymethods]
impl PyConnectorConfig {
    /// Create a federation connector configuration.
    ///
    /// Args:
    ///     name: Connector display name.
    ///     url: Remote server HTTP URL.
    ///     username: SCRAM auth username.
    ///     password: SCRAM auth password.
    ///     id_prefix: Optional prefix for federated entity IDs.
    ///     ws_url: Optional WebSocket URL (defaults to ws:// derived from url).
    ///     sync_interval_secs: Sync interval in seconds (default: server-configured).
    #[new]
    #[pyo3(signature = (name, url, username, password, id_prefix = None, ws_url = None, sync_interval_secs = None))]
    fn new(
        name: String,
        url: String,
        username: String,
        password: String,
        id_prefix: Option<String>,
        ws_url: Option<String>,
        sync_interval_secs: Option<u64>,
    ) -> Self {
        Self {
            inner: ConnectorConfig {
                name,
                url,
                username,
                password,
                id_prefix,
                ws_url,
                sync_interval_secs,
                client_cert: None,
                client_key: None,
                ca_cert: None,
                domain: None,
            },
        }
    }

    /// The connector name.
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    /// The remote server URL.
    #[getter]
    fn url(&self) -> &str {
        &self.inner.url
    }

    fn __repr__(&self) -> String {
        format!(
            "ConnectorConfig(name='{}', url='{}')",
            self.inner.name, self.inner.url
        )
    }
}

// ── Federation ──

/// Federation manager — connects to remote Haystack servers for distributed queries.
///
/// Supports entity caching, WebSocket-first transport, and automatic sync.
#[pyclass(name = "Federation")]
pub struct PyFederation {
    pub(crate) inner: Federation,
}

#[pymethods]
impl PyFederation {
    #[new]
    fn new() -> Self {
        Self {
            inner: Federation::new(),
        }
    }

    /// Load federation config from a TOML file.
    #[staticmethod]
    fn from_toml(path: &str) -> PyResult<Self> {
        Federation::from_toml_file(path)
            .map(|inner| Self { inner })
            .map_err(|e| PyErr::new::<exceptions::HaystackError, _>(e))
    }

    /// Load federation config from a TOML string.
    #[staticmethod]
    fn from_toml_str(content: &str) -> PyResult<Self> {
        Federation::from_toml_str(content)
            .map(|inner| Self { inner })
            .map_err(|e| PyErr::new::<exceptions::HaystackError, _>(e))
    }

    /// Add a connector to the federation.
    fn add(&mut self, config: &PyConnectorConfig) {
        self.inner.add(config.inner.clone());
    }

    /// Synchronously sync all connectors. Returns list of (name, result_string).
    fn sync_all(&self, py: Python<'_>) -> PyResult<Vec<(String, String)>> {
        let rt = get_runtime()?;
        let results = py.detach(|| rt.block_on(self.inner.sync_all()));
        Ok(results
            .into_iter()
            .map(|(name, res)| {
                let msg = match res {
                    Ok(n) => format!("ok: {} entities", n),
                    Err(e) => format!("error: {}", e),
                };
                (name, msg)
            })
            .collect())
    }

    /// Return all cached entities across all connectors.
    fn all_cached_entities(&self, py: Python<'_>) -> PyResult<Vec<Py<PyAny>>> {
        self.inner
            .all_cached_entities()
            .into_iter()
            .map(|d| {
                Ok(PyHDict::from_core(&d)
                    .into_pyobject(py)?
                    .into_any()
                    .unbind())
            })
            .collect()
    }

    /// Filter cached entities using a Haystack filter expression.
    #[pyo3(signature = (filter_expr, limit = 0))]
    fn filter_cached(&self, filter_expr: &str, limit: usize) -> PyResult<PyHGrid> {
        let entities = self
            .inner
            .filter_cached_entities(filter_expr, limit)
            .map_err(|e| PyErr::new::<exceptions::HaystackError, _>(e))?;
        // Build a grid from the entities
        let mut grid = haystack_core::data::HGrid::new();
        for entity in entities {
            grid.rows.push(entity);
        }
        Ok(PyHGrid::from_core(&grid))
    }

    /// Status of each connector: list of (name, cached_entity_count).
    fn status(&self) -> Vec<(String, usize)> {
        self.inner.status()
    }

    /// Number of connectors.
    fn connector_count(&self) -> usize {
        self.inner.connector_count()
    }

    fn __repr__(&self) -> String {
        format!("Federation(connectors={})", self.inner.connector_count())
    }
}

// ── HisStore ──

/// In-memory history storage for time-series point data.
#[pyclass(name = "HisStore")]
pub struct PyHisStore {
    pub(crate) inner: HisStore,
}

#[pymethods]
impl PyHisStore {
    /// Create an empty in-memory history store.
    #[new]
    fn new() -> Self {
        Self {
            inner: HisStore::new(),
        }
    }

    /// Number of historical items for a given entity ID.
    fn len(&self, id: &str) -> usize {
        self.inner.len(id)
    }

    fn __repr__(&self) -> String {
        "HisStore()".to_string()
    }
}

// ── HaystackServer ──

/// Embedded Haystack HTTP API server.
///
/// Builder-pattern configuration: set graph, namespace, auth, federation,
/// then call run() (blocking) or run_background() (returns immediately).
/// Note: with_namespace/with_auth/with_federation consume their argument
/// (the original Python object becomes empty after the call).
///
/// Examples:
///     server = HaystackServer(graph)
///     server = server.with_auth(auth).port(8080)
///     server.run()  # blocks
#[pyclass(name = "HaystackServer")]
pub struct PyHaystackServer {
    inner: Option<HaystackServer>,
    /// Stores error from run_background() for later retrieval.
    bg_error: Arc<Mutex<Option<String>>>,
}

#[pymethods]
impl PyHaystackServer {
    /// Create a server with a SharedGraph as the entity store.
    #[new]
    fn new(graph: &PySharedGraph) -> Self {
        // Clone the inner SharedGraph (Arc-based, cheap)
        let sg = graph.clone_inner();
        Self {
            inner: Some(HaystackServer::new(sg)),
            bg_error: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the ontology namespace for validation and spec management.
    /// Warning: consumes the namespace — the original object becomes empty.
    fn with_namespace(&mut self, ns: &mut PyDefNamespace) -> PyResult<()> {
        let taken = std::mem::replace(&mut ns.inner, haystack_core::ontology::DefNamespace::new());
        if let Some(server) = self.inner.take() {
            self.inner = Some(server.with_namespace(taken));
        }
        Ok(())
    }

    /// Set the auth manager.
    /// Warning: consumes the auth manager — the original object becomes empty.
    fn with_auth(&mut self, auth: &mut PyAuthManager) {
        let taken = std::mem::replace(&mut auth.inner, AuthManager::empty());
        if let Some(server) = self.inner.take() {
            self.inner = Some(server.with_auth(taken));
        }
    }

    /// Set the federation manager.
    /// Warning: consumes the federation — the original object becomes empty.
    fn with_federation(&mut self, fed: &mut PyFederation) {
        let taken = std::mem::replace(&mut fed.inner, Federation::new());
        if let Some(server) = self.inner.take() {
            self.inner = Some(server.with_federation(taken));
        }
    }

    /// Set the listen port (default 8080).
    fn port(&mut self, port: u16) {
        if let Some(server) = self.inner.take() {
            self.inner = Some(server.port(port));
        }
    }

    /// Set the listen host (default "0.0.0.0").
    fn host(&mut self, host: &str) {
        if let Some(server) = self.inner.take() {
            self.inner = Some(server.host(host));
        }
    }

    /// Run the server (blocks the current thread, releases GIL).
    fn run(&mut self, py: Python<'_>) -> PyResult<()> {
        let server = self
            .inner
            .take()
            .ok_or_else(|| PyErr::new::<exceptions::HaystackError, _>("Server already consumed"))?;
        let rt = get_runtime()?;
        py.detach(|| rt.block_on(server.run()))
            .map_err(|e| PyErr::new::<exceptions::HaystackError, _>(e.to_string()))
    }

    /// Run the server in a background thread. Returns immediately.
    /// Check bg_error() for any startup or runtime errors.
    fn run_background(&mut self) -> PyResult<()> {
        let server = self
            .inner
            .take()
            .ok_or_else(|| PyErr::new::<exceptions::HaystackError, _>("Server already consumed"))?;
        let error_slot = Arc::clone(&self.bg_error);
        // actix-web uses Rc (not Send), so we need a dedicated thread with its own runtime
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    if let Ok(mut slot) = error_slot.lock() {
                        *slot = Some(format!("failed to create runtime: {}", e));
                    }
                    return;
                }
            };
            if let Err(e) = rt.block_on(server.run()) {
                if let Ok(mut slot) = error_slot.lock() {
                    *slot = Some(e.to_string());
                }
            }
        });
        Ok(())
    }

    /// Retrieve the background server error, if any.
    fn bg_error(&self) -> Option<String> {
        self.bg_error.lock().ok().and_then(|slot| slot.clone())
    }

    fn __repr__(&self) -> String {
        if self.inner.is_some() {
            "HaystackServer(ready)".to_string()
        } else {
            "HaystackServer(consumed)".to_string()
        }
    }
}
