// Python bindings for HaystackClient — synchronous wrapper over async Rust client.
// Uses a shared tokio runtime to block on async operations.
// GIL is released during all blocking I/O via py.detach().

use pyo3::prelude::*;

use haystack_client::client::HaystackClient;
use haystack_client::tls::TlsConfig;
use haystack_client::transport::http::HttpTransport;
use haystack_client::transport::ws::WsTransport;

use crate::convert::py_to_kind;
use crate::data::{PyHDict, PyHGrid};
use crate::exceptions;

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

fn client_err(e: haystack_client::ClientError) -> PyErr {
    PyErr::new::<exceptions::ClientError, _>(e.to_string())
}

// ── TlsConfig ──

/// TLS client configuration for mTLS connections.
///
/// Load certificates from files for mutual TLS authentication.
#[pyclass(name = "TlsConfig")]
pub struct PyTlsConfig {
    pub(crate) inner: TlsConfig,
}

#[pymethods]
impl PyTlsConfig {
    /// Load TLS client certificate, key, and optional CA from file paths.
    #[staticmethod]
    #[pyo3(signature = (cert_path, key_path, ca_path = None))]
    fn from_files(cert_path: &str, key_path: &str, ca_path: Option<&str>) -> PyResult<Self> {
        TlsConfig::from_files(cert_path, key_path, ca_path)
            .map(|inner| Self { inner })
            .map_err(|e| PyErr::new::<exceptions::ClientError, _>(e))
    }

    fn __repr__(&self) -> String {
        format!(
            "TlsConfig(cert={}B, key={}B, ca={})",
            self.inner.client_cert_pem.len(),
            self.inner.client_key_pem.len(),
            self.inner
                .ca_cert_pem
                .as_ref()
                .map_or("None".to_string(), |c| format!("{}B", c.len()))
        )
    }
}

// ── HaystackClient (HTTP) ──

/// Haystack HTTP client with SCRAM authentication.
///
/// Provides synchronous wrappers around all Haystack REST API operations.
/// All methods block until completion (uses internal tokio runtime).
/// The GIL is released during network I/O so other Python threads can run.
///
/// Examples:
///     client = HaystackClient.connect("http://localhost:8080", "admin", "pass")
///     sites = client.read("site")
///     client.close()
#[pyclass(name = "HaystackClient")]
pub struct PyHaystackClient {
    inner: HaystackClient<HttpTransport>,
}

#[pymethods]
impl PyHaystackClient {
    /// Connect to a Haystack server over HTTP with SCRAM auth.
    #[staticmethod]
    fn connect(py: Python<'_>, url: &str, username: &str, password: &str) -> PyResult<Self> {
        let rt = get_runtime()?;
        let url = url.to_string();
        let username = username.to_string();
        let password = password.to_string();
        let client = py
            .detach(|| rt.block_on(HaystackClient::connect(&url, &username, &password)))
            .map_err(client_err)?;
        Ok(Self { inner: client })
    }

    /// Connect with mTLS.
    #[staticmethod]
    fn connect_tls(
        py: Python<'_>,
        url: &str,
        username: &str,
        password: &str,
        tls: &PyTlsConfig,
    ) -> PyResult<Self> {
        let rt = get_runtime()?;
        let url = url.to_string();
        let username = username.to_string();
        let password = password.to_string();
        let tls_inner = tls.inner.clone();
        let client = py
            .detach(|| {
                rt.block_on(HaystackClient::connect_with_tls(
                    &url, &username, &password, &tls_inner,
                ))
            })
            .map_err(client_err)?;
        Ok(Self { inner: client })
    }

    // -- Standard ops --

