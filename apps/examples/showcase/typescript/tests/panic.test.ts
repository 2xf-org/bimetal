// A Rust panic aborts the WebAssembly module, so these calls run in this
// file's own process. The Node.js addon stays usable after a panic, as the
// Python extension does. The tests run in order: the first panics, and the
// second makes calls after the panic.

import assert from "node:assert/strict";
import { it } from "node:test";

import { Ledger, Mailbox, countWords, itemAt } from "showcase";

import { addon } from "./backend.ts";

// Opened before the panic, to show what its methods do after it.
const ledger = new Ledger("Ada");

it("throws a Rust panic as an Error with the panic's message", () => {
  assert.equal(itemAt(["a", "b"], 1), "b");
  assert.throws(
    () => {
      // Closing does not throw, also once the module is aborted, so the
      // panic is not wrapped in a `SuppressedError`.
      using _held = new Ledger("Grace");
      itemAt(["a", "b"], 5);
    },
    (error: unknown) =>
      error instanceof Error &&
      (addon
        ? error.message === "Rust panicked: index 5 is out of range for 2 items" && error.cause === undefined
        : error.message.startsWith("Rust panicked: index 5 is out of range for 2 items (at ") &&
          // The `WebAssembly.RuntimeError` that the panic trapped with.
          error.cause instanceof Error &&
          error.cause.name === "RuntimeError"),
  );
});

it(addon ? "keeps the addon usable" : "keeps every later call out of the aborted module", async () => {
  if (addon) {
    assert.equal(itemAt(["a"], 0), "a");
    using grace = new Ledger("Grace");
    assert.equal(grace.balance(), 0n);
    assert.equal(ledger.balance(), 0n);
    assert.equal(await countWords("one two"), 2);
    await using mailbox = await Mailbox.open("Ada");
    assert.equal(mailbox.recipient(), "Ada");
    assert.equal(ledger.close(), undefined);
    return;
  }
  const aborted = (error: unknown) =>
    error instanceof Error &&
    /^An earlier Rust panic aborted the WebAssembly module \(index 5 is out of range for 2 items \(at .+\)\); reload the page or restart the process to call it again\.$/.test(
      error.message,
    );

  assert.throws(() => itemAt(["a"], 0), aborted);
  assert.throws(() => new Ledger("Grace"), aborted);
  assert.throws(() => ledger.balance(), aborted);
  await assert.rejects(countWords("one two"), aborted);
  await assert.rejects(Mailbox.open("Ada"), aborted);
  assert.equal(ledger.close(), undefined);
});
