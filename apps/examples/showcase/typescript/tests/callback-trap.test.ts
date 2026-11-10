// A trap inside a function that Rust calls, and a `using` block over a trap.
// Each aborts the WebAssembly module, so these calls run in this file's own
// process, in order.

import assert from "node:assert/strict";
import { it } from "node:test";

import { nest } from "showcase";
import { Session, Shelf } from "showcase/shelf";

const shelf = new Shelf("traps", 3);
shelf.add("a");
const session: Session = shelf.session();

it("a trap inside a function reaches the caller, not a result", () => {
  assert.throws(
    () =>
      shelf.visit(() => {
        nest(1_000_000);
        return true;
      }),
    (error: unknown) => error instanceof Error && /trapped|aborted/.test(error.message),
  );
});

it("a resource with an exit method reads as closed, and disposal stays quiet", () => {
  assert.equal(session.closed, true);
  session[Symbol.dispose]();
  assert.throws(() => shelf.size(), /aborted the WebAssembly module/);
});
