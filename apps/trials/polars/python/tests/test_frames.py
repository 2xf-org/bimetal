"""
Polars through the generated Python package.
"""

from __future__ import annotations

import unittest

import pyarrow as pa

from frames import Batches, ColumnInfo, Frame, FrameError, Query, Session

TRADES = pa.table(
    {
        "symbol": ["AAPL", "MSFT", "AAPL", "NVDA", "AAPL"],
        "price": [190.5, 410.25, 191.0, 880.0, 189.75],
        "size": [100, 50, 200, 10, 300],
    }
)
CSV = b"name,age\nAda,36\nGrace,45\nAlan,41\n"


def trades() -> Frame:
    """
    A frame of `TRADES`.
    """
    return Frame(TRADES)


class FrameTests(unittest.TestCase):
    def test_arrow_round_trip(self) -> None:
        table = trades().to_arrow()
        self.assertIsInstance(table, pa.Table)
        self.assertEqual(table.column("symbol").to_pylist(), TRADES.column("symbol").to_pylist())
        self.assertEqual(table.column("price").to_pylist(), TRADES.column("price").to_pylist())

    def test_a_record_batch_stream_crosses(self) -> None:
        chunked = pa.concat_tables([TRADES, TRADES])
        self.assertEqual(len(Frame(chunked)), 10)

    def test_read_csv(self) -> None:
        frame = Frame.read_csv(CSV)
        self.assertEqual(frame.columns, ["name", "age"])
        self.assertEqual(frame["age"], [36, 45, 41])

    def test_csv_without_a_header_uses_the_default(self) -> None:
        self.assertEqual(Frame.read_csv(b"1,2\n3,4\n", has_header=False).height, 2)

    def test_from_rows_and_to_rows(self) -> None:
        rows = [{"a": 1, "b": "x"}, {"a": 2, "b": "y"}]
        self.assertEqual(Frame.from_rows(rows).to_rows(), rows)

    def test_len_and_shape(self) -> None:
        frame = trades()
        self.assertEqual(len(frame), 5)
        self.assertEqual((frame.height, frame.width), (5, 3))

    def test_schema(self) -> None:
        self.assertEqual(trades().schema[1], ColumnInfo(name="price", dtype="f64"))

    def test_repr_is_the_polars_table(self) -> None:
        text = repr(trades())
        self.assertIn("shape: (5, 3)", text)
        self.assertIn("NVDA", text)
        self.assertEqual(str(trades()), text)

    def test_in_and_getitem(self) -> None:
        frame = trades()
        self.assertIn("price", frame)
        self.assertNotIn("nope", frame)
        self.assertEqual(frame["size"][:2], [100, 50])
        with self.assertRaises(KeyError):
            frame["nope"]

    def test_eq(self) -> None:
        self.assertEqual(trades(), trades())
        self.assertNotEqual(trades(), trades().head(2))

    def test_head_defaults(self) -> None:
        self.assertEqual(len(pa_frame(10).head()), 5)
        self.assertEqual(len(pa_frame(10).head(count=2)), 2)

    def test_to_csv(self) -> None:
        self.assertEqual(Frame.read_csv(CSV).to_csv(), CSV.decode())


def pa_frame(rows: int) -> Frame:
    """
    A frame made from a pyarrow table, whose one column `n` counts `rows` rows from zero.
    """
    return Frame(pa.table({"n": list(range(rows))}))


class ArrowTests(unittest.TestCase):
    def test_a_dictionary_column_reads_as_strings(self) -> None:
        table = pa.table({"symbol": pa.array(["a", "b", "a"]).dictionary_encode()})
        self.assertEqual(Frame(table).schema[0].dtype, "str")
        self.assertEqual(Frame(table)["symbol"], ["a", "b", "a"])

    def test_nulls_cross(self) -> None:
        frame = Frame(pa.table({"n": [1, None, 3]}))
        self.assertEqual(frame["n"], [1, None, 3])
        self.assertEqual(frame.to_arrow().column("n").to_pylist(), [1, None, 3])

    def test_an_empty_table_keeps_its_columns(self) -> None:
        frame = Frame(pa.table({"n": pa.array([], pa.int64())}))
        self.assertEqual((frame.height, frame.columns), (0, ["n"]))

    def test_a_python_polars_frame_crosses_both_ways(self) -> None:
        # Polars is not a dependency of this project, so the test runs only where it is installed.
        try:
            import polars
        except ImportError:
            self.skipTest("the polars package is not installed")
        original = polars.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
        frame = Frame(original)
        self.assertEqual(frame["b"], ["x", "y", "z"])
        back = polars.from_arrow(frame.to_arrow())
        self.assertEqual(back.to_dicts(), original.to_dicts())


