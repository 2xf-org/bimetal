#![cfg(all(feature = "python", not(target_arch = "wasm32")))]

//! Integration tests for native Python registration and value conversion.
//!
//! These tests embed a live interpreter, so they only compile with the
//! `python` feature and the binary must find libpython at load time:
//! `mise run test` supplies both.

mod fixtures;

/// An export whose result panics as it converts, which only this suite
/// links: the contract suite lists the fixture's exports exactly.
mod panics {
    use bimetal::Model;
    use serde::{Deserialize, Serialize, Serializer};

    /// A value whose conversion to Python panics.
    #[allow(dead_code, reason = "the conversion panics before it reads the field")]
    #[derive(Debug, Deserialize, Model)]
    pub struct Unconvertible {
        /// A value that Python never sees.
        pub value: u32,
    }

    impl Serialize for Unconvertible {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            panic!("the value cannot convert");
        }
    }

    /// Return a value whose conversion to Python panics, after yielding to
    /// the executor.
    #[bimetal::export]
    pub async fn convert_later() -> Unconvertible {
        tokio::task::yield_now().await;
        Unconvertible { value: 1 }
    }
}

/// Synchronous exports and Bimetal's runtime: constructors and factories run
/// inside it, so they can spawn tasks on it, and other calls do not.
mod spawning {
    use tokio::runtime::Handle;
    use tokio::task::JoinHandle;

    /// A value that a background task computes.
    pub struct Background {
        task: JoinHandle<u32>,
    }

    #[bimetal::export]
    impl Background {
        /// Start doubling a value on a task.
        #[bimetal(constructor)]
        #[must_use]
        pub fn new(value: u32) -> Self {
            Self {
                task: tokio::spawn(async move { value * 2 }),
            }
        }

        /// Start tripling a value on a task.
        #[bimetal(constructor)]
        #[must_use]
        pub fn tripled(value: u32) -> Self {
            Self {
                task: tokio::spawn(async move { value * 3 }),
            }
        }

        /// Return whether the method runs inside a Tokio runtime.
        #[must_use]
        pub fn in_runtime(&self) -> bool {
            Handle::try_current().is_ok()
        }

        /// Wait for the task's value.
        pub async fn result(&mut self) -> u32 {
            (&mut self.task).await.expect("the task finishes")
        }
    }
}

/// An async export whose calls work as long as they ask, and never wait.
mod pacing {
    use std::time::{Duration, Instant};

    /// Keep the thread busy for `micros` microseconds, and return them.
    #[bimetal::export]
    pub async fn busy_for(micros: u64) -> u64 {
        let deadline = Instant::now() + Duration::from_micros(micros);
        while Instant::now() < deadline {
            std::hint::spin_loop();
        }
        micros
    }
}

/// Exports that only this suite calls.
mod sequences {
    /// Join labels with commas.
    #[bimetal::export]
    #[must_use]
    pub fn join_labels(labels: Option<Vec<String>>) -> String {
        labels.unwrap_or_default().join(",")
    }

    /// Return a length, under a name that hides a Python builtin.
    #[bimetal::export]
    #[must_use]
    pub fn echo_length(bytes: u32) -> u32 {
        bytes
    }
}

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::mem;

use bimetal::__private::pyo3::types::{
    PyAnyMethods, PyByteArray, PyByteArrayMethods, PyBytes, PyDict, PyDictMethods, PyTypeMethods,
};
use bimetal::__private::pyo3::{Bound, PyErr, PyResult, Python};

use crate::fixtures::{lookup, python};

