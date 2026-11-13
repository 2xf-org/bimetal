"""Time the probe's crossings from Python, for apps/benchmark/lab.py.

Usage: lab_python.py <project> <rounds> <samples> [<case>...]

<project> is the Python project that holds the generated `probe` package.
Each case runs <rounds> times, interleaved with the others, and each time
takes <samples> samples of a batch of calls; the batch is as many calls as
take about a millisecond. The script prints one JSON line per case with the
nanoseconds per call of every sample. With case names, it times only those.

A case named `<name>-raw` calls the native function that the generated
wrapper of `<name>` calls, with the arguments that the wrapper would pass, so
the difference is the wrapper's cost. A case named `floor-*` makes no call
into Rust: it is the least that the same kind of crossing costs in Python.
A named case that this host does not define is skipped.
"""

import asyncio
import functools
import itertools
import json
import sys
import threading
import time
from collections.abc import Awaitable, Callable
from datetime import UTC, datetime
from typing import Any

# The calls of one sample take about this long.
SAMPLE_NS = 1_000_000
# Concurrent callers in the cases that measure throughput.
CALLERS = 32
# Rounds of `spin` that take about 170 µs on one core of an Apple M5.
HEAVY_ROUNDS = 100_000

Sync = Callable[[], object]
Async = Callable[[], Awaitable[object]]


def native_of(wrapper: Callable[..., object]) -> Callable[..., object]:
    """The native function that a generated wrapper calls: a global of its
    module whose value is a builtin, or the extension attribute it names. A
    brief function that the module binds is the native function itself."""
    from probe import _native

    code = getattr(wrapper, "__code__", None)
    if code is None:
        return wrapper
    for name in code.co_names:
        value = wrapper.__globals__.get(name)
        if type(value).__name__ == "builtin_function_or_method" and getattr(value, "__module__", None) != "builtins":
            return value
    for constant in code.co_consts:
        if isinstance(constant, str) and constant.startswith("__bimetal_function_"):
            return getattr(_native, constant)
    raise SystemExit(f"no native function in {wrapper.__name__}")


def passed(runtime: Any, annotation: Any, value: Any) -> Any:
    """What a generated wrapper passes first for `value`: the value itself
    where the extension reads models as they are, and otherwise its dump, by
    this runtime's adapter or the older `dump_host` of a base build."""
    if hasattr(runtime, "Revalidate"):
        return value
    adapter = getattr(runtime, "Adapter", None)
    if adapter is None:
        return runtime.dump_host(annotation, value)
    return adapter(lambda: annotation).dump(value)


class Contender(threading.Thread):
    """A thread that runs Python code until stopped, so that every other
    thread competes with it for the interpreter lock."""

    def __init__(self) -> None:
        super().__init__(daemon=True)
        self.stopped = False

    def run(self) -> None:
        count = 0
        while not self.stopped:
            count += 1


class Waker(threading.Thread):
    """A Python thread that settles futures through `call_soon_threadsafe`,
    the least that a call that hands its work to another thread costs."""

    def __init__(self, loop: asyncio.AbstractEventLoop) -> None:
        super().__init__(daemon=True)
        self.loop = loop
        self.queue: Any = __import__("queue").SimpleQueue()

    def run(self) -> None:
        while (future := self.queue.get()) is not None:
            self.loop.call_soon_threadsafe(future.set_result, 1)

    def wake(self) -> Awaitable[object]:
        future = self.loop.create_future()
        self.queue.put(future)
        return future


def batch_size(call: Sync) -> int:
    """How many calls take about SAMPLE_NS."""
    count = 1
    while True:
        start = time.perf_counter_ns()
        for _ in itertools.repeat(None, count):
            call()
        if time.perf_counter_ns() - start >= SAMPLE_NS or count >= 1 << 22:
            return count
        count *= 2


