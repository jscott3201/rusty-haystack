//! Explicit blocking H4 facade over the same native owner as the asyncio API.
use std::{future::Future, thread::ThreadId, time::Duration};

use haystack_client::{ClientConfig, ClientError};
use parking_lot::Mutex;
use pyo3::exceptions::{PyRuntimeError, PyTimeoutError, PyValueError};
use pyo3::prelude::*;
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::async_bridge;
use crate::client::PyTlsConfig;
use crate::data::PyHGrid;
use crate::http_owner::{Operation, Owner, Phase, Settings};

/// An explicitly owned blocking HTTP/SCRAM session, bound on first valid use.
#[pyclass(name = "SyncHaystackClient", module = "rusty_haystack.client", frozen)]
pub(crate) struct PySyncHaystackClient {
    owner: Owner,
    affinity: Mutex<Option<ThreadId>>,
    timeout: Duration,
}

fn duration(timeout: f64) -> PyResult<Duration> {
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(PyValueError::new_err("timeout must be positive and finite"));
    }
    let value = Duration::try_from_secs_f64(timeout)
        .map_err(|_| PyValueError::new_err("timeout is out of range"))?;
    if value.is_zero() || Instant::now().checked_add(value).is_none() {
        return Err(PyValueError::new_err("timeout is out of range"));
    }
    Ok(value)
}

fn native_error(error: ClientError) -> PyErr {
    match error {
        ClientError::Timeout(_) => PyTimeoutError::new_err("synchronous client deadline expired"),
        error => crate::async_client::native_error(error),
    }
}

fn check_deadline(deadline: Instant) -> PyResult<()> {
    if Instant::now() >= deadline {
        Err(native_error(ClientError::Timeout(Duration::ZERO)))
    } else {
        Ok(())
    }
}

fn check_blocking_context(py: Python<'_>) -> PyResult<()> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(PyRuntimeError::new_err(
            "SyncHaystackClient cannot block inside an entered Tokio runtime",
        ));
    }
    match py.import("asyncio")?.call_method0("get_running_loop") {
        Ok(_) => Err(PyRuntimeError::new_err(
            "SyncHaystackClient cannot block a running asyncio loop; use AsyncHaystackClient or run the entire synchronous flow in one worker thread",
        )),
        Err(error) if error.is_instance_of::<PyRuntimeError>(py) => Ok(()),
        Err(error) => Err(error),
    }
}

/// The runtime is process-wide. Only native futures cross the detached wait;
/// conversion and Python exception handling happen back on the caller thread.
fn wait<T: Send>(
    py: Python<'_>,
    deadline: Instant,
    future: impl Future<Output = Result<T, ClientError>> + Send,
) -> PyResult<T> {
    py.detach(|| {
        async_bridge::runtime().block_on(async {
            tokio::time::timeout_at(deadline, future)
                .await
                .unwrap_or(Err(ClientError::Timeout(Duration::ZERO)))
        })
    })
    .map_err(native_error)
}

fn finish_operation<T: Send, R>(
    py: Python<'_>,
    mut operation: Operation<T>,
    deadline: Instant,
    convert: impl FnOnce(T) -> PyResult<R>,
) -> PyResult<R> {
    let result = wait(py, deadline, operation.result()).and_then(|value| {
        check_deadline(deadline)?;
        convert(value)
    });
    // Conversion and GIL reacquisition consume the original budget as well.
    // Keep both the admission ticket and connect cancellation authority until
    // this final publication check has completed.
    let result = check_deadline(deadline).and(result);
    if result.is_err() {
        operation.cancellation().cancel();
    }
    drop(operation);
    result
}

impl PySyncHaystackClient {
    fn deadline(&self, timeout: Option<f64>) -> PyResult<Instant> {
        let now = Instant::now();
        let budget = timeout
            .map(duration)
            .transpose()?
            .unwrap_or(self.timeout)
            .min(self.timeout);
        now.checked_add(budget)
            .ok_or_else(|| PyValueError::new_err("timeout is out of range"))
    }

    fn admit<T>(
        &self,
        py: Python<'_>,
        action: impl FnOnce() -> Result<T, ClientError>,
    ) -> PyResult<T> {
        // No Python call occurs under the affinity lock. Rejected context,
        // input, or native admission cannot bind a previously unused client.
        check_blocking_context(py)?;
        let thread = std::thread::current().id();
        let mut affinity = self.affinity.lock();
        if affinity.is_some_and(|bound| bound != thread) {
            return Err(PyRuntimeError::new_err(
                "SyncHaystackClient belongs to a different OS thread",
            ));
        }
        let value = action().map_err(native_error)?;
        *affinity = Some(thread);
        Ok(value)
    }

    fn close_until(&self, py: Python<'_>, deadline: Instant) -> PyResult<()> {
        self.admit(py, || {
            self.owner.begin_close();
            Ok(())
        })?;
        let result = wait(py, deadline, self.owner.wait_closed(deadline));
        check_deadline(deadline).and(result)
    }
}

