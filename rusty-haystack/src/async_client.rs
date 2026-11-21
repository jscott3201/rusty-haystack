//! Small asyncio API over a resource-owning HTTP client.
use std::{sync::Arc, time::Duration};

use haystack_client::{ClientConfig, ClientError};
use parking_lot::Mutex;
use pyo3::exceptions::{PyRuntimeError, PyTimeoutError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyList;
use pyo3_async_runtimes::TaskLocals;
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::async_bridge;
use crate::client::PyTlsConfig;
use crate::data::PyHGrid;
use crate::http_owner::{Cancellation, Owner, Phase, Settings};

struct Affinity {
    event_loop: Py<PyAny>,
    thread: std::thread::ThreadId,
}

/// An explicitly owned asyncio HTTP/SCRAM session. Construct first, then await
/// connect (or use async with). Methods return Futures, not coroutine objects.
#[pyclass(name = "AsyncHaystackClient", module = "rusty_haystack.client", frozen)]
pub(crate) struct PyAsyncHaystackClient {
    owner: Owner,
    h5: bool,
    pub(crate) context: Arc<ClientContext>,
}

pub(crate) struct ClientContext {
    affinity: Mutex<Option<Affinity>>,
    timeout: Duration,
}

#[pyclass(frozen)]
struct CancelOperation {
    cancellation: Cancellation,
}
#[pymethods]
impl CancelOperation {
    fn __call__(&self, future: &Bound<'_, PyAny>) -> PyResult<()> {
        if future.call_method0("cancelled")?.is_truthy()? {
            self.cancellation.cancel();
        }
        Ok(())
    }
}

pub(crate) fn native_error(error: ClientError) -> PyErr {
    match error {
        ClientError::H5(error) => crate::h5::error(error),
        ClientError::Timeout(_) => PyTimeoutError::new_err("async client deadline expired"),
        error => crate::exceptions::ClientError::new_err(error.to_string()),
    }
}

fn validate_h5_id(id: &crate::kinds::PyRef_) -> PyResult<()> {
    if id.val.len() > haystack_client::h5::MAX_ID_BYTES {
        return Err(PyValueError::new_err(
            "H5 IDs allow at most 8192 UTF-8 bytes",
        ));
    }
    Ok(())
}

impl ClientContext {
    /// All Python calls happen outside the affinity mutex. Identity comparisons
    /// themselves use pointer equality, never user-overridable Python methods.
    pub(crate) fn context(
        &self,
        py: Python<'_>,
        deadline: Option<f64>,
    ) -> PyResult<(TaskLocals, Instant)> {
        let event_loop = py.import("asyncio")?.call_method0("get_running_loop")?;
        let thread = std::thread::current().id();
        {
            let mut affinity = self.affinity.lock();
            if let Some(bound) = affinity.as_ref() {
                if bound.thread != thread || !bound.event_loop.bind(py).is(&event_loop) {
                    return Err(PyRuntimeError::new_err(
                        "async client belongs to a different event loop or OS thread",
                    ));
                }
            } else {
                *affinity = Some(Affinity {
                    event_loop: event_loop.clone().unbind(),
                    thread,
                });
            }
        }
        // Convert the bound loop's clock once. Explicit deadlines may shorten
        // the constructor budget; neither queueing nor HTTP resets this budget.
        let now = Instant::now();
        let budget = if let Some(deadline) = deadline {
            if !deadline.is_finite() {
                return Err(PyValueError::new_err("deadline must be finite"));
            }
            let loop_now: f64 = event_loop.call_method0("time")?.extract()?;
            let remaining = deadline - loop_now;
            if !loop_now.is_finite() || !remaining.is_finite() {
                return Err(PyValueError::new_err("deadline is out of range"));
            }
            if remaining <= 0.0 {
                Duration::ZERO
            } else {
                Duration::try_from_secs_f64(remaining)
                    .map_err(|_| PyValueError::new_err("deadline is out of range"))?
                    .min(self.timeout)
            }
        } else {
            self.timeout
        };
        let deadline = now
            .checked_add(budget)
            .ok_or_else(|| PyValueError::new_err("deadline is out of range"))?;
        Ok((TaskLocals::new(event_loop).copy_context(py)?, deadline))
    }
}
impl PyAsyncHaystackClient {
    fn start_h5<'py>(
        &self,
        py: Python<'py>,
        request: haystack_client::h5::ReadRequest,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if !self.h5 {
            return Err(PyValueError::new_err(
                "selected reads require h5=H5Config()",
            ));
        }
        request.validate().map_err(|_| {
            PyValueError::new_err(
                "H5 reads allow at most 1000 IDs of at most 8192 UTF-8 bytes each",
            )
        })?;
        let (locals, deadline) = self.context.context(py, deadline)?;
        let mut operation = self
            .owner
            .read_h5(request, deadline)
            .map_err(native_error)?;
        let cancellation = operation.cancellation();
        let control = cancellation.read_control(deadline);
        let future = async_bridge::future(py, locals, async move {
            let value = operation.result().await;
            control.check().map_err(native_error)?;
            let result = value.map_err(native_error).and_then(|value| {
                async_bridge::with_python(|py| {
                    control.check().map_err(native_error)?;
                    crate::convert::kind_to_py(py, &value)
                })
            });
            control.check().map_err(native_error)?;
            drop(operation);
            result
        })?;
        add_cancellation(&future, cancellation)?;
        Ok(future)
    }

    fn start_connect<'py>(
        &self,
        py: Python<'py>,
        deadline: Option<f64>,
        value: Py<PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.context.context(py, deadline)?;
        // Initialize the shared bridge before starting a native owner, so its
        // reaper can always run even when Future creation or notification fails.
        let _ = async_bridge::runtime();
        let mut operation = self.owner.connect(deadline).map_err(native_error)?;
        let cancellation = operation.cancellation();
        let future = async_bridge::future(py, locals, async move {
            operation.result().await.map_err(native_error)?;
            // `value` is already a Python object. The operation guard is held
            // through all client-owned conversion; bridge notification is separate.
            drop(operation);
            Ok(value)
        })?;
        add_cancellation(&future, cancellation)?;
        Ok(future)
    }
}

