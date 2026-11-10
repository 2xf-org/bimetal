//! A forked child of a process whose runtime already ran async calls.
//!
//! `fork` copies only the calling thread, so this test runs in a binary of its
//! own: a thread of another test could hold a lock that the child then waits
//! for forever.

#![cfg(all(feature = "python", unix))]

mod fixtures;

use std::ffi::CStr;

use bimetal::__private::pyo3::PyResult;

use crate::fixtures::python;

/// A forked child does not have the parent's runtime threads, so it starts a
/// runtime of its own. The child reports through its exit code, and the
/// parent stops waiting for it after a timeout.
const FORK_SCENARIO: &CStr = cr#"
import asyncio
import os
import selectors
import signal
import time


async def call(value):
    return await asyncio.wait_for(double_later(value), 5)


def child():
    try:
        return 0 if asyncio.run(call(21)) == 42 else 1
    except BaseException:
        return 2


def wait_for_child(pid):
    deadline = time.monotonic() + 30
    while True:
        finished, status = os.waitpid(pid, os.WNOHANG)
        if finished:
            break
        if time.monotonic() > deadline:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            raise AssertionError("the forked child did not finish")
        time.sleep(0.01)
    assert os.waitstatus_to_exitcode(status) == 0, os.waitstatus_to_exitcode(status)


def fork_and_wait():
    pid = os.fork()
    if pid == 0:
        os._exit(child())
    wait_for_child(pid)


async def typed_error():
    try:
        await asyncio.wait_for(check_count_later(0), 5)
    except RuntimeError as error:
        assert error.args[0] == "invalid_count", error.args
    else:
        raise AssertionError("a count of 0 is invalid")


async def panic():
    try:
        await asyncio.wait_for(panic_later("the call panics"), 5)
    except PanicException as error:
        assert str(error) == "the call panics", error
    else:
        raise AssertionError("the call panics")


async def cancelled_hold():
    ledger = await Ledger.open("first")
    holding = ledger.hold()
    await asyncio.sleep(0.05)
    holding.cancel()
    await asyncio.sleep(0)


# Start the runtime before the fork, as a server does before its workers fork.
# Each fork follows a call at once, while the runtime's threads may still be
# finishing it: a result, a typed error, a panic, or a cancellation.
for value in range(20):
    assert asyncio.run(call(value)) == value * 2
    fork_and_wait()
for scenario in (typed_error, panic, cancelled_hold):
    asyncio.run(scenario())
    fork_and_wait()


async def cancel_in_child():
    ledger = await Ledger.open("first")
    holding = ledger.hold()
    await asyncio.sleep(0.05)
    pid = os.fork()
    if pid == 0:
        # The child keeps running the parent's loop, and cancels the parent's
        # call, whose task belongs to the parent's runtime.
        errors = []
        holding.get_loop().set_exception_handler(lambda loop, context: errors.append(context))
        holding.cancel()
        await asyncio.sleep(0)
        await asyncio.sleep(0)
        os._exit(0 if holding.cancelled() and not errors else 3)
    wait_for_child(pid)
    holding.cancel()
    assert await asyncio.wait_for(ledger.record("second"), 5) == 2


# The child runs this loop too, so its selector holds no kernel object: a
# kqueue does not survive a fork, and an epoll instance stays the parent's.
loop = asyncio.SelectorEventLoop(selectors.SelectSelector())
try:
    loop.run_until_complete(cancel_in_child())
finally:
    loop.close()

# The parent's runtime keeps working.
assert asyncio.run(call(2)) == 4
"#;

/// A stream that the parent returned cannot be read in a forked child, whose
/// pulls fail at once instead of waiting for the parent's runtime, and whose
/// drop leaves it alone. The parent still reads it. It runs after
/// [`FORK_SCENARIO`], which started the runtime.
#[cfg(feature = "arrow-api")]
const STREAM_FORK_SCENARIO: &CStr = cr#"
import os
import signal
import time


def wait_for_child(pid):
    deadline = time.monotonic() + 30
    while True:
        finished, status = os.waitpid(pid, os.WNOHANG)
        if finished:
            break
        if time.monotonic() > deadline:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)
            raise AssertionError("the forked child did not finish")
        time.sleep(0.01)
    assert os.waitstatus_to_exitcode(status) == 0, os.waitstatus_to_exitcode(status)


reader = count(3, 1, None, None)
pid = os.fork()
if pid == 0:
    try:
        read = outcomes(reader, 2)
        del reader
        expected = "the stream belongs to the process that this one forked from"
        os._exit(0 if all(outcome.endswith(expected) for outcome in read) else 4)
    except BaseException:
        os._exit(5)
wait_for_child(pid)
assert outcomes(reader, 4) == ["rows:1", "rows:1", "rows:1", "end"]
"#;

#[test]
fn async_calls_run_in_a_forked_child() -> PyResult<()> {
    python::run_scenario(
        FORK_SCENARIO,
        &["doubleLater", "checkCountLater", "panicLater"],
        &["Ledger"],
    )?;
    #[cfg(feature = "arrow-api")]
    python::run_scenario(STREAM_FORK_SCENARIO, &["count", "outcomes"], &[])?;
    Ok(())
}
