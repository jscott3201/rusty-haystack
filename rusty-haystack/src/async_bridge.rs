//! One process-wide Tokio/Python bridge for async bindings. Client I/O runtimes
//! have independent ownership; closing a client never claims to destroy this.
//! Embedded applications borrow this runtime and join their native lifecycle's
//! tracked work/resources without shutting down the shared bridge.
use std::future::Future;
use std::pin::Pin;

use parking_lot::{Condvar, Mutex};
use pyo3::prelude::*;
use pyo3_async_runtimes::{TaskLocals, generic};

#[derive(Default)]
struct AttachmentState {
    sealed: bool,
    active: usize,
}

#[derive(Default)]
struct AttachmentGate {
    state: Mutex<AttachmentState>,
    drained: Condvar,
}

static ATTACHMENTS: AttachmentGate = AttachmentGate {
    state: Mutex::new(AttachmentState {
        sealed: false,
        active: 0,
    }),
    drained: Condvar::new(),
};

impl AttachmentGate {
    fn admit(&self) -> Option<AttachmentLease<'_>> {
        let mut state = self.state.lock();
        if state.sealed {
            return None;
        }
        state.active += 1;
        Some(AttachmentLease(self))
    }

    fn try_attach<T>(&self, callback: impl for<'py> FnOnce(Python<'py>) -> T) -> Option<T> {
        // Reserve before waiting for the GIL, and release only after try_attach
        // has dropped its attachment guard. A flag checked before attachment
        // would leave an uncounted waiter racing interpreter finalization.
        let _lease = self.admit()?;
        Python::try_attach(callback)
    }

    fn seal_and_drain(&self, py: Python<'_>) {
        self.state.lock().sealed = true;
        // Normal CPython atexit runs before finalizing thread states. Admitted
        // workers must be able to acquire the GIL while this hook joins them.
        // Neither the GIL nor the mutex is held while waiting for a lease.
        py.detach(|| {
            let mut state = self.state.lock();
            while state.active != 0 {
                self.drained.wait(&mut state);
            }
        });
    }
}

struct AttachmentLease<'a>(&'a AttachmentGate);
impl Drop for AttachmentLease<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock();
        state.active -= 1;
        if state.active == 0 {
            self.0.drained.notify_all();
        }
    }
}

#[pyfunction]
fn _shutdown_bridge(py: Python<'_>) {
    ATTACHMENTS.seal_and_drain(py);
}

pub(crate) fn register_shutdown(py: Python<'_>) -> PyResult<()> {
    py.import("atexit")?
        .call_method1("register", (wrap_pyfunction!(_shutdown_bridge, py)?,))?;
    Ok(())
}

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    pyo3_async_runtimes::tokio::get_runtime()
}

// The pinned generic adapter attaches unconditionally in its terminal notifier.
// Keep that callback inside the shutdown admission lease and attachment guard,
// and abandon delivery if its originating loop has already closed.
struct BridgeRuntime;
impl generic::Runtime for BridgeRuntime {
    type JoinError = tokio::task::JoinError;
    type JoinHandle = tokio::task::JoinHandle<()>;

    fn spawn<F>(future: F) -> Self::JoinHandle
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let locals = LOCALS.try_with(Clone::clone).ok();
        runtime().spawn(async move {
            if let Some(locals) = locals {
                LOCALS.scope(locals, future).await;
            } else {
                future.await;
            }
        })
    }

    fn spawn_blocking<F>(callback: F) -> Self::JoinHandle
    where
        F: FnOnce() + Send + 'static,
    {
        let locals = LOCALS.try_with(Clone::clone).ok();
        runtime().spawn_blocking(move || {
            let _ = ATTACHMENTS.try_attach(|py| {
                let closed = locals.as_ref().is_some_and(|locals| {
                    locals
                        .event_loop(py)
                        .call_method0("is_closed")
                        .and_then(|value| value.extract::<bool>())
                        .unwrap_or(false)
                });
                if !closed {
                    callback();
                }
            });
        })
    }
}

tokio::task_local! { static LOCALS: TaskLocals; }
impl generic::ContextExt for BridgeRuntime {
    fn scope<F, R>(locals: TaskLocals, future: F) -> Pin<Box<dyn Future<Output = R> + Send>>
    where
        F: Future<Output = R> + Send + 'static,
    {
        Box::pin(LOCALS.scope(locals, future))
    }
    fn get_task_locals() -> Option<TaskLocals> {
        LOCALS.try_with(Clone::clone).ok()
    }
}