fn borrowed_h5_ids<'py>(
    ids: impl Iterator<Item = Bound<'py, PyAny>>,
) -> PyResult<Vec<PyRef<'py, crate::kinds::PyRef_>>> {
    ids.take(haystack_client::h5::MAX_IDS + 1)
        .enumerate()
        .map(|(index, item)| {
            // The Python list can grow after the length precheck. Bound both
            // the iterator's allocation hint and references borrowed here.
            if index == haystack_client::h5::MAX_IDS {
                return Err(PyValueError::new_err("H5 reads allow at most 1000 IDs"));
            }
            let id = item.extract::<PyRef<'_, crate::kinds::PyRef_>>()?;
            validate_h5_id(&id)?;
            Ok(id)
        })
        .collect()
}
fn add_cancellation(future: &Bound<'_, PyAny>, cancellation: Cancellation) -> PyResult<()> {
    let callback = Py::new(
        future.py(),
        CancelOperation {
            cancellation: cancellation.clone(),
        },
    )?;
    if let Err(error) = future.call_method1("add_done_callback", (callback,)) {
        cancellation.cancel();
        return Err(error);
    }
    Ok(())
}

#[pymethods]
impl PyAsyncHaystackClient {
    #[new]
    #[pyo3(signature = (url, username, password, *, tls=None, timeout=30.0, tls_verify=true, h5=None))]
    fn new(
        url: String,
        username: String,
        password: String,
        tls: Option<&PyTlsConfig>,
        timeout: f64,
        tls_verify: bool,
        h5: Option<&crate::h5::PyH5Config>,
    ) -> PyResult<Self> {
        if !timeout.is_finite() || timeout <= 0.0 {
            return Err(PyValueError::new_err("timeout must be positive and finite"));
        }
        let timeout = Duration::try_from_secs_f64(timeout)
            .map_err(|_| PyValueError::new_err("timeout is out of range"))?;
        if Instant::now().checked_add(timeout).is_none() {
            return Err(PyValueError::new_err("timeout is out of range"));
        }
        let config = ClientConfig {
            timeout,
            auth_timeout: timeout,
            tls: tls.map(|tls| tls.inner.clone()),
            tls_verify,
            ..ClientConfig::default()
        };
        Ok(Self {
            h5: h5.is_some(),
            owner: Owner::new(Settings {
                url,
                username,
                password: Zeroizing::new(password),
                config,
            }),
            context: Arc::new(ClientContext {
                affinity: Mutex::new(None),
                timeout,
            }),
        })
    }

