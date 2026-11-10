// A trap without a panic, here a stack overflow that throws a `RangeError`,
// aborts the WebAssembly module as a panic does, so these calls run in this
// file's own process. The tests run in order: the first traps, and the
// second makes calls after the trap.

import assert from "node:assert/strict";
import { it } from "node:test";

import { Ledger, countWords, nest } from "showcase";

const ledger = new Ledger("Ada");

it("throws a trap as an Error that names it", () => {
  assert.equal(nest(10), 10);
  assert.throws(
    () => nest(1_000_000),
    (error: unknown) =>
      error instanceof Error &&
      error.message === "The WebAssembly module trapped: RangeError: Maximum call stack size exceeded" &&
      error.cause instanceof RangeError,
  );
});

it("keeps every later call out of the aborted module", async () => {
  const aborted = (error: unknown) =>
    error instanceof Error &&
    error.message ===
      "An earlier WebAssembly trap aborted the WebAssembly module (RangeError: Maximum call stack size exceeded); reload the page or restart the process to call it again.";

  assert.throws(() => nest(1), aborted);
  assert.throws(() => ledger.balance(), aborted);
  await assert.rejects(countWords("one two"), aborted);
  assert.equal(ledger.close(), undefined);
});
