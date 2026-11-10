"""The samples of docs/tutorial.md, in its order."""

import unittest

from pydantic import ValidationError

from greeter import GreeterError, GreetingRequest, Tone, greet
from greeter import Guestbook


class TutorialTest(unittest.TestCase):
    def test_a_call(self) -> None:
        greeting = greet(GreetingRequest(name="Ada", tone=Tone.Formal, excited=True))
        print(greeting.message)

    def test_a_field_rule(self) -> None:
        with self.assertRaises(ValidationError):
            GreetingRequest(name="", tone=Tone.Casual)

    def test_an_error(self) -> None:
        with self.assertRaises(GreeterError) as caught:
            greet(GreetingRequest(name="   ", tone=Tone.Casual))
        self.assertEqual(caught.exception.code, "blank_name")

    def test_a_resource(self) -> None:
        with Guestbook("launch") as guestbook:
            guestbook.sign("Ada")
            self.assertEqual(guestbook.names(), ["Ada"])


if __name__ == "__main__":
    unittest.main()