    #[getter]
    fn state(&self) -> &'static str {
        self.owner.phase().name()
    }
    #[getter]
    fn closed(&self) -> bool {
        self.owner.phase() == Phase::Closed
    }

    #[pyo3(signature = (*, deadline=None))]
    fn connect<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        self.start_connect(py, deadline, py.None())
    }

    #[pyo3(signature = (filter, limit=100, *, deadline=None))]
    fn read<'py>(
        &self,
        py: Python<'py>,
        filter: &str,
        limit: usize,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.context.context(py, deadline)?;
        if !(1..=1000).contains(&limit) {
            return Err(PyValueError::new_err("limit must be between 1 and 1000"));
        }
        if filter.len() > 8192 {
            return Err(PyValueError::new_err("filter exceeds 8192 UTF-8 bytes"));
        }
        let mut operation = self
            .owner
            .read(filter.to_owned(), limit, deadline)
            .map_err(native_error)?;
        let cancellation = operation.cancellation();
        let future = async_bridge::future(py, locals, async move {
            let grid = operation
                .result()
                .await
                .map_err(native_error)?
                .ok_or_else(|| native_error(ClientError::ConnectionClosed))?;
            // Complete conversion now, rather than passing a pyclass to the
            // bridge's later generic IntoPyObject conversion after guard release.
            let result = convert_read_result(&grid);
            drop(operation);
            result
        })?;
        add_cancellation(&future, cancellation)?;
        Ok(future)
    }

    #[pyo3(signature = (id=None, *, checked=true, deadline=None))]
    fn read_by_id<'py>(
        &self,
        py: Python<'py>,
        id: Option<&crate::kinds::PyRef_>,
        checked: bool,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if let Some(id) = id {
            validate_h5_id(id)?;
        }
        self.start_h5(
            py,
            haystack_client::h5::ReadRequest::ById {
                id: id.map(|id| haystack_core::kinds::HRef::from_val(&id.val)),
                checked,
            },
            deadline,
        )
    }

    #[pyo3(signature = (ids, *, checked=true, deadline=None))]
    fn read_by_ids<'py>(
        &self,
        py: Python<'py>,
        ids: &Bound<'_, PyList>,
        checked: bool,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        if ids.len() > haystack_client::h5::MAX_IDS {
            return Err(PyValueError::new_err("H5 reads allow at most 1000 IDs"));
        }
        // Borrow only a bounded number of references, validate all byte lengths,
        // then copy owned IDs. PyList iteration does not invoke subclass hooks.
        let ids = borrowed_h5_ids(ids.iter())?;
        self.start_h5(
            py,
            haystack_client::h5::ReadRequest::ByIds {
                ids: ids
                    .iter()
                    .map(|id| haystack_core::kinds::HRef::from_val(&id.val))
                    .collect(),
                checked,
            },
            deadline,
        )
    }

    #[pyo3(signature = (ids, *, key, lease_ms=60_000, cleanup_timeout=2.0))]
    fn watch(
        &self,
        py: Python<'_>,
        ids: Vec<String>,
        key: String,
        lease_ms: u64,
        cleanup_timeout: f64,
    ) -> PyResult<crate::async_watch::PyAsyncWatch> {
        let _ = self.context.context(py, None)?;
        if !cleanup_timeout.is_finite() || cleanup_timeout <= 0.0 || cleanup_timeout > 30.0 {
            return Err(PyValueError::new_err(
                "cleanup_timeout must be finite and between 0 and 30 seconds",
            ));
        }
        let handle = self
            .owner
            .watch(ids, key, lease_ms, Duration::from_secs_f64(cleanup_timeout))
            .map_err(native_error)?;
        Ok(crate::async_watch::PyAsyncWatch {
            handle,
            context: self.context.clone(),
        })
    }

    #[pyo3(signature = (*, deadline=None))]
    fn close<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.context.context(py, deadline)?;
        let expired = deadline <= Instant::now() && self.owner.phase() != Phase::Closed;
        self.owner.begin_close(); // Even a valid already-expired budget seals admission.
        let owner = self.owner.clone();
        async_bridge::future(py, locals, async move {
            if expired {
                return Err(PyTimeoutError::new_err("async client deadline expired"));
            }
            owner.wait_closed(deadline).await.map_err(native_error)?;
            async_bridge::with_python(|py| Ok(py.None()))
        })
    }

    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        let client = slf.get();
        client.start_connect(py, None, slf.clone_ref(py).into_any())
    }

    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: &Bound<'py, PyAny>,
        exc_value: Option<Py<PyAny>>,
        _traceback: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.context.context(py, None)?;
        self.owner.begin_close();
        let owner = self.owner.clone();
        async_bridge::future(py, locals, async move {
            let result = owner.wait_closed(deadline).await;
            async_bridge::with_python(|py| finish_exit(py, result, exc_value))
        })
    }
}
impl Drop for PyAsyncHaystackClient {
    fn drop(&mut self) {
        self.owner.begin_close();
    }
}