#[pymethods]
impl PySyncHaystackClient {
    #[new]
    #[pyo3(signature = (url, username, password, *, tls=None, timeout=30.0, tls_verify=true))]
    fn new(
        url: String,
        username: String,
        password: String,
        tls: Option<&PyTlsConfig>,
        timeout: f64,
        tls_verify: bool,
    ) -> PyResult<Self> {
        let timeout = duration(timeout)?;
        let config = ClientConfig {
            timeout,
            auth_timeout: timeout,
            tls: tls.map(|tls| tls.inner.clone()),
            tls_verify,
            ..ClientConfig::default()
        };
        Ok(Self {
            owner: Owner::new(Settings {
                url,
                username,
                password: Zeroizing::new(password),
                config,
            }),
            affinity: Mutex::new(None),
            timeout,
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

    #[pyo3(signature = (*, timeout=None))]
    fn connect(&self, py: Python<'_>, timeout: Option<f64>) -> PyResult<()> {
        let deadline = self.deadline(timeout)?;
        let operation = self.admit(py, || {
            // Initialize the shared reaper runtime before starting native I/O.
            let _ = async_bridge::runtime();
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout(Duration::ZERO));
            }
            self.owner.connect(deadline)
        })?;
        finish_operation(py, operation, deadline, |_| Ok(()))
    }

    #[pyo3(signature = (filter, limit=100, *, timeout=None))]
    fn read(
        &self,
        py: Python<'_>,
        filter: &str,
        limit: usize,
        timeout: Option<f64>,
    ) -> PyResult<Py<PyHGrid>> {
        let deadline = self.deadline(timeout)?;
        if !(1..=1000).contains(&limit) {
            return Err(PyValueError::new_err("limit must be between 1 and 1000"));
        }
        if filter.len() > 8192 {
            return Err(PyValueError::new_err("filter exceeds 8192 UTF-8 bytes"));
        }
        let operation = self.admit(py, || {
            if Instant::now() >= deadline {
                return Err(ClientError::Timeout(Duration::ZERO));
            }
            self.owner.read(filter.to_owned(), limit, deadline)
        })?;
        finish_operation(py, operation, deadline, |grid| {
            let grid = grid.ok_or_else(|| native_error(ClientError::ConnectionClosed))?;
            Py::new(py, PyHGrid::from_core(&grid)?)
        })
    }

    #[pyo3(signature = (*, timeout=None))]
    fn close(&self, py: Python<'_>, timeout: Option<f64>) -> PyResult<()> {
        self.close_until(py, self.deadline(timeout)?)
    }

    fn __enter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        slf.borrow(py).connect(py, None)?;
        Ok(slf)
    }

    fn __exit__(
        &self,
        py: Python<'_>,
        exc_type: &Bound<'_, PyAny>,
        exc_value: Option<Py<PyAny>>,
        traceback: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let _ = (exc_type, traceback);
        finish_exit(py, self.close(py, None), exc_value)
    }
}

fn finish_exit(py: Python<'_>, result: PyResult<()>, exc_value: Option<Py<PyAny>>) -> PyResult<()> {
    if let Err(error) = result {
        if let Some(exception) = exc_value {
            let _ = exception.call_method1(
                py,
                "add_note",
                (format!("SyncHaystackClient cleanup: {error}"),),
            );
        } else {
            return Err(error);
        }
    }
    Ok(())
}

