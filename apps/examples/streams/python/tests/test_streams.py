"""
Streams of record batches, read and passed by pyarrow, DuckDB, and Polars.
"""

from __future__ import annotations

import asyncio
import collections.abc
import threading
import unittest

import duckdb
import polars as pl
import pyarrow as pa

from streams import StreamError, count, open_streams, scale, total, total_later

SCHEMA = pa.schema([pa.field("n", pa.int64())])


class Source:
    """
    A reader whose batches a Python generator makes as Rust reads them.
    """

    def __init__(self, values: list[list[int]], fail_at: int | None = None) -> None:
        self.values = values
        self.fail_at = fail_at
        self.made = 0
        self.closed = False
        self.threads: set[str] = set()

    def generate(self) -> collections.abc.Iterator[pa.RecordBatch]:
        try:
            for index, numbers in enumerate(self.values):
                if index == self.fail_at:
                    raise ValueError("the source broke")
                self.made += 1
                self.threads.add(threading.current_thread().name)
                yield pa.record_batch([pa.array(numbers, pa.int64())], schema=SCHEMA)
        finally:
            self.closed = True

    def reader(self) -> pa.RecordBatchReader:
        return pa.RecordBatchReader.from_batches(SCHEMA, self.generate())


class ReturnedStreamTest(unittest.TestCase):
    def test_pyarrow_reads_one_batch_at_a_time(self) -> None:
        reader = count(3, 4)
        self.assertIsInstance(reader, pa.RecordBatchReader)
        self.assertEqual(reader.read_next_batch().column(0).to_pylist(), [0, 1, 2, 3])
        self.assertEqual(reader.read_all().column("n").to_pylist(), list(range(4, 12)))

    def test_duckdb_reads_it(self) -> None:
        relation = duckdb.from_arrow(count(3, 4))
        self.assertEqual(relation.aggregate("sum(n)").fetchall(), [(66,)])

    def test_a_duckdb_query_pulls_it_from_its_own_threads(self) -> None:
        connection = duckdb.connect()
        connection.execute("SET threads TO 4")
        reader = count(100, 1000)
        result = connection.sql("SELECT sum(n) FROM reader").fetchall()
        self.assertEqual(result, [(sum(range(100_000)),)])

    def test_polars_reads_it(self) -> None:
        frame = pl.from_arrow(count(3, 4))
        assert isinstance(frame, pl.DataFrame)
        self.assertEqual(frame["n"].to_list(), list(range(12)))

    def test_an_error_fails_every_later_read(self) -> None:
        reader = count(3, 2, fail_at=1)
        self.assertEqual(reader.read_next_batch().num_rows, 2)
        for _ in range(2):
            with self.assertRaises(pa.ArrowInvalid) as caught:
                reader.read_next_batch()
            self.assertEqual(str(caught.exception), "External error: stopped: the count stopped at batch 1")

    def test_query_engines_raise_the_error(self) -> None:
        message = "stopped: the count stopped at batch 1"
        with self.assertRaisesRegex(duckdb.Error, message):
            duckdb.from_arrow(count(3, 2, fail_at=1)).fetchall()
        with self.assertRaisesRegex(Exception, message):
            pl.from_arrow(count(3, 2, fail_at=1))

    def test_a_reader_that_closes_or_finishes_drops_the_stream(self) -> None:
        before = open_streams()
        reader = count(3, 2)
        reader.read_next_batch()
        self.assertEqual(open_streams(), before + 1)
        reader.close()
        self.assertEqual(open_streams(), before)

        finished = count(3, 2)
        finished.read_all()
        self.assertEqual(open_streams(), before)
        unread = count(3, 2)
        self.assertEqual(open_streams(), before + 1)
        del unread
        self.assertEqual(open_streams(), before)


class StreamParameterTest(unittest.TestCase):
    def test_sync_and_async_exports_read_each_source(self) -> None:
        sources: dict[str, collections.abc.Callable[[], object]] = {
            "duckdb relation": lambda: duckdb.sql("SELECT range AS n FROM range(1000)"),
            "polars frame": lambda: pl.DataFrame({"n": list(range(1000))}),
            "python generator": lambda: Source([list(range(500)), list(range(500, 1000))]).reader(),
        }
        for name, source in sources.items():
            with self.subTest(name):
                self.assertEqual(total(source()), sum(range(1000)))  # type: ignore[arg-type]
                self.assertEqual(
                    asyncio.run(total_later(source())),  # type: ignore[arg-type]
                    sum(range(1000)),
                )

    def test_a_python_source_runs_while_the_event_loop_waits(self) -> None:
        source = Source([[1], [2], [3]])

        async def main() -> int:
            return await asyncio.wait_for(total_later(source.reader()), 10)

        self.assertEqual(asyncio.run(main()), 6)
        # A blocking thread of Rust's makes each batch.
        self.assertNotIn(threading.current_thread().name, source.threads)

    def test_a_reader_that_stops_early_reads_no_further(self) -> None:
        source = Source([[1], [2], [3]])
        self.assertEqual(total(source.reader(), limit=1), 1)
        self.assertEqual(source.made, 1)
        self.assertTrue(source.closed)

    def test_a_failing_source_fails_the_read_of_its_batch(self) -> None:
        reads: list[collections.abc.Callable[[pa.RecordBatchReader], int]] = [
            total,
            lambda reader: asyncio.run(total_later(reader)),
        ]
        for read in reads:
            source = Source([[1], [2]], fail_at=1)
            with self.assertRaises(StreamError) as caught:
                read(source.reader())
            self.assertEqual(caught.exception.code, "read")
            self.assertIn("the source broke", str(caught.exception))
            self.assertEqual(source.made, 1)


class PassedThroughTest(unittest.TestCase):
    def test_duckdb_reads_a_python_source_that_rust_scales(self) -> None:
        source = Source([list(range(1000))] * 10)
        reader = asyncio.run(scale(source.reader(), 2))
        # Nothing is read until DuckDB asks.
        self.assertEqual(source.made, 0)
        connection = duckdb.connect()
        connection.execute("SET threads TO 4")
        result = connection.sql("SELECT sum(n) FROM reader").fetchall()
        self.assertEqual(result, [(2 * 10 * sum(range(1000)),)])
        self.assertEqual(source.made, 10)


if __name__ == "__main__":
    unittest.main()
