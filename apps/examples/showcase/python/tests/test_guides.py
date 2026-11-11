"""The Python samples of docs/guides, so that they run."""

from __future__ import annotations

import unittest

import numpy as np
import pyarrow as pa

from showcase import (
    Ledger,
    Mailbox,
    ParseError,
    Priority,
    Retry,
    Settings,
    StrokeLift,
    StrokeMoveTo,
    count_words,
    echo_settings,
    parse_priority,
    retrace,
    reverse,
    running_total,
)
from showcase.columns import split
from showcase.geometry import Point
from showcase.shelf import Shelf, ShelfError, ShelfErrorFull, offset
from showcase.text import phrase


class FunctionsTest(unittest.TestCase):
    def test_functions(self) -> None:
        self.assertEqual(phrase("one two").word_count, 2)

    def test_default_arguments(self) -> None:
        self.assertEqual(offset(4), 1.5)
        self.assertEqual(offset(4, scale=1.0), 3.0)


class ModelsTest(unittest.TestCase):
    def test_structs(self) -> None:
        self.assertFalse(echo_settings(Settings(version="v1")).verbose)

    def test_defaults(self) -> None:
        self.assertEqual(Settings(version="v1").retry, Retry(attempts=3, factor=2.0))

    def test_enums(self) -> None:
        self.assertEqual(Priority.High, "high")

    def test_external_enums(self) -> None:
        start = StrokeMoveTo(value=Point(x=1.0, y=2.0))
        self.assertEqual(retrace([StrokeLift(), start]), [start, StrokeLift()])


class ErrorsTest(unittest.TestCase):
    def test_a_code(self) -> None:
        with self.assertRaises(ParseError) as caught:
            parse_priority("")
        self.assertEqual(caught.exception.code, "empty")

    def test_details(self) -> None:
        full = Shelf("fiction", 1)
        full.add("a")
        with self.assertRaises(ShelfError) as caught:
            full.add("c")
        self.assertEqual(caught.exception.code, "full")
        self.assertEqual(caught.exception.details, ShelfErrorFull(limit=1, title="c"))


def books(*titles: str) -> Shelf:
    value = Shelf("fiction", 3)
    for title in titles:
        value.add(title)
    return value


class ResourcesTest(unittest.TestCase):
    def test_a_class(self) -> None:
        with Ledger("Ada") as ledger:
            self.assertEqual(ledger.deposit(5), 5)

    def test_a_factory(self) -> None:
        self.assertEqual(Ledger.with_balance("Ada", 5).balance(), 5)

    def test_consuming(self) -> None:
        value = books("a", "b")
        self.assertEqual(value.seal(), ["a", "b"])


class ProtocolsTest(unittest.TestCase):
    def test_properties(self) -> None:
        value = books()
        value.name = "history"
        self.assertEqual(value.name, "history")
        self.assertEqual(len(value), 0)

    def test_with(self) -> None:
        value = books()
        with value.session() as session:
            session.add("a")
        self.assertEqual(len(value), 1)

    def test_iteration(self) -> None:
        value = books("a", "b")
        self.assertEqual(list(value.titles()), ["a", "b"])


class AsyncTest(unittest.IsolatedAsyncioTestCase):
    async def test_a_function(self) -> None:
        self.assertEqual(await count_words("one two three"), 3)

    async def test_a_resource(self) -> None:
        async with await Mailbox.open("Ada") as mailbox:
            self.assertEqual(await mailbox.deliver("hello"), 1)

    async def test_iteration(self) -> None:
        value = books("a", "b")
        self.assertEqual([title async for title in value.title_stream()], ["a", "b"])


class CallbacksTest(unittest.TestCase):
    def test_a_function(self) -> None:
        value = books("a", "b")
        self.assertEqual(value.visit(lambda title, index: index < 0), 1)


class ValuesTest(unittest.TestCase):
    def test_bytes(self) -> None:
        self.assertEqual(reverse(b"abc"), b"cba")

    def test_buffers(self) -> None:
        self.assertEqual(running_total(np.array([1, 2, 3], dtype=np.int64)).tolist(), [1, 3, 6])

    def test_arrow(self) -> None:
        batch = pa.record_batch({"price": [1.5, 2.5]})
        self.assertEqual(split(batch, 1).num_rows, 2)


if __name__ == "__main__":
    unittest.main()
