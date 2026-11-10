// The samples of docs/tutorial.md that greeter.test.ts doesn't already hold.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Guestbook } from "greeter";

test("a resource signs and reads", () => {
  using guestbook = new Guestbook("launch");
  guestbook.sign("Ada");
  assert.deepEqual(guestbook.names(), ["Ada"]);
});
