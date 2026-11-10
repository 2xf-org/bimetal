"""Resources that make and take resources, and the Python features they implement."""

from __future__ import annotations

import asyncio
import threading
import unittest
from collections.abc import Callable

from showcase.shelf import (
    AsyncSession,
    Labeler,
    Session,
    Shelf,
    ShelfError,
    ShelfErrorFull,
    Shortage,
    ShortageDetails,
    TitleStream,
    Titles,
    count_both,
    double_size,
    offset,
)


def shelf(*titles: str, limit: int = 3) -> Shelf:
    result = Shelf("fiction", limit)
    for title in titles:
        result.add(title)
    return result


def finishing(call: Callable[[], object]) -> object:
    """Run `call` on another thread, so that a deadlock fails a test instead of hanging it."""
    outcome: list[object] = []
    failure: list[BaseException] = []

    def run() -> None:
        try:
            outcome.append(call())
        except BaseException as error:
            failure.append(error)

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    thread.join(timeout=10)
    if thread.is_alive():
        raise AssertionError("the call waited for itself")
    if failure:
        raise failure[0]
    return outcome[0]


class ProtocolTest(unittest.TestCase):
    def test_defaults_and_keywords(self) -> None:
        self.assertEqual(Shelf().name, "shelf")
        self.assertEqual(Shelf("a").name, "a")
        with self.assertRaises(ShelfError):
            full = Shelf(limit=0)
            full.add("x")
        self.assertEqual(offset(3), 1.0)
        self.assertEqual(offset(3, by=1), 2.0)
        self.assertEqual(offset(3, 1, scale=2.0), 8.0)

    def test_pointer_sized_integers(self) -> None:
        self.assertEqual(double_size(21), 42)
        with self.assertRaises(ValueError):
            double_size(-1)

    def test_getter_and_setter(self) -> None:
        value = shelf()
        self.assertEqual(value.name, "fiction")
        value.name = "poetry"
        self.assertEqual(value.name, "poetry")

    def test_len_and_bool(self) -> None:
        self.assertEqual(len(shelf()), 0)
        self.assertEqual(len(shelf("a", "b")), 2)
        self.assertFalse(shelf())
        self.assertTrue(shelf("a"))

    def test_repr_and_str(self) -> None:
        self.assertEqual(repr(shelf("a")), 'Shelf("fiction", 1 books)')
        self.assertEqual(str(shelf()), "the fiction shelf")

    def test_contains(self) -> None:
        self.assertIn("a", shelf("a"))
        self.assertNotIn("b", shelf("a"))

    def test_items(self) -> None:
        value = shelf("a", "b")
        self.assertEqual(value[1], "b")
        with self.assertRaises(KeyError):
            value[9]
        value[0] = "c"
        self.assertEqual(value[0], "c")
        del value[0]
        self.assertEqual(value[0], "b")

    def test_call(self) -> None:
        self.assertEqual(shelf("apple", "pear", "plum")("p"), ["apple", "pear", "plum"])
        self.assertEqual(shelf("apple", "plum")("um"), ["plum"])

    def test_eq_and_hash(self) -> None:
        self.assertEqual(shelf("a"), shelf("a"))
        self.assertNotEqual(shelf("a"), shelf("b"))
        self.assertNotEqual(shelf("a"), "a")
        self.assertEqual(hash(shelf("abc")), hash(shelf("xyz")))

    def test_optional_bytes(self) -> None:
        self.assertEqual(shelf("a").first_bytes(), b"a")
        self.assertIsNone(shelf().first_bytes())

    def test_closed_says_whether_it_closed(self) -> None:
        value = shelf()
        self.assertFalse(value.closed)
        value.close()
        self.assertTrue(value.closed)


