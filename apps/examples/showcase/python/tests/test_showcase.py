"""Call every export of the showcase contract through the compiled extension."""

import asyncio
import math
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
import warnings
from datetime import UTC, date, datetime, timedelta, timezone
from pathlib import Path

import numpy as np
import pyarrow as pa
from pydantic import ValidationError

import showcase
from showcase.columns import (
    ColumnError,
    add_totals,
    batches,
    count_rows,
    split,
    sum_column,
    with_note,
)
from showcase.columns import totals as stream_totals
from showcase.columns import describe as describe_schema
from showcase.columns import scale as scale_column
from showcase import (
    CONTRACT_VERSION,
    DEFAULT_LABEL,
    ArrowArrayExportable,
    ArrowSchemaExportable,
    ArrowStreamExportable,
    FIRST_STROKE,
    NO_STROKES,
    PAUSE,
    EventCreated,
    EventDeleted,
    EventMoved,
    Entry,
    Holiday,
    Ledger,
    LockedError,
    Mailbox,
    Numbers,
    PanicException,
    ParseError,
    Priority,
    Record,
    Retry,
    Settings,
    Statement,
    StrokeAgain,
    StrokeLift,
    StrokeLineTo,
    StrokeMoveTo,
    StrokePause,
    bounds,
    corners,
    count_tags,
    count_words,
    count_words_at_once,
    describe,
    distance_from,
    echo_numbers,
    echo_record,
    echo_settings,
    find_item,
    finish,
    longer,
    next_day,
    observe,
    fingerprint,
    item_at,
    parse_priority,
    ping,
    qualify,
    repeat,
    retrace,
    reverse,
    route_length,
    running_total,
    scale,
    theme_of,
    total,
    totals,
    touch,
    wrap,
)
from showcase.geometry import GeometryError, Point, direction, distance
from showcase.text import Phrase, Words, phrase
from showcase.text import list as list_words


def record(**changes: object) -> Record:
    values: dict[str, object] = {
        "id": "r-1",
        "label": "First",
        "priority": Priority.High,
        "score": 90,
        "stars": 4,
        "note": None,
        "tags": ["a", "b"],
        "counters": {"views": 3},
        "ranges": {"week": (1, 7)},
        "created_at": datetime(2026, 7, 22, 12, 0, tzinfo=UTC),
        "local_time": datetime(2026, 7, 22, 9, 0, tzinfo=timezone(timedelta(hours=-3))),
        "metadata": {"nested": [1, "two", None], "wide": 2**63},
        "checksum": [0, 255],
        "location": Point(x=1.5, y=-2.0),
    }
    values.update(changes)
    return Record.model_validate(values)


