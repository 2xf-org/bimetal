"""DataFusion through the generated Python package."""

import unittest

import pyarrow as pa

from query import Batches, Frame, QueryError, Session

TRADES = pa.record_batch(
    {
        "symbol": ["AAPL", "MSFT", "AAPL", "NVDA", "AAPL"],
        "price": [190.5, 410.25, 191.0, 880.0, 189.75],
        "size": [100, 50, 200, 10, 300],
    }
)


def session() -> Session:
    result = Session()
    result.add_batch("trades", TRADES)
    return result


class QueryTests(unittest.IsolatedAsyncioTestCase):
    async def test_query_returns_a_pyarrow_table(self) -> None:
        table = await session().query(
            "select symbol, sum(size) as volume from trades group by symbol order by symbol"
        )
        self.assertIsInstance(table, pa.Table)
        self.assertEqual(table.column("symbol").to_pylist(), ["AAPL", "MSFT", "NVDA"])
        self.assertEqual(table.column("volume").to_pylist(), [600, 50, 10])

    async def test_sql_returns_a_frame_that_refines(self) -> None:
        frame = await session().sql("select * from trades")
        self.assertIsInstance(frame, Frame)
        narrowed = frame.filter("symbol = 'AAPL'").select(["price", "size * 2 as doubled"]).limit(2, skip=1)
        self.assertEqual(await narrowed.count(), 2)
        table = await narrowed.collect()
        self.assertEqual(table.column_names, ["price", "doubled"])
        self.assertEqual(table.column("doubled").to_pylist(), [400, 600])

    async def test_schema_is_a_property(self) -> None:
        frame = await session().sql("select symbol, price from trades")
        self.assertIsInstance(frame.schema, pa.Schema)
        self.assertEqual(frame.schema.names, ["symbol", "price"])

    async def test_async_for_over_batches(self) -> None:
        frame = await session().sql("select * from trades")
        batches = await frame.stream()
        self.assertIsInstance(batches, Batches)
        rows = 0
        async for batch in batches:
            self.assertIsInstance(batch, pa.RecordBatch)
            rows += batch.num_rows
        self.assertEqual(rows, TRADES.num_rows)

    async def test_async_with_closes_the_session(self) -> None:
        async with session() as open_session:
            self.assertEqual((await open_session.query("select 1 as one")).num_rows, 1)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            await open_session.query("select 1")

    async def test_a_bad_query_is_typed(self) -> None:
        with self.assertRaises(QueryError) as caught:
            await session().query("select nope from trades")
        self.assertEqual(caught.exception.code, "plan")
        self.assertIn("nope", caught.exception.details.message)

    async def test_a_frame_can_register_as_a_table(self) -> None:
        open_session = session()
        frame = await open_session.sql("select * from trades where size > 100")
        await open_session.add_frame("big", frame)
        self.assertEqual((await open_session.query("select count(*) as n from big")).column("n").to_pylist(), [2])

    async def test_a_table_argument(self) -> None:
        open_session = Session()
        open_session.add_table("t", pa.table({"x": [1, 2, 3]}))
        self.assertEqual((await open_session.query("select sum(x) as s from t")).column("s").to_pylist(), [6])

    async def test_a_python_function_runs_in_sql(self) -> None:
        open_session = session()
        calls = []

        def notional(price: float, size: float) -> float:
            calls.append(price)
            return price * size

        open_session.add_function("notional", notional)
        table = await open_session.query(
            "select notional(price, cast(size as double)) as n from trades where symbol = 'NVDA'"
        )
        self.assertEqual(table.column("n").to_pylist(), [8800.0])
        self.assertEqual(calls, [880.0])

    async def test_an_exception_in_a_python_function_fails_the_query(self) -> None:
        open_session = session()

        def broken(price: float, size: float) -> float:
            raise ValueError("no quotes today")

        open_session.add_function("broken", broken)
        with self.assertRaisesRegex(QueryError, "no quotes today"):
            await open_session.query("select broken(price, price) from trades")


if __name__ == "__main__":
    unittest.main()
