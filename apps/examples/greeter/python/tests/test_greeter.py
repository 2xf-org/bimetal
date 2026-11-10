"""Exercise the generated Python package against the compiled Rust extension."""

import unittest

import numpy as np
from pydantic import ValidationError

from greeter import (
    GUESTBOOK_CAPACITY,
    GreeterError,
    Greeting,
    GreetingRequest,
    Guestbook,
    PanicException,
    Tone,
    greet,
)
from greeter.stats import normalize


class GreetTest(unittest.TestCase):
    def test_returns_a_typed_model(self) -> None:
        greeting = greet(GreetingRequest(name="Ada", tone=Tone.Formal, excited=True))

        self.assertEqual(greeting, Greeting(message="Good day, Ada!", tone=Tone.Formal))

    def test_applies_field_defaults(self) -> None:
        self.assertEqual(greet(GreetingRequest(name="Ada", tone=Tone.Casual)).message, "Hi, Ada.")

    def test_rejects_values_outside_field_rules_before_calling_rust(self) -> None:
        with self.assertRaises(ValidationError):
            GreetingRequest(name="", tone=Tone.Casual)

    def test_validates_a_dictionary_passed_for_a_model(self) -> None:
        request = {"name": "Ada", "tone": "formal", "excited": True}

        self.assertEqual(greet(request).message, "Good day, Ada!")  # type: ignore[arg-type]
        with self.assertRaises(ValidationError):
            greet({**request, "name": ""})  # type: ignore[arg-type]

    def test_exports_the_exception_that_a_rust_panic_raises(self) -> None:
        self.assertTrue(issubclass(PanicException, BaseException))

    def test_raises_rust_errors_with_a_stable_code(self) -> None:
        with self.assertRaises(GreeterError) as caught:
            greet(GreetingRequest(name="   ", tone=Tone.Casual))

        self.assertEqual(caught.exception.code, "blank_name")
        self.assertEqual(str(caught.exception), "the name is blank")


class GuestbookTest(unittest.TestCase):
    def test_keeps_state_in_rust_between_calls(self) -> None:
        with Guestbook("launch") as guestbook:
            for name in ["Ada", "Grace", "Linus"][:GUESTBOOK_CAPACITY]:
                guestbook.sign(name)
            with self.assertRaises(GreeterError) as caught:
                guestbook.sign("Ken")

            self.assertEqual(guestbook.event(), "launch")
            self.assertEqual(guestbook.names(), ["Ada", "Grace", "Linus"])
            self.assertEqual(caught.exception.code, "guestbook_full")

    def test_rejects_calls_after_leaving_the_with_block(self) -> None:
        with Guestbook("launch") as guestbook:
            pass

        with self.assertRaises(RuntimeError):
            guestbook.names()


class StatsTest(unittest.TestCase):
    def test_round_trips_numpy_buffers(self) -> None:
        normalized = normalize(np.array([2.0, -4.0, 1.0]))

        self.assertEqual(normalized.dtype, np.float64)
        np.testing.assert_array_equal(normalized, [0.5, -1.0, 0.25])


if __name__ == "__main__":
    unittest.main()
