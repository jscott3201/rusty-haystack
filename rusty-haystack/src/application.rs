//! Controlled embedding of the native application, borrowing the shared bridge.
use std::{
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use haystack_app::{
    AllowAll, ApplicationBuilder, ApplicationHandle, ApplicationOwner, ApplicationState,
    OutputProfile, Principal, ReadContext, ReadError, ReadLimits, ReadOutput, ReadQuery,
    ReadRequest, TerminationReport, WorkGuard,
};
use haystack_core::graph::{EntityGraph, SharedGraph};
use haystack_server::{HaystackServer, auth::AuthManager};
use parking_lot::Mutex;
use pyo3::{
    exceptions::{PyRuntimeError, PyTimeoutError, PyValueError},
    prelude::*,
};
use pyo3_async_runtimes::TaskLocals;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{async_bridge, data::PyHGrid, graph::PySharedGraph, server::PyAuthManager};

const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PENDING_READS: usize = 16;

struct EmbeddingPolicy;
impl haystack_app::ReadPolicy for EmbeddingPolicy {
    fn snapshot(
        &self,
        principal: &Principal,
    ) -> Result<Arc<dyn haystack_app::PolicySnapshot>, ReadError> {
        match principal {
            Principal::TrustedEmbedding { .. } => Ok(Arc::new(AllowAll)),
            Principal::Authenticated { permissions, .. }
                if permissions
                    .iter()
                    .any(|permission| permission == "read" || permission == "admin") =>
            {
                Ok(Arc::new(AllowAll))
            }
            _ => Err(ReadError::Forbidden),
        }
    }
}

fn application_error(error: impl std::fmt::Display) -> PyErr {
    crate::exceptions::ApplicationError::new_err(error.to_string())
}
fn read_error(error: ReadError) -> PyErr {
    match error {
        ReadError::Deadline => timeout(),
        error => application_error(error),
    }
}
fn timeout() -> PyErr {
    PyTimeoutError::new_err("application observation deadline expired")
}
fn state_name(state: ApplicationState) -> &'static str {
    match state {
        ApplicationState::Starting => "starting",
        ApplicationState::Running => "running",
        ApplicationState::Closing => "closing",
        ApplicationState::Closed => "closed",
        ApplicationState::Failed => "failed",
    }
}

// Shared by owner and borrowed views; all Python calls occur without locks.
struct Affinity {
    event_loop: Py<PyAny>,
    thread: std::thread::ThreadId,
}
impl Affinity {
    fn capture(py: Python<'_>) -> PyResult<Self> {
        Ok(Self {
            event_loop: py
                .import("asyncio")?
                .call_method0("get_running_loop")?
                .unbind(),
            thread: std::thread::current().id(),
        })
    }
    fn context(&self, py: Python<'_>, deadline: Option<f64>) -> PyResult<(TaskLocals, Instant)> {
        let event_loop = py.import("asyncio")?.call_method0("get_running_loop")?;
        if self.thread != std::thread::current().id() || !self.event_loop.bind(py).is(&event_loop) {
            return Err(PyRuntimeError::new_err(
                "application belongs to a different event loop or OS thread",
            ));
        }
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
                    .min(OBSERVATION_TIMEOUT)
            }
        } else {
            OBSERVATION_TIMEOUT
        };
        Ok((TaskLocals::new(event_loop).copy_context(py)?, now + budget))
    }
}