class QueryTests(unittest.TestCase):
    def test_lazy_returns_a_query(self) -> None:
        self.assertIsInstance(trades().lazy(), Query)

    def test_group_by_aggregates(self) -> None:
        result = (
            trades()
            .lazy()
            .group_by(["symbol"], ["sum(size) AS volume", "max(price) AS high"])
            .sort("volume", descending=True)
            .collect()
        )
        self.assertEqual(result["symbol"], ["AAPL", "MSFT", "NVDA"])
        self.assertEqual(result["volume"], [600, 50, 10])
        self.assertEqual(result["high"], [191.0, 410.25, 880.0])

    def test_a_chain_of_operations(self) -> None:
        result = (
            trades()
            .lazy()
            .filter("size >= 100")
            .with_columns(["price * size AS notional"])
            .select(["symbol", "notional"])
            .sort("notional", descending=True)
            .limit(1)
            .collect()
        )
        self.assertEqual(result["notional"], [189.75 * 300])

    def test_explain_shows_the_optimized_plan(self) -> None:
        plan = trades().lazy().filter("size > 1").select(["symbol"]).explain()
        self.assertIn("SELECTION", plan.upper().replace("FILTER", "SELECTION"))

    def test_sql_on_a_frame(self) -> None:
        result = trades().sql("select symbol, count(*) as n from self group by symbol order by symbol")
        self.assertEqual(result["n"], [3, 1, 1])

    def test_join(self) -> None:
        names = Frame.from_rows([{"symbol": "AAPL", "name": "Apple"}, {"symbol": "NVDA", "name": "Nvidia"}])
        joined = trades().join(names, ["symbol"])
        self.assertEqual(len(joined), 4)
        self.assertIn("name", joined)
        self.assertEqual(len(trades().join(names, ["symbol"], how="left")), 5)

    def test_stack(self) -> None:
        self.assertEqual(len(trades().stack(trades())), 10)

    def test_a_query_runs_many_times(self) -> None:
        query = trades().lazy().filter("size > 100")
        self.assertEqual(len(query.collect()), 2)
        self.assertEqual(len(query.collect()), 2)


class SessionTests(unittest.TestCase):
    def test_sql_across_frames(self) -> None:
        with Session() as session:
            session.register("trades", trades())
            session.register("names", Frame.from_rows([{"symbol": "NVDA", "name": "Nvidia"}]))
            self.assertEqual(sorted(session.tables), ["names", "trades"])
            result = session.sql("select n.name, t.size from trades t join names n on t.symbol = n.symbol")
            self.assertEqual(result.to_rows(), [{"name": "Nvidia", "size": 10}])


class FunctionTests(unittest.TestCase):
    def test_a_python_function_maps_a_column(self) -> None:
        result = trades().map_column("size", "doubled", lambda value: value * 2)
        self.assertEqual(result["doubled"][:2], [200.0, 100.0])

    def test_an_exception_reaches_the_caller(self) -> None:
        def broken(value: float) -> float:
            raise ValueError("no values today")

        with self.assertRaises(FrameError) as caught:
            trades().map_column("size", "x", broken)
        self.assertEqual(caught.exception.code, "function")
        self.assertIn("no values today", caught.exception.details.message)


class BatchTests(unittest.TestCase):
    def test_for_over_batches(self) -> None:
        batches = pa_frame(25).batches(size=10)
        self.assertIsInstance(batches, Batches)
        sizes = [batch.num_rows for batch in batches]
        self.assertEqual(sizes, [10, 10, 5])


class ErrorTests(unittest.TestCase):
    def test_a_missing_column_is_typed(self) -> None:
        with self.assertRaises(FrameError) as caught:
            trades().lazy().select(["nope"]).collect()
        self.assertEqual(caught.exception.code, "missing_column")
        self.assertIn("nope", caught.exception.details.message)

    def test_bad_sql_is_typed(self) -> None:
        with self.assertRaises(FrameError) as caught:
            trades().sql("select from where")
        self.assertEqual(caught.exception.code, "invalid_query")

    def test_a_join_of_no_such_kind(self) -> None:
        with self.assertRaises(FrameError):
            trades().join(trades(), ["symbol"], how="sideways")


class LargeFrameTests(unittest.TestCase):
    def test_a_million_rows_group_quickly(self) -> None:
        frame = Frame(pa.table({"k": [i % 100 for i in range(1_000_000)], "v": list(range(1_000_000))}))
        result = frame.lazy().group_by(["k"], ["sum(v) AS total"]).sort("k").collect()
        self.assertEqual(len(result), 100)
        self.assertEqual(result["total"][0], sum(range(0, 1_000_000, 100)))


class AsyncTests(unittest.IsolatedAsyncioTestCase):
    async def test_collect_from_an_async_call(self) -> None:
        result = await trades().lazy().filter("size > 100").collect_async()
        self.assertEqual(len(result), 2)


if __name__ == "__main__":
    unittest.main()
