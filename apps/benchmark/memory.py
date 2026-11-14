"""
Run one memory case of apps/benchmark/run.py in the showcase's Python package.

Usage: memory.py <project> memory <case> <calls>

<project> is the Python project that holds the `showcase` package. The script
sets up the input of <case> and makes <calls> calls, so that `/usr/bin/time`
can report the process's peak memory.
"""

import sys
import typing

ROWS = 1_000_000


def bytes_input(length: int) -> bytes:
    """
    Make the input of a bytes case.

    Args:
        length: The input's length, a multiple of 256.

    Returns:
        The input.
    """
    return bytes(range(256)) * (length // 256)


def arrow_input() -> typing.Any:
    """
    Make the input of the arrow case: a batch of ROWS rows with a float64 and an int64 column.

    Returns:
        The record batch.
    """
    # Only the arrow case imports pyarrow, which would otherwise add to the bytes cases' peak memory.
    import numpy
    import pyarrow

    return pyarrow.record_batch(
        {
            "price": numpy.arange(ROWS, dtype=numpy.float64) * 0.5,
            "quantity": numpy.arange(ROWS, dtype=numpy.int64),
        }
    )


def split_batch(batch: typing.Any) -> None:
    """
    Split a batch through the showcase, and check the rows that come back.

    Args:
        batch: The record batch.

    Raises:
        SystemExit: If the split returns another row count.
    """
    from showcase import columns

    if columns.split(batch, ROWS).num_rows != ROWS:
        raise SystemExit("split returned the wrong row count")


def memory(case: str, calls: int) -> None:
    """
    Set up a case's input, and make its calls.

    Args:
        case: The case's name.
        calls: The calls to make, or zero for the idle process.

    Raises:
        SystemExit: If the case is unknown.
    """
    # The showcase loads from the project that main puts on the path.
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
    """
    Run the case that the command line names.
    """
    arguments = sys.argv[1:]
    if len(arguments) != 4 or arguments[1] != "memory":
        raise SystemExit(__doc__)
    sys.path.insert(0, arguments[0])
    memory(arguments[2], int(arguments[3]))


if __name__ == "__main__":
    main()