struct View {
    native: ApplicationHandle,
    affinity: Affinity,
    pending: Arc<Semaphore>,
}
impl View {
    fn ready<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.affinity.context(py, deadline)?;
        let handle = self.native.clone();
        async_bridge::future(py, locals, async move {
            let ready = observe(deadline, handle.ready())
                .await?
                .map_err(application_error)?;
            let listener = ready.listeners.first().ok_or_else(|| {
                application_error("application listener did not report an address")
            })?;
            let address = (listener.address.ip().to_string(), listener.address.port());
            async_bridge::with_python(|py| Ok(address.into_pyobject(py)?.into_any().unbind()))
        })
    }
    fn terminated<'py>(
        &self,
        py: Python<'py>,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.affinity.context(py, deadline)?;
        let handle = self.native.clone();
        async_bridge::future(py, locals, async move {
            let report = termination(&handle, deadline).await?;
            async_bridge::with_python(|py| {
                Py::new(py, PyApplicationTermination::from(report)).map(Py::into_any)
            })
        })
    }
    fn read<'py>(
        &self,
        py: Python<'py>,
        filter: &str,
        limit: usize,
        cursor: Option<&str>,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.affinity.context(py, deadline)?;
        if !(1..=1000).contains(&limit) {
            return Err(PyValueError::new_err("limit must be between 1 and 1000"));
        }
        if filter.len() > 8192 || cursor.as_ref().is_some_and(|cursor| cursor.len() > 8192) {
            return Err(PyValueError::new_err(
                "filter and cursor must each fit 8192 UTF-8 bytes",
            ));
        }
        if deadline <= Instant::now() {
            return Err(timeout());
        }
        let permit = reserve_read(&self.pending)?;
        let work = ReadWork {
            guard: self.native.admit().map_err(application_error)?,
            _permit: permit,
        };
        let cancel = CancellationToken::new();
        let cancellation = cancel.clone();
        let stop_on_drop = cancel.clone().drop_guard();
        let context = ReadContext::new(
            Principal::TrustedEmbedding {
                subject: "python-embedding".into(),
            },
            deadline.into_std(),
            cancel,
        );
        let mut request =
            ReadRequest::new(ReadQuery::Filter(filter.to_owned()), OutputProfile::Typed);
        request.page_size = limit;
        request.cursor = cursor.map(str::to_owned);
        let service = self.native.read_service();
        let future = async_bridge::future(py, locals, async move {
            let _stop_on_drop = stop_on_drop;
            let admission = service
                .begin_admitted(context, work.guard.child())
                .await
                .map_err(read_error)?;
            let page = admission.read_joined(request).await.map_err(read_error)?;
            let ReadOutput::Typed(grid) = page.output else {
                return Err(application_error("unexpected read output"));
            };
            convert_read(&grid, work)
        })?;
        add_cancel(&future, cancellation)?;
        Ok(future)
    }
}

struct ReadWork {
    guard: WorkGuard,
    _permit: OwnedSemaphorePermit,
}
fn reserve_read(pending: &Arc<Semaphore>) -> PyResult<OwnedSemaphorePermit> {
    pending
        .clone()
        .try_acquire_owned()
        .map_err(|_| application_error("too many pending Python reads"))
}
fn convert_read(grid: &haystack_core::data::HGrid, work: ReadWork) -> PyResult<Py<PyAny>> {
    convert_read_with(work, |py| {
        Py::new(py, PyHGrid::from_core(grid)?).map(Py::into_any)
    })
}
fn convert_read_with(
    work: ReadWork,
    convert: impl for<'py> FnOnce(Python<'py>) -> PyResult<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    let result = async_bridge::with_python(convert);
    // Native termination includes successful and failed Python conversion.
    drop(work);
    result
}
async fn observe<T>(
    deadline: Instant,
    future: impl std::future::Future<Output = T>,
) -> PyResult<T> {
    if deadline <= Instant::now() {
        return Err(timeout());
    }
    tokio::time::timeout_at(deadline, future)
        .await
        .map_err(|_| timeout())
}
async fn termination(handle: &ApplicationHandle, deadline: Instant) -> PyResult<TerminationReport> {
    // A completed immutable receipt remains observable with an expired budget.
    if handle.is_terminated() {
        Ok(handle.terminated().await)
    } else {
        observe(deadline, handle.terminated()).await
    }
}
async fn joined_close(handle: &ApplicationHandle, deadline: Instant) -> PyResult<()> {
    termination(handle, deadline)
        .await?
        .close
        .map(|_| ())
        .map_err(application_error)
}

#[pyclass(frozen)]
struct CancelObserver {
    cancellation: CancellationToken,
}
#[pymethods]
impl CancelObserver {
    fn __call__(&self, future: &Bound<'_, PyAny>) -> PyResult<()> {
        if future.call_method0("cancelled")?.is_truthy()? {
            self.cancellation.cancel();
        }
        Ok(())
    }
}
fn add_cancel(future: &Bound<'_, PyAny>, cancellation: CancellationToken) -> PyResult<()> {
    let callback = match Py::new(
        future.py(),
        CancelObserver {
            cancellation: cancellation.clone(),
        },
    ) {
        Ok(callback) => callback,
        Err(error) => {
            cancellation.cancel();
            return Err(error);
        }
    };
    if let Err(error) = future.call_method1("add_done_callback", (&callback,)) {
        cancellation.cancel();
        return Err(error);
    }
    Ok(())
}
// Owned by the Python coroutine adapter, including before its first poll. Native
// readiness alone cannot acknowledge that the awaiting Task received success.
#[pyclass(frozen)]
struct EnterGuard {
    owner: Arc<ApplicationOwner>,
    entered: AtomicBool,
}
#[pymethods]
impl EnterGuard {
    fn entered(&self) {
        self.entered.store(true, Ordering::Relaxed);
    }
    fn close(&self) {
        if !self.entered.load(Ordering::Relaxed) {
            self.owner.request_close();
        }
    }
}
impl Drop for EnterGuard {
    fn drop(&mut self) {
        if !self.entered.load(Ordering::Relaxed) {
            self.owner.request_close();
        }
    }
}

