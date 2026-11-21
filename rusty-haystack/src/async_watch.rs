//! Python views over owned native snapshots; mutable rows are independent copies.
use crate::{
    async_bridge,
    async_client::{ClientContext, native_error},
    data::PyHDict,
    watch_owner::{Action, Event, Handle},
};
use haystack_core::codecs::subscription::{
    SubscriptionDelivery, SubscriptionOutcome as Outcome, SubscriptionRejection, SubscriptionResync,
};
use pyo3::{
    exceptions::{PyRuntimeError, PyStopAsyncIteration},
    prelude::*,
    types::PyTuple,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[pyclass(name = "WatchDelivery", module = "rusty_haystack.client", frozen)]
pub(crate) struct PyWatchDelivery {
    inner: Arc<SubscriptionDelivery>,
}
fn hex(value: &[u8; 16]) -> String {
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}
#[pymethods]
impl PyWatchDelivery {
    #[getter]
    fn authority(&self) -> String {
        hex(&self.inner.watch.authority)
    }
    #[getter]
    fn watch_id(&self) -> String {
        hex(&self.inner.watch.watch)
    }
    #[getter]
    fn key(&self) -> &str {
        &self.inner.watch.key
    }
    #[getter]
    fn dataset(&self) -> String {
        hex(&self.inner.dataset)
    }
    #[getter]
    fn incarnation(&self) -> String {
        hex(&self.inner.incarnation)
    }
    #[getter]
    fn catalog_generation(&self) -> u64 {
        self.inner.catalog_generation
    }
    #[getter]
    fn scope_generation(&self) -> u64 {
        self.inner.scope_generation
    }
    #[getter]
    fn token(&self) -> String {
        hex(&self.inner.token)
    }
    #[getter(from_revision)]
    fn get_from_revision(&self) -> u64 {
        self.inner.from
    }
    #[getter]
    fn through(&self) -> u64 {
        self.inner.through
    }
    #[getter]
    fn initial(&self) -> bool {
        self.inner.initial
    }
    #[getter]
    fn changed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        let rows: PyResult<Vec<_>> = self
            .inner
            .rows
            .iter()
            .map(|row| Py::new(py, PyHDict::from_core(row)?))
            .collect();
        PyTuple::new(py, rows?)
    }
    #[getter]
    fn removed<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, &self.inner.removed)
    }
}
#[pyclass(name = "WatchControl", module = "rusty_haystack.client", frozen)]
pub(crate) struct PyWatchControl {
    event: Event,
    #[pyo3(get)]
    local_joined: bool,
}
#[pymethods]
impl PyWatchControl {
    #[getter]
    fn kind(&self) -> &'static str {
        match &self.event {
            Event::Ready => "ready",
            Event::NotCreated => "not_created",
            Event::End => "stopped",
            Event::Wire(outcome) => match outcome {
                Outcome::Authority { .. } => "authority",
                Outcome::Delivery(_) => "delivery",
                Outcome::Idle { .. } => "idle",
                Outcome::Acknowledged { .. } => "acknowledged",
                Outcome::Renewed { .. } => "renewed",
                Outcome::Closed { .. } => "closed",
                Outcome::Rejected(_) => "rejected",
                Outcome::Resync(_) => "resync",
                Outcome::Unknown => "unknown",
            },
        }
    }
    #[getter]
    fn reason(&self) -> Option<&'static str> {
        match &self.event {
            Event::Wire(Outcome::Rejected(reason)) => Some(match reason {
                SubscriptionRejection::Invalid => "invalid",
                SubscriptionRejection::Forbidden => "forbidden",
                SubscriptionRejection::Conflict => "conflict",
                SubscriptionRejection::Capacity => "capacity",
                SubscriptionRejection::Limit => "limit",
                SubscriptionRejection::Pending => "pending",
            }),
            Event::Wire(Outcome::Resync(reason)) => Some(match reason {
                SubscriptionResync::Authority => "authority",
                SubscriptionResync::SessionExpired => "session_expired",
                SubscriptionResync::Revoked => "revoked",
                SubscriptionResync::LeaseExpired => "lease_expired",
                SubscriptionResync::Gap => "gap",
                SubscriptionResync::Incarnation => "incarnation",
                SubscriptionResync::Catalog => "catalog",
                SubscriptionResync::Policy => "policy",
                SubscriptionResync::ScopeChanged => "scope_changed",
                SubscriptionResync::Shutdown => "shutdown",
                SubscriptionResync::Overflow => "overflow",
                SubscriptionResync::Unknown => "unknown",
            }),
            _ => None,
        }
    }
    #[getter]
    fn watch_id(&self) -> Option<String> {
        if let Event::Wire(outcome) = &self.event {
            outcome.watch().map(|watch| hex(&watch.watch))
        } else {
            None
        }
    }
    #[getter]
    fn key(&self) -> Option<String> {
        if let Event::Wire(outcome) = &self.event {
            outcome.watch().map(|watch| watch.key.clone())
        } else {
            None
        }
    }
    #[getter]
    fn token(&self) -> Option<String> {
        match &self.event {
            Event::Wire(Outcome::Acknowledged { token, .. }) => Some(hex(token)),
            _ => None,
        }
    }
    #[getter]
    fn through(&self) -> Option<u64> {
        match &self.event {
            Event::Wire(Outcome::Acknowledged { through, .. }) => Some(*through),
            Event::Wire(Outcome::Idle { acknowledged, .. }) => Some(*acknowledged),
            _ => None,
        }
    }
    #[getter]
    fn scope_generation(&self) -> Option<u64> {
        match &self.event {
            Event::Wire(
                Outcome::Acknowledged {
                    scope_generation, ..
                }
                | Outcome::Idle {
                    scope_generation, ..
                },
            ) => Some(*scope_generation),
            _ => None,
        }
    }
}
#[pyclass(name = "AsyncWatch", module = "rusty_haystack.client", frozen)]
pub(crate) struct PyAsyncWatch {
    pub(crate) handle: Handle,
    pub(crate) context: Arc<ClientContext>,
}
impl PyAsyncWatch {
    fn start<'py>(
        &self,
        py: Python<'py>,
        action: Action,
        deadline: Option<f64>,
        iteration: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.context.context(py, deadline)?;
        if iteration && self.handle.closed() {
            return async_bridge::future(py, locals, async {
                Err(PyStopAsyncIteration::new_err(()))
            });
        }
        let mut operation = self
            .handle
            .operation(action, deadline)
            .map_err(native_error)?;
        async_bridge::future(py, locals, async move {
            let event = operation.result().await.map_err(native_error)?;
            let value = async_bridge::with_python(|py| convert(py, event, false, iteration));
            drop(operation);
            value
        })
    }
}
fn convert(
    py: Python<'_>,
    event: Event,
    local_joined: bool,
    iteration: bool,
) -> PyResult<Py<PyAny>> {
    match event {
        Event::Wire(Outcome::Delivery(inner)) => {
            Py::new(py, PyWatchDelivery { inner }).map(Py::into_any)
        }
        Event::End if iteration => Err(PyStopAsyncIteration::new_err(())),
        event => Py::new(
            py,
            PyWatchControl {
                event,
                local_joined,
            },
        )
        .map(Py::into_any),
    }
}
#[pyclass(frozen)]
struct EnterGuard {
    handle: Handle,
    entered: AtomicBool,
}
#[pymethods]
impl EnterGuard {
    fn entered(&self) {
        self.entered.store(true, Ordering::Release);
    }
    fn close(&self) {
        if !self.entered.load(Ordering::Acquire) {
            self.handle.begin_close();
        }
    }
}
impl Drop for EnterGuard {
    fn drop(&mut self) {
        if !self.entered.load(Ordering::Acquire) {
            self.handle.begin_close();
        }
    }
}
#[pymethods]
impl PyAsyncWatch {
    #[getter]
    fn key(&self) -> &str {
        self.handle.key()
    }
    #[getter]
    fn state(&self) -> &'static str {
        self.handle.phase()
    }
    #[getter]
    fn closed(&self) -> bool {
        self.handle.closed()
    }
    #[getter]
    fn pending(&self) -> Option<PyWatchDelivery> {
        self.handle.pending().map(|inner| PyWatchDelivery { inner })
    }
    #[pyo3(signature=(*, deadline=None))]
    fn ready<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        self.start(py, Action::Ready, deadline, false)
    }
    #[pyo3(signature=(*, deadline=None))]
    fn next<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        self.start(py, Action::Next, deadline, true)
    }
    fn __aiter__(slf: Py<Self>) -> Py<Self> {
        slf
    }
    fn __anext__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.next(py, None)
    }
    #[pyo3(signature=(delivery, *, deadline=None))]
    fn ack<'py>(
        &self,
        py: Python<'py>,
        delivery: &PyWatchDelivery,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.start(py, Action::Ack(delivery.inner.clone()), deadline, false)
    }
    #[pyo3(signature=(*, deadline=None))]
    fn resume<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        self.start(py, Action::Resume, deadline, false)
    }
    #[pyo3(signature=(lease_ms, *, deadline=None))]
    fn renew<'py>(
        &self,
        py: Python<'py>,
        lease_ms: u64,
        deadline: Option<f64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        self.start(py, Action::Renew(lease_ms), deadline, false)
    }
    #[pyo3(signature=(*, deadline=None))]
    fn aclose<'py>(&self, py: Python<'py>, deadline: Option<f64>) -> PyResult<Bound<'py, PyAny>> {
        let (locals, deadline) = self.context.context(py, deadline)?;
        self.handle.begin_close();
        let handle = self.handle.clone();
        async_bridge::future(py, locals, async move {
            let report = handle.wait_closed(deadline).await.map_err(native_error)?;
            async_bridge::with_python(|py| convert(py, report, true, false))
        })
    }
    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Bound<'_, PyAny>> {
        let watch = slf.get();
        let _ = watch.context.context(py, None)?;
        if watch.handle.closed() {
            return Err(PyRuntimeError::new_err("watch is closed"));
        }
        let guard = Py::new(
            py,
            EnterGuard {
                handle: watch.handle.clone(),
                entered: AtomicBool::new(false),
            },
        )?;
        py.import("rusty_haystack._watch")?
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
        let (locals, deadline) = self.context.context(py, None)?;
        self.handle.begin_close();
        let handle = self.handle.clone();
        async_bridge::future(py, locals, async move {
            let result = handle
                .wait_closed(deadline)
                .await
                .map_err(native_error)
                .and_then(|event| {
                    if event.confirms_release() {
                        Ok(())
                    } else {
                        Err(crate::exceptions::ClientError::new_err(
                            "watch remote cleanup is unconfirmed; inspect aclose() report",
                        ))
                    }
                });
            async_bridge::with_python(|py| {
                if let Err(error) = result {
                    if let Some(exception) = exc_value {
                        let _ = exception.call_method1(
                            py,
                            "add_note",
                            (format!("Watch cleanup: {error}"),),
                        );
                    } else {
                        return Err(error);
                    }
                }
                Ok(py.None())
            })
        })
    }
}
impl Drop for PyAsyncWatch {
    fn drop(&mut self) {
        self.handle.begin_close();
    }
}