async def async_batch_size(call: Async) -> int:
    count = 1
    while True:
        start = time.perf_counter_ns()
        for _ in itertools.repeat(None, count):
            await call()
        if time.perf_counter_ns() - start >= SAMPLE_NS or count >= 1 << 20:
            return count
        count *= 2


def time_sync(call: Sync, batch: int, samples: int, per_call: int) -> list[float]:
    timed = []
    for _ in range(samples):
        start = time.perf_counter_ns()
        for _ in itertools.repeat(None, batch):
            call()
        timed.append((time.perf_counter_ns() - start) / (batch * per_call))
    return timed


async def time_async(call: Async, batch: int, samples: int, per_call: int) -> list[float]:
    timed = []
    for _ in range(samples):
        start = time.perf_counter_ns()
        for _ in itertools.repeat(None, batch):
            await call()
        timed.append((time.perf_counter_ns() - start) / (batch * per_call))
    return timed


def cases(loop: asyncio.AbstractEventLoop) -> dict[str, tuple[bool, Callable[[], Any], int]]:
    """Each case: whether it awaits, the call, and how many calls one call
    of it makes."""
    import pyarrow

    import probe
    from probe import _runtime

    rows = [
        probe.Row(
            id=index,
            name=f"row {index}",
            score=index * 0.5,
            active=index % 2 == 0,
            tags=["a", "b"],
            created_at=datetime(2026, 7, 14, 12, 0, tzinfo=UTC),
        )
        for index in range(100)
    ]
    point = probe.Point(x=3.0, y=4.0)
    passed_rows = passed(_runtime, list[probe.Row], rows)
    passed_point = passed(_runtime, probe.Point, point)
    hundred = list(range(100))
    words = ["alpha", "beta", "gamma", "alpha", "delta", "beta", "alpha", "omega", "pi", "rho"]
    text = "x" * 100
    kilobyte = bytes(range(256)) * 4
    megabyte = kilobyte * 1024
    batch = pyarrow.record_batch({"price": [0.5], "quantity": [1]})
    counter = probe.Counter(0)
    native_counter = counter.native_resource
    waker = Waker(loop)
    waker.start()

    def call_back(value: int) -> int:
        return value + 1

    def fail() -> None:
        try:
            probe.fail()
        except probe.ProbeError:
            pass

    native_fail = native_of(probe.fail)

    def fail_raw() -> None:
        try:
            native_fail()
        except RuntimeError:
            pass

    def read_stream() -> None:
        reader = probe.stream(100, 10)
        for _ in reader:
            pass

    async def gather(call: Async) -> None:
        await asyncio.gather(*(call() for _ in range(CALLERS)))

    async def contended(call: Async) -> None:
        contender = Contender()
        contender.start()
        try:
            await asyncio.gather(*(call() for _ in range(CALLERS)))
        finally:
            contender.stopped = True
            contender.join()

    def floor_function() -> int:
        return len(hundred)

    def sync(name: str, call: Sync) -> tuple[str, tuple[bool, Sync, int]]:
        return name, (False, call, 1)

    def awaited(name: str, call: Async, per_call: int = 1) -> tuple[str, tuple[bool, Async, int]]:
        return name, (True, call, per_call)

    p = functools.partial
    raw = native_of
    return dict(
        [
            sync("floor-call", p(len, hundred)),
            sync("floor-function", floor_function),
            sync("noop", probe.noop),
            sync("noop-raw", raw(probe.noop)),
            *(
                [sync("noop-brief", probe.noop_brief), sync("noop-brief-raw", raw(probe.noop_brief))]
                if hasattr(probe, "noop_brief")
                else []
            ),
            sync("add", p(probe.add, 1, 2)),
            sync("add-raw", p(raw(probe.add), 1, 2)),
            sync("echo", p(probe.echo, text)),
            sync("echo-raw", p(raw(probe.echo), text)),
            sync("numbers-100", p(probe.numbers, 100)),
            sync("numbers-100-raw", p(raw(probe.numbers), 100)),
            sync("total-100", p(probe.total, hundred)),
            sync("total-100-raw", p(raw(probe.total), hundred)),
            sync("tally-10", p(probe.tally, words)),
            sync("tally-10-raw", p(raw(probe.tally), words)),
            sync("bounds-100", p(probe.bounds, hundred)),
            sync("bounds-100-raw", p(raw(probe.bounds), hundred)),
            sync("point", p(probe.point, 3.0, 4.0)),
            sync("point-raw", p(raw(probe.point), 3.0, 4.0)),
            sync("length", p(probe.length, point)),
            sync("length-raw", p(raw(probe.length), passed_point)),
            sync("rows-100", p(probe.rows, 100)),
            sync("rows-100-raw", p(raw(probe.rows), 100)),
            sync("count-tags-100", p(probe.count_tags, rows)),
            sync("count-tags-100-raw", p(raw(probe.count_tags), passed_rows)),
            sync("reverse-1k", p(probe.reverse, kilobyte)),
            sync("reverse-1k-raw", p(raw(probe.reverse), kilobyte)),
            sync("reverse-1m", p(probe.reverse, megabyte)),
            sync("fail", fail),
            sync("fail-raw", fail_raw),
            sync("call", p(probe.call, call_back, 1)),
            sync("method-get", counter.get),
            sync("method-get-raw", native_counter.get),
            sync("method-add", p(counter.add, 1)),
            sync("method-add-raw", p(native_counter.add, 1)),
            sync("one-row", probe.one_row),
            sync("one-row-raw", raw(probe.one_row)),
            sync("batch-rows", p(probe.batch_rows, batch)),
            sync("batch-rows-raw", p(raw(probe.batch_rows), batch)),
            sync("stream-100x10", read_stream),
            awaited("floor-ready", p(_ready, loop)),
            awaited("ready", p(probe.ready, 1)),
            awaited("ready-raw", p(raw(probe.ready), 1)),
            awaited("ready-numbers-100", p(probe.ready_numbers, 100)),
            awaited("ready-numbers-100-raw", p(raw(probe.ready_numbers), 100)),
            awaited("floor-wake", waker.wake),
            awaited("wait", p(probe.wait, 1)),
            awaited("wait-raw", p(raw(probe.wait), 1)),
            awaited("method-add-later", p(counter.add_later, 1)),
            awaited("wait-32", p(gather, p(probe.wait, 1)), CALLERS),
            awaited("wait-32-contended", p(contended, p(probe.wait, 1)), CALLERS),
            awaited("floor-wake-32", p(gather, waker.wake), CALLERS),
            awaited("spin-async-32", p(gather, p(probe.spin_async, HEAVY_ROUNDS)), CALLERS),
            sync("spin", p(probe.spin, HEAVY_ROUNDS)),
        ]
    )


def _ready(loop: asyncio.AbstractEventLoop) -> Awaitable[object]:
    future = loop.create_future()
    future.set_result(1)
    return future


async def main() -> None:
    arguments = sys.argv[1:]
    if len(arguments) < 3:
        raise SystemExit(__doc__)
    sys.path.insert(0, arguments[0])
    rounds, samples = int(arguments[1]), int(arguments[2])
    chosen = set(arguments[3:])
    loop = asyncio.get_running_loop()
    every = cases(loop)
    selected = {name: case for name, case in every.items() if not chosen or name in chosen}
    batches: dict[str, int] = {}
    for name, (is_async, call, _) in selected.items():
        batches[name] = await async_batch_size(call) if is_async else batch_size(call)
    timed: dict[str, list[float]] = {name: [] for name in selected}
    for _ in range(rounds):
        for name, (is_async, call, per_call) in selected.items():
            if is_async:
                timed[name] += await time_async(call, batches[name], samples, per_call)
            else:
                timed[name] += time_sync(call, batches[name], samples, per_call)
    for name, samples_ns in timed.items():
        print(json.dumps({"case": name, "batch": batches[name], "ns": samples_ns}), flush=True)


if __name__ == "__main__":
    asyncio.run(main())
