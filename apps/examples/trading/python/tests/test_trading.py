"""Trade through the generated package from asyncio tasks, threads, and a
forked child, against the engine's Tokio task."""

import asyncio
import os
import signal
import time
import traceback
import unittest
import warnings

from pydantic import ValidationError

from trading import ConfigError, Engine, EngineConfig, Listing, PanicException
from trading.core import (
    Order,
    OrderError,
    OrderRequest,
    OrderStateExpired,
    OrderStateFilled,
    Side,
    notional,
)

CASH = 100_000_000
"""The cash of each test's account, in cents: enough for every order."""


def config(*, quote_interval_ms: int = 1, seed: int | None = 7) -> EngineConfig:
    """Two listings, quoted every `quote_interval_ms`. Tests assert what holds
    for any prices, and seed them only so that a failure repeats."""
    return EngineConfig(
        cash=CASH,
        listings=[Listing(symbol="ACME", price=10_000), Listing(symbol="GLOBEX", price=2_500)],
        quote_interval_ms=quote_interval_ms,
        seed=seed,
    )


def order(side: Side = Side.Buy, quantity: int = 1, *, symbol: str = "ACME", limit: int | None = None) -> OrderRequest:
    return OrderRequest(symbol=symbol, side=side, quantity=quantity, limit=limit)


def filled_price(placed: Order) -> int:
    assert isinstance(placed.state, OrderStateFilled), placed
    return placed.state.price


class OrderTest(unittest.IsolatedAsyncioTestCase):
    async def test_a_thousand_gathered_orders_fill_once_each(self) -> None:
        async with await Engine.connect(config()) as engine:
            orders = await asyncio.gather(*(engine.submit(order()) for _ in range(1_000)))

            fills = engine.fills()
            snapshot = engine.snapshot()

        self.assertEqual(sorted(placed.id for placed in orders), list(range(1, 1_001)))
        prices = {placed.id: filled_price(placed) for placed in orders}
        self.assertEqual(fills.num_rows, 1_000)
        self.assertEqual(fills.column_names, ["order_id", "symbol", "side", "quantity", "price", "sequence"])
        rows = fills.to_pylist()
        self.assertEqual({row["order_id"]: row["price"] for row in rows}, prices)
        self.assertEqual({(row["symbol"], row["side"], row["quantity"]) for row in rows}, {("ACME", "buy", 1)})
        self.assertEqual(snapshot.positions, {"ACME": 1_000})
        self.assertEqual(snapshot.orders, 1_000)
        self.assertEqual(snapshot.cash, CASH - sum(notional(price, 1) for price in prices.values()))

    async def test_sells_and_limits(self) -> None:
        async with await Engine.connect(config()) as engine:
            await engine.submit(order(quantity=5))
            sold = await engine.submit(order(Side.Sell, 2))
            expired = await engine.submit(order(quantity=1, limit=1))

            snapshot = engine.snapshot()

        self.assertIsInstance(sold.state, OrderStateFilled)
        self.assertEqual(snapshot.positions, {"ACME": 3})
        assert isinstance(expired.state, OrderStateExpired)
        self.assertGreater(expired.state.quote, 1)
        self.assertEqual(snapshot.orders, 3)

    async def test_refused_orders_raise_typed_errors(self) -> None:
        async with await Engine.connect(config()) as engine:
            refusals = [
                (order(symbol="INITECH"), "unknown_symbol", "`INITECH` is not listed"),
                (order(quantity=CASH), "insufficient_balance", "cents are needed, but the account has 100000000"),
                (order(Side.Sell, symbol="GLOBEX"), "insufficient_position", "sells 1 of `GLOBEX`, but the account holds 0"),
            ]
            for request, code, message in refusals:
                with self.subTest(code=code), self.assertRaises(OrderError) as caught:
                    await engine.submit(request)
                self.assertEqual(caught.exception.code, code)
                self.assertIn(message, str(caught.exception))

            self.assertEqual(engine.snapshot().orders, 0, "the venue accepted none of them")

    async def test_invalid_values_fail_before_rust(self) -> None:
        with self.assertRaises(ValidationError):
            order(quantity=0)
        with self.assertRaises(ValidationError):
            EngineConfig(cash=1, listings=[], quote_interval_ms=1)
        listing = Listing(symbol="ACME", price=1)
        with self.assertRaises(ConfigError) as caught:
            await Engine.connect(EngineConfig(cash=1, listings=[listing, listing], quote_interval_ms=1))
        self.assertEqual(caught.exception.code, "duplicate_symbol")

    async def test_withdrawals_settle_and_check_the_balance(self) -> None:
        async with await Engine.connect(config()) as engine:
            self.assertEqual(await engine.withdraw(1_000), CASH - 1_000)
            with self.assertRaises(OrderError) as caught:
                await engine.withdraw(CASH)
            self.assertEqual(caught.exception.code, "insufficient_balance")
            self.assertEqual(engine.snapshot().cash, CASH - 1_000)

    async def test_quotes_stream_from_the_venue_task(self) -> None:
        # Without a seed, the operating system seeds the prices.
        async with await Engine.connect(config(seed=None)) as engine:
            opening = engine.snapshot().quotes
            quotes = await engine.quotes(20)

        self.assertEqual([quote.symbol for quote in opening], ["ACME", "GLOBEX"])
        self.assertEqual(len(quotes), 20)
        self.assertEqual({quote.symbol for quote in quotes}, {"ACME", "GLOBEX"})
        for earlier, later in zip(quotes, quotes[2:]):
            self.assertEqual(later.symbol, earlier.symbol)
            self.assertEqual(later.sequence, earlier.sequence + 1)
            self.assertLessEqual(abs(later.bid - earlier.bid), 2)
        self.assertTrue(all(quote.ask == quote.bid + 1 for quote in quotes))

    async def test_cancelling_a_call_lets_go_of_the_engine(self) -> None:
        # No quote arrives during the test, so the call waits until cancelled.
        async with await Engine.connect(config(quote_interval_ms=600_000)) as engine:
            waiting = asyncio.create_task(engine.quotes(1))
            await asyncio.sleep(0)
            waiting.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await waiting

            # A withdrawal holds the engine alone, so it runs only once the
            # cancelled call has dropped its share.
            self.assertEqual(await asyncio.wait_for(engine.withdraw(1), timeout=30), CASH - 1)

    async def test_closing_lets_running_calls_finish(self) -> None:
        engine = await Engine.connect(config())
        running = [asyncio.create_task(engine.submit(order())) for _ in range(100)]
        # One turn of the event loop starts every task's call.
        await asyncio.sleep(0)
        engine.close()

        orders = await asyncio.gather(*running)

        self.assertEqual(len({placed.id for placed in orders}), 100)
        with self.assertRaisesRegex(RuntimeError, r"Engine\.snapshot\(\): the resource is closed"):
            engine.snapshot()
        with self.assertRaisesRegex(RuntimeError, r"Engine\.submit\(\): the resource is closed"):
            await engine.submit(order())


