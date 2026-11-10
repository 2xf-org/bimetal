// A panic or a trap in an async call rejects the call's promise, and the
// process keeps running. Each aborts the WebAssembly module, which rejects
// every other pending call too, so each scenario runs in a child process of
// its own, which handles no `uncaughtException`. The Node.js addon stays
// usable after a panic, as the Python extension does.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { describe as suite, it } from "node:test";

import { addon } from "./backend.ts";

const child = `
const [, showcase, failure] = process.argv;
const { Mailbox, countWords, findItem, nestLater, qualify } = await import(showcase);
const message = (error) => (error instanceof Error ? error.message : "not an Error");
const settled = (promise) => promise.then(() => "resolved", message);
const thrown = (call) => {
  try {
    call();
    return "returned";
  } catch (error) {
    return message(error);
  }
};

console.log(await findItem(["a"], 0));
const mailbox = await Mailbox.open("Ada");
// The delivery holds the mailbox while it pauses, when the other call fails.
const delivery = mailbox.deliver("hello");
console.log(await settled(failure === "panic" ? findItem(["a"], 3) : nestLater(1_000_000)));
console.log(await settled(delivery));
const later = [
  thrown(() => qualify("geometry", "Point")),
  thrown(() => mailbox.count()),
  thrown(() => mailbox.close()),
  await settled(countWords("one two")),
  await settled(mailbox.search("hello")),
  await settled(Mailbox.open("Grace")),
];
console.log(JSON.stringify(later));
`;

/** Run the child, and return the lines it printed. */
function run(failure: "panic" | "trap"): string[] {
  const showcase = new URL("../showcase/index.js", import.meta.url).href;
  // A later call that entered the aborted module would never settle.
  const output = execFileSync(process.execPath, ["--input-type=module", "-e", child, showcase, failure], {
    encoding: "utf8",
    timeout: 10_000,
  });
  return output.trim().split("\n");
}

/** What every later call reports, closing aside, which does nothing. */
function later(aborted: string): string[] {
  return [aborted, aborted, "returned", aborted, aborted, aborted];
}

suite("async calls", () => {
  it(addon ? "reject a call that panics, and leave the others" : "reject pending calls with a panic, and every later call at once", () => {
    const [found, panicked, delivery, calls] = run("panic");

    assert.equal(found, "a");
    if (addon) {
      assert.equal(panicked, "Rust panicked: index 3 is out of range for 1 items");
      assert.equal(delivery, "resolved");
      const closed = "Mailbox.search(): the resource is closed";
      assert.deepEqual(JSON.parse(calls ?? "null"), ["returned", "returned", "returned", "resolved", closed, "resolved"]);
      return;
    }
    assert.match(panicked ?? "", /^Rust panicked: index 3 is out of range for 1 items \(at .+\)$/);
    assert.equal(delivery, panicked);
    const aborted = `An earlier Rust panic aborted the WebAssembly module (${panicked?.slice("Rust panicked: ".length)}); reload the page or restart the process to call it again.`;
    assert.deepEqual(JSON.parse(calls ?? "null"), later(aborted));
  });

  // A Rust stack overflow ends a native process.
  it("reject pending calls with a trap without a panic, such as a stack overflow", { skip: addon }, () => {
    const [found, trapped, delivery, calls] = run("trap");

    assert.equal(found, "a");
    assert.equal(trapped, "The WebAssembly module trapped: RangeError: Maximum call stack size exceeded");
    assert.equal(delivery, trapped);
    const aborted =
      "An earlier WebAssembly trap aborted the WebAssembly module (RangeError: Maximum call stack size exceeded); reload the page or restart the process to call it again.";
    assert.deepEqual(JSON.parse(calls ?? "null"), later(aborted));
  });
});
