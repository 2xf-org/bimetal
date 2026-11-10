// Uses the generated declarations the way a browser application does. The
// resource classes declare `[Symbol.dispose]`, so their declarations must
// bring the ESNext.Disposable library themselves.

import { Ledger, Mailbox, nativeReady, qualify } from "showcase";

export async function openLedger(owner: string): Promise<Ledger> {
  await nativeReady;
  const ledger = new Ledger(owner);
  const dispose: () => void = ledger[Symbol.dispose];
  dispose.call(ledger);
  return Ledger.withBalance(qualify("ledgers", owner), 0n);
}

export async function closeMailbox(mailbox: Mailbox): Promise<void> {
  await mailbox[Symbol.asyncDispose]();
}
