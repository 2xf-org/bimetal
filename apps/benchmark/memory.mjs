// Run one memory case of apps/benchmark/run.py in the showcase's npm package,
// on whichever backend it was built for.
//
// Usage: node memory.mjs <project> memory <case> <calls>
//
// <project> is the npm project that holds the `showcase` package and its
// node_modules. The script sets up the input of <case> and makes <calls>
// calls, so that `/usr/bin/time` can report the process's peak memory.

import { realpathSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [project, mode, memoryCase, memoryCalls] = process.argv.slice(2);
if (mode !== "memory" || memoryCalls === undefined) {
  throw new Error("usage: memory.mjs <project> memory <case> <calls>");
}
const load = (path) => import(pathToFileURL(realpathSync(join(project, path))).href);
// The package's own copy of apache-arrow, so that its tables encode.
const arrow = await load("node_modules/apache-arrow/Arrow.node.mjs");
const showcase = await load("showcase/index.js");
const columns = await load("showcase/columns/index.js");
await showcase.nativeReady;

const ROWS = 1_000_000;

function bytesInput(length) {
  const bytes = new Uint8Array(length);
  for (let index = 0; index < length; index++) bytes[index] = index & 0xff;
  return bytes;
}

function arrowInput() {
  const price = new Float64Array(ROWS);
  const quantity = new BigInt64Array(ROWS);
  for (let row = 0; row < ROWS; row++) {
    price[row] = row * 0.5;
    quantity[row] = BigInt(row);
  }
  const table = arrow.tableFromArrays({ price, quantity });
  if (table.batches.length !== 1) throw new Error(`expected one batch, got ${table.batches.length}`);
  return table.batches[0];
}

let call;
if (memoryCase === "bytes-1mb" || memoryCase === "bytes-100mb") {
  const bytes = bytesInput(memoryCase === "bytes-1mb" ? 1 << 20 : 100 << 20);
  call = () => showcase.reverse(bytes);
} else if (memoryCase === "arrow-1m") {
  const batch = arrowInput();
  call = () => {
    const table = columns.split(batch, ROWS);
    if (table.numRows !== ROWS) throw new Error(`split returned ${table.numRows} rows`);
  };
} else {
  throw new Error(`unknown memory case ${memoryCase}`);
}
for (let index = 0; index < Number(memoryCalls); index++) call();
