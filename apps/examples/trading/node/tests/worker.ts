// A worker thread of workers.test.ts: it loads the addon in its own Node.js
// environment, places `workerData` orders on an engine of its own, and posts
// back what the account holds.

import { parentPort, workerData } from "node:worker_threads";

import { Engine } from "trading";
import { Side } from "trading/core";

/** What a worker posts back. */
export interface Traded {
  readonly ids: ReadonlyArray<bigint>;
  readonly held: bigint | undefined;
}

async function trade(count: number): Promise<Traded> {
  await using engine = await Engine.connect({
    cash: 100_000_000n,
    listings: [{ symbol: "ACME", price: 10_000n }],
    quoteIntervalMs: 1,
    seed: 11n,
  });
  const orders = await Promise.all(
    Array.from({ length: count }, () => engine.submit({ symbol: "ACME", side: Side.Buy, quantity: 1n })),
  );
  return { ids: orders.map((placed) => placed.id), held: engine.snapshot().positions["ACME"] };
}

if (parentPort !== null) {
  parentPort.postMessage(await trade(workerData as number));
}
