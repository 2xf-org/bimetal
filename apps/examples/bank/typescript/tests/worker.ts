// A worker thread of workers.test.ts: it loads the addon in its own Node.js
// environment, settles `workerData` transfers in a bank of its own, and posts
// back what they did.

import { parentPort, workerData } from "node:worker_threads";

import { Bank } from "bank";

/** What a worker posts back. */
export interface Paid {
  readonly ids: ReadonlyArray<bigint>;
  readonly received: bigint | undefined;
}

async function pay(count: number): Promise<Paid> {
  using bank = new Bank([
    { owner: "Ada", balance: 100_000_000n },
    { owner: "Grace", balance: 0n },
  ]);
  const receipts = await Promise.all(
    Array.from({ length: count }, () => bank.transfer({ sender: "Ada", recipient: "Grace", amount: 1n })),
  );
  return { ids: receipts.map((receipt) => receipt.id), received: bank.statement().balances["Grace"] };
}

if (parentPort !== null) {
  parentPort.postMessage(await pay(workerData as number));
}