/// Immutable selected configuration; start consumes it and borrows the bridge.
#[pyclass(name = "ApplicationBuilder", module = "rusty_haystack.server", frozen)]
pub(crate) struct PyApplicationBuilder {
    configuration: Mutex<Option<Configuration>>,
}
struct Configuration {
    builder: ApplicationBuilder,
    server: HaystackServer,
    auth: Option<Py<PyAuthManager>>,
}
#[pymethods]
impl PyApplicationBuilder {
    #[new]
    #[pyo3(signature=(graph=None, *, host="127.0.0.1", port=0, auth=None))]
    fn new(
        graph: Option<&PySharedGraph>,
        host: &str,
        port: u16,
        auth: Option<Py<PyAuthManager>>,
    ) -> PyResult<Self> {
        let host: IpAddr = host
            .parse()
            .map_err(|_| PyValueError::new_err("host must be a numeric IP address"))?;
        let graph = graph.map_or_else(
            || SharedGraph::new(EntityGraph::new()),
            PySharedGraph::clone_inner,
        );
        let builder = ApplicationBuilder::new(
            graph.clone(),
            Arc::new(EmbeddingPolicy),
            ReadLimits::default(),
        )
        .map_err(read_error)?;
        let host = match host {
            IpAddr::V4(address) => address.to_string(),
            IpAddr::V6(address) => format!("[{address}]"),
        };
        let server = HaystackServer::new(graph)
            .with_scoped_reads(builder.handle())
            .host(&host)
            .port(port);
        Ok(Self {
            configuration: Mutex::new(Some(Configuration {
                builder,
                server,
                auth,
            })),
        })
    }
    fn start(&self, py: Python<'_>) -> PyResult<PyApplicationOwner> {
        let affinity = Affinity::capture(py)?;
        let runtime = async_bridge::runtime();
        let mut configuration = self.configuration.lock();
        let auth = configuration
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("application builder already consumed"))?
            .auth
            .as_ref()
            .map(|auth| auth.clone_ref(py));
        let mut auth = auth
            .as_ref()
            .map(|auth| auth.try_borrow_mut(py))
            .transpose()?;
        if let Some(auth) = auth.as_ref() {
            auth.check_live()?;
        }
        // Only native/PyO3 borrow checks above; no Python callbacks under this lock.
        let Configuration {
            builder,
            mut server,
            ..
        } = configuration.take().expect("checked configuration");
        drop(configuration);
        if let Some(auth) = auth.as_mut() {
            let taken = std::mem::replace(&mut auth.inner, AuthManager::empty());
            auth.consumed = true;
            server = server.with_auth(taken);
        }
        drop(auth);
        let owner = builder
            .owned_resource(server.into_listener())
            .start(runtime.handle())
            .map_err(application_error)?;
        let view = Arc::new(View {
            native: owner.handle(),
            affinity,
            pending: Arc::new(Semaphore::new(MAX_PENDING_READS)),
        });
        Ok(PyApplicationOwner {
            owner: Arc::new(owner),
            view,
        })
    }
}