class ModelTest(unittest.TestCase):
    def test_numbers_keep_every_width(self) -> None:
        numbers = Numbers(
            flag=True,
            tiny=255,
            small=-32768,
            medium=4_294_967_295,
            signed=-2_147_483_648,
            large=2**64 - 1,
            signed_large=-(2**63),
            single=0.5,
            double=math.pi,
        )

        self.assertEqual(echo_numbers(numbers), numbers)

    def test_models_accept_python_names_and_wire_names(self) -> None:
        numbers = Numbers(
            flag=False,
            tiny=1,
            small=2,
            medium=3,
            signed=4,
            large=5,
            signed_large=-6,
            single=0.5,
            double=0.25,
        )
        wire = numbers.model_dump(by_alias=True)

        self.assertEqual(wire["signedLarge"], -6)
        self.assertNotIn("signed_large", wire)
        self.assertEqual(numbers.model_dump()["signed_large"], -6)
        self.assertEqual(Numbers(**wire), numbers)
        self.assertEqual(Numbers.model_validate(wire), numbers)
        self.assertEqual(Numbers.model_validate(numbers.model_dump()), numbers)

    def test_dates_need_a_utc_offset(self) -> None:
        with self.assertRaises(ValidationError):
            record(created_at=datetime(2026, 7, 22, 12, 0))

    def test_records_round_trip_every_field_type(self) -> None:
        original = record()

        echoed = echo_record(original)

        self.assertEqual(echoed, original)
        self.assertEqual(echoed.local_time.utcoffset(), timedelta(hours=-3))
        self.assertEqual(echoed.ranges["week"], (1, 7))
        self.assertIsInstance(echoed.location, Point)

    def test_models_are_frozen(self) -> None:
        with self.assertRaises(ValidationError):
            record().label = "changed"  # type: ignore[misc]

    def test_field_rules_reject_invalid_values(self) -> None:
        invalid: list[dict[str, object]] = [
            {"label": ""},
            {"label": "x" * 41},
            {"stars": 0},
            {"stars": 6},
            {"tags": ["t"] * 6},
            {"priority": "urgent"},
        ]
        for changes in invalid:
            with self.subTest(changes=changes), self.assertRaises(ValidationError):
                record(**changes)

    def test_rust_checks_the_rules_of_models_that_pydantic_did_not_validate(self) -> None:
        # `model_copy` does not validate its update; Rust checks the rule.
        unchecked = record().model_copy(update={"label": "x" * 41})

        with self.assertRaises(ValueError) as caught:
            echo_record(unchecked)
        self.assertNotIsInstance(caught.exception, ValidationError)
        self.assertEqual(
            str(caught.exception),
            "echo_record() argument 'record': `label` must have at most 40 characters, got 41",
        )

    def test_required_options_must_be_given(self) -> None:
        values = record().model_dump()
        del values["note"]

        with self.assertRaises(ValidationError):
            Record(**values)

    def test_defaults_and_literals(self) -> None:
        settings = Settings(version="v1")

        self.assertEqual(echo_settings(settings), Settings(version="v1", verbose=False, prefix=""))
        self.assertEqual(settings.tags, [])
        self.assertIsNot(settings.tags, Settings(version="v1").tags)
        self.assertEqual(settings.retry, Retry(attempts=3, factor=2.0))
        self.assertTrue(settings.color)
        self.assertEqual(echo_settings(settings).icon, "brackets-curly")
        self.assertEqual(settings.theme, "dark")
        # An absent optional model crosses as `None`, and a `None` field that
        # the caller sets is not the default function's value.
        self.assertIsNone(theme_of(None))
        self.assertEqual(theme_of(settings), "dark")
        self.assertIsNone(theme_of(Settings(version="v1", theme=None)))
        self.assertEqual(record().revision, 0)
        with self.assertRaises(ValidationError):
            Settings(version="v2")  # type: ignore[arg-type]

    def test_tagged_unions_pick_the_variant_by_tag(self) -> None:
        self.assertEqual(describe(EventCreated(record_id="r-1")), "created r-1")
        self.assertEqual(
            describe(EventMoved(record_id="r-1", to=Point(x=2.0, y=3.0))), "moved r-1 to 2, 3"
        )
        self.assertEqual(
            describe(EventDeleted(record_id="r-1", reason="duplicate")), "deleted r-1: duplicate"
        )

    def test_external_unions_cross_in_serde_form_and_pick_the_variant_by_name(self) -> None:
        line = StrokeLineTo(to=Point(x=3.0, y=4.0), width=0.5)
        self.assertEqual(
            line.model_dump(by_alias=True), {"line_to": {"to": {"x": 3.0, "y": 4.0}, "width": 0.5}}
        )
        self.assertEqual(StrokeLift().model_dump(), "lift")
        self.assertEqual(FIRST_STROKE, StrokeMoveTo(value=Point(x=0.0, y=0.0)))
        self.assertEqual(
            retrace([StrokeMoveTo(value=Point(x=1.0, y=2.0)), line, StrokeLift()]),
            [StrokeLift(), line, StrokeMoveTo(value=Point(x=1.0, y=2.0))],
        )
        # Values in the wire form are read as their variants.
        wire: list[object] = ["lift", {"move_to": {"x": 1.0, "y": 2.0}}]
        self.assertEqual(
            retrace(wire),  # type: ignore[arg-type]
            [StrokeMoveTo(value=Point(x=1.0, y=2.0)), StrokeLift()],
        )
        unknown: list[object] = [{"curve_to": {"x": 1.0}}]
        with self.assertRaisesRegex(ValidationError, "does not match any of the expected tags"):
            retrace(unknown)  # type: ignore[arg-type]
        # Serde also reads a variant without data from its name with null.
        named: list[object] = [{"lift": None}]
        self.assertEqual(retrace(named), [StrokeLift()])  # type: ignore[arg-type]

    def test_external_variants_nest_and_drop_nothing_serde_needs(self) -> None:
        twice = StrokeAgain(value=StrokeAgain(value=StrokeLift()))
        with warnings.catch_warnings():
            warnings.simplefilter("error")
            self.assertEqual(twice.model_dump(by_alias=True), {"again": {"again": "lift"}})
            self.assertEqual(retrace([twice, StrokePause(value=2.0)]), [StrokePause(value=2.0), twice])
        # Serde writes a `None` value as null under the variant's name.
        self.assertEqual(StrokePause(value=None).model_dump(exclude_none=True), {"pause": None})
        self.assertEqual(retrace([StrokePause(value=None)]), [StrokePause(value=None)])

    def test_durations_cross_as_nanoseconds_and_arrays_as_tuples(self) -> None:
        self.assertEqual(longer(2_500_000_000), 2_500_000_000)
        self.assertEqual(longer(1), 1_000_000_000)
        self.assertEqual(total([1_500_000_000, None, 2]), 1_500_000_002)
        self.assertEqual(
            corners((2.0, 1.0)),
            (Point(x=0.0, y=0.0), Point(x=2.0, y=0.0), Point(x=2.0, y=1.0), Point(x=0.0, y=1.0)),
        )
        self.assertEqual(NO_STROKES, ())
        self.assertEqual(PAUSE, 1_500_000_000)
        self.assertEqual(next_day(date(2028, 2, 28)), date(2028, 2, 29))
        self.assertEqual(next_day(), date(2028, 2, 29))
        self.assertEqual(next_day(date=date(2028, 12, 31)), date(2029, 1, 1))
        christmas = observe(Holiday(date=date(2027, 12, 25)))
        self.assertEqual(christmas, Holiday(date=date(2027, 12, 25), observed=date(2027, 12, 27)))
        # A duration is a non-negative count of nanoseconds.
        for wrong in [-1, True, {"secs": 1}, {"secs": 0, "nanos": 1_000_000_000}]:
            with self.subTest(wrong=wrong), self.assertRaises(ValidationError):
                longer(wrong)  # type: ignore[arg-type]

    def test_a_unit_variant_is_a_default(self) -> None:
        self.assertEqual(finish([]), [StrokeLift()])
        self.assertEqual(finish([], StrokePause(value=1.0)), [StrokePause(value=1.0)])


