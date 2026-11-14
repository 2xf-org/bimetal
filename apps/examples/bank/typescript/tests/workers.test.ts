// Two worker threads load the addon in Node.js environments of their own,
// while the main thread moves money in its environment, all at once.

import assert from "node:assert/strict";
import { it } from "node:test";
import { Worker } from "node:worker_threads";

import { Bank } from "bank";

import type { Paid } from "./worker.ts";

/** Run worker.ts, and resolve with what it posts once it has exited. */
function inWorker(transfers: number): Promise<Paid> {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL("./worker.ts", import.meta.url), { workerData: transfers });
    let paid: Paid | undefined;
    worker.once("message", (message: Paid) => (paid = message));
    worker.once("error", reject);
    worker.once("exit", (code) =>
      code === 0 && paid !== undefined ? resolve(paid) : reject(new Error(`the worker exited with ${code}`)),
    );
  });
}

it("loads the addon in two workers beside the main thread", async () => {
  const workers = Promise.all([inWorker(200), inWorker(300)]);
  using bank = new Bank([
    { owner: "Ada", balance: 100_000_000n },
    { owner: "Grace", balance: 0n },
  ]);
  const main = await Promise.all(
    Array.from({ length: 100 }, () => bank.transfer({ sender: "Ada", recipient: "Grace", amount: 1n })),
  );

  const [first, second] = await workers;

  assert.equal(main.length, 100);
  assert.equal(bank.statement().balances["Grace"], 100n);
  for (const [paid, count] of [
    [first, 200],
    [second, 300],
  ] as const) {
    assert.equal(paid.received, BigInt(count));
    assert.deepEqual(
      [...paid.ids].sort((left, right) => Number(left - right)),
      Array.from({ length: count }, (_, index) => BigInt(index + 1)),
    );
  }
});