class ReturnedResourceTest(unittest.TestCase):
    def test_a_method_returns_a_resource(self) -> None:
        self.assertIsInstance(shelf().session(), Session)

    def test_a_returned_resource_has_no_constructor(self) -> None:
        with self.assertRaisesRegex(TypeError, "other exports return it"):
            Session()

    def test_an_option_of_a_resource(self) -> None:
        self.assertIsInstance(shelf().try_session(), Session)
        self.assertIsNone(shelf("a", limit=1).try_session())

    def test_a_list_of_resources(self) -> None:
        parts = shelf("a", "b", "c").split(2)
        self.assertEqual([len(part) for part in parts], [2, 1])
        self.assertTrue(all(isinstance(part, Shelf) for part in parts))


class ResourceArgumentTest(unittest.TestCase):
    def test_a_shared_borrow_reads_and_leaves_it_open(self) -> None:
        target, source = shelf(), shelf("a", "b")
        target.copy_from(source)
        self.assertEqual(len(target), 2)
        self.assertEqual(len(source), 2)

    def test_an_exclusive_borrow(self) -> None:
        source, target = shelf("a"), shelf("b")
        source.give_to(target)
        self.assertEqual(len(source), 0)
        self.assertEqual(len(target), 2)

    def test_taking_a_resource_closes_it(self) -> None:
        target, source = shelf("a"), shelf("b")
        target.absorb(source)
        self.assertEqual(len(target), 2)
        self.assertTrue(source.closed)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            target.absorb(source)

    def test_an_optional_resource(self) -> None:
        self.assertEqual(shelf("a").count_with(None), 1)
        self.assertEqual(shelf("a").count_with(shelf("b", "c")), 3)

    def test_another_class_is_a_type_error(self) -> None:
        with self.assertRaisesRegex(TypeError, "must be a Shelf, not str"):
            shelf().copy_from("a")  # type: ignore[arg-type]

    def test_one_resource_cannot_be_passed_twice(self) -> None:
        value = shelf("a")
        for call in (
            lambda: value.copy_from(value),
            lambda: value.give_to(value),
            lambda: value.count_with(value),
            lambda: count_both(value, value),
        ):
            with self.assertRaisesRegex(RuntimeError, "passed twice"):
                call()
        self.assertEqual(count_both(value, shelf("b", "c")), 3)

    def test_a_resource_equals_itself(self) -> None:
        value = shelf("a")
        self.assertEqual(value, value)


class ConsumingTest(unittest.TestCase):
    def test_a_self_method_closes_the_resource(self) -> None:
        value = shelf("a", "b")
        self.assertEqual(value.seal(), ["a", "b"])
        self.assertTrue(value.closed)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            value.seal()


class WithTest(unittest.TestCase):
    def test_a_finished_block_commits_and_closes(self) -> None:
        value = shelf()
        with value.session() as session:
            session.add("a")
            self.assertEqual(len(value), 0)
        self.assertEqual(len(value), 1)
        self.assertTrue(session.closed)

    def test_a_raising_block_rolls_back_and_the_exception_propagates(self) -> None:
        value = shelf()
        with self.assertRaisesRegex(ValueError, "boom"):
            with value.session() as session:
                session.add("a")
                raise ValueError("boom")
        self.assertEqual(len(value), 0)
        self.assertTrue(session.closed)

    def test_a_closed_resource_skips_exit(self) -> None:
        value = shelf()
        with value.session() as session:
            session.add("a")
            session.close()
        self.assertEqual(len(value), 0)

    def test_an_error_from_exit_raises_with_its_type_and_still_closes(self) -> None:
        value = shelf(limit=0)
        with self.assertRaises(ShelfError) as caught:
            with value.session() as session:
                session.add("a")
        self.assertEqual(caught.exception.code, "full")
        self.assertTrue(session.closed)

    def test_a_plain_with_refuses_async_hooks(self) -> None:
        async def main() -> None:
            session = await shelf().async_session()
            with self.assertRaisesRegex(TypeError, "needs `async with`"):
                with session:
                    pass

        asyncio.run(main())

    def test_a_resource_without_hooks_still_closes(self) -> None:
        with shelf() as value:
            self.assertFalse(value.closed)
        self.assertTrue(value.closed)


