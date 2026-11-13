// Time the probe's crossings from TypeScript, on whichever backend its npm
// package was built for, for apps/benchmark/lab.py.
//
// Usage: node lab_node.mjs <project> <rounds> <samples> [<case>...]
//
// <project> is the npm project that holds the generated `probe` package and
// its node_modules. Each case runs <rounds> times, interleaved with the
// others, and each time takes <samples> samples of a batch of calls; the batch
// is as many calls as take about a millisecond. The script prints one JSON line
// per case with the nanoseconds per call of every sample. With case names, it
// times only those.
//
// A case named `<name>-raw` calls the native function that the generated
// wrapper of `<name>` calls, with the arguments that the wrapper would pass,
// so the difference is the wrapper's cost. A case named `floor-*` makes no
// call into Rust. A named case that this host does not define is skipped.

import { realpathSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [project, roundsArgument, samplesArgument, ...chosen] = process.argv.slice(2);
const load = (path) => import(pathToFileURL(realpathSync(join(project, path))).href);
const arrow = await load("node_modules/apache-arrow/Arrow.node.mjs");
const probe = await load("probe/index.js");
const runtime = await load("probe/runtime.js");
await probe.nativeReady;

// The calls of one sample take about this long.
const SAMPLE_NS = 1_000_000;
// Concurrent callers in the cases that measure throughput.
const CALLERS = 32;
// Rounds of `spin` that take about 170 µs on one core of an Apple M5.
const HEAVY_ROUNDS = 100_000;

// The native function that a generated wrapper calls, by the name in its
// source.
function nativeOf(wrapper) {
  const name = String(wrapper).match(/__bimetal_function_\w+/)?.[0];
  if (name === undefined) throw new Error(`no native function in ${wrapper.name}`);
  return runtime.native[name];
}

const hundred = Array.from({ length: 100 }, (_, index) => index);
const hundredBig = hundred.map(BigInt);
const words = ["alpha", "beta", "gamma", "alpha", "delta", "beta", "alpha", "omega", "pi", "rho"];
const text = "x".repeat(100);
const kilobyte = new Uint8Array(1024).map((_, index) => index & 0xff);
const megabyte = new Uint8Array(1 << 20).map((_, index) => index & 0xff);
const rows = Array.from({ length: 100 }, (_, index) => ({
  id: BigInt(index),
  name: `row ${index}`,
  score: index * 0.5,
  active: index % 2 === 0,
  tags: ["a", "b"],
  createdAt: "2026-07-14T12:00:00Z",
}));
const point = { x: 3, y: 4 };
const batch = arrow.tableFromArrays({
  price: new Float64Array([0.5]),
  quantity: new BigInt64Array([1n]),
}).batches[0];
const counter = new probe.Counter(0n);

async function readStream() {
  for await (const _ of probe.stream(100, 10)) {
    // Each batch is one `next()`.
  }
}

const gather = (call) => () => Promise.all(Array.from({ length: CALLERS }, call));

// Each case: whether it awaits, the call, and how many calls one call of it
// makes.
const sync = (call) => [false, call, 1];
const awaited = (call, perCall = 1) => [true, call, perCall];
// The native functions, each found once, so that a case times the call.
const native = Object.fromEntries(
  ["noop", "add", "echo", "numbers", "total", "tally", "bounds", "point", "length", "rows", "countTags", "reverse", "ready", "readyNumbers", "wait"]
    .map((name) => [name, nativeOf(probe[name])]),
);
const cases = {
  "noop": sync(probe.noop),
  "noop-raw": sync(native.noop),
  "add": sync(() => probe.add(1n, 2n)),
  "add-raw": sync(() => native.add(1n, 2n)),
  "echo": sync(() => probe.echo(text)),
  "echo-raw": sync(() => native.echo(text)),
  "numbers-100": sync(() => probe.numbers(100)),
  "numbers-100-raw": sync(() => native.numbers(100)),
  "total-100": sync(() => probe.total(hundredBig)),
  "total-100-raw": sync(() => native.total(hundredBig)),
  "tally-10": sync(() => probe.tally(words)),
  "tally-10-raw": sync(() => native.tally(words)),
  "bounds-100": sync(() => probe.bounds(hundredBig)),
  "bounds-100-raw": sync(() => native.bounds(hundredBig)),
  "point": sync(() => probe.point(3, 4)),
  "point-raw": sync(() => native.point(3, 4)),
  "length": sync(() => probe.length(point)),
  "length-raw": sync(() => native.length(point)),
  "rows-100": sync(() => probe.rows(100)),
  "rows-100-raw": sync(() => native.rows(100)),
  "count-tags-100": sync(() => probe.countTags(rows)),
  "count-tags-100-raw": sync(() => native.countTags(rows)),
  "reverse-1k": sync(() => probe.reverse(kilobyte)),
  "reverse-1k-raw": sync(() => native.reverse(kilobyte)),
  "reverse-1m": sync(() => probe.reverse(megabyte)),
  "fail": sync(() => {
    try {
      probe.fail();
    } catch {
      // The error is the cost.
    }
  }),
  "call": sync(() => probe.call((value) => value + 1n, 1n)),
  "method-get": sync(() => counter.get()),
  "method-add": sync(() => counter.add(1n)),
  "one-row": sync(() => probe.oneRow()),
  "batch-rows": sync(() => probe.batchRows(batch)),
  "stream-100x10": awaited(readStream),
  "floor-ready": awaited(() => Promise.resolve(1)),
  "ready": awaited(() => probe.ready(1n)),
  "ready-raw": awaited(() => native.ready(1n)),
  "ready-numbers-100": awaited(() => probe.readyNumbers(100)),
  "ready-numbers-100-raw": awaited(() => native.readyNumbers(100)),
  "floor-turn": awaited(() => new Promise((resolve) => setImmediate(resolve))),
  "wait": awaited(() => probe.wait(1n)),
  "wait-raw": awaited(() => native.wait(1n)),
  "method-add-later": awaited(() => counter.addLater(1n)),
  "wait-32": awaited(gather(() => probe.wait(1n)), CALLERS),
  "spin-async-32": awaited(gather(() => probe.spinAsync(HEAVY_ROUNDS)), CALLERS),
  "spin": sync(() => probe.spin(HEAVY_ROUNDS)),
};

const selected = Object.entries(cases).filter(([name]) => chosen.length === 0 || chosen.includes(name));

// Sync calls run without `await`, which would add a microtask turn to each.
async function run(isAsync, call, count) {
  if (isAsync) {
    for (let index = 0; index < count; index++) await call();
  } else {
    for (let index = 0; index < count; index++) call();
  }
}

async function batchSize(isAsync, call) {
  for (let count = 1; ; count *= 2) {
    const start = process.hrtime.bigint();
    await run(isAsync, call, count);
    if (Number(process.hrtime.bigint() - start) >= SAMPLE_NS || count >= 1 << 22) return count;
  }
}

const batches = new Map();
for (const [name, [isAsync, call]] of selected) batches.set(name, await batchSize(isAsync, call));
const timed = new Map(selected.map(([name]) => [name, []]));
for (let round = 0; round < Number(roundsArgument); round++) {
  for (const [name, [isAsync, call, perCall]] of selected) {
    const count = batches.get(name);
    for (let sample = 0; sample < Number(samplesArgument); sample++) {
      const start = process.hrtime.bigint();
      await run(isAsync, call, count);
      timed.get(name).push(Number(process.hrtime.bigint() - start) / (count * perCall));
    }
  }
}
counter.close();
for (const [name, ns] of timed) console.log(JSON.stringify({ case: name, batch: batches.get(name), ns }));
