// The package as browser projects see it: the asynchronous loader, and the
// declarations without Node.js types.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe as suite, it } from "node:test";

function run(script: URL, ...args: string[]): string {
  const child = spawnSync(process.execPath, [fileURLToPath(script), ...args], { encoding: "utf8" });
  assert.equal(child.status, 0, `${child.stdout}\n${child.stderr}`);
  return child.stdout;
}

suite("browsers", () => {
  const loader = new URL("./browser/loader.ts", import.meta.url);
  const notReady =
    "Error: The WebAssembly module is not ready. Browsers load it asynchronously: `await nativeReady`, which the package exports, once before the first call.";
  const early = {
    function: `threw ${notReady}`,
    constructor: `threw ${notReady}`,
    asyncFunction: `rejected ${notReady}`,
    asyncFactory: `rejected ${notReady}`,
  };

  it("explain calls made before nativeReady resolves", () => {
    assert.deepEqual(JSON.parse(run(loader)), {
      early,
      loaded: "resolved undefined",
      later: {
        function: "returned geometry.Point",
        constructor: "returned [object Object]",
        asyncFunction: "resolved 2",
        asyncFactory: "resolved [object Object]",
      },
    });
  });

  it("report a module that failed to load from every later call", () => {
    const wasmUrl = new URL("../showcase/native/native_bg.wasm", import.meta.url).href;
    const failed = `Error: The WebAssembly module failed to load from ${wasmUrl}: fetch failed`;
    // The failed fetch settles before the early calls are made.
    const { loaded, later } = JSON.parse(run(loader, "unreachable"));

    assert.deepEqual({ loaded, later }, {
      loaded: "rejected TypeError: fetch failed",
      later: {
        function: `threw ${failed}`,
        constructor: `threw ${failed}`,
        asyncFunction: `rejected ${failed}`,
        asyncFactory: `rejected ${failed}`,
      },
    });
  });

  it("type-check the declarations with only the ES2022 and DOM libraries", () => {
    run(
      new URL("../node_modules/typescript/bin/tsc", import.meta.url),
      "-p",
      fileURLToPath(new URL("./browser/tsconfig.json", import.meta.url)),
    );
  });
});
