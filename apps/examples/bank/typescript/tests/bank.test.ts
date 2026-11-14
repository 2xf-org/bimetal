// Move money through the generated npm package, which runs the bank as a
// Node.js addon. `tsc` also checks this file against the generated declarations.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { Table } from "apache-arrow";

import { Account, AccountError, Bank, OpenError, TransferError, interest, type Transfer } from "bank";

/** Ada's opening balance in each test's bank, in cents: enough for every transfer. */
const FUNDS = 100_000_000n;

/** A bank where Ada holds `FUNDS` and Grace holds nothing. */
function bank(): Bank {
  return new Bank([
    { owner: "Ada", balance: FUNDS },
    { owner: "Grace", balance: 0n },
  ]);
}

function pay(amount = 1n, overrides: Partial<Transfer> = {}): Transfer {
  return { sender: "Ada", recipient: "Grace", amount, ...overrides };
}

/** Whether `error` is the typed error with `code`. */
const refused = (code: string) => (error: unknown) => error instanceof TransferError && error.code === code;

describe("an account", () => {
  it("runs programs that print what their comments say", () => {
    for (const [program, printed] of [
      ["quickstart.ts", "70\ninsufficient_funds\n"],
      ["transfers.ts", "1n { Ada: 70n, Grace: 30n }\ninsufficient_funds\n"],
    ]) {
      const path = fileURLToPath(new URL(`../${program}`, import.meta.url));
      assert.equal(execFileSync(process.execPath, [path], { encoding: "utf8" }), printed);
    }
  });

  it("keeps its balance when a withdrawal is refused", () => {
    using account = new Account();
    account.deposit(5);
    assert.throws(
      () => account.withdraw(6),
      (error: unknown) => error instanceof AccountError && error.code === "insufficient_funds",
    );
    assert.equal(account.balance(), 5);
  });
});

describe("transfers", () => {
  it("settles a thousand transfers from Promise.all once each", async () => {
    using opened = bank();

    const receipts = await Promise.all(Array.from({ length: 1_000 }, () => opened.transfer(pay())));

    assert.deepEqual(
      receipts.map((receipt) => receipt.id).sort((left, right) => Number(left - right)),
      Array.from({ length: 1_000 }, (_, index) => BigInt(index + 1)),
    );
    const ledger = opened.ledger();
    assert.ok(ledger instanceof Table);
    assert.deepEqual(
      ledger.schema.fields.map((field) => field.name),
      ["id", "sender", "recipient", "amount"],
    );
    assert.equal(ledger.numRows, 1_000);
    const statement = opened.statement();
    assert.deepEqual(statement.balances, { Ada: FUNDS - 1_000n, Grace: 1_000n });
    assert.equal(statement.transfers, 1_000n);
  });

  it("rejects refused transfers with typed errors", async () => {
    using opened = bank();

    await assert.rejects(opened.transfer(pay(1n, { recipient: "Linus" })), refused("unknown_account"));
    await assert.rejects(
      opened.transfer(pay(FUNDS + 1n)),
      (error: unknown) =>
        refused("insufficient_funds")(error) &&
        (error as Error).message === "100000001 cents are needed, but the account has 100000000",
    );
    assert.equal(opened.statement().transfers, 0n);
  });

  it("checks arguments before they reach the bank", async () => {
    using opened = bank();

    await assert.rejects(opened.transfer(pay(0n)), RangeError);
    assert.throws(() => new Bank([{ owner: "", balance: 1n }]), RangeError);
    assert.throws(
      () =>
        new Bank([
          { owner: "Ada", balance: 1n },
          { owner: "Ada", balance: 2n },
        ]),
      (error: unknown) => error instanceof OpenError && error.code === "duplicate_owner",
    );
  });

  it("streams receipts from the settlement task", async () => {
    using opened = bank();

    const waiting = opened.receipts(10);
    const sent = [];
    for (let amount = 1n; amount <= 10n; amount += 1n) {
      sent.push(await opened.transfer(pay(amount)));
    }

    assert.deepEqual(await waiting, sent);
  });

  it("lets running calls finish when the bank closes", async () => {
    const opened = bank();
    const running = Array.from({ length: 100 }, () => opened.transfer(pay()));
    opened.close();

    const receipts = await Promise.all(running);

    assert.equal(new Set(receipts.map((receipt) => receipt.id)).size, 100);
    assert.throws(() => opened.statement(), /^Error: Bank\.statement\(\): the resource is closed$/);
    await assert.rejects(opened.transfer(pay()), /^Error: Bank\.transfer\(\): the resource is closed$/);
  });
});

describe("the bank's lock", () => {
  it("throws Busy for a sync call while a withdrawal holds the bank", async () => {
    using opened = bank();

    // The withdrawal holds the bank alone from the call until it settles.
    const withdrawal = opened.withdraw("Ada", 1_000n);
    assert.throws(
      () => opened.statement(),
      (error: unknown) =>
        error instanceof Error && error.message === "Bank.statement(): the resource is busy with another call",
    );
    // An async call waits for the bank instead.
    const transferred = opened.transfer(pay());

    assert.equal(await withdrawal, FUNDS - 1_000n);
    assert.equal((await transferred).id, 1n);
    assert.equal(opened.statement().balances["Ada"], FUNDS - 1_001n);
    await assert.rejects(opened.withdraw("Ada", FUNDS), refused("insufficient_funds"));
  });
});

describe("panics", () => {
  it("throw an Error with the panic's message, and the addon stays usable", async () => {
    assert.throws(
      () => interest(2n ** 64n - 1n, 2n),
      (error: unknown) =>
        error instanceof Error && error.message === "Rust panicked: 18446744073709551615 cents at 2% overflow a u64",
    );

    assert.equal(interest(250n, 4n), 10n);
    using opened = bank();
    assert.equal((await opened.transfer(pay())).id, 1n);
  });
});
