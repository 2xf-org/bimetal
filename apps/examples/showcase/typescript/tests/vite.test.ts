// The package as Vite serves it to Node.js, as vitest does: a file outside the
// project root comes from `/@fs/<absolute path>`, which `import.meta.url` then
// holds. The hooks below load the package from such URLs, and the runtime
// reads its WebAssembly module from the path inside them.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import { it } from "node:test";

const origin = "http://localhost:5173";
const served = `${origin}/@fs`;
const packageDirectory = new URL("../showcase/", import.meta.url).href;

function viteUrl(fileUrl: string): string {
  return served + new URL(fileUrl).pathname;
}

function fileUrl(viteUrl: string): string {
  return `file://${viteUrl.slice(served.length)}`;
}

registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier.startsWith(served)) {
      return { url: specifier, shortCircuit: true };
    }
    const parent = context.parentURL;
    if (!parent?.startsWith(served)) {
      return nextResolve(specifier, context);
    }
    // The package's own modules come from Vite too; apache-arrow from disk.
    const resolved = nextResolve(specifier, { ...context, parentURL: fileUrl(parent) });
    return resolved.url.startsWith(packageDirectory)
      ? { ...resolved, url: viteUrl(resolved.url), shortCircuit: true }
      : resolved;
  },
  load(url, context, nextLoad) {
    if (!url.startsWith(served)) {
      return nextLoad(url, context);
    }
    return { format: "module", source: readFileSync(new URL(fileUrl(url))), shortCircuit: true };
  },
});

it("loads the WebAssembly module from a Vite `/@fs/` URL", async () => {
  const showcase: typeof import("showcase") = await import(viteUrl(`${packageDirectory}index.js`));

  await showcase.nativeReady;
  assert.equal(showcase.qualify("geometry", "Point"), "geometry.Point");
});