#[pyclass(name = "ApplicationOwner", module = "rusty_haystack.server", frozen)]
pub(crate) struct PyApplicationOwner {
    owner: Arc<ApplicationOwner>,
    view: Arc<View>,
}
#[pymethods]
impl PyApplicationOwner {
    fn handle(&self) -> PyApplicationHandle {
        PyApplicationHandle {
            view: self.view.clone(),
        }
    }
    #[getter]
    fn state(&self) -> &'static str {
        state_name(self.view.native.state())
    }
    #[getter]
    fn closed(&self) -> bool {
        self.view.native.is_terminated()
    }
    #[pyo3(signature=(*, deadline=None))]
    fn ready<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        self.view.ready(py, deadline)
    }
    #[pyo3(signature=(filter, limit=100, *, cursor=None, deadline=None))]
    fn read<'py>(
        &self,
        py: Python<'py>,
        filter: &str,
        limit: usize,
        cursor: Option<&str>,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.view.read(py, filter, limit, cursor, deadline)
    }
    #[pyo3(signature=(*, deadline=None))]
    fn terminated<'py>(
        &self,
        py: Python<'py>,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.view.terminated(py, deadline)
    }
    #[pyo3(signature=(*, deadline=None))]
    fn close<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.view.affinity.context(py, deadline)?;
        let expired = deadline <= Instant::now() && !self.view.native.is_terminated();
        self.owner.request_close();
        let handle = self.view.native.clone();
        async_bridge::future(py, locals, async move {
            if expired {
                return Err(timeout());
            }
            joined_close(&handle, deadline).await?;
            async_bridge::with_python(|py| Ok(py.None()))
        })
    }
    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        let app = slf.get();
        let _context = app.view.affinity.context(py, None)?;
        let guard = Py::new(
            py,
            EnterGuard {
                owner: app.owner.clone(),
                entered: AtomicBool::new(false),
            },
        )?;
        py.import("rusty_haystack._application")?
            .getattr("Entry")?
            .call1((slf, guard))
    }
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: &Bound<'py, PyAny>,
        exc_value: Option<Py<PyAny>>,
        _traceback: &Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.view.affinity.context(py, None)?;
        self.owner.request_close();
        let handle = self.view.native.clone();
        async_bridge::future(py, locals, async move {
            let result = joined_close(&handle, deadline).await;
            async_bridge::with_python(|py| finish_exit(py, result, exc_value))
        })
    }
}
impl Drop for PyApplicationOwner {
    fn drop(&mut self) {
        self.owner.request_close();
    }
}
fn finish_exit(
    py: Python<'_>,
    result: PyResult<()>,
    exception: Option<Py<PyAny>>,
) -> PyResult<Py<PyAny>> {
    if let Err(error) = result {
        if let Some(exception) = exception {
            let _ =
                exception.call_method1(py, "add_note", (format!("Application cleanup: {error}"),));
        } else {
            return Err(error);
        }
    }
    Ok(py.None())
}

#[pyclass(name = "ApplicationHandle", module = "rusty_haystack.server", frozen)]
pub(crate) struct PyApplicationHandle {
    view: Arc<View>,
}
#[pymethods]
impl PyApplicationHandle {
    #[getter]
    fn state(&self) -> &'static str {
        state_name(self.view.native.state())
    }
    #[getter]
    fn closed(&self) -> bool {
        self.view.native.is_terminated()
    }
    #[pyo3(signature=(*, deadline=None))]
    fn ready<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        self.view.ready(py, deadline)
    }
    #[pyo3(signature=(filter, limit=100, *, cursor=None, deadline=None))]
    fn read<'py>(
        &self,
        py: Python<'py>,
        filter: &str,
        limit: usize,
        cursor: Option<&str>,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.view.read(py, filter, limit, cursor, deadline)
    }
    #[pyo3(signature=(*, deadline=None))]
    fn terminated<'py>(
        &self,
        py: Python<'py>,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.view.terminated(py, deadline)
    }
}

