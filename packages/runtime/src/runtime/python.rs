//! Registration and async support for the generated Python extension module.
//!
//! An async export runs to its first wait on the caller's thread, and from
//! there on the shared runtime of [`executor`](super::executor). One that
//! takes a `Callback` or a stream starts on the runtime.
//! [`awaitable`] settles an asyncio future with its result, which its
//! [`deferred`] conversion makes once the interpreter lock is held.
//!
//! A panic becomes pyo3's `PanicException` in every call. pyo3 catches the
//! panics of synchronous calls; [`awaitable`] catches those of async calls and
//! of their conversions, which would otherwise lose their message or leave
//! the awaitable unresolved.

use std::future::Future;
use std::mem;
use std::sync::{Arc, Mutex};

use pyo3::IntoPyObjectExt;
use pyo3::exceptions::PyImportError;
use pyo3::panic::PanicException;
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyCFunction, PyString};
use pyo3::{intern, wrap_pyfunction};

use crate::ir::{discovery, python_names};
use crate::registry;
use crate::runtime::executor::{self, Abort, Start, Started};

/// A function that adds one generated export to a Python module.
pub struct Registration(
    /// Register the generated export on the supplied extension module.
    pub fn(&Bound<'_, PyModule>) -> PyResult<()>,
);

inventory::collect!(Registration);

/// Add every linked Python export to an extension module, the exception that
/// a Rust panic raises, as `PanicException`, and the hash of the
/// application's contract, which the generated package compares with its own
/// at import.
///
/// # Errors
///
/// Returns `ImportError` when the linked exports are invalid, and a Python
/// exception when an export cannot be added to the module.
pub fn register(
    module: &Bound<'_, PyModule>,
    package_name: &str,
    package_version: &str,
) -> PyResult<()> {
    let contract_hash = registry::contract_hash(package_name, package_version)
        .map_err(|error| PyImportError::new_err(error.to_string()))?;
    module.add(discovery::CONTRACT_HASH_ATTRIBUTE, contract_hash)?;
    module.add(
        python_names::PANIC_EXCEPTION,
        module.py().get_type::<PanicException>(),
    )?;
    for registration in inventory::iter::<Registration> {
        (registration.0)(module)?;
    }
    Ok(())
}

/// Start an async call and return an asyncio future for its result, which
/// the call's [`deferred`] conversion makes.
///
/// When `start` is [`Start::Caller`], the call runs to its first wait on the
/// caller's thread, without the interpreter lock. A call that finishes there
/// returns a future that is done, so `await` does not suspend the caller. A
/// call that waits, or that `start` gives to a worker, goes on the shared
/// Tokio runtime.
///
/// Apart from a `Callback` argument, whose last clone PyO3 releases through
/// its reference pool, the Tokio task owns no Python object. The event loop
/// and the future wait in a [`Target`], which is locked only with the
/// interpreter lock held, so a fork, which holds it too, never copies a lock
/// of the call held. A blocking thread that
/// holds the interpreter lock converts the result and hands it to the event
/// loop, so delivering a result never makes a runtime worker wait for the
/// lock.
///
/// Cancelling the future aborts the call: the Rust future drops as soon as
/// a runtime thread runs the aborted task, or right after a poll in
/// progress. A panic in the call or in its conversion raises
/// `PanicException` with the panic's message.
///
/// # Errors
///
/// Returns a Python exception when no asyncio event loop is running.
#[doc(hidden)]
pub fn awaitable<F, D, T>(py: Python<'_>, start: Start, future: F) -> PyResult<Py<PyAny>>
where
    F: Future<Output = PyResult<Deferred<D>>> + Send + 'static,
    D: for<'py> FnOnce(Python<'py>) -> PyResult<T> + Send + 'static,
    T: for<'py> IntoPyObject<'py>,
{
    static GET_RUNNING_LOOP: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    let event_loop = GET_RUNNING_LOOP
        .import(py, "asyncio", "get_running_loop")?
        .call0()?;
    let result = event_loop.call_method0(intern!(py, "create_future"))?;
    let call = executor::catch_panic(future, panic_exception);
    let call = match py.detach(|| executor::first_poll(call, start)) {
        Started::Ready(outcome) => {
            let (setter, value) = settlement(py, outcome);
            result.call_method1(setter, (value,))?;
            return Ok(result.unbind());
        }
        Started::Pending(call) => call,
    };
    let target = Arc::new(Target(Mutex::new(Some(Waiting {
        event_loop: event_loop.unbind(),
        future: result.clone().unbind(),
    }))));
    let delivery = Arc::clone(&target);
    let task = executor::spawn(async move {
        let outcome = call.await;
        drop(executor::spawn_blocking(move || {
            // An interpreter that is shutting down resumes no one, and the
            // target's Python objects cannot drop without it.
            if Python::try_attach(|py| delivery.resolve(py, outcome)).is_none() {
                mem::forget(delivery);
            }
        }));
    });
    let cancel = CancelOnDone {
        task: Abort::new(&task),
        target,
    };
    if let Err(error) = result.call_method1(intern!(py, "add_done_callback"), (cancel.clone(),)) {
        cancel.cancel(py);
        return Err(error);
    }
    Ok(result.unbind())
}

/// Convert a call's outcome, and return the method of an asyncio future that
/// settles it with the result, and that result. A panic in the conversion is
/// an outcome too.
fn settlement<'py, D, T>(
    py: Python<'py>,
    outcome: PyResult<Deferred<D>>,
) -> (&'py Bound<'py, PyString>, Bound<'py, PyAny>)
where
    D: FnOnce(Python<'py>) -> PyResult<T>,
    T: IntoPyObject<'py>,
{
    let converted = outcome.and_then(|Deferred(convert)| {
        executor::catch_error(|| convert(py)?.into_bound_py_any(py), panic_exception)
    });
    match converted {
        Ok(value) => (intern!(py, "set_result"), value),
        Err(error) => (
            intern!(py, "set_exception"),
            error.into_value(py).into_bound(py).into_any(),
        ),
    }
}

/// The event loop and the asyncio future of an async call that has not
/// resolved yet.
struct Waiting {
    event_loop: Py<PyAny>,
    future: Py<PyAny>,
}

/// Where an async call's result goes. Only code that holds the interpreter
/// lock locks it, and a fork holds that lock too, so a forked child never
/// finds it held.
struct Target(Mutex<Option<Waiting>>);

/// The reason to expect when locking a [`Target`]: only `Option::take` runs
/// under the lock, and it cannot panic.
const UNPOISONED: &str = "no code panics while holding an async call's target";

impl Target {
    /// Take the call's event loop and future, unless the call already
    /// resolved or was cancelled.
    fn take(&self, _py: Python<'_>) -> Option<Waiting> {
        self.0.lock().expect(UNPOISONED).take()
    }

    /// Convert the call's outcome and schedule it on the call's event loop.
    fn resolve<D, T>(&self, py: Python<'_>, outcome: PyResult<Deferred<D>>)
    where
        D: for<'py> FnOnce(Python<'py>) -> PyResult<T>,
        T: for<'py> IntoPyObject<'py>,
    {
        static SETTLE: PyOnceLock<Py<PyCFunction>> = PyOnceLock::new();
        let Some(waiting) = self.take(py) else {
            return;
        };
        let future = waiting.future.bind(py);
        // The conversion runs on a blocking thread, which a panic would end
        // without settling the future.
        let (setter, value) = settlement(py, outcome);
        // Scheduling fails when the event loop is closed, which leaves no one
        // to resume, or when Python is out of memory; either way the result
        // drops with the error.
        let _ = SETTLE
            .get_or_try_init(py, || wrap_pyfunction!(settle, py).map(Bound::unbind))
            .and_then(|settle| {
                waiting.event_loop.bind(py).call_method1(
                    intern!(py, "call_soon_threadsafe"),
                    (settle.bind(py), future, future.getattr(setter)?, value),
                )
            });
    }
}

/// Settle `future` through `setter` unless it is already done, as it is when
/// the caller cancelled it while the result was on its way.
#[pyfunction]
fn settle(
    future: &Bound<'_, PyAny>,
    setter: &Bound<'_, PyAny>,
    value: &Bound<'_, PyAny>,
) -> PyResult<()> {
    if !future
        .call_method0(intern!(future.py(), "done"))?
        .is_truthy()?
    {
        setter.call1((value,))?;
    }
    Ok(())
}

/// Abort an async call when the caller cancels its future, which asyncio
/// reports by calling this once the future is done.
#[pyclass(frozen, module = "bimetal", skip_from_py_object)]
#[derive(Clone)]
struct CancelOnDone {
    task: Abort,
    target: Arc<Target>,
}

impl CancelOnDone {
    /// Abort the call, and drop its event loop and future.
    fn cancel(&self, py: Python<'_>) {
        self.task.abort();
        drop(self.target.take(py));
    }
}

#[pymethods]
impl CancelOnDone {
    fn __call__(&self, future: &Bound<'_, PyAny>) -> PyResult<()> {
        let py = future.py();
        if future.call_method0(intern!(py, "cancelled"))?.is_truthy()? {
            self.cancel(py);
        }
        Ok(())
    }
}

/// Defer a result conversion until the interpreter lock is held.
#[doc(hidden)]
pub fn deferred<D, T>(convert: D) -> Deferred<D>
where
    D: for<'py> FnOnce(Python<'py>) -> PyResult<T>,
    T: for<'py> IntoPyObject<'py>,
{
    Deferred(convert)
}

/// A result whose conversion to Python runs once the lock is held. It does
/// nothing until converted.
#[doc(hidden)]
#[must_use]
pub struct Deferred<D>(D);

/// Build the exception for a panic, with its message, as pyo3 does for a
/// synchronous call.
#[cold]
fn panic_exception(message: String) -> PyErr {
    PanicException::new_err((message,))
}
