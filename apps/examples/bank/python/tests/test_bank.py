"""
Move money through the generated package from asyncio tasks, threads and a forked child, against the bank's Tokio task.
"""

from __future__ import annotations

import asyncio
import os
import pathlib
import signal
import subprocess
import sys
import time
import traceback
import unittest
import warnings

import pydantic

from bank import (
    Account,
    AccountError,
    Bank,
    OpenError,
    Opening,
    PanicException,
    Receipt,
    Transfer,
    TransferError,
    interest,
)

# Ada's opening balance in each test's bank, in cents: enough for every transfer.
FUNDS = 100_000_000
PROJECT = pathlib.Path(__file__).parent.parent


def bank() -> Bank:
    """
    Open a bank where Ada holds `FUNDS` and Grace holds nothing.
    """
    return Bank([Opening(owner="Ada", balance=FUNDS), Opening(owner="Grace", balance=0)])


def pay(amount: int = 1, *, sender: str = "Ada", recipient: str = "Grace") -> Transfer:
    """
    Return a transfer of `amount` from `sender` to `recipient`.
    """
    return Transfer(sender=sender, recipient=recipient, amount=amount)


class AccountTest(unittest.TestCase):
    def test_the_programs_print_what_their_comments_say(self) -> None:
        for program, printed in [
            ("quickstart.py", "70\ninsufficient_funds\n"),
            ("transfers.py", "1 {'Ada': 70, 'Grace': 30}\ninsufficient_funds\n"),
        ]:
            with self.subTest(program=program):
                path = PROJECT / program
                result = subprocess.run([sys.executable, path], capture_output=True, text=True, check=True)
                self.assertEqual(result.stdout, printed)

    def test_withdrawing_more_than_the_balance_changes_nothing(self) -> None:
        with Account() as account:
            account.deposit(5)
            with self.assertRaises(AccountError) as caught:
                account.withdraw(6)
            self.assertEqual(caught.exception.code, "insufficient_funds")
            self.assertEqual(account.balance(), 5)


class TransferTest(unittest.IsolatedAsyncioTestCase):
    async def test_a_thousand_gathered_transfers_settle_once_each(self) -> None:
        with bank() as opened:
            receipts = await asyncio.gather(*(opened.transfer(pay()) for _ in range(1_000)))
            ledger = opened.ledger()
            statement = opened.statement()

        self.assertEqual(sorted(receipt.id for receipt in receipts), list(range(1, 1_001)))
        self.assertEqual(ledger.num_rows, 1_000)
        self.assertEqual(ledger.column_names, ["id", "sender", "recipient", "amount"])
        rows = {(row["sender"], row["recipient"], row["amount"]) for row in ledger.to_pylist()}
        self.assertEqual(rows, {("Ada", "Grace", 1)})
        self.assertEqual(statement.balances, {"Ada": FUNDS - 1_000, "Grace": 1_000})
        self.assertEqual(statement.transfers, 1_000)

    async def test_refused_transfers_raise_typed_errors(self) -> None:
        with bank() as opened:
            refusals = [
                (pay(recipient="Linus"), "unknown_account", "`Linus` has no account"),
                (pay(FUNDS + 1), "insufficient_funds", "100000001 cents are needed, but the account has 100000000"),
                (
                    pay(sender="Grace", recipient="Ada"),
                    "insufficient_funds",
                    "1 cents are needed, but the account has 0",
                ),
            ]
            for transfer, code, message in refusals:
                with self.subTest(code=code), self.assertRaises(TransferError) as caught:
                    await opened.transfer(transfer)
                self.assertEqual(caught.exception.code, code)
                self.assertEqual(str(caught.exception), message)

            self.assertEqual(opened.statement().transfers, 0, "none of them settled")

    def test_invalid_values_fail_before_rust(self) -> None:
        with self.assertRaises(pydantic.ValidationError):
            pay(0)
        with self.assertRaises(pydantic.ValidationError):
            Opening(owner="", balance=1)
        with self.assertRaises(OpenError) as caught:
            Bank([Opening(owner="Ada", balance=1), Opening(owner="Ada", balance=2)])
        self.assertEqual(caught.exception.code, "duplicate_owner")

    async def test_withdrawals_settle_and_check_the_balance(self) -> None:
        with bank() as opened:
            self.assertEqual(await opened.withdraw("Ada", 1_000), FUNDS - 1_000)
            with self.assertRaises(TransferError) as caught:
                await opened.withdraw("Ada", FUNDS)
            self.assertEqual(caught.exception.code, "insufficient_funds")
            self.assertEqual(opened.statement().balances["Ada"], FUNDS - 1_000)

    async def test_receipts_stream_from_the_settlement_task(self) -> None:
        with bank() as opened:
            waiting = asyncio.create_task(opened.receipts(10))
            # One turn of the event loop starts the call, which then waits.
            await asyncio.sleep(0)
            sent = [await opened.transfer(pay(amount)) for amount in range(1, 11)]
            received = await waiting

        self.assertEqual(received, sent)
        self.assertIsInstance(received[0], Receipt)
        self.assertEqual([receipt.transfer.amount for receipt in received], list(range(1, 11)))

    async def test_cancelling_a_call_lets_go_of_the_bank(self) -> None:
        with bank() as opened:
            # No transfer settles during the test, so the call waits until cancelled.
            waiting = asyncio.create_task(opened.receipts(1))
            await asyncio.sleep(0)
            waiting.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await waiting

            # A withdrawal holds the bank alone, so it runs only once the
            # cancelled call has dropped its share.
            self.assertEqual(await asyncio.wait_for(opened.withdraw("Ada", 1), timeout=30), FUNDS - 1)

    async def test_closing_lets_running_calls_finish(self) -> None:
        opened = bank()
        running = [asyncio.create_task(opened.transfer(pay())) for _ in range(100)]
        # One turn of the event loop starts every task's call.
        await asyncio.sleep(0)
        opened.close()

        receipts = await asyncio.gather(*running)

        self.assertEqual(len({receipt.id for receipt in receipts}), 100)
        with self.assertRaisesRegex(RuntimeError, r"Bank\.statement\(\): the resource is closed"):
            opened.statement()
        with self.assertRaisesRegex(RuntimeError, r"Bank\.transfer\(\): the resource is closed"):
            await opened.transfer(pay())


