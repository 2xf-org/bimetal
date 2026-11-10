"""Time calls into the showcase's Python package, or run one memory case, for
apps/benchmark/run.py.

Usage: calls.py <project> time [<case> <batch> <samples>]...
       calls.py <project> memory <case> <calls>

<project> is the Python project that holds the `showcase` package. `time`
runs <samples> batches of <batch> calls for each case that this file
defines, and prints each batch's nanoseconds, one JSON line per case.
`memory` sets up the input of <case> and makes <calls> calls.
"""

import asyncio
import json
import sys
import time
from collections.abc import Callable
from datetime import UTC, datetime, timedelta, timezone
from typing import Any

MODELS = 10_000
ROWS = 1_000_000
# The rows of each batch of a stream case's table.
STREAM_BATCH = 10_000


async def measure(
    name: str, counts: dict[str, tuple[int, int]], call: Callable[[], Any], *, awaited: bool = False
) -> None:
    """Time the batches of `call` that `counts` gives for `name`, awaiting
    each call if `awaited`."""
    if name not in counts:
        raise SystemExit(f"no batch and sample counts for {name}")
    batch, samples = counts[name]
    elapsed = []
    for _ in range(samples):
        start = time.perf_counter_ns()
        if awaited:
            for _ in range(batch):
                await call()
        else:
            for _ in range(batch):
                call()
        elapsed.append(time.perf_counter_ns() - start)
    print(json.dumps({"case": name, "batch_ns": elapsed}), flush=True)


def bytes_input(length: int) -> bytes:
    return bytes(range(256)) * (length // 256)


def arrow_input() -> Any:
    import numpy
    import pyarrow

    return pyarrow.record_batch(
        {
            "price": numpy.arange(ROWS, dtype=numpy.float64) * 0.5,
            "quantity": numpy.arange(ROWS, dtype=numpy.int64),
        }
    )


def split_batch(batch: Any) -> None:
    from showcase.columns import split

    if split(batch, ROWS).num_rows != ROWS:
        raise SystemExit("split returned the wrong row count")


def stream_input() -> Any:
    """The rows of `arrow_input`, as a table of STREAM_BATCH-row batches."""
    import pyarrow

    batch = arrow_input()
    return pyarrow.Table.from_batches(
        [batch.slice(offset, STREAM_BATCH) for offset in range(0, ROWS, STREAM_BATCH)]
    )


def read_stream(table: Any) -> None:
    from showcase.columns import batches

    if batches(table).read_all().num_rows != ROWS:
        raise SystemExit("the stream returned the wrong row count")


def native_function(wrapper: Callable[..., object]) -> Callable[..., object]:
    """The native function that a generated wrapper calls, to separate the
    wrapper's cost from the boundary's."""
    from showcase import _native

    (name,) = (
        constant
        for constant in wrapper.__code__.co_consts
        if isinstance(constant, str) and constant.startswith("__bimetal_function_")
    )
    function: Callable[..., object] = getattr(_native, name)
    return function


async def time_cases(counts: dict[str, tuple[int, int]]) -> None:
    import showcase
    from showcase._runtime import dump_host
    from showcase.columns import batches, sum_column
    from showcase.geometry import Point

    record = showcase.Record.model_validate(
        {
            "id": "r-1",
            "label": "First",
            "priority": showcase.Priority.High,
            "score": 90,
            "stars": 4,
            "note": None,
            "tags": ["a", "b"],
            "counters": {"views": 3},
            "ranges": {"week": (1, 7)},
            "created_at": datetime(2026, 7, 22, 12, 0, tzinfo=UTC),
            "local_time": datetime(2026, 7, 22, 9, 0, tzinfo=timezone(timedelta(hours=-3))),
            "metadata": {"nested": [1, "two", None]},
            "checksum": [0, 255],
            "location": Point(x=1.5, y=-2.0),
        }
    )
    records = [record.model_copy() for _ in range(MODELS)]
    # What the generated `count_tags` passes to its native function.
    prepared = dump_host(list[showcase.Record], records)
    count_tags = native_function(showcase.count_tags)
    ledger = showcase.Ledger("Ada")
    for _ in range(MODELS):
        ledger.deposit(1)
    payload = bytes_input(1 << 20)
    batch = arrow_input()
    table = stream_input()
    cases: list[tuple[str, Callable[[], object]]] = [
        ("ping", showcase.ping),
        ("ping-raw", native_function(showcase.ping)),
        ("bytes-1mb", lambda: showcase.reverse(payload)),
        ("models-in-10k", lambda: showcase.count_tags(records)),
        ("models-in-10k-raw", lambda: count_tags(prepared)),
        ("models-out-10k", ledger.history),
        ("models-out-10k-raw", ledger.native_resource.history),
        ("arrow-1m", lambda: split_batch(batch)),
        ("stream-out-1m", lambda: read_stream(table)),
    ]
    for name, call in cases:
        await measure(name, counts, call)
    await measure("async", counts, lambda: showcase.count_words("a b c"), awaited=True)
    await measure(
        "stream-through-1m",
        counts,
        lambda: sum_column(batches(table), "price"),
        awaited=True,
    )
    ledger.close()


def memory(case: str, calls: int) -> None:
    import showcase

    if case in ("bytes-1mb", "bytes-100mb"):
        payload = bytes_input((1 if case == "bytes-1mb" else 100) << 20)
        for _ in range(calls):
            showcase.reverse(payload)
    elif case == "arrow-1m":
        batch = arrow_input()
        for _ in range(calls):
            split_batch(batch)
    else:
        raise SystemExit(f"unknown memory case {case}")


def main() -> None:
    arguments = sys.argv[1:]
    if len(arguments) < 2:
        raise SystemExit(__doc__)
    sys.path.insert(0, arguments[0])
    mode = arguments[1]
    if mode == "time" and len(arguments) % 3 == 2:
        given = arguments[2:]
        counts = {given[index]: (int(given[index + 1]), int(given[index + 2])) for index in range(0, len(given), 3)}
        asyncio.run(time_cases(counts))
    elif mode == "memory" and len(arguments) == 4:
        memory(arguments[2], int(arguments[3]))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
