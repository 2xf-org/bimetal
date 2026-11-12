// Time calls into the showcase's npm package, on whichever backend it was
// built for, or run one memory case, for apps/benchmark/run.py.
//
// Usage: node calls.mjs <project> time [<case> <batch> <samples>]...
//        node calls.mjs <project> memory <case> <calls>
//
// <project> is the npm project that holds the `showcase` package and its
// node_modules. `time` runs <samples> batches of <batch> calls for each case
// that this file defines, and prints each batch's nanoseconds, one JSON line
// per case. `memory` sets up the input of <case> and makes <calls> calls.

import { realpathSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [project, mode, ...given] = process.argv.slice(2);
const load = (path) => import(pathToFileURL(realpathSync(join(project, path))).href);
// The package's own copy of apache-arrow, so that its tables encode.
const arrow = await load("node_modules/apache-arrow/Arrow.node.mjs");
const showcase = await load("showcase/index.js");
const columns = await load("showcase/columns/index.js");
await showcase.nativeReady;

const MODELS = 10_000;
const ROWS = 1_000_000;
// The rows of each batch of a stream case's table.
const STREAM_BATCH = 10_000;

function record() {
  return {
    id: "r-1",
    label: "First",
    priority: showcase.Priority.High,
    score: 90,
    stars: 4,
    note: null,
    tags: ["a", "b"],
    counters: { views: 3 },
    ranges: { week: [1, 7] },
    createdAt: "2026-07-22T12:00:00Z",
    localTime: "2026-07-22T09:00:00-03:00",
    metadata: { nested: [1, "two", null] },
    checksum: [0, 255],
    location: { x: 1.5, y: -2 },
    revision: 0,
  };
}

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

function splitBatch(batch) {
  const table = columns.split(batch, ROWS);
  if (table.numRows !== ROWS) throw new Error(`split returned ${table.numRows} rows`);
}

// The rows of `arrowInput`, as a table of STREAM_BATCH-row batches.
function streamInput() {
  const batch = arrowInput();
  const parts = [];
  for (let offset = 0; offset < ROWS; offset += STREAM_BATCH) {
    parts.push(batch.slice(offset, offset + STREAM_BATCH));
  }
  return new arrow.Table(parts);
}

async function readStream(table) {
  let rows = 0;
  for await (const batch of columns.batches(table)) rows += batch.numRows;
  if (rows !== ROWS) throw new Error(`the stream returned ${rows} rows`);
}

// The generated wrapper's native function, found in its source, to separate
// the wrapper's cost from the boundary's.
const runtime = await load("showcase/runtime.js");
const pingName = String(showcase.ping).match(/__bimetal_function_\w+/)[0];
const pingRaw = runtime.native[pingName];

// Each case's batch size and sample count, from the arguments of `time`.
const counts = new Map();
for (let index = 0; index + 2 < given.length; index += 3) {
  counts.set(given[index], [Number(given[index + 1]), Number(given[index + 2])]);
}

// Sync calls run without `await`, which would add a microtask turn to each.
async function measure(name, call, { awaited = false } = {}) {
  if (!counts.has(name)) throw new Error(`no batch and sample counts for ${name}`);
  const [batch, samples] = counts.get(name);
  const elapsed = [];
  for (let sample = 0; sample < samples; sample++) {
    const start = performance.now();
    if (awaited) {
      for (let index = 0; index < batch; index++) await call();
    } else {
      for (let index = 0; index < batch; index++) call();
    }
    elapsed.push((performance.now() - start) * 1e6);
  }
  console.log(JSON.stringify({ case: name, batch_ns: elapsed }));
}

if (mode === "time") {
  const bytes = bytesInput(1 << 20);
  const records = Array.from({ length: MODELS }, record);
  const ledger = new showcase.Ledger("Ada");
  for (let index = 0; index < MODELS; index++) ledger.deposit(1n);
  const batch = arrowInput();
  const table = streamInput();
  await measure("ping", showcase.ping);
  await measure("ping-raw", pingRaw);
  await measure("bytes-1mb", () => showcase.reverse(bytes));
  await measure("models-in-10k", () => showcase.countTags(records));
  await measure("models-out-10k", () => ledger.history());
  await measure("arrow-1m", () => splitBatch(batch));
  await measure("stream-out-1m", () => readStream(table), { awaited: true });
  await measure("async", () => showcase.countWords("a b c"), { awaited: true });
  await measure("async-ready", () => showcase.countWordsAtOnce("a b c"), { awaited: true });
  await measure("stream-through-1m", () => columns.sumColumn(columns.batches(table), "price"), {
    awaited: true,
  });
  ledger.close();
} else if (mode === "memory") {
  const [memoryCase, memoryCalls] = given;
  const calls = Number(memoryCalls);
  let call;
  if (memoryCase === "bytes-1mb" || memoryCase === "bytes-100mb") {
    const bytes = bytesInput(memoryCase === "bytes-1mb" ? 1 << 20 : 100 << 20);
    call = () => showcase.reverse(bytes);
  } else if (memoryCase === "arrow-1m") {
    const batch = arrowInput();
    call = () => splitBatch(batch);
  } else {
    throw new Error(`unknown memory case ${memoryCase}`);
  }
  for (let index = 0; index < calls; index++) call();
} else {
  throw new Error("usage: calls.mjs <project> time [<case> <batch> <samples>]... | memory <case> <calls>");
}