class FunctionTest(unittest.TestCase):
    def test_dates_and_models_cross_together(self) -> None:
        at = datetime(2027, 1, 1, tzinfo=UTC)

        touched = touch(record(revision=4), at)

        self.assertEqual(touched.revision, 5)
        self.assertEqual(touched.created_at, at)

    def test_naive_dates_fail_before_reaching_rust(self) -> None:
        with self.assertRaisesRegex(ValidationError, "should have timezone info"):
            touch(record(), datetime(2027, 1, 1))

    def test_json_values_cross_by_their_own_rules(self) -> None:
        def crossed(metadata: object) -> object:
            return echo_record(record(metadata=metadata)).metadata

        at = datetime(2027, 1, 1, tzinfo=UTC)
        self.assertEqual(
            crossed({"at": at, "raw": b"\x00\xff"}),
            {"at": at.isoformat(), "raw": [0, 255]},
        )
        self.assertEqual(crossed({"samples": np.array([1, 2])}), {"samples": [1, 2]})
        with self.assertRaisesRegex(ValueError, "has no UTC offset"):
            crossed({"at": datetime(2027, 1, 1)})
        # Only a dump that crosses follows these rules. The caller's own dump is Pydantic's.
        naive = record(metadata={"at": datetime(2027, 1, 1)})
        self.assertEqual(naive.model_dump(mode="json")["metadata"], {"at": "2027-01-01T00:00:00"})

    def test_dictionaries_for_models_meet_the_model_rules(self) -> None:
        self.assertEqual(echo_settings({"version": "v1"}), Settings(version="v1"))  # type: ignore[arg-type]
        self.assertEqual(describe({"kind": "created", "recordId": "r-1"}), "created r-1")  # type: ignore[arg-type]
        self.assertEqual(echo_record(record().model_dump()), record())  # type: ignore[arg-type]
        invalid = [
            lambda: echo_settings({"version": "v2"}),  # type: ignore[arg-type]
            lambda: echo_settings({"version": "v1", "extra": 1}),  # type: ignore[arg-type]
            # The tag picks the variant, as Serde requires it to.
            lambda: describe({"recordId": "r-1"}),  # type: ignore[arg-type]
            lambda: describe({"kind": "renamed", "recordId": "r-1"}),  # type: ignore[arg-type]
            lambda: echo_record({**record().model_dump(), "stars": 9}),  # type: ignore[arg-type]
            lambda: route_length([{"x": 0, "y": "north"}]),  # type: ignore[list-item]
            lambda: count_tags([{**record().model_dump(), "label": ""}]),  # type: ignore[list-item]
        ]
        for call in invalid:
            with self.subTest(call=call), self.assertRaises(ValidationError):
                call()

    def test_an_optional_model_crosses_absent_as_a_model_or_as_a_dictionary(self) -> None:
        # An absent model skips Pydantic, and a present one meets the model's rules.
        self.assertEqual(distance_from(Point(x=3, y=4), None), 5.0)
        self.assertEqual(distance_from(Point(x=4, y=6), Point(x=1, y=2)), 5.0)
        self.assertEqual(distance_from(Point(x=4, y=6), {"x": 1, "y": 2}), 5.0)  # type: ignore[arg-type]
        with self.assertRaises(ValidationError):
            distance_from(Point(x=0, y=0), {"x": 0, "y": "north"})  # type: ignore[arg-type]

    def test_parameters_named_like_builtins_gain_an_underscore(self) -> None:
        self.assertEqual(reverse(bytes_=b"\x01\x02"), b"\x02\x01")
        self.assertEqual(repeat(str_="ab", int_=3), "ababab")

    def test_arguments_of_the_wrong_type_name_the_parameter(self) -> None:
        with self.assertRaisesRegex(TypeError, r"^repeat\(\) argument 'str_': "):
            repeat(1, 2)  # type: ignore[arg-type]
        with self.assertRaisesRegex(
            TypeError,
            r"^scale\(\) argument 'samples' must be a buffer of float32 values, "
            r"such as a NumPy array, not list$",
        ):
            scale([1.0], 2.0)  # type: ignore[arg-type]
        with self.assertRaisesRegex(TypeError, r"^scale\(\) argument 'factor' must be real number"):
            scale(np.array([1.0], dtype=np.float32), "x")  # type: ignore[arg-type]
        # The type is right, but the value is not.
        with self.assertRaisesRegex(ValueError, r"^repeat\(\) argument 'int_': "):
            repeat("ab", -1)

    def test_parameters_named_like_keywords_gain_a_suffix(self) -> None:
        self.assertEqual(qualify(package="geometry", class_value="Point"), "geometry.Point")

    def test_a_brief_function_that_passes_values_on_is_the_native_function(self) -> None:
        self.assertEqual(type(bounds).__name__, "builtin_function_or_method")
        self.assertEqual((bounds.__name__, bounds.__module__), ("bounds", "showcase.api"))
        self.assertIn("smallest and largest", bounds.__doc__ or "")
        self.assertEqual(bounds(values=[2, 5]), (2, 5))
        with self.assertRaisesRegex(TypeError, r"^bounds\(\) missing 1 required positional"):
            bounds()  # type: ignore[call-arg]
        self.assertEqual(type(repeat).__name__, "function")

    def test_lists_maps_tuples_and_options(self) -> None:
        self.assertEqual(count_tags([record(), record(tags=["b"])]), {"a": 1, "b": 2})
        self.assertEqual(bounds([3, -1, 7]), (-1, 7))
        self.assertIsNone(bounds([]))
        self.assertEqual(totals([("a", 1), ("b", 2), ("a", 3)]), {"a": 4, "b": 2})

    def test_json_values_keep_their_shape(self) -> None:
        value = {"list": [1, 2.5, "x", None, True], "wide": 2**64 - 1}

        self.assertEqual(wrap(value), {"wrapped": value})

    def test_unit_returns_none(self) -> None:
        self.assertIsNone(ping())  # type: ignore[func-returns-value]

    def test_bytes(self) -> None:
        self.assertEqual(reverse(b"\x01\x02\x03"), b"\x03\x02\x01")
        self.assertEqual(fingerprint(b"\x01\x02\x03\x04\x05"), b"\x04\x02\x03\x04")

    def test_buffers_return_writable_typed_arrays(self) -> None:
        totals_array = running_total(np.array([1, 2, 3], dtype=np.int64))
        scaled = scale(np.array([1.0, -2.0], dtype=np.float32), 0.5)

        np.testing.assert_array_equal(totals_array, [1, 3, 6])
        self.assertEqual(totals_array.dtype, np.int64)
        self.assertEqual(scaled.dtype, np.float32)
        np.testing.assert_array_equal(scaled, [0.5, -1.0])
        scaled[0] = 9.0
        self.assertEqual(scaled[0], 9.0)

    def test_buffers_reject_the_wrong_element_type(self) -> None:
        with self.assertRaisesRegex(
            BufferError,
            r"^running_total\(\) argument 'values' must hold native-endian int64 values, "
            r"not dtype\('float64'\); convert it with `\.astype\('int64'\)`$",
        ):
            running_total(np.array([1.0, 2.0]))

    def test_buffers_reject_the_other_byte_order(self) -> None:
        # Read as native, the bytes of 1 would be 72057594037927936.
        values = np.array([1, 2, 3], dtype=np.dtype(np.int64).newbyteorder())
        with self.assertRaisesRegex(BufferError, r"not dtype\('[<>]i8'\)"):
            running_total(values)
        np.testing.assert_array_equal(running_total(values.astype(np.int64)), [1, 3, 6])

    def test_constants(self) -> None:
        self.assertEqual(CONTRACT_VERSION, 3)
        self.assertEqual(DEFAULT_LABEL, "untitled")


