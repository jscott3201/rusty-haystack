// Python bindings for HaystackServer — embedded server, auth, and history.
// Uses a shared tokio runtime; `run()` blocks the calling Python thread.
// GIL is released during all blocking I/O via py.detach().

use std::sync::{Arc, Mutex};

use pyo3::prelude::*;

use haystack_server::app::HaystackServer;
use haystack_server::auth::AuthManager;
use haystack_server::his_store::HisStore;

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
    /// Set once the manager has been handed to a server.
    ///
    /// `with_auth` moves the live SCRAM state out and leaves `AuthManager::empty()`
    /// behind. That empty manager authenticates nobody, so the old behaviour failed
    /// closed — but it stayed callable, and `add_user` on it went somewhere the
    /// server could never see. The object is poisoned instead, so the mistake is
    /// reported where it is made rather than as "my users mysteriously do not work".
    pub(crate) consumed: bool,
}

impl PyAuthManager {
    fn check_live(&self) -> PyResult<()> {
        if self.consumed {
            return Err(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                "this AuthManager was given to a HaystackServer and is no longer \
                 usable; configure it fully before calling with_auth",
            ));
        }
        Ok(())
    }
}

#[pymethods]
impl PyAuthManager {
    /// Create a disabled (no-auth) manager.
    #[staticmethod]
    fn empty() -> Self {
        Self {
            inner: AuthManager::empty(),
            consumed: false,
        }
    }

    /// Load auth configuration from a TOML file path.
    #[staticmethod]
    fn from_toml(path: &str) -> PyResult<Self> {
        AuthManager::from_toml(path)
            .map(|inner| Self {
                inner,
                consumed: false,
            })
            .map_err(PyErr::new::<exceptions::AuthError, _>)
    }

    /// Load auth configuration from a TOML string.
    #[staticmethod]
    fn from_toml_str(content: &str) -> PyResult<Self> {
        AuthManager::from_toml_str(content)
            .map(|inner| Self {
                inner,
                consumed: false,
            })
            .map_err(PyErr::new::<exceptions::AuthError, _>)
    }

    /// Whether authentication is enabled (has users configured).
    fn is_enabled(&self) -> PyResult<bool> {
        self.check_live()?;
        Ok(self.inner.is_enabled())
    }

    // Deliberately does not raise. A repr that throws breaks debuggers and
    // tracebacks, which is exactly where you look when you hit the poison.
    fn __repr__(&self) -> String {
        if self.consumed {
            "AuthManager(consumed)".to_string()
        } else {
            format!("AuthManager(enabled={})", self.inner.is_enabled())
        }
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
/// Builder-pattern configuration: set graph, namespace, auth,
/// then call run() (blocking) or run_background() (returns immediately).
/// Note: with_auth consumes its argument (the original AuthManager becomes
/// empty after the call). with_namespace does not — it copies.
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
    ///
    /// The namespace is copied into the server, so `ns` stays usable. The server
    /// holds its own mutable copy because lib load/unload endpoints mutate it;
    /// later changes on either side do not affect the other.
    fn with_namespace(&mut self, ns: &PyDefNamespace) -> PyResult<()> {
        if let Some(server) = self.inner.take() {
            self.inner = Some(server.with_namespace((*ns.inner).clone()));
        }
        Ok(())
    }

    /// Set the auth manager, consuming it.
    ///
    /// `auth` is unusable afterwards: an AuthManager holds live SCRAM state — the
    /// in-flight handshakes, the issued tokens, and the server secret used to derive
    /// anti-enumeration challenges — so it moves rather than being shared or copied.
    /// Every later call on it raises RuntimeError instead of silently doing nothing.
    fn with_auth(&mut self, auth: &mut PyAuthManager) -> PyResult<()> {
        auth.check_live()?;
        // Claim the server BEFORE poisoning the manager. Taking it first would
        // destroy a perfectly good AuthManager on an already-consumed server and
        // still report success — trading one silent failure for another.
        let server = self
            .inner
            .take()
            .ok_or_else(|| PyErr::new::<exceptions::HaystackError, _>("Server already consumed"))?;
        let taken = std::mem::replace(&mut auth.inner, AuthManager::empty());
        auth.consumed = true;
        self.inner = Some(server.with_auth(taken));
        Ok(())
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
        // Spawn a dedicated thread with its own runtime for the background server
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
            if let Err(e) = rt.block_on(server.run())
                && let Ok(mut slot) = error_slot.lock()
            {
                *slot = Some(e.to_string());
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
