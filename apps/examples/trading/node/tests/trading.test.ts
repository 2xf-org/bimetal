// Trade through the generated npm package, which runs the engine as a Node.js
// addon. `tsc` also checks this file against the generated declarations.

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { Table } from "apache-arrow";

import { ConfigError, Engine, type EngineConfig } from "trading";
import { OrderError, Side, notional, type Order, type OrderRequest } from "trading/core";

/** The cash of each test's account, in cents: enough for every order. */
const CASH = 100_000_000n;

/**
 * Two listings, quoted every millisecond. Tests assert what holds for any
 * prices, and seed them only so that a failure repeats.
 */
function config(overrides: Partial<EngineConfig> = {}): EngineConfig {
  return {
    cash: CASH,
    listings: [
      { symbol: "ACME", price: 10_000n },
      { symbol: "GLOBEX", price: 2_500n },
    ],
    quoteIntervalMs: 1,
    seed: 7n,
    ...overrides,
  };
}

function order(side: Side = Side.Buy, quantity = 1n, overrides: Partial<OrderRequest> = {}): OrderRequest {
  return { symbol: "ACME", side, quantity, ...overrides };
}

function filledPrice(placed: Order): bigint {
  if (placed.state.status !== "filled") {
    assert.fail(`order ${placed.id} expired`);
  }
  return placed.state.price;
}

/** Whether `error` is the typed error with `code`. */
const refused = (code: string) => (error: unknown) => error instanceof OrderError && error.code === code;

describe("orders", () => {
  it("fills a thousand orders from Promise.all once each", async () => {
    await using engine = await Engine.connect(config());

    const orders = await Promise.all(Array.from({ length: 1_000 }, () => engine.submit(order())));

    const prices = new Map(orders.map((placed) => [placed.id, filledPrice(placed)]));
    assert.deepEqual(
      [...prices.keys()].sort((left, right) => Number(left - right)),
      Array.from({ length: 1_000 }, (_, index) => BigInt(index + 1)),
    );
    const fills = engine.fills();
    assert.ok(fills instanceof Table);
    assert.deepEqual(
      fills.schema.fields.map((field) => field.name),
      ["order_id", "symbol", "side", "quantity", "price", "sequence"],
    );
    assert.equal(fills.numRows, 1_000);
    const ids = fills.getChild("order_id")!.toArray() as BigUint64Array;
    const filled = fills.getChild("price")!.toArray() as BigUint64Array;
    for (let row = 0; row < fills.numRows; row += 1) {
      assert.equal(filled[row], prices.get(ids[row]!));
    }
    const snapshot = engine.snapshot();
    assert.deepEqual(snapshot.positions, { ACME: 1_000n });
    assert.equal(snapshot.orders, 1_000n);
    const spent = [...prices.values()].reduce((total, price) => total + notional(price, 1n), 0n);
    assert.equal(snapshot.cash, CASH - spent);
  });

  it("sells what the account holds, and expires orders past their limit", async () => {
    await using engine = await Engine.connect(config());

    await engine.submit(order(Side.Buy, 5n));
    const sold = await engine.submit(order(Side.Sell, 2n));
    const expired = await engine.submit(order(Side.Buy, 1n, { limit: 1n }));

    assert.equal(sold.state.status, "filled");
    assert.equal(expired.state.status, "expired");
    assert.deepEqual(engine.snapshot().positions, { ACME: 3n });
  });

  it("rejects refused orders with typed errors", async () => {
    await using engine = await Engine.connect(config());

    await assert.rejects(engine.submit(order(Side.Buy, 1n, { symbol: "INITECH" })), refused("unknown_symbol"));
    await assert.rejects(engine.submit(order(Side.Buy, CASH)), refused("insufficient_balance"));
    await assert.rejects(
      engine.submit(order(Side.Sell, 1n, { symbol: "GLOBEX" })),
      (error: unknown) =>
        refused("insufficient_position")(error) &&
        (error as Error).message === "the order sells 1 of `GLOBEX`, but the account holds 0",
    );
    assert.equal(engine.snapshot().orders, 0n);
  });

  it("checks arguments before they reach the venue", async () => {
    await using engine = await Engine.connect(config());

    await assert.rejects(engine.submit(order(Side.Buy, 0n)), RangeError);
    await assert.rejects(engine.submit(order("hold" as Side)), TypeError);
    await assert.rejects(Engine.connect(config({ listings: [] })), RangeError);
    const listing = { symbol: "ACME", price: 1n };
    await assert.rejects(
      Engine.connect(config({ listings: [listing, listing] })),
      (error: unknown) => error instanceof ConfigError && error.code === "duplicate_symbol",
    );
  });

  it("streams quotes from the venue task", async () => {
    // Without a seed, the operating system seeds the prices.
    await using engine = await Engine.connect(config({ seed: null }));

    const quotes = await engine.quotes(20);

    assert.equal(quotes.length, 20);
    for (let index = 2; index < quotes.length; index += 1) {
      const [earlier, later] = [quotes[index - 2]!, quotes[index]!];
      assert.equal(later.symbol, earlier.symbol);
      assert.equal(later.sequence, earlier.sequence + 1n);
      assert.ok(later.bid - earlier.bid <= 2n && earlier.bid - later.bid <= 2n);
    }
    assert.ok(quotes.every((quote) => quote.ask === quote.bid + 1n));
  });

  it("lets running calls finish when the engine closes", async () => {
    const engine = await Engine.connect(config());
    const running = Array.from({ length: 100 }, () => engine.submit(order()));
    engine.close();

    const orders = await Promise.all(running);

    assert.equal(new Set(orders.map((placed) => placed.id)).size, 100);
    assert.throws(() => engine.snapshot(), /^Error: Engine\.snapshot\(\): the resource is closed$/);
    await assert.rejects(engine.submit(order()), /^Error: Engine\.submit\(\): the resource is closed$/);
  });
});

describe("the engine's lock", () => {
  it("throws Busy for a sync call while a withdrawal holds the engine", async () => {
    await using engine = await Engine.connect(config());

    // The withdrawal holds the engine alone from the call until it settles.
    const withdrawal = engine.withdraw(1_000n);
    assert.throws(
      () => engine.snapshot(),
      (error: unknown) =>
        error instanceof Error &&
        error.message === "Engine.snapshot(): the resource is busy with another call",
    );
    // An async call waits for the engine instead.
    const submitted = engine.submit(order());

    assert.equal(await withdrawal, CASH - 1_000n);
    const price = filledPrice(await submitted);
    assert.equal(engine.snapshot().cash, CASH - 1_000n - price);
    await assert.rejects(engine.withdraw(CASH), refused("insufficient_balance"));
  });
});

describe("panics", () => {
  it("throw an Error with the panic's message, and the addon stays usable", async () => {
    assert.throws(
      () => notional(2n ** 64n - 1n, 2n),
      (error: unknown) =>
        error instanceof Error &&
        error.message === "Rust panicked: 2 units at 18446744073709551615 cents overflow a u64",
    );

    assert.equal(notional(250n, 4n), 1_000n);
    await using engine = await Engine.connect(config());
    assert.equal((await engine.submit(order())).state.status, "filled");
  });
});