class IterationTest(unittest.TestCase):
    def test_for_walks_to_the_end(self) -> None:
        titles = shelf("a", "b").titles()
        self.assertIsInstance(titles, Titles)
        self.assertEqual(list(titles), ["a", "b"])
        self.assertEqual(list(titles), [])

    def test_next_stops(self) -> None:
        titles = iter(shelf("a").titles())
        self.assertEqual(next(titles), "a")
        with self.assertRaises(StopIteration):
            next(titles)


class ErrorDetailsTest(unittest.TestCase):
    def test_details_are_a_typed_model(self) -> None:
        with self.assertRaises(ShelfError) as caught:
            shelf("a", limit=1).add("b")
        self.assertEqual(caught.exception.code, "full")
        self.assertIsInstance(caught.exception.details, ShelfErrorFull)
        details = caught.exception.details
        assert isinstance(details, ShelfErrorFull)
        self.assertEqual(details.limit, 1)
        self.assertEqual(details.title, "b")

    def test_a_code_without_details_has_none(self) -> None:
        with self.assertRaises(ShelfError) as caught:
            shelf().add("")
        self.assertEqual(caught.exception.code, "untitled")
        self.assertIsNone(caught.exception.details)


    def test_an_error_struct_declares_details_too(self) -> None:
        shelf("a").require(1)
        with self.assertRaises(Shortage) as caught:
            shelf("a").require(2)
        self.assertEqual(caught.exception.code, "shortage")
        self.assertEqual(caught.exception.details, ShortageDetails(held=1, wanted=2))


class CallbackTest(unittest.TestCase):
    def test_a_function_runs_and_can_stop(self) -> None:
        seen: list[tuple[str, int]] = []

        def visitor(title: str, index: int) -> bool:
            seen.append((title, index))
            return index < 1

        self.assertEqual(shelf("a", "b", "c").visit(visitor), 2)
        self.assertEqual(seen, [("a", 0), ("b", 1)])

    def test_a_function_can_read_the_resource_it_was_called_from(self) -> None:
        value = shelf("a")
        self.assertEqual(finishing(lambda: value.visit(lambda title, index: len(value) == 1)), 1)

    def test_a_function_cannot_change_the_resource_it_was_called_from(self) -> None:
        value = shelf("a")

        def visitor(title: str, index: int) -> bool:
            value.add("b")
            return True

        with self.assertRaises(ShelfError) as caught:
            finishing(lambda: value.visit(visitor))
        self.assertEqual(caught.exception.code, "visitor")
        self.assertIn("Shelf.add(): the resource is busy", str(caught.exception))
        self.assertEqual(len(value), 1)
        value.add("b")
        self.assertEqual(len(value), 2)

    def test_a_function_cannot_take_the_resource_it_was_called_from_as_an_argument(self) -> None:
        value = shelf("a")
        other = shelf()

        def visitor(title: str, index: int) -> bool:
            other.give_to(value)
            return True

        with self.assertRaises(ShelfError) as caught:
            finishing(lambda: value.visit(visitor))
        self.assertIn("Shelf.give_to(): the resource is busy", str(caught.exception))

    def test_a_function_cannot_read_a_resource_that_its_call_holds_alone(self) -> None:
        value = shelf("a")

        def transform(title: str) -> str:
            return f"{title}:{len(value)}"

        with self.assertRaises(ShelfError) as caught:
            finishing(lambda: value.rewrite(transform))
        self.assertIn("Shelf.size(): the resource is busy", str(caught.exception))
        self.assertEqual(value.at(0), "a")

    def test_a_function_can_use_another_resource(self) -> None:
        value = shelf("a")
        other = shelf()

        def transform(title: str) -> str:
            other.add(title)
            return title.upper()

        finishing(lambda: value.rewrite(transform))
        self.assertEqual(value.at(0), "A")
        self.assertEqual(other.at(0), "a")

    def test_a_call_after_a_function_can_wait_again(self) -> None:
        value = shelf("a")
        value.visit(lambda title, index: True)
        started = threading.Event()
        release = threading.Event()

        def holder(title: str) -> str:
            started.set()
            release.wait()
            return title

        writer = threading.Thread(target=lambda: value.rewrite(holder))
        writer.start()
        started.wait()
        reader: list[int] = []
        waiting = threading.Thread(target=lambda: reader.append(len(value)))
        waiting.start()
        waiting.join(timeout=0.2)
        self.assertTrue(waiting.is_alive(), "outside a function, a call waits for the lock")
        release.set()
        writer.join()
        waiting.join()
        self.assertEqual(reader, [1])
    def test_an_exception_reaches_the_caller_as_text(self) -> None:
        def visitor(title: str, index: int) -> bool:
            raise KeyError("no such book")

        with self.assertRaises(ShelfError) as caught:
            shelf("a").visit(visitor)
        self.assertIn("no such book", str(caught.exception))

    def test_a_wrong_result_is_an_error(self) -> None:
        def visitor(title: str, index: int) -> bool:
            return {"not": "a bool"}  # type: ignore[return-value]

        with self.assertRaises(ShelfError):
            shelf("a").visit(visitor)

    def test_a_value_that_is_not_callable_is_a_type_error(self) -> None:
        with self.assertRaisesRegex(TypeError, "callable"):
            shelf().visit("not a function")  # type: ignore[arg-type]

    def test_rust_keeps_a_function_after_the_call(self) -> None:
        labeler = shelf().labeler(lambda name: name.upper())
        self.assertIsInstance(labeler, Labeler)
        self.assertEqual(labeler.label("ada"), "ADA")