impl Drop for PySyncHaystackClient {
    fn drop(&mut self) {
        self.owner.begin_close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_owner::tests::{HeldOwner, held_owner, ready_operation};
    use pyo3::types::PyDict;
    use std::ffi::CString;

    fn client(owner: Owner, timeout: Duration) -> PySyncHaystackClient {
        PySyncHaystackClient {
            owner,
            affinity: Mutex::new(None),
            timeout,
        }
    }

    #[test]
    fn synchronous_close_joins_runtime_then_conversion_and_repeated_close() {
        Python::initialize();
        let HeldOwner {
            owner,
            release,
            thread_exited,
            conversion,
        } = held_owner();
        let client = client(owner, Duration::from_secs(2));
        Python::attach(|py| {
            assert!(
                client
                    .close(py, Some(0.01))
                    .unwrap_err()
                    .is_instance_of::<PyTimeoutError>(py)
            );
            assert_eq!(client.state(), "closing");
            assert!(thread_exited.try_recv().is_err());
            release.send(()).unwrap();
            py.detach(move || thread_exited.recv_timeout(Duration::from_secs(2)))
                .unwrap();
            assert!(
                client
                    .close(py, Some(0.01))
                    .unwrap_err()
                    .is_instance_of::<PyTimeoutError>(py)
            );
            assert_eq!(client.state(), "closing");
            drop(conversion);
            client.close(py, None).unwrap();
            assert!(client.closed());
            client.close(py, None).unwrap();
        });
    }

    #[test]
    fn entered_runtime_rejection_has_no_affinity_or_owner_effect() {
        Python::initialize();
        let client = PySyncHaystackClient::new(
            "http://localhost:1".into(),
            "user".into(),
            "password".into(),
            None,
            2.0,
            true,
        )
        .unwrap();
        Python::attach(|py| {
            let entered = async_bridge::runtime().enter();
            for result in [
                client.connect(py, None),
                client.close(py, None),
                client.read(py, "site", 100, None).map(|_| ()),
            ] {
                let error = result.unwrap_err();
                assert!(error.is_instance_of::<PyRuntimeError>(py));
                assert!(error.to_string().contains("Tokio"));
            }
            drop(entered);
            assert_eq!(client.state(), "new");
            assert!(client.affinity.lock().is_none());
        });
        std::thread::spawn(move || Python::attach(|py| client.close(py, None).unwrap()))
            .join()
            .unwrap();
    }

    #[test]
    fn conversion_holds_ticket_and_expiry_prevents_success_publication() {
        Python::initialize();
        let HeldOwner {
            owner,
            release,
            thread_exited,
            conversion,
        } = held_owner();
        release.send(()).unwrap();
        thread_exited.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(conversion);
        let operation = ready_operation(&owner, false);
        Python::attach(|py| {
            let deadline = Instant::now() + Duration::from_millis(20);
            let error = finish_operation(py, operation, deadline, |_| {
                owner.begin_close();
                // A real already-returned native thread is insufficient while
                // this result conversion still owns its operation ticket.
                assert!(
                    wait(
                        py,
                        Instant::now() + Duration::from_millis(2),
                        owner.wait_closed(deadline)
                    )
                    .is_err()
                );
                assert_eq!(owner.phase(), Phase::Closing);
                while Instant::now() < deadline {
                    std::thread::yield_now();
                }
                Ok("must not publish")
            })
            .unwrap_err();
            assert!(error.is_instance_of::<PyTimeoutError>(py));
            wait(
                py,
                Instant::now() + Duration::from_secs(2),
                owner.wait_closed(Instant::now() + Duration::from_secs(2)),
            )
            .unwrap();
            assert_eq!(owner.phase(), Phase::Closed);
        });
    }

    #[test]
    fn already_expired_connect_result_cancels_owner_before_publication() {
        Python::initialize();
        let HeldOwner {
            owner,
            release,
            thread_exited,
            conversion,
        } = held_owner();
        release.send(()).unwrap();
        thread_exited.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(conversion);
        let operation = ready_operation(&owner, true);
        Python::attach(|py| {
            let result: PyResult<()> = finish_operation(
                py,
                operation,
                Instant::now() - Duration::from_millis(1),
                |_| panic!("expired native result reached conversion"),
            );
            assert!(result.unwrap_err().is_instance_of::<PyTimeoutError>(py));
            wait(
                py,
                Instant::now() + Duration::from_secs(2),
                owner.wait_closed(Instant::now() + Duration::from_secs(2)),
            )
            .unwrap();
            assert_eq!(owner.phase(), Phase::Closed);
        });
    }

    #[test]
    fn timed_out_context_exit_preserves_body_even_when_annotation_fails() {
        Python::initialize();
        let HeldOwner {
            owner,
            release,
            thread_exited,
            conversion,
        } = held_owner();
        let client = client(owner, Duration::from_millis(10));
        Python::attach(|py| {
            let body = PyValueError::new_err("original body")
                .value(py)
                .clone()
                .into_any()
                .unbind();
            client
                .__exit__(
                    py,
                    py.None().bind(py),
                    Some(body.clone_ref(py)),
                    py.None().bind(py),
                )
                .unwrap();
            let notes: Vec<String> = body.getattr(py, "__notes__").unwrap().extract(py).unwrap();
            assert_eq!(notes.len(), 1);
            assert!(
                notes[0].contains("SyncHaystackClient cleanup")
                    && notes[0].contains("deadline expired")
            );
            let scope = PyDict::new(py);
            py.run(&CString::new("class BodyError(Exception):\n    def add_note(self, note):\n        raise RuntimeError('annotation failed')\nbody = BodyError('original')").unwrap(), Some(&scope), Some(&scope)).unwrap();
            let body = scope.get_item("body").unwrap().unwrap().unbind();
            client
                .__exit__(py, py.None().bind(py), Some(body), py.None().bind(py))
                .unwrap();
            assert!(
                client
                    .__exit__(py, py.None().bind(py), None, py.None().bind(py))
                    .unwrap_err()
                    .is_instance_of::<PyTimeoutError>(py)
            );
            assert_eq!(client.state(), "closing");
            release.send(()).unwrap();
            py.detach(move || thread_exited.recv_timeout(Duration::from_secs(2)))
                .unwrap();
            drop(conversion);
            wait(
                py,
                Instant::now() + Duration::from_secs(2),
                client
                    .owner
                    .wait_closed(Instant::now() + Duration::from_secs(2)),
            )
            .unwrap();
            client.close(py, None).unwrap();
        });
    }
}
