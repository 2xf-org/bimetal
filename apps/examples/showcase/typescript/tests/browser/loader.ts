// Loads the package the way a browser does, and prints what each call did as
// JSON. Without `process`, the runtime fetches its WebAssembly module and
// instantiates it asynchronously; this fetch reads it from disk, or with the
// `unreachable` argument fails the way a network does.
//
// `browser.test.ts` runs this file in its own process, because hiding
// `process` would disturb the test runner.

import { readFile } from "node:fs/promises";

// The package's Arrow adapters import apache-arrow, which then loads with
// `process` in place.
import "apache-arrow";

const { argv, stdout } = process;
const unreachable = argv[2] === "unreachable";
const processProperty = Object.getOwnPropertyDescriptor(globalThis, "process")!;

globalThis.fetch = async (input) => {
  if (unreachable) throw new TypeError("fetch failed");
  return new Response(await readFile(new URL(String(input))), {
    headers: { "content-type": "application/wasm" },
  });
};
Reflect.deleteProperty(globalThis, "process");

function outcome(call: () => unknown): string {
  try {
    return `returned ${String(call())}`;
  } catch (error) {
    return error instanceof Error ? `threw ${error.constructor.name}: ${error.message}` : `threw ${String(error)}`;
  }
}

async function settled(call: () => Promise<unknown>): Promise<string> {
  try {
    return `resolved ${String(await call())}`;
  } catch (error) {
    return error instanceof Error ? `rejected ${error.constructor.name}: ${error.message}` : `rejected ${String(error)}`;
  }
}

const showcase = await import("showcase");
const calls = async () => ({
  function: outcome(() => showcase.qualify("geometry", "Point")),
  constructor: outcome(() => new showcase.Ledger("Ada")),
  asyncFunction: await settled(() => showcase.countWords("one two")),
  asyncFactory: await settled(() => showcase.Mailbox.open("Ada")),
});
const early = await calls();
const loaded = await settled(() => showcase.nativeReady);
const later = await calls();

Object.defineProperty(globalThis, "process", processProperty);
stdout.write(JSON.stringify({ early, loaded, later }));