fn convert_read_result(grid: &haystack_core::data::HGrid) -> PyResult<Py<PyAny>> {
    async_bridge::with_python(|py| Py::new(py, PyHGrid::from_core(grid)?).map(Py::into_any))
}

fn finish_exit(
    py: Python<'_>,
    result: Result<(), ClientError>,
    exc_value: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    if let Err(error) = result {
        if let Some(exception) = exc_value {
            // User-overridden add_note may fail. Never replace the original
            // body exception with a cleanup or annotation error.
            let _ = exception.call_method1(
                py,
                "add_note",
                (format!(
                    "AsyncHaystackClient cleanup: {}",
                    native_error(error)
                ),),
            );
        } else {
            return Err(native_error(error));
        }
    }
    Ok(py.None())
}

#[cfg(test)]
mod tests {
    #[test]
    fn h5_list_growth_after_length_check_cannot_expand_borrowed_batch() {
        Python::initialize();
        Python::attach(|py| {
            let id = Py::new(py, crate::kinds::PyRef_::new("a".into(), None)).unwrap();
            let ids = PyList::new(
                py,
                (0..haystack_client::h5::MAX_IDS).map(|_| id.clone_ref(py)),
            )
            .unwrap();
            assert_eq!(ids.len(), haystack_client::h5::MAX_IDS);
            assert_eq!(
                borrowed_h5_ids(ids.iter()).unwrap().len(),
                haystack_client::h5::MAX_IDS
            );
            // This is the interleaving between the caller's length precheck and
            // iterator creation; the source can grow on another Python thread.
            ids.append(id).unwrap();
            let error = borrowed_h5_ids(ids.iter())
                .err()
                .expect("grown list must fail");
            assert!(error.is_instance_of::<PyValueError>(py));
        });
    }
    use super::*;
    use pyo3::types::PyDict;
    use std::ffi::CString;

    #[test]
    fn successful_read_conversion_without_python_returns_error() {
        const CHILD: &str = "HAYSTACK_TEST_UNAVAILABLE_READ_CONVERSION";
        if std::env::var_os(CHILD).is_some() {
            // A successful native result exists, but this isolated process has
            // never initialized Python. This deterministically covers attachment
            // unavailability without finalizing an interpreter used by any test.
            let grid = haystack_core::data::HGrid::from_parts(
                haystack_core::data::HDict::new(),
                vec![haystack_core::data::HCol::new("id")],
                vec![haystack_core::data::HDict::new()],
            );
            assert!(convert_read_result(&grid).is_err());
            // A positive control exercises the same successful grid conversion
            // after initialization; this test never finalizes Python.
            Python::initialize();
            let converted = convert_read_result(&grid).unwrap();
            Python::attach(|py| assert_eq!(converted.bind(py).len().unwrap(), 1));
            return;
        }
        // cargo test can share an interpreter with other tests. Always isolate
        // this seam in a supervised fresh process, including under nextest.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "async_client::tests::successful_read_conversion_without_python_returns_error",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("unavailable-interpreter conversion child did not exit");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "unavailable-interpreter conversion child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn cleanup_error_preserves_body_exception_even_when_annotation_fails() {
        Python::initialize();
        Python::attach(|py| {
            let body = PyValueError::new_err("original body error")
                .value(py)
                .clone()
                .into_any()
                .unbind();
            assert!(
                finish_exit(
                    py,
                    Err(ClientError::Timeout(Duration::ZERO)),
                    Some(body.clone_ref(py))
                )
                .unwrap()
                .is_none(py)
            );
            let notes: Vec<String> = body.getattr(py, "__notes__").unwrap().extract(py).unwrap();
            assert_eq!(notes.len(), 1);
            assert!(notes[0].contains("deadline expired"));
            let scope = PyDict::new(py);
            py.run(&CString::new("class BodyError(Exception):\n    def add_note(self, note):\n        raise RuntimeError('annotation failed')\nbody = BodyError('original')").unwrap(), Some(&scope), Some(&scope)).unwrap();
            let body = scope.get_item("body").unwrap().unwrap().unbind();
            assert!(
                finish_exit(py, Err(ClientError::ConnectionClosed), Some(body))
                    .unwrap()
                    .is_none(py)
            );
            assert!(
                finish_exit(py, Err(ClientError::Timeout(Duration::ZERO)), None)
                    .unwrap_err()
                    .is_instance_of::<PyTimeoutError>(py)
            );
        });
    }
}