pub(crate) fn future<'py, F>(
    py: Python<'py>,
    locals: TaskLocals,
    future: F,
) -> PyResult<Bound<'py, PyAny>>
where
    F: Future<Output = PyResult<Py<PyAny>>> + Send + 'static,
{
    let _lease = ATTACHMENTS.admit().ok_or_else(|| {
        pyo3::exceptions::PyRuntimeError::new_err("Python bridge is shutting down")
    })?;
    // Preserve the public Tokio helper context too, so later async bindings
    // can compose explicit generic and ordinary Tokio conversions. Scope here:
    // ContextExt::scope itself cannot add a static bound to its generic output.
    let future = pyo3_async_runtimes::tokio::scope(locals.clone(), future);
    LOCALS.sync_scope(locals.clone(), || {
        generic::future_into_py_with_locals::<BridgeRuntime, _, _>(py, locals, future)
    })
}

pub(crate) fn with_python<T>(
    callback: impl for<'py> FnOnce(Python<'py>) -> PyResult<T>,
) -> PyResult<T> {
    ATTACHMENTS.try_attach(callback).unwrap_or_else(|| {
        Err(pyo3::exceptions::PyRuntimeError::new_err(
            "Python interpreter is unavailable",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use generic::{ContextExt, Runtime};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    #[test]
    fn shutdown_drains_held_attachment_and_rejects_late_attachment() {
        Python::initialize();
        let gate = Arc::new(AttachmentGate::default());
        let called = Arc::new(AtomicBool::new(false));
        let worker_gate = Arc::clone(&gate);
        let worker_called = Arc::clone(&called);
        let (entered, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release, release_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            worker_gate.try_attach(|py| {
                entered.send(()).unwrap();
                py.detach(move || release_rx.recv_timeout(Duration::from_secs(3)).unwrap());
                assert!(worker_gate.state.lock().sealed);
                worker_called.store(true, Ordering::SeqCst);
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let releasing_gate = Arc::clone(&gate);
        let releaser = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                let state = releasing_gate.state.lock();
                if state.sealed {
                    assert_eq!(state.active, 1);
                    break;
                }
                drop(state);
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            release.send(()).unwrap();
        });
        Python::attach(|py| {
            // The callback is held by a channel, including on free-threaded
            // CPython. Draining must detach so ordinary CPython can reattach.
            assert!(!called.load(Ordering::SeqCst));
            gate.seal_and_drain(py);
            assert!(called.load(Ordering::SeqCst));
            assert_eq!(gate.state.lock().active, 0);
        });
        releaser.join().unwrap();
        assert_eq!(worker.join().unwrap(), Some(()));

        let (sent, received) = std::sync::mpsc::sync_channel(1);
        let late = Python::attach(|_| {
            let gate = Arc::clone(&gate);
            let late = std::thread::spawn(move || {
                let result = gate.try_attach(|_| panic!("late callback entered Python"));
                sent.send(result.is_none()).unwrap();
            });
            // A rejected worker completes while this thread holds the GIL;
            // it must not try to acquire Python and merely skip the callback.
            assert!(received.recv_timeout(Duration::from_secs(3)).unwrap());
            late
        });
        late.join().unwrap();
        Python::attach(|py| gate.seal_and_drain(py));
    }

    #[test]
    fn terminal_notification_keeps_owning_loop_after_conversion_scope_ends() {
        Python::initialize();
        for closed in [false, true] {
            let called = Arc::new(AtomicBool::new(false));
            Python::attach(|py| {
                let event_loop = py
                    .import("asyncio")
                    .unwrap()
                    .call_method0("new_event_loop")
                    .unwrap();
                let locals = TaskLocals::new(event_loop.clone());
                if closed {
                    event_loop.call_method0("close").unwrap();
                }
                let called = Arc::clone(&called);
                // Match the pinned generic converter: the result notifier is
                // spawned after ContextExt::scope has already returned, inside
                // two nested Runtime::spawn calls.
                let job = LOCALS.sync_scope(locals.clone(), || {
                    BridgeRuntime::spawn(async move {
                        BridgeRuntime::spawn(async move {
                            BridgeRuntime::scope(locals, async {}).await;
                            BridgeRuntime::spawn_blocking(move || {
                                called.store(true, Ordering::SeqCst);
                            })
                            .await
                            .unwrap();
                        })
                        .await
                        .unwrap();
                    })
                });
                py.detach(|| {
                    runtime().block_on(async {
                        tokio::time::timeout(Duration::from_secs(3), job)
                            .await
                            .unwrap()
                            .unwrap();
                    });
                });
                event_loop.call_method0("close").unwrap();
            });
            assert_eq!(called.load(Ordering::SeqCst), !closed);
        }
    }
}