fn job_request(py: Python<'_>, count: u32) -> PyResult<Bound<'_, PyDict>> {
    let request = PyDict::new(py);
    request.set_item("displayName", "release")?;
    request.set_item("count", count)?;
    request.set_item("mode", "deterministic")?;
    request.set_item("dryRun", false)?;
    let context = PyDict::new(py);
    context.set_item("createdAt", "2026-07-22T12:00:00Z")?;
    let metadata = PyDict::new(py);
    metadata.set_item("wide", u64::MAX)?;
    context.set_item("metadata", metadata)?;
    context.set_item("labels", BTreeMap::from([("team", "bimetal")]))?;
    context.set_item("range", (-2, 4))?;
    context.set_item("note", Option::<String>::None)?;
    context.set_item("fingerprint", [1_u8, 2, 3])?;
    request.set_item("context", context)?;
    Ok(request)
}

fn error_details(py: Python<'_>, error: &PyErr) -> PyResult<(String, String)> {
    let (code, message, _details): (String, String, Option<String>) =
        error.value(py).getattr("args")?.extract()?;
    Ok((code, message))
}

#[test]
fn functions_decode_call_and_encode_real_values() -> PyResult<()> {
    python::with_registered_module(|py, module, manifest| {
        let execute_job = &lookup::function(manifest, "executeJob").native_name;
        let reverse_bytes = &lookup::function(manifest, "reverseBytes").native_name;
        let scale_samples = &lookup::function(manifest, "scaleSamples").native_name;
        let echo_u64 = &lookup::function(manifest, "echoU64").native_name;
        let echo_wide_numbers = &lookup::function(manifest, "echoWideNumbers").native_name;

        let samples = py
            .import("array")?
            .getattr("array")?
            .call1(("d", vec![1.5_f64, 2.0, 4.5]))?;
        let result = module.getattr(execute_job)?.call1((
            job_request(py, 3)?,
            PyBytes::new(py, &[1_u8, 2, 3]),
            samples,
        ))?;
        let result = result.cast::<PyDict>()?;
        assert_eq!(
            result
                .get_item("id")?
                .expect("result must contain id")
                .extract::<String>()?,
            "job-release"
        );
        assert_eq!(
            result
                .get_item("acceptedCount")?
                .expect("result must contain acceptedCount")
                .extract::<u32>()?,
            3
        );
        let sample_total = result
            .get_item("sampleTotal")?
            .expect("result must contain sampleTotal")
            .extract::<f64>()?;
        assert!((sample_total - 8.0).abs() < f64::EPSILON);
        let event = result
            .get_item("event")?
            .expect("result must contain event")
            .cast_into::<PyDict>()?;
        assert_eq!(
            event
                .get_item("kind")?
                .expect("event must contain kind")
                .extract::<String>()?,
            "completed"
        );

        let reversed = module
            .getattr(reverse_bytes)?
            .call1((PyBytes::new(py, &[3_u8, 1, 4]),))?
            .extract::<Vec<u8>>()?;
        assert_eq!(reversed, [4, 1, 3]);

        let samples = py
            .import("array")?
            .getattr("array")?
            .call1(("d", vec![1.5_f64, -2.0]))?;
        let scaled = module.getattr(scale_samples)?.call1((samples, 2.0))?;
        let scaled: Vec<f64> = scaled
            .cast::<PyByteArray>()?
            .to_vec()
            .chunks_exact(mem::size_of::<f64>())
            .map(|bytes| f64::from_ne_bytes(bytes.try_into().expect("one f64")))
            .collect();
        assert_eq!(scaled, [3.0, -4.0]);

        for value in [0, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
            assert_eq!(
                module
                    .getattr(echo_u64)?
                    .call1((value,))?
                    .extract::<u64>()?,
                value,
            );
        }

        let wide = PyDict::new(py);
        wide.set_item("value", u64::MAX)?;
        wide.set_item("values", [i64::MAX as u64 + 1, u64::MAX])?;
        wide.set_item("optional", u64::MAX)?;
        let echoed = module
            .getattr(echo_wide_numbers)?
            .call1((wide,))?
            .cast_into::<PyDict>()?;
        assert_eq!(
            echoed
                .get_item("value")?
                .expect("wide result contains value")
                .extract::<u64>()?,
            u64::MAX,
        );

        Ok(())
    })
}

#[test]
fn functions_run_without_the_interpreter_lock() -> PyResult<()> {
    // A long native call that kept the lock would stall every other Python
    // thread, such as an API's event loop awaiting it from a worker thread.
    python::with_registered_module(|_py, module, manifest| {
        let held = module
            .getattr(&lookup::function(manifest, "holdsInterpreterLock").native_name)?
            .call0()?
            .extract::<bool>()?;
        assert!(
            !held,
            "exported functions must release the interpreter lock"
        );
        let probe = module
            .getattr(&lookup::resource(manifest, "LockProbe").native_name)?
            .call0()?;
        assert!(
            !probe.call_method0("held")?.extract::<bool>()?,
            "resource constructors must release the interpreter lock"
        );
        assert!(
            !probe.call_method0("holdsLockNow")?.extract::<bool>()?,
            "resource methods must release the interpreter lock"
        );

        Ok(())
    })
}

#[test]
fn rust_checks_the_field_rules_of_arguments() -> PyResult<()> {
    python::with_registered_module(|py, module, manifest| {
        let samples = py
            .import("array")?
            .getattr("array")?
            .call1(("d", vec![1.0_f64]))?;
        // The Rust function would fail with its own typed error; the rule
        // stops the call first.
        let error = module
            .getattr(&lookup::function(manifest, "executeJob").native_name)?
            .call1((job_request(py, 0)?, PyBytes::new(py, &[]), samples))
            .expect_err("a count below its rule must fail");

        assert_eq!(
            exception(py, &error)?,
            (
                "ValueError".to_owned(),
                "execute_job() argument 'request': `count` must be at least 1, got 0".to_owned()
            )
        );
        Ok(())
    })
}

#[test]
fn rust_checks_the_field_rules_of_returns() -> PyResult<()> {
    python::with_registered_module(|py, module, manifest| {
        let error = module
            .getattr(&lookup::function(manifest, "renameJob").native_name)?
            .call1((job_request(py, 3)?, "x"))
            .expect_err("a returned name below its rule must fail");

        assert_eq!(
            exception(py, &error)?,
            (
                "RuntimeError".to_owned(),
                "rename_job() returned a value that breaks a field rule: `displayName` must have at least 2 characters, got 1".to_owned()
            )
        );
        Ok(())
    })
}

#[test]
fn externally_tagged_enums_cross_in_their_serde_form() -> PyResult<()> {
    python::run_scenario(
        cr#"
assert echo_step("queued") == "queued"
assert echo_step({"stopped": {"acceptedCount": 3}}) == {"stopped": {"acceptedCount": 3}}
request = {"displayName": "release", "count": 2, "mode": "fast", "dryRun": False, "context": None}
assert echo_step({"running": request}) == {"running": request}
"#,
        &["echoStep"],
        &[],
    )
}

#[test]
fn durations_arrays_and_paths_cross_as_serde_writes_them() -> PyResult<()> {
    python::run_scenario(
        cr#"
window = {"timeout": {"secs": 90, "nanos": 5}, "hours": [9, 17], "starts": "2026-11-07"}
assert echo_window(window) == {"timeout": {"secs": 90, "nanos": 5}, "hours": (9, 17), "starts": "2026-11-07"}, echo_window(window)
assert file_name("/tmp/report.csv") == "report.csv"
assert file_name("/") is None
assert count_stops({"stops": [4, 8, 15]}) == 3
"#,
        &["echoWindow", "fileName", "countStops"],
        &[],
    )
}

/// A feature gate decides which members Python has, and a gated async call
/// makes the same first-poll choice as an ungated one.
#[test]
fn a_feature_gate_decides_whether_python_has_a_member() -> PyResult<()> {
    let scenario = if cfg!(feature = "alloy") {
        cr#"
till = Till.holding(5)
assert till.take(2) == 7
assert till.receipt() == {"total": 7}
assert not hasattr(till, "plainTotal")


# A gated async call starts where an ungated one does: on this thread, unless
# it takes a function.
import asyncio
import threading


async def main():
    settled = till.settle()
    assert settled.done(), "a call that does not wait is done when it returns"
    assert settled.result() == 7

    callers = []

    def doubled(total):
        callers.append(threading.get_ident())
        return total * 2

    # A function crosses with the adapter of its result, as the generated
    # code passes it. These need none, since Rust reads their results as
    # they are.
    called = till.settleWith((doubled, None))
    assert not called.done(), "a call that takes a function starts on a worker"
    assert await asyncio.wait_for(called, 5) == 14
    assert callers != [threading.get_ident()], callers


asyncio.run(main())
"#
    } else {
        cr#"
till = Till()
assert till.take(2) == 2
assert till.plainTotal() == 2
assert not hasattr(till, "receipt")
assert not hasattr(Till, "holding")
"#
    };
    python::run_scenario(scenario, &[], &["Till"])
}

#[cfg(feature = "alloy")]
#[test]
fn alloy_types_cross_as_hex_strings() -> PyResult<()> {
    python::run_scenario(
        cr#"
transfer = {
    "from": "0x00110b8e0000000000000000000000000000beef",
    "amount": "0xde0b6b3a7640000",
    "transaction": "0x" + "ab" * 32,
    "nonce": "0x7",
    "change": "-1000000000000000000",
}
echoed = echo_transfer(transfer)
assert echoed["from"].lower() == transfer["from"], echoed
assert int(echoed["amount"], 16) == 10**18, echoed
assert echoed["transaction"] == transfer["transaction"], echoed
assert echoed["nonce"] == "0x7", echoed
assert echoed["change"] == transfer["change"], echoed
"#,
        &["echoTransfer"],
        &[],
    )
}

#[test]
fn rust_checks_the_field_rules_inside_variants_under_their_names() -> PyResult<()> {
    python::with_registered_module(|py, module, manifest| {
        let echo_step = module.getattr(&lookup::function(manifest, "echoStep").native_name)?;
        let stopped = PyDict::new(py);
        let fields = PyDict::new(py);
        fields.set_item("acceptedCount", 0)?;
        stopped.set_item("stopped", fields)?;
        let error = echo_step
            .call1((stopped,))
            .expect_err("a count below its rule must fail");
        assert_eq!(
            exception(py, &error)?,
            (
                "ValueError".to_owned(),
                "echo_step() argument 'step': `stopped.acceptedCount` must be at least 1, got 0"
                    .to_owned()
            )
        );

        let running = PyDict::new(py);
        let request = job_request(py, 0)?;
        request.set_item("context", Option::<String>::None)?;
        running.set_item("running", request)?;
        let error = echo_step
            .call1((running,))
            .expect_err("a count below its rule must fail");
        assert_eq!(
            exception(py, &error)?,
            (
                "ValueError".to_owned(),
                "echo_step() argument 'step': `running.count` must be at least 1, got 0".to_owned()
            )
        );
        Ok(())
    })
}

/// Return the class name and the message of a Python exception.
fn exception(py: Python<'_>, error: &PyErr) -> PyResult<(String, String)> {
    Ok((
        error.get_type(py).name()?.to_string(),
        error.value(py).to_string(),
    ))
}

#[test]
fn argument_errors_name_the_callable_and_the_parameter() -> PyResult<()> {
    python::with_registered_module(|py, module, manifest| {
        let echo_u64 = module.getattr(&lookup::function(manifest, "echoU64").native_name)?;
        let reverse_bytes =
            module.getattr(&lookup::function(manifest, "reverseBytes").native_name)?;
        let scale_samples =
            module.getattr(&lookup::function(manifest, "scaleSamples").native_name)?;
        let counter_type = module.getattr(&lookup::resource(manifest, "Counter").native_name)?;
        let samples = py
            .import("array")?
            .getattr("array")?
            .call1(("d", vec![1.0_f64]))?;
        let integers = py
            .import("array")?
            .getattr("array")?
            .call1(("i", vec![1_i32]))?;

        let (kind, message) = exception(py, &echo_u64.call1(("one",)).expect_err("a str"))?;
        assert_eq!(kind, "TypeError");
        assert!(
            message.starts_with("echo_u64() argument 'value': "),
            "{message}"
        );
        // The type is right, but the value is not.
        let (kind, message) = exception(py, &echo_u64.call1((-1,)).expect_err("a negative"))?;
        assert_eq!(kind, "ValueError");
        assert!(
            message.starts_with("echo_u64() argument 'value': "),
            "{message}"
        );
        assert_eq!(
            exception(
                py,
                &scale_samples
                    .call1((samples.clone(), "x"))
                    .expect_err("a str factor")
            )?,
            (
                "TypeError".to_owned(),
                "scale_samples() argument 'factor' must be real number, not str".to_owned()
            )
        );
        assert_eq!(
            exception(
                py,
                &scale_samples
                    .call1((vec![1.0_f64], 2.0))
                    .expect_err("a list of samples")
            )?,
            (
                "TypeError".to_owned(),
                "scale_samples() argument 'samples' must be a buffer of float64 values, such as a NumPy array, not list".to_owned()
            )
        );
        assert_eq!(
            exception(
                py,
                &scale_samples
                    .call1((integers, 2.0))
                    .expect_err("a buffer of integers")
            )?,
            (
                "BufferError".to_owned(),
                "scale_samples() argument 'samples' must hold native-endian float64 values, not values of the buffer format 'i'".to_owned()
            )
        );
        assert_eq!(
            exception(py, &reverse_bytes.call1(("abc",)).expect_err("a str"))?,
            (
                "TypeError".to_owned(),
                "reverse_bytes() argument 'input': a bytes-like object is required, not 'str'"
                    .to_owned()
            )
        );
        // A `str` is a sequence of characters, but not a list of strings.
        let join_labels = module.getattr(&lookup::function(manifest, "joinLabels").native_name)?;
        assert_eq!(
            join_labels.call1((vec!["a", "b"],))?.extract::<String>()?,
            "a,b"
        );
        assert_eq!(
            exception(py, &join_labels.call1(("ab",)).expect_err("a str"))?,
            (
                "TypeError".to_owned(),
                "join_labels() argument 'labels' must be a list, not str".to_owned()
            )
        );
        // Errors use the parameter's name in the generated wrapper.
        let echo_length = module.getattr(&lookup::function(manifest, "echoLength").native_name)?;
        let (kind, message) = exception(py, &echo_length.call1(("one",)).expect_err("a str"))?;
        assert_eq!(kind, "TypeError");
        assert!(
            message.starts_with("echo_length() argument 'bytes_': "),
            "{message}"
        );
        // The class constructor is the class itself in Python.
        let (kind, message) = exception(py, &counter_type.call1(("one",)).expect_err("a str"))?;
        assert_eq!(kind, "TypeError");
        assert!(
            message.starts_with("Counter() argument 'initial_value': "),
            "{message}"
        );

        Ok(())
    })
}

/// Buffers in this machine's byte order cross, whether or not their format
/// says so, as ctypes' does; buffers in the other byte order fail rather than
/// cross with their bytes reversed.
const BUFFER_SCENARIO: &CStr = cr#"
import ctypes
import sys

little, big = ctypes.c_double.__ctype_le__, ctypes.c_double.__ctype_be__
native, foreign = (little, big) if sys.byteorder == "little" else (big, little)
samples = memoryview((native * 2)(1.5, -2.0))
assert samples.format in ("<d", ">d"), samples.format
assert memoryview(scale_samples(samples, 2.0)).cast("d").tolist() == [3.0, -4.0]

reversed_samples = memoryview((foreign * 2)(1.5, -2.0))
try:
    scale_samples(reversed_samples, 2.0)
except BufferError as error:
    format = reversed_samples.format
    assert str(error) == (
        "scale_samples() argument 'samples' must hold native-endian float64 values, "
        f"not values of the buffer format '{format}'"
    ), str(error)
else:
    raise AssertionError("a buffer in the other byte order must fail")
"#;

#[test]
fn buffers_cross_only_in_the_native_byte_order() -> PyResult<()> {
    python::run_scenario(BUFFER_SCENARIO, &["scaleSamples"], &[])
}

/// A `KeyboardInterrupt` while Rust reads an argument stays one, rather than
/// becoming the `ValueError` of a bad value.
const INTERRUPT_SCENARIO: &CStr = cr#"
class Interrupting(list):
    def __len__(self):
        raise KeyboardInterrupt


try:
    echo_wide_numbers({"value": 1, "values": Interrupting(), "optional": None})
except KeyboardInterrupt:
    pass
else:
    raise AssertionError("the interrupt must propagate")
"#;

#[test]
fn interrupts_while_reading_arguments_propagate() -> PyResult<()> {
    python::run_scenario(INTERRUPT_SCENARIO, &["echoWideNumbers"], &[])
}

#[test]
fn the_module_records_the_hash_of_its_contract() -> PyResult<()> {
    python::with_registered_module(|_, module, _| {
        let contract =
            bimetal::registry::contract_json(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                .expect("the test registry must be valid");

        assert_eq!(
            module
                .getattr("__bimetal_contract__")?
                .extract::<String>()?,
            bimetal::ir::discovery::contract_hash(contract.as_bytes())
        );
        Ok(())
    })
}

#[test]
fn resources_support_factories_state_and_close() -> PyResult<()> {
    python::with_registered_module(|py, module, manifest| {
        let counter_type = module.getattr(&lookup::resource(manifest, "Counter").native_name)?;

        let negative = counter_type
            .call1((-1,))
            .expect_err("a negative counter must fail");
        assert_eq!(
            error_details(py, &negative)?,
            (
                "negative_start".to_owned(),
                "the initial counter value cannot be negative".to_owned(),
            )
        );

        let counter = counter_type.call1((10,))?;
        assert_eq!(counter.call_method1("add", (5,))?.extract::<i64>()?, 15);
        assert_eq!(
            counter
                .call_method1("addMany", (1, 2, 3, 4, 5, 6))?
                .extract::<i64>()?,
            36
        );
        assert!(!counter.hasattr("reset")?);
        counter.call_method0("close")?;
        let closed = counter
            .call_method1("add", (1,))
            .expect_err("a closed resource must reject later calls");
        assert_eq!(
            closed.to_string(),
            "RuntimeError: Counter.add(): the resource is closed"
        );

        let zero = counter_type.call_method0("fromZero")?;
        assert_eq!(zero.call_method1("add", (2,))?.extract::<i64>()?, 2);

        Ok(())
    })
}

/// Drive the async fixtures from asyncio: results, typed errors, concurrency,
/// async factories, cancellation, and closing while a call runs.
const ASYNC_SCENARIO: &CStr = cr#"
import asyncio


async def expect_error(awaitable, args):
    try:
        await awaitable
    except RuntimeError as error:
        assert error.args == args, error.args
    else:
        raise AssertionError(f"expected {args}")


async def main():
    assert await double_later(21) == 42
    assert await asyncio.gather(*(double_later(value) for value in range(4))) == [0, 2, 4, 6]
    await expect_error(
        check_count_later(0), ("invalid_count", "count must be between 1 and 100", None)
    )

    try:
        Ledger()
    except TypeError:
        pass
    else:
        raise AssertionError("a resource with only async constructors has no constructor")
    await expect_error(
        Ledger.open(""), ("invalid_count", "count must be between 1 and 100", None)
    )

    ledger = await Ledger.open("first")
    assert await ledger.record("second") == 2
    assert ledger.count() == 2
    # Two `&self` calls pass the two-party barrier only if they run at once.
    assert await asyncio.wait_for(asyncio.gather(ledger.meet(), ledger.meet()), 5) == [2, 2]

    # Cancelling a call drops its Rust future, which releases the value.
    holding = ledger.hold()
    await asyncio.sleep(0.05)
    holding.cancel()
    assert await asyncio.wait_for(ledger.record("third"), 5) == 3

    # A call that is running when the resource closes still finishes.
    pending = ledger.record("fourth")
    ledger.close()
    assert await pending == 4
    try:
        ledger.count()
    except RuntimeError as error:
        assert str(error) == "Ledger.count(): the resource is closed", error
    else:
        raise AssertionError("a closed resource must reject later calls")


asyncio.run(main())
"#;

#[test]
fn async_calls_resolve_asyncio_awaitables() -> PyResult<()> {
    python::run_scenario(
        ASYNC_SCENARIO,
        &["doubleLater", "checkCountLater"],
        &["Ledger"],
    )
}

/// A call that finishes before its first wait returns a future that is done:
/// no worker runs it and nothing is scheduled on the event loop. Its body
/// runs on the caller's thread without the interpreter lock, and a call that
/// waits goes on from there on a worker. A call that takes a Python function
/// starts on a worker, so a function that waits for the event loop does not
/// wait for its own thread.
const READY_SCENARIO: &CStr = cr#"
import asyncio
import threading


async def main():
    future = double_at_once(4)
    assert future.done()
    assert future.result() == 8
    assert await double_at_once(5) == 10

    failed = check_count_at_once(0)
    assert failed.done()
    error = failed.exception()
    assert isinstance(error, RuntimeError), error
    assert error.args[0] == "invalid_count", error.args

    panicked = panic_at_once("async panic at once")
    assert panicked.done()
    try:
        panicked.result()
    except PanicException as error:
        assert str(error) == "async panic at once", str(error)
    else:
        raise AssertionError("a panic before the first wait must raise")

    assert not await holds_interpreter_lock_at_once()

    waiting = threads_of_a_call()
    assert not waiting.done()
    before, after = await asyncio.wait_for(waiting, 5)
    assert before != after, (before, after)
    # Each call ran its first part on this thread.
    again, _ = await asyncio.wait_for(threads_of_a_call(), 5)
    assert again == before, (again, before)

    callers = []

    def tripled(value):
        callers.append(threading.get_ident())
        return value * 3

    called = call_at_once((tripled, None), 4)
    assert not called.done(), "a call that takes a function starts on a worker"
    assert await asyncio.wait_for(called, 5) == 12
    assert callers != [threading.get_ident()], callers

    loop = asyncio.get_running_loop()

    async def doubled(value):
        return value * 2

    def through_the_loop(value):
        return asyncio.run_coroutine_threadsafe(doubled(value), loop).result(timeout=5)

    assert await asyncio.wait_for(call_at_once((through_the_loop, None), 4), 10) == 8

    # A call that is done has no task to abort.
    cancelled = double_at_once(1)
    assert not cancelled.cancel()


asyncio.run(main())

try:
    double_at_once(1)
except RuntimeError as error:
    assert str(error) == "no running event loop", error
else:
    raise AssertionError("a call that is ready needs a running event loop too")
"#;

#[test]
fn a_call_that_does_not_wait_is_done_when_it_returns() -> PyResult<()> {
    python::run_scenario(
        READY_SCENARIO,
        &[
            "doubleAtOnce",
            "checkCountAtOnce",
            "callAtOnce",
            "panicAtOnce",
            "holdsInterpreterLockAtOnce",
            "threadsOfACall",
        ],
        &[],
    )
}

/// An async call needs a running event loop, a loop that closes while a call
/// runs drops its result quietly, and a result that arrives after the caller
/// cancelled the call is dropped too.
const ASYNC_EDGE_SCENARIO: &CStr = cr#"
import asyncio
import io
import sys
import time


try:
    double_later(1)
except RuntimeError as error:
    assert str(error) == "no running event loop", error
else:
    raise AssertionError("an async call needs a running event loop")


def start():
    global pending
    pending = double_later(1)
    # The loop stops before it runs again, so nothing settles the call.
    loop.stop()


async def call(value):
    return await asyncio.wait_for(double_later(value), 5)


loop = asyncio.new_event_loop()
loop.call_soon(start)
loop.run_forever()
loop.close()
stderr, sys.stderr = sys.stderr, io.StringIO()
try:
    # The call finishes and finds its loop closed.
    time.sleep(0.2)
    printed = sys.stderr.getvalue()
finally:
    sys.stderr = stderr
assert printed == "", printed
assert not pending.done()
assert asyncio.run(call(2)) == 4


async def cancel_after_completion():
    errors = []
    asyncio.get_running_loop().set_exception_handler(lambda loop, context: errors.append(context))
    future = double_later(1)
    # The loop's thread sleeps while the result is scheduled on the loop.
    time.sleep(0.2)
    future.cancel()
    for _ in range(3):
        await asyncio.sleep(0)
    assert future.cancelled()
    assert errors == [], errors


asyncio.run(cancel_after_completion())
"#;

#[test]
fn async_calls_need_a_loop_and_drop_results_that_no_one_awaits() -> PyResult<()> {
    python::run_scenario(ASYNC_EDGE_SCENARIO, &["doubleLater"], &[])
}

/// The results of calls that wait reach their own loop: many at once, on a
/// loop that has no readers and is woken through `call_soon_threadsafe`, on
/// two loops of two threads at once, and while another thread competes for
/// the interpreter lock. A loop that closes takes its mailbox's socket with
/// it, so 40 loops in turn leave fewer than 40 more descriptors open. The
/// other tests of this process open and close descriptors of their own
/// meanwhile, but a leak would leave at least one for each loop.
const MAILBOX_SCENARIO: &CStr = cr#"
import asyncio
import os
import sys
import threading


class NoReaders(asyncio.SelectorEventLoop):
    def add_reader(self, *arguments):
        raise NotImplementedError


async def many(offset):
    values = range(offset, offset + 64)
    results = await asyncio.wait_for(asyncio.gather(*(double_later(value) for value in values)), 10)
    assert results == [value * 2 for value in values], results
    # A call that the caller cancels while its result is on its way drops it.
    cancelled = double_later(1)
    cancelled.cancel()
    assert await asyncio.wait_for(double_later(offset), 5) == offset * 2
    assert cancelled.cancelled()


for make in (asyncio.new_event_loop, NoReaders):
    loop = make()
    try:
        loop.run_until_complete(many(0))
    finally:
        loop.close()

failures = []


def in_thread(offset):
    try:
        asyncio.run(many(offset))
    except BaseException as error:
        failures.append(error)


threads = [threading.Thread(target=in_thread, args=(offset,)) for offset in (100, 200)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join(30)
assert failures == [], failures


stopped = False


def compete():
    count = 0
    while not stopped:
        count += 1


contender = threading.Thread(target=compete)
contender.start()
try:
    asyncio.run(many(300))
finally:
    stopped = True
    contender.join()

if sys.platform != "win32":
    descriptors = "/dev/fd" if os.path.isdir("/dev/fd") else "/proc/self/fd"
    asyncio.run(many(0))
    before = len(os.listdir(descriptors))
    for _ in range(40):
        asyncio.run(many(0))
    after = len(os.listdir(descriptors))
    assert after - before < 40, (before, after)
"#;

/// An async export that works briefly finishes on the caller's thread. Once
/// one of its calls works much longer than a hand-off to a worker costs, its
/// later calls start on workers, so they leave the event loop free, and they
/// come back once its calls work briefly again. A call of an export times
/// itself one time in eight while the export works briefly, and an export of
/// 2 ms calls moves after two of them in a row.
const PACE_SCENARIO: &CStr = cr#"
import asyncio


async def main():
    assert busy_for(0).done()
    for _ in range(12):
        moved = busy_for(2000)
        if not moved.done():
            break
    else:
        raise AssertionError("an export that works long moves within twelve calls")
    assert await asyncio.wait_for(moved, 5) == 2000
    later = busy_for(2000)
    assert not later.done(), "a call of an export that works long starts on a worker"
    assert await asyncio.wait_for(later, 5) == 2000
    for _ in range(40):
        assert await asyncio.wait_for(busy_for(0), 5) == 0
    assert busy_for(0).done(), "calls that work briefly again finish where they start"


asyncio.run(main())
"#;

#[test]
fn async_calls_that_work_long_start_on_workers() -> PyResult<()> {
    python::run_scenario(PACE_SCENARIO, &["busyFor"], &[])
}

#[test]
fn waiting_calls_reach_their_own_loop() -> PyResult<()> {
    python::run_scenario(MAILBOX_SCENARIO, &["doubleLater"], &[])
}

/// An async `&mut self` call holds the value from when it is made, before
/// the event loop runs again, until the Rust call returns. A synchronous call
/// made right after it waits for it, and sees what it did, though the loop
/// that would convert the async call's result is the one that waits.
const LOCK_SCENARIO: &CStr = cr#"
import asyncio


async def main():
    ledger = await Ledger.open("first")
    pending = ledger.record("second")
    assert ledger.count() == 2
    assert await asyncio.wait_for(pending, 5) == 2
    assert ledger.count() == 2


asyncio.run(main())
"#;

#[test]
fn async_calls_hold_the_value_from_the_call_until_they_finish() -> PyResult<()> {
    python::run_scenario(LOCK_SCENARIO, &[], &["Ledger"])
}

/// Synchronous constructors and factories run inside Bimetal's runtime, so they
/// can spawn tasks on it; synchronous functions and methods do not, so a
/// package without resources or async calls never starts its threads.
const SPAWN_SCENARIO: &CStr = cr#"
import asyncio


assert not in_runtime()


async def main():
    background = Background(4)
    assert await asyncio.wait_for(background.result(), 5) == 8
    assert not background.inRuntime()
    tripled = Background.tripled(4)
    assert await asyncio.wait_for(tripled.result(), 5) == 12


asyncio.run(main())
"#;

#[test]
fn sync_constructors_can_spawn_tasks() -> PyResult<()> {
    python::run_scenario(SPAWN_SCENARIO, &["inRuntime"], &["Background"])
}

/// Round-trip every Arrow value through Bimetal's own exporters and importers,
/// which needs no pyarrow in the test interpreter.
#[cfg(feature = "arrow-api")]
const ARROW_SCENARIO: &CStr = cr#"
table = sample_table()
assert summarize_table(table) == {"rows": 3, "batches": 2, "columns": ["n", "label"]}
# An export can be read more than once.
assert summarize_table(table)["rows"] == 3

batch = first_batch(table)
assert summarize_table(batch_as_table(batch)) == {
    "rows": 2,
    "batches": 1,
    "columns": ["n", "label"],
}
labels = column_at(batch, 1)
assert array_length(labels) == 2
# Consumers pass the requested schema positionally.
schema_capsule, array_capsule = labels.__arrow_c_array__(None)
assert type(array_capsule).__name__ == "PyCapsule"

schema = tag_schema(schema_of(table), "source", "fixture")
assert schema_metadata(schema) == {"source": "fixture"}

try:
    summarize_table(batch)
except TypeError as error:
    assert "__arrow_c_stream__" in str(error), error
else:
    raise AssertionError("a batch is not a stream")
"#;

#[cfg(feature = "arrow-api")]
#[test]
fn arrow_values_cross_through_the_pycapsule_interface() -> PyResult<()> {
    python::run_scenario(
        ARROW_SCENARIO,
        &[
            "sampleTable",
            "summarizeTable",
            "firstBatch",
            "batchAsTable",
            "columnAt",
            "arrayLength",
            "schemaOf",
            "tagSchema",
            "schemaMetadata",
        ],
        &[],
    )
}

/// Streams cross lazily both ways, and Bimetal's own importer reads Bimetal's own
/// exports, which needs no pyarrow. A Python function feeds some streams,
/// which a blocking thread calls: a pull that kept the interpreter lock while
/// it waited would deadlock, as the table import's pulls would. An async call
/// that takes a stream starts on a worker, even one that reads the stream
/// before its first wait, so its reads never block the event loop's thread.
#[cfg(feature = "arrow-api")]
const STREAM_SCENARIO: &CStr = cr#"
import asyncio


def run(call):
    async def main():
        return await asyncio.wait_for(call(), 5)
    return asyncio.run(main())


def run_on_worker(call):
    async def main():
        pending = call()
        assert not pending.done(), "a call that takes a stream starts on a worker"
        return await asyncio.wait_for(pending, 5)
    return asyncio.run(main())


def counted(batches, rows, fail_at=None, panic_at=None):
    return count(batches, rows, fail_at, panic_at)


def tens(index):
    return None if index == 3 else index * 10


def fails(index):
    if index == 1:
        raise ValueError("no second value")
    return index


assert sum(counted(3, 2)) == 15
assert run(lambda: sum_later(counted(3, 2))) == 15
assert sum(run(lambda: count_later(3, 2))) == 15
assert summarize_table(counted(2, 3)) == {"rows": 6, "batches": 2, "columns": ["n"]}
assert run(lambda: sum_on_worker(counted(3, 2))) == 15
assert live_streams() == 0

for source in (generate, generate_blocking):
    assert sum(source((tens, None))) == 30, source
    assert run(lambda: sum_later(source((tens, None)))) == 30, source
    assert run(lambda: sum_on_worker(source((tens, None)))) == 30, source
    assert run_on_worker(lambda: sum_at_once(source((tens, None)))) == 30, source
    assert summarize_table(source((tens, None)))["rows"] == 3, source
    try:
        sum(source((fails, None)))
    except RuntimeError as error:
        assert error.args[0] == "read_error", error.args
        assert "no second value" in error.args[1], error.args
    else:
        raise AssertionError("the source fails")

# An error ends the stream, and every later read fails the same way.
failed = outcomes(counted(3, 1, fail_at=1), 4)
assert failed[0] == "rows:1", failed
# arrow-rs 59 and 60 word the reader's own prefix differently.
assert failed[1].endswith(
    "External error: invalid_count: count must be between 1 and 100"
), failed
assert failed[1:] == [failed[1]] * 3, failed
assert outcomes(counted(1, 1), 3) == ["rows:1", "end", "end"]
panicked = outcomes(counted(2, 1, panic_at=1), 3)
assert panicked[1].endswith("External error: panic: batch 1 panics"), panicked
assert panicked[2] == panicked[1], panicked

# A reader that stops early, or never reads, drops the stream.
assert first(counted(3, 2)) == 1
reader = counted(3, 2)
assert live_streams() == 1
capsule = reader.__arrow_c_stream__()
try:
    reader.__arrow_c_stream__()
except RuntimeError as error:
    assert "only once" in str(error), error
else:
    raise AssertionError("a stream exports once")
del reader
assert live_streams() == 1
del capsule
assert live_streams() == 0
"#;

#[cfg(feature = "arrow-api")]
#[test]
fn streams_cross_one_batch_at_a_time() -> PyResult<()> {
    python::run_scenario(
        STREAM_SCENARIO,
        &[
            "count",
            "countLater",
            "liveStreams",
            "generate",
            "generateBlocking",
            "sum",
            "sumLater",
            "sumOnWorker",
            "sumAtOnce",
            "first",
            "outcomes",
            "summarizeTable",
        ],
        &[],
    )
}

/// Panics in sync calls, in async calls, and in the conversion of an async
/// result all raise `PanicException` with the panic's message. Every await
/// has a timeout, so a call that never resolves fails instead of hanging.
const PANIC_SCENARIO: &CStr = cr#"
import asyncio


assert issubclass(PanicException, BaseException)
assert not issubclass(PanicException, Exception)


def expect_panic(call, message):
    try:
        call()
    except PanicException as error:
        assert str(error) == message, str(error)
    else:
        raise AssertionError(f"expected a panic: {message}")


async def expect_async_panic(awaitable, message):
    try:
        await asyncio.wait_for(awaitable, 5)
    except PanicException as error:
        assert str(error) == message, str(error)
    else:
        raise AssertionError(f"expected a panic: {message}")


expect_panic(lambda: panic_now("sync panic"), "sync panic")


async def main():
    await expect_async_panic(panic_later("async panic"), "async panic")
    await expect_async_panic(convert_later(), "the value cannot convert")
    # The runtime still runs calls after a panic.
    assert await asyncio.wait_for(double_later(4), 5) == 8


asyncio.run(main())
"#;

#[test]
fn panics_raise_the_panic_exception_with_their_message() -> PyResult<()> {
    python::run_scenario(
        PANIC_SCENARIO,
        &["panicNow", "panicLater", "convertLater", "doubleLater"],
        &[],
    )
}
