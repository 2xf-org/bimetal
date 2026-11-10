// Two worker threads load the addon in Node.js environments of their own,
// while the main thread trades in its environment, all at once.

import assert from "node:assert/strict";
import { it } from "node:test";
import { Worker } from "node:worker_threads";

import { Engine } from "trading";
import { Side } from "trading/core";

import type { Traded } from "./worker.ts";

/** Run worker.ts, and resolve with what it posts once it has exited. */
function inWorker(orders: number): Promise<Traded> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("./worker.ts", import.meta.url), { workerData: orders });
    let traded: Traded | undefined;
    worker.once("message", (message: Traded) => (traded = message));
    worker.once("error", reject);
    worker.once("exit", (code) =>
      code === 0 && traded !== undefined ? resolve(traded) : reject(new Error(`the worker exited with ${code}`)),
    );
  });
}

it("loads the addon in two workers beside the main thread", async () => {
  const workers = Promise.all([inWorker(200), inWorker(300)]);
  await using engine = await Engine.connect({
    cash: 100_000_000n,
    listings: [{ symbol: "ACME", price: 10_000n }],
    quoteIntervalMs: 1,
  });
  const main = await Promise.all(
    Array.from({ length: 100 }, () => engine.submit({ symbol: "ACME", side: Side.Buy, quantity: 1n })),
  );

  const [first, second] = await workers;

  assert.equal(main.length, 100);
  assert.deepEqual(engine.snapshot().positions, { ACME: 100n });
  for (const [traded, count] of [
    [first, 200],
    [second, 300],
  ] as const) {
    assert.equal(traded.held, BigInt(count));
    assert.deepEqual(
      [...traded.ids].sort((left, right) => Number(left - right)),
      Array.from({ length: count }, (_, index) => BigInt(index + 1)),
    );
  }
});