#[pyclass(
    name = "ApplicationTermination",
    module = "rusty_haystack.server",
    frozen
)]
pub(crate) struct PyApplicationTermination {
    #[pyo3(get)]
    drain_expired: Option<bool>,
    #[pyo3(get)]
    close_error: Option<String>,
    cleanup_errors: Vec<String>,
}
impl From<TerminationReport> for PyApplicationTermination {
    fn from(report: TerminationReport) -> Self {
        Self {
            drain_expired: report.close.as_ref().ok().map(|close| close.drain_expired),
            close_error: report.close.err().map(|error| error.to_string()),
            cleanup_errors: report
                .cleanup_errors
                .into_iter()
                .map(|error| error.to_string())
                .collect(),
        }
    }
}
#[pymethods]
impl PyApplicationTermination {
    #[getter]
    fn cleanup_errors<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyTuple>> {
        pyo3::types::PyTuple::new(py, &self.cleanup_errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haystack_core::{
        data::{HDict, HGrid},
        kinds::Kind,
    };

    fn builder() -> ApplicationBuilder {
        ApplicationBuilder::new(
            SharedGraph::new(EntityGraph::new()),
            Arc::new(EmbeddingPolicy),
            ReadLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn python_conversion_guard_delays_termination_on_success_and_failure() {
        Python::initialize();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        for invalid in [false, true] {
            let owner = builder().start(runtime.handle()).unwrap();
            runtime.block_on(owner.ready()).unwrap();
            let pending = Arc::new(Semaphore::new(1));
            let work = ReadWork {
                guard: owner.handle().admit().unwrap(),
                _permit: reserve_read(&pending).unwrap(),
            };
            let mut row = HDict::new();
            if invalid {
                let mut value = Kind::Null;
                for _ in 0..70 {
                    value = Kind::List(vec![value]);
                }
                row.set("deep", value);
            }
            let grid = HGrid::from_parts(HDict::new(), vec![], vec![row]);
            let (entered, receive) = std::sync::mpsc::sync_channel(1);
            let (release, wait) = std::sync::mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                convert_read_with(work, |py| {
                    entered.send(()).unwrap();
                    py.detach(move || wait.recv_timeout(Duration::from_secs(2)).unwrap());
                    Py::new(py, PyHGrid::from_core(&grid)?).map(Py::into_any)
                })
            });
            receive.recv_timeout(Duration::from_secs(2)).unwrap();
            owner.request_close();
            // The actual conversion callback is held independently of the GIL.
            assert!(reserve_read(&pending).is_err());
            assert!(
                runtime
                    .block_on(async {
                        tokio::time::timeout(Duration::from_millis(30), owner.terminated()).await
                    })
                    .is_err()
            );
            assert!(!owner.handle().is_terminated());
            release.send(()).unwrap();
            assert_eq!(worker.join().unwrap().is_err(), invalid);
            assert_eq!(pending.available_permits(), 1);
            let report = runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(2), owner.terminated())
                    .await
                    .unwrap()
            });
            assert!(report.close.is_ok());
            assert_eq!(owner.handle().outstanding_tasks(), 0);
        }
    }

    #[test]
    fn pending_read_capacity_rejects_until_owned_permit_is_released() {
        Python::initialize();
        let pending = Arc::new(Semaphore::new(MAX_PENDING_READS));
        let mut permits = (0..MAX_PENDING_READS)
            .map(|_| reserve_read(&pending).unwrap())
            .collect::<Vec<_>>();
        assert!(reserve_read(&pending).is_err());
        drop(permits.pop());
        let resumed = reserve_read(&pending).unwrap();
        assert!(reserve_read(&pending).is_err());
        drop(resumed);
        drop(permits);
        assert_eq!(pending.available_permits(), MAX_PENDING_READS);
    }

    #[test]
    fn wrapper_observation_preserves_native_timeout_until_actual_termination() {
        Python::initialize();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let owner = builder()
                .shutdown_policy(haystack_app::ShutdownPolicy {
                    drain_timeout: Duration::ZERO,
                    stop_timeout: Duration::from_millis(10),
                })
                .start(runtime.handle())
                .unwrap();
            owner.ready().await.unwrap();
            let handle = owner.handle();
            let work = handle.admit().unwrap();
            let failure = owner.close().await.unwrap_err();
            assert_eq!(handle.state(), ApplicationState::Failed);
            assert!(!handle.is_terminated());
            let observation = joined_close(&handle, Instant::now() + Duration::from_millis(20))
                .await
                .unwrap_err();
            Python::attach(|py| assert!(observation.is_instance_of::<PyTimeoutError>(py)));
            drop(work);
            let report = termination(&handle, Instant::now() + Duration::from_secs(2))
                .await
                .unwrap();
            assert_eq!(report.close, Err(failure.clone()));
            assert!(handle.is_terminated());
            let repeated = joined_close(&handle, Instant::now()).await.unwrap_err();
            Python::attach(|py| {
                assert!(repeated.is_instance_of::<crate::exceptions::ApplicationError>(py))
            });
            assert_eq!(
                PyApplicationTermination::from(report).close_error,
                Some(failure.to_string())
            );
        });
    }

    #[test]
    fn context_exit_preserves_body_when_cleanup_and_annotation_fail() {
        use pyo3::types::PyDict;
        use std::ffi::CString;
        Python::initialize();
        Python::attach(|py| {
            let scope = PyDict::new(py);
            py.run(&CString::new("class BodyError(Exception):\n    def add_note(self, note):\n        raise RuntimeError('annotation failed')\nbody = BodyError('body sentinel')").unwrap(), Some(&scope), Some(&scope)).unwrap();
            let body = scope.get_item("body").unwrap().unwrap().unbind();
            assert!(
                finish_exit(py, Err(timeout()), Some(body))
                    .unwrap()
                    .is_none(py)
            );
            assert!(
                finish_exit(py, Err(timeout()), None)
                    .unwrap_err()
                    .is_instance_of::<PyTimeoutError>(py)
            );
        });
    }
}