class ErrorTest(unittest.TestCase):
    def test_errors_carry_a_stable_code_and_the_rust_message(self) -> None:
        with self.assertRaises(ParseError) as empty:
            parse_priority("  ")
        with self.assertRaises(ParseError) as unknown:
            parse_priority("urgent")

        self.assertEqual(empty.exception.code, "empty")
        self.assertEqual(unknown.exception.code, "unknown_value")
        self.assertEqual(str(unknown.exception), "unknown value `urgent`")
        self.assertEqual(parse_priority("high"), Priority.High)

    def test_errors_from_another_namespace(self) -> None:
        with self.assertRaises(GeometryError) as caught:
            route_length([Point(x=0, y=0), Point(x=0, y=0)])

        self.assertEqual(caught.exception.code, "same_point")
        self.assertEqual(route_length([Point(x=0, y=0), Point(x=3, y=4)]), 5.0)


class PanicTest(unittest.IsolatedAsyncioTestCase):
    def test_a_panic_raises_its_message(self) -> None:
        self.assertEqual(item_at(["a", "b"], 1), "b")
        with self.assertRaisesRegex(PanicException, "index 5 is out of range for 2 items"):
            item_at(["a", "b"], 5)

    async def test_an_async_panic_raises_from_await(self) -> None:
        self.assertEqual(await find_item(["a"], 0), "a")
        with self.assertRaisesRegex(PanicException, "index 3 is out of range for 1 items"):
            await find_item(["a"], 3)


