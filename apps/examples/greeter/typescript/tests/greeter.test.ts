// Exercise the generated TypeScript package against the compiled Node.js addon.

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { GUESTBOOK_CAPACITY, GreeterError, Guestbook, Tone, greet } from "greeter";
import { normalize } from "greeter/stats";

describe("greet", () => {
  it("returns a plain object", () => {
    assert.deepEqual(greet({ name: "Ada", tone: Tone.Formal, excited: true }), {
      message: "Good day, Ada!",
      tone: "formal",
    });
  });

  it("applies field defaults", () => {
    assert.equal(greet({ name: "Ada", tone: Tone.Casual }).message, "Hi, Ada.");
  });

  it("throws Rust errors with a stable code", () => {
    assert.throws(
      () => greet({ name: "   ", tone: Tone.Casual }),
      (error: unknown) =>
        error instanceof GreeterError && error.code === "blank_name" && error.message === "the name is blank",
    );
  });
});

describe("Guestbook", () => {
  it("keeps state in Rust between calls", () => {
    using guestbook = new Guestbook("launch");
    for (const name of ["Ada", "Grace", "Linus"].slice(0, GUESTBOOK_CAPACITY)) {
      guestbook.sign(name);
    }
    assert.throws(
      () => guestbook.sign("Ken"),
      (error: unknown) => error instanceof GreeterError && error.code === "guestbook_full",
    );
    assert.equal(guestbook.event(), "launch");
    assert.deepEqual(guestbook.names(), ["Ada", "Grace", "Linus"]);
  });

  it("rejects calls after its using block ends", () => {
    let closed: Guestbook;
    {
      using guestbook = new Guestbook("launch");
      closed = guestbook;
    }
    assert.throws(() => closed.names(), /resource is closed/);
  });
});

describe("stats.normalize", () => {
  it("round-trips typed arrays", () => {
    const normalized = normalize(new Float64Array([2, -4, 1]));

    assert.ok(normalized instanceof Float64Array);
    assert.deepEqual([...normalized], [0.5, -1, 0.25]);
  });
});