    /// Query the server's about information.
    ///
    /// Returns a grid with a single row containing server metadata:
    /// haystackVersion, productName, productVersion, tz, etc.
    fn about(&self, py: Python<'_>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.about()))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Query the list of operations supported by the server.
    ///
    /// Returns a grid where each row describes an available op (name, summary).
    fn ops(&self, py: Python<'_>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.ops()))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Query the wire formats supported by the server.
    ///
    /// Returns a grid listing MIME types and codec names (e.g., text/zinc, application/json).
    fn formats(&self, py: Python<'_>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.formats()))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Query the ontology libraries loaded on the server.
    fn libs(&self, py: Python<'_>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.libs()))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Read entities matching a Haystack filter expression.
    ///
    /// Args:
    ///     filter: Haystack filter string (e.g., 'site', 'equip and siteRef==@site-1').
    ///     limit: Maximum number of results (None for unlimited).
    #[pyo3(signature = (filter, limit = None))]
    fn read(&self, py: Python<'_>, filter: &str, limit: Option<usize>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.read(filter, limit)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Read entities by their ref ID strings.
    ///
    /// Args:
    ///     ids: List of entity ref values (e.g., ['site-1', 'equip-2']).
    fn read_by_ids(&self, py: Python<'_>, ids: Vec<String>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        let grid = py
            .detach(|| rt.block_on(self.inner.read_by_ids(&id_refs)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Navigate the entity tree.
    ///
    /// Args:
    ///     nav_id: Parent nav ID to list children of, or None for root.
    #[pyo3(signature = (nav_id = None))]
    fn nav(&self, py: Python<'_>, nav_id: Option<&str>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.nav(nav_id)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Query ontology definitions.
    ///
    /// Args:
    ///     filter: Optional filter to narrow defs (e.g., 'equip').
    #[pyo3(signature = (filter = None))]
    fn defs(&self, py: Python<'_>, filter: Option<&str>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.defs(filter)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    // -- Watch ops --

    /// Subscribe to entity watches for real-time updates.
    ///
    /// Args:
    ///     ids: Entity ref values to watch.
    ///     lease: Optional lease duration (e.g., '1min', '1hr').
    #[pyo3(signature = (ids, lease = None))]
    fn watch_sub(
        &self,
        py: Python<'_>,
        ids: Vec<String>,
        lease: Option<&str>,
    ) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        let grid = py
            .detach(|| rt.block_on(self.inner.watch_sub(&id_refs, lease)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Poll a watch for changed entities since last poll.
    ///
    /// Args:
    ///     watch_id: Watch identifier from watch_sub response.
    fn watch_poll(&self, py: Python<'_>, watch_id: &str) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.watch_poll(watch_id)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Unsubscribe from a watch.
    ///
    /// Args:
    ///     watch_id: Watch identifier.
    ///     ids: Specific IDs to unwatch, or None to close entire watch.
    #[pyo3(signature = (watch_id, ids = None))]
    fn watch_unsub(
        &self,
        py: Python<'_>,
        watch_id: &str,
        ids: Option<Vec<String>>,
    ) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let id_strs = ids.unwrap_or_default();
        let id_refs: Vec<&str> = id_strs.iter().map(|s| s.as_str()).collect();
        let grid = py
            .detach(|| rt.block_on(self.inner.watch_unsub(watch_id, &id_refs)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    // -- Point write --

    /// Write a value to a writable point at a priority level.
    ///
    /// Args:
    ///     id: Point entity ref value.
    ///     level: Priority level 1-17 (1=highest, 17=default).
    ///     val: Value to write (Number, Bool, Str, or None for auto).
    fn point_write(
        &self,
        py: Python<'_>,
        id: &str,
        level: u8,
        val: &Bound<'_, PyAny>,
    ) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let kind = py_to_kind(val)?;
        let grid = py
            .detach(|| rt.block_on(self.inner.point_write(id, level, kind)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    // -- History --

    /// Read historical time-series data for a point.
    ///
    /// Args:
    ///     id: Point entity ref value.
    ///     range: Time range string (e.g., 'today', 'yesterday', '2024-01-01,2024-01-31').
    fn his_read(&self, py: Python<'_>, id: &str, range: &str) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.his_read(id, range)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Write historical time-series data for a point.
    ///
    /// Args:
    ///     id: Point entity ref value.
    ///     items: List of HDict rows with 'ts' (datetime) and 'val' tags.
    fn his_write(
        &self,
        py: Python<'_>,
        id: &str,
        items: Vec<PyRef<'_, PyHDict>>,
    ) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let dicts: Vec<haystack_core::data::HDict> =
            items.iter().map(|d| d.inner.clone()).collect();
        let grid = py
            .detach(|| rt.block_on(self.inner.his_write(id, dicts)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    // -- Actions --

    /// Invoke a named action on an entity.
    ///
    /// Args:
    ///     id: Target entity ref value.
    ///     action: Action name string.
    ///     args: Optional HDict of action parameters.
    #[pyo3(signature = (id, action, args = None))]
    fn invoke_action(
        &self,
        py: Python<'_>,
        id: &str,
        action: &str,
        args: Option<&PyHDict>,
    ) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let args_dict = args.map(|d| d.inner.clone()).unwrap_or_default();
        let grid = py
            .detach(|| rt.block_on(self.inner.invoke_action(id, action, args_dict)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    // -- Library management --

    /// Query ontology specs (entity type definitions).
    ///
    /// Args:
    ///     lib: Optional library name to filter specs.
    #[pyo3(signature = (lib = None))]
    fn specs(&self, py: Python<'_>, lib: Option<&str>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.specs(lib)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Get a single ontology spec by qualified name.
    ///
    /// Args:
    ///     qname: Fully qualified spec name (e.g., 'ph::Site').
    fn spec(&self, py: Python<'_>, qname: &str) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.spec(qname)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Load an ontology library from Trio source.
    ///
    /// Args:
    ///     name: Library name.
    ///     source: Trio-formatted definition source string.
    fn load_lib(&self, py: Python<'_>, name: &str, source: &str) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.load_lib(name, source)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Unload an ontology library by name.
    fn unload_lib(&self, py: Python<'_>, name: &str) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.unload_lib(name)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Export an ontology library as Trio source.
    fn export_lib(&self, py: Python<'_>, name: &str) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.export_lib(name)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Validate entities against the server's ontology namespace.
    ///
    /// Args:
    ///     entities: List of HDict entities to validate.
    fn validate(&self, py: Python<'_>, entities: Vec<PyRef<'_, PyHDict>>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let dicts: Vec<haystack_core::data::HDict> =
            entities.iter().map(|d| d.inner.clone()).collect();
        let grid = py
            .detach(|| rt.block_on(self.inner.validate(dicts)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    // -- Session management --

    /// Close the client connection and release resources.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let rt = get_runtime()?;
        py.detach(|| rt.block_on(self.inner.close()))
            .map_err(client_err)
    }

    /// Send a raw op request. Returns the response grid.
    fn call(&self, py: Python<'_>, op: &str, req: &PyHGrid) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.call(op, &req.inner)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    fn __repr__(&self) -> String {
        "HaystackClient(http)".to_string()
    }
}

// ── WsClient (WebSocket) ──

/// Haystack WebSocket client with SCRAM authentication.
///
/// Connects via HTTP for auth, then upgrades to WebSocket for all operations.
/// Ideal for long-lived connections and watch subscriptions.
#[pyclass(name = "WsClient")]
pub struct PyWsClient {
    inner: HaystackClient<WsTransport>,
}

#[pymethods]
impl PyWsClient {
    /// Connect via HTTP for auth, then upgrade to WebSocket for ops.
    #[staticmethod]
    fn connect(
        py: Python<'_>,
        url: &str,
        ws_url: &str,
        username: &str,
        password: &str,
    ) -> PyResult<Self> {
        let rt = get_runtime()?;
        let url = url.to_string();
        let ws_url = ws_url.to_string();
        let username = username.to_string();
        let password = password.to_string();
        let client = py
            .detach(|| {
                rt.block_on(HaystackClient::connect_ws(
                    &url, &ws_url, &username, &password,
                ))
            })
            .map_err(client_err)?;
        Ok(Self { inner: client })
    }

    /// Query the server's about information over WebSocket.
    fn about(&self, py: Python<'_>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.about()))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Query supported operations over WebSocket.
    fn ops(&self, py: Python<'_>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.ops()))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Read entities matching a Haystack filter expression.
    ///
    /// Args:
    ///     filter: Haystack filter string (e.g., 'site', 'equip and siteRef==@site-1').
    ///     limit: Maximum number of results (None for unlimited).
    #[pyo3(signature = (filter, limit = None))]
    fn read(&self, py: Python<'_>, filter: &str, limit: Option<usize>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.read(filter, limit)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Read entities by their ref ID strings.
    ///
    /// Args:
    ///     ids: List of entity ref values (e.g., ['site-1', 'equip-2']).
    fn read_by_ids(&self, py: Python<'_>, ids: Vec<String>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        let grid = py
            .detach(|| rt.block_on(self.inner.read_by_ids(&id_refs)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Navigate the entity tree.
    ///
    /// Args:
    ///     nav_id: Parent nav ID to list children of, or None for root.
    #[pyo3(signature = (nav_id = None))]
    fn nav(&self, py: Python<'_>, nav_id: Option<&str>) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.nav(nav_id)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Subscribe to entity watches for real-time updates.
    ///
    /// Args:
    ///     ids: Entity ref values to watch.
    ///     lease: Optional lease duration (e.g., '1min', '1hr').
    #[pyo3(signature = (ids, lease = None))]
    fn watch_sub(
        &self,
        py: Python<'_>,
        ids: Vec<String>,
        lease: Option<&str>,
    ) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        let grid = py
            .detach(|| rt.block_on(self.inner.watch_sub(&id_refs, lease)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Poll a watch for changed entities since last poll.
    ///
    /// Args:
    ///     watch_id: Watch identifier from watch_sub response.
    fn watch_poll(&self, py: Python<'_>, watch_id: &str) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.watch_poll(watch_id)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Read historical time-series data for a point.
    ///
    /// Args:
    ///     id: Point entity ref value.
    ///     range: Time range string (e.g., 'today', 'yesterday', '2024-01-01,2024-01-31').
    fn his_read(&self, py: Python<'_>, id: &str, range: &str) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.his_read(id, range)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Write a value to a writable point at a priority level.
    ///
    /// Args:
    ///     id: Point entity ref value.
    ///     level: Priority level 1-17 (1=highest, 17=default).
    ///     val: Value to write (Number, Bool, Str, or None for auto).
    fn point_write(
        &self,
        py: Python<'_>,
        id: &str,
        level: u8,
        val: &Bound<'_, PyAny>,
    ) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let kind = py_to_kind(val)?;
        let grid = py
            .detach(|| rt.block_on(self.inner.point_write(id, level, kind)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Send a raw op request over WebSocket. Returns the response grid.
    fn call(&self, py: Python<'_>, op: &str, req: &PyHGrid) -> PyResult<PyHGrid> {
        let rt = get_runtime()?;
        let grid = py
            .detach(|| rt.block_on(self.inner.call(op, &req.inner)))
            .map_err(client_err)?;
        Ok(PyHGrid::from_core(&grid))
    }

    /// Close the WebSocket connection.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let rt = get_runtime()?;
        py.detach(|| rt.block_on(self.inner.close()))
            .map_err(client_err)
    }

    fn __repr__(&self) -> String {
        "WsClient(websocket)".to_string()
    }
}