class PackageTest(unittest.TestCase):
    def test_the_root_exports_the_panic_exception_and_arrow_protocols(self) -> None:
        self.assertTrue(issubclass(PanicException, BaseException))
        self.assertFalse(issubclass(PanicException, Exception))
        protocols = [ArrowArrayExportable, ArrowSchemaExportable, ArrowStreamExportable]
        for export in [PanicException, *protocols]:
            self.assertIn(export.__name__, showcase.__all__)

    def test_a_missing_extension_says_how_to_build_it(self) -> None:
        source = Path(showcase.__file__).parent
        with tempfile.TemporaryDirectory() as directory:
            copy = Path(directory) / "showcase"
            shutil.copytree(source, copy, ignore=shutil.ignore_patterns("_native.abi3.so", "_native.pyd"))
            (copy / "_native.pyi").unlink()
            result = subprocess.run(
                [sys.executable, "-c", "import showcase"],
                cwd=directory,
                capture_output=True,
                text=True,
                check=False,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ImportError: the native extension showcase._native is missing", result.stderr)
        self.assertIn("run `bimetal build`", result.stderr)

    def test_an_extension_from_other_sources_says_how_to_rebuild_it(self) -> None:
        source = Path(showcase.__file__).parent
        with tempfile.TemporaryDirectory() as directory:
            copy = Path(directory) / "showcase"
            shutil.copytree(source, copy)
            # As if the sources had changed since the extension was built.
            runtime = copy / "_runtime.py"
            recorded = re.compile(r'(require_contract\()"[0-9a-f]{16}"')
            text, count = recorded.subn(r'\1"0000000000000000"', runtime.read_text())
            self.assertEqual(count, 1)
            runtime.write_text(text)
            result = subprocess.run(
                [sys.executable, "-c", "import showcase"],
                cwd=directory,
                capture_output=True,
                text=True,
                check=False,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "ImportError: the native extension showcase._native was built from other Rust sources",
            result.stderr,
        )
        self.assertIn("run `bimetal build`", result.stderr)


class NamespaceTest(unittest.TestCase):
    def test_linked_packages_and_modules_are_namespaces(self) -> None:
        self.assertEqual(distance(Point(x=0, y=0), Point(x=3, y=4)), 5.0)
        self.assertEqual(direction(Point(x=0, y=0), Point(x=0, y=2)), Point(x=0, y=1))
        self.assertEqual(phrase("one two  three"), Phrase(text="one two  three", word_count=3))

    def test_exports_and_fields_may_have_the_names_of_builtins(self) -> None:
        words = list_words("one three")

        self.assertEqual(words.list, ["one", "three"])
        self.assertEqual(words.lengths, [3, 5])
        self.assertEqual(words.json_value, '["one","three"]')
        self.assertEqual(words.model_dump(by_alias=True)["json"], '["one","three"]')
        self.assertEqual(Words.model_validate({"list": [], "lengths": [], "json": "[]"}).json_value, "[]")

    def test_an_export_may_have_the_name_of_its_package(self) -> None:
        self.assertEqual(showcase.showcase(), "showcase")
        # Other namespaces' modules are imported under aliases, which the
        # export cannot replace, so their errors still raise.
        with self.assertRaises(GeometryError):
            route_length([Point(x=0, y=0), Point(x=0, y=0)])


class ResourceTest(unittest.TestCase):
    def test_keeps_state_and_reports_errors(self) -> None:
        with Ledger("Ada") as ledger:
            self.assertEqual(ledger.deposit(10), 10)
            self.assertEqual(ledger.deposit(-3), 7)
            ledger.lock()
            with self.assertRaises(LockedError) as caught:
                ledger.deposit(1)

            self.assertEqual(caught.exception.code, "locked_error")
            self.assertEqual(ledger.owner(), "Ada")
            self.assertEqual(ledger.history(), [Entry(amount=10, balance=10), Entry(amount=-3, balance=7)])

    def test_a_method_that_a_feature_gates(self) -> None:
        with Ledger.with_balance("Ada", 5) as ledger:
            ledger.deposit(2)
            self.assertEqual(ledger.statement(), Statement(owner="Ada", balance=7, entries=2))

    def test_a_gated_async_method_starts_where_an_ungated_one_starts(self) -> None:
        # On the caller's thread, unless it takes a function, which starts on
        # a worker.
        callers: list[int] = []

        def label(owner: str) -> str:
            callers.append(threading.get_ident())
            return f"{owner}?"

        async def main() -> str:
            with Ledger.with_balance("Ada", 5) as ledger:
                call = ledger.count_entries_at_once()
                with self.assertRaises(StopIteration) as finished:
                    call.send(None)
                self.assertEqual(finished.exception.value, 1)
                return await ledger.label_owner_at_once(label)

        self.assertEqual(asyncio.run(main()), "Ada?")
        self.assertNotEqual(callers, [threading.get_ident()])

    def test_extra_constructors_and_constructor_errors(self) -> None:
        with Ledger.with_balance("Grace", 100) as ledger:
            self.assertEqual(ledger.balance(), 100)
        with self.assertRaises(ParseError):
            Ledger(" ")

    def test_skipped_methods_are_not_exported(self) -> None:
        self.assertFalse(hasattr(Ledger, "clear"))

    def test_calls_after_close_fail(self) -> None:
        ledger = Ledger("Ada")
        ledger.close()

        with self.assertRaisesRegex(RuntimeError, "resource is closed"):
            ledger.balance()


class AsyncTest(unittest.IsolatedAsyncioTestCase):
    async def test_async_functions_return_awaitables(self) -> None:
        self.assertEqual(await count_words("one two three"), 3)
        counts = await asyncio.gather(count_words("a"), count_words("a b"))
        self.assertEqual(counts, [1, 2])

    async def test_an_async_call_that_does_not_wait_does_not_suspend(self) -> None:
        self.assertEqual(await count_words_at_once("one two three"), 3)
        counts = await asyncio.gather(count_words_at_once("a"), count_words_at_once("a b"))
        self.assertEqual(counts, [1, 2])
        # The call's future is done when the call returns, so its coroutine
        # finishes in one step, without giving the event loop a turn.
        call = count_words_at_once("a b")
        with self.assertRaises(StopIteration) as finished:
            call.send(None)
        self.assertEqual(finished.exception.value, 2)

    async def test_async_factories_and_methods(self) -> None:
        async with await Mailbox.open("Ada") as mailbox:
            self.assertEqual(await mailbox.deliver("hello there"), 1)
            self.assertEqual(await mailbox.deliver("goodbye"), 2)
            self.assertEqual(mailbox.count(), 2)
            self.assertEqual(await mailbox.search("hello"), ["hello there"])
            self.assertEqual(mailbox.recipient(), "Ada")

    async def test_async_errors_and_missing_constructor(self) -> None:
        with self.assertRaises(ParseError) as caught:
            await Mailbox.open(" ")
        self.assertEqual(caught.exception.code, "empty")
        with self.assertRaises(TypeError):
            Mailbox()

    async def test_calls_that_overlap_close_still_finish(self) -> None:
        mailbox = await Mailbox.open("Ada")
        # A coroutine runs only once it is scheduled, so start the call first.
        delivery = asyncio.create_task(mailbox.deliver("late"))
        await asyncio.sleep(0)
        mailbox.close()

        self.assertEqual(await delivery, 1)
        with self.assertRaisesRegex(RuntimeError, "resource is closed"):
            mailbox.count()


class ArrowTest(unittest.TestCase):
    def test_arrays_cross_without_serde(self) -> None:
        scaled = scale_column(pa.array([1.0, None, 2.5]), 2.0)

        self.assertIsInstance(scaled, pa.Array)
        self.assertEqual(scaled.to_pylist(), [2.0, None, 5.0])
        with self.assertRaises(ColumnError) as caught:
            scale_column(pa.array([1, 2]), 2.0)
        self.assertEqual(caught.exception.code, "wrong_type")

    def test_record_batches(self) -> None:
        batch = pa.record_batch({"price": [2.0, 3.5], "quantity": [3, 2]})

        totals = add_totals(batch)

        self.assertIsInstance(totals, pa.RecordBatch)
        self.assertEqual(totals.schema.names, ["price", "quantity", "total"])
        self.assertEqual(totals.column("total").to_pylist(), [6.0, 7.0])

    def test_tables_keep_their_batches(self) -> None:
        table = split(pa.record_batch({"n": [0, 1, 2, 3, 4]}), 2)

        self.assertIsInstance(table, pa.Table)
        self.assertEqual(table.column("n").to_pylist(), [0, 1, 2, 3, 4])
        self.assertEqual([batch.num_rows for batch in table.to_batches()], [2, 2, 1])

    def test_schemas_keep_their_metadata(self) -> None:
        schema = pa.schema([("price", pa.float64()), ("quantity", pa.int64())])

        self.assertEqual(describe_schema(schema), ["price: Float64", "quantity: Int64"])
        noted = with_note(schema, "prices in euros")
        self.assertIsInstance(noted, pa.Schema)
        self.assertEqual(noted.metadata, {b"note": b"prices in euros"})

    def test_any_arrow_producer_is_accepted(self) -> None:
        batch = pa.record_batch({"n": [1, 2, 3]})
        reader = pa.RecordBatchReader.from_batches(batch.schema, [batch])

        self.assertEqual(split(batch, 1).num_rows, 3)
        self.assertEqual(describe_schema(reader.schema), ["n: Int64"])
        with self.assertRaises(TypeError):
            scale_column(pa.chunked_array([[1.0]]), 2.0)  # type: ignore[arg-type]

    def test_a_returned_stream_is_a_record_batch_reader(self) -> None:
        reader = batches(split(pa.record_batch({"n": [0, 1, 2, 3, 4]}), 2))

        self.assertIsInstance(reader, pa.RecordBatchReader)
        self.assertEqual([batch.num_rows for batch in reader], [2, 2, 1])


class AsyncArrowTest(unittest.IsolatedAsyncioTestCase):
    async def test_async_calls_take_arrow_values(self) -> None:
        reader = pa.RecordBatchReader.from_batches(
            pa.schema([("n", pa.int64())]), [pa.record_batch({"n": [1, 2, 3]})]
        )

        self.assertEqual(await count_rows(pa.table({"n": [1, 2]})), 2)
        self.assertEqual(await count_rows(reader), 3)

    async def test_streams_pass_through_rust(self) -> None:
        prices = pa.table({"price": [0.5, 1.5], "quantity": [2, 2]})

        stream = await stream_totals(prices)
        self.assertEqual(stream.read_all().column("total").to_pylist(), [1.0, 3.0])
        self.assertEqual(await sum_column(batches(prices), "price"), 2.0)
        with self.assertRaises(ColumnError) as caught:
            await sum_column(prices, "missing")
        self.assertEqual(caught.exception.code, "missing_column")


if __name__ == "__main__":
    unittest.main()