class AsyncResourceTest(unittest.IsolatedAsyncioTestCase):
    async def test_an_async_method_returns_a_resource(self) -> None:
        self.assertIsInstance(await shelf().async_session(), AsyncSession)

    async def test_async_with_runs_async_hooks(self) -> None:
        value = shelf("a")
        session = await value.async_session()
        async with session as held:
            self.assertEqual(held, 1)
            self.assertTrue(session.opened)
            session.add("b")
        self.assertEqual(len(value), 2)
        self.assertTrue(session.closed)

    async def test_a_raising_async_with_rolls_back(self) -> None:
        value = shelf()
        session = await value.async_session()
        with self.assertRaisesRegex(ValueError, "boom"):
            async with session:
                session.add("a")
                raise ValueError("boom")
        self.assertEqual(len(value), 0)

    async def test_async_with_runs_sync_hooks(self) -> None:
        value = shelf()
        async with value.session() as session:
            session.add("a")
        self.assertEqual(len(value), 1)

    async def test_async_for_walks_to_the_end(self) -> None:
        stream = shelf("a", "b").title_stream()
        self.assertIsInstance(stream, TitleStream)
        self.assertEqual([title async for title in stream], ["a", "b"])

    async def test_an_async_call_borrows_a_resource(self) -> None:
        target, source = shelf(), shelf("a")
        await target.copy_later(source)
        self.assertEqual(len(target), 1)

    async def test_an_async_self_method_closes_the_resource(self) -> None:
        value = shelf("a", "b")
        self.assertEqual(await value.seal_later(), 2)
        self.assertTrue(value.closed)

    async def test_a_function_runs_from_an_async_call(self) -> None:
        self.assertEqual(await shelf().label_later(lambda name: name + "!"), "fiction!")

    async def test_a_function_from_an_async_call_can_call_sync_methods(self) -> None:
        other = shelf("a", "b")
        label = await shelf().label_later(lambda name: f"{name}:{len(other)}")
        self.assertEqual(label, "fiction:2")

    async def test_a_function_from_an_async_call_cannot_change_its_own_resource(self) -> None:
        value = shelf()

        def label(name: str) -> str:
            value.add("b")
            return name

        with self.assertRaises(ShelfError) as caught:
            await asyncio.wait_for(value.label_later(label), timeout=10)
        self.assertEqual(caught.exception.code, "visitor")
        self.assertIn("Shelf.add(): the resource is busy", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