class ThreadTest(unittest.IsolatedAsyncioTestCase):
    async def test_threads_read_the_account_while_orders_fill(self) -> None:
        async with await Engine.connect(config()) as engine:

            def read() -> list[tuple[int, int, int]]:
                seen: list[tuple[int, int, int]] = []
                for _ in range(200):
                    snapshot = engine.snapshot()
                    seen.append((snapshot.orders, snapshot.cash, engine.fills().num_rows))
                return seen

            async def buy_all() -> list[Order]:
                return await asyncio.gather(*(engine.submit(order()) for _ in range(500)))

            readers = asyncio.gather(*(asyncio.to_thread(read) for _ in range(8)))
            bought, seen = await asyncio.gather(buy_all(), readers)

            self.assertEqual(len(bought), 500)
            for views in seen:
                orders = [view[0] for view in views]
                cash = [view[1] for view in views]
                rows = [view[2] for view in views]
                self.assertEqual(orders, sorted(orders), "orders only ever count up")
                self.assertEqual(cash, sorted(cash, reverse=True), "buys only ever spend")
                self.assertEqual(rows, sorted(rows), "fills only ever grow")
            self.assertEqual(engine.snapshot().orders, 500)


class PanicTest(unittest.IsolatedAsyncioTestCase):
    async def test_a_panic_raises_and_the_extension_stays_usable(self) -> None:
        self.assertEqual(notional(250, 4), 1_000)
        with self.assertRaises(PanicException) as caught:
            notional(2**64 - 1, 2)
        self.assertIn("2 units at 18446744073709551615 cents overflow a u64", str(caught.exception))

        self.assertEqual(notional(250, 4), 1_000)
        async with await Engine.connect(config()) as engine:
            self.assertIsInstance((await engine.submit(order())).state, OrderStateFilled)


async def trade_once() -> Order:
    async with await Engine.connect(config()) as engine:
        return await engine.submit(order())


@unittest.skipUnless(hasattr(os, "fork"), "the platform cannot fork")
class ForkTest(unittest.TestCase):
    def test_a_forked_child_trades_on_a_runtime_of_its_own(self) -> None:
        # The parent's runtime is running when it forks.
        self.assertIsInstance(asyncio.run(trade_once()).state, OrderStateFilled)
        with warnings.catch_warnings():
            # Python 3.12 and later warn about forking a process with threads.
            warnings.simplefilter("ignore", DeprecationWarning)
            child = os.fork()
        if child == 0:
            try:
                status = 0 if isinstance(asyncio.run(trade_once()).state, OrderStateFilled) else 1
            except BaseException:
                traceback.print_exc()
                status = 2
            os._exit(status)

        deadline = time.monotonic() + 60
        while (finished := os.waitpid(child, os.WNOHANG))[0] == 0:
            if time.monotonic() > deadline:
                os.kill(child, signal.SIGKILL)
                os.waitpid(child, 0)
                self.fail("the forked child did not finish within a minute")
            time.sleep(0.01)
        self.assertEqual(os.waitstatus_to_exitcode(finished[1]), 0)


if __name__ == "__main__":
    unittest.main()