class ThreadTest(unittest.IsolatedAsyncioTestCase):
    async def test_threads_read_the_bank_while_transfers_settle(self) -> None:
        with bank() as opened:

            def read() -> list[tuple[int, int, int]]:
                seen: list[tuple[int, int, int]] = []
                for _ in range(200):
                    statement = opened.statement()
                    seen.append((statement.transfers, statement.balances["Ada"], opened.ledger().num_rows))
                return seen

            async def pay_all() -> list[Receipt]:
                return await asyncio.gather(*(opened.transfer(pay()) for _ in range(500)))

            readers = asyncio.gather(*(asyncio.to_thread(read) for _ in range(8)))
            paid, seen = await asyncio.gather(pay_all(), readers)

            self.assertEqual(len(paid), 500)
            for views in seen:
                transfers = [view[0] for view in views]
                funds = [view[1] for view in views]
                rows = [view[2] for view in views]
                self.assertEqual(transfers, sorted(transfers), "transfers only ever count up")
                self.assertEqual(funds, sorted(funds, reverse=True), "Ada only ever pays")
                self.assertEqual(rows, sorted(rows), "the ledger only ever grows")
            self.assertEqual(opened.statement().transfers, 500)


class PanicTest(unittest.IsolatedAsyncioTestCase):
    async def test_a_panic_raises_and_the_extension_stays_usable(self) -> None:
        self.assertEqual(interest(250, 4), 10)
        with self.assertRaises(PanicException) as caught:
            interest(2**64 - 1, 2)
        self.assertIn("18446744073709551615 cents at 2% overflow a u64", str(caught.exception))

        self.assertEqual(interest(250, 4), 10)
        with bank() as opened:
            self.assertEqual((await opened.transfer(pay())).id, 1)


async def pay_once() -> Receipt:
    """
    Settle one transfer in a new bank, and close the bank.
    """
    with bank() as opened:
        return await opened.transfer(pay())


@unittest.skipUnless(hasattr(os, "fork"), "the platform cannot fork")
class ForkTest(unittest.TestCase):
    def test_a_forked_child_settles_on_a_runtime_of_its_own(self) -> None:
        # The parent's runtime is running when it forks.
        self.assertEqual(asyncio.run(pay_once()).id, 1)
        with warnings.catch_warnings():
            # Python 3.12 and later warn about forking a process with threads.
            warnings.simplefilter("ignore", DeprecationWarning)
            child = os.fork()
        if child == 0:
            try:
                status = 0 if asyncio.run(pay_once()).id == 1 else 1
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
