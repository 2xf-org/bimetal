// Polars through the generated npm package, on Node.js.
import assert from "node:assert/strict";
import test from "node:test";
import { inspect } from "node:util";

import { Table, tableFromArrays } from "apache-arrow";

import { Batches, Frame, FrameError, Query, Session } from "frames";

function trades() {
  return new Frame(
    tableFromArrays({
      symbol: ["AAPL", "MSFT", "AAPL", "NVDA", "AAPL"],
      price: new Float64Array([190.5, 410.25, 191.0, 880.0, 189.75]),
      size: new Int32Array([100, 50, 200, 10, 300]),
    }),
  );
}
const CSV = "name,age\nAda,36\nGrace,45\nAlan,41\n";

test("arrow round trip", () => {
  const table = trades().toArrow();
  assert.ok(table instanceof Table);
  assert.deepEqual([...table.getChild("symbol")], ["AAPL", "MSFT", "AAPL", "NVDA", "AAPL"]);
});

test("read csv", () => {
  const frame = Frame.readCsv(new TextEncoder().encode(CSV));
  assert.deepEqual(frame.columns, ["name", "age"]);
  assert.deepEqual(frame.column("age"), [36, 45, 41]);
});

test("from rows and to rows", () => {
  const rows = [{ a: 1, b: "x" }, { a: 2, b: "y" }];
  assert.deepEqual(Frame.fromRows(rows).toRows(), rows);
});

test("shape, display, and inspect", () => {
  const frame = trades();
  assert.equal(frame.length, 5);
  assert.equal(frame.width, 3);
  assert.match(String(frame), /shape: \(5, 3\)/);
  assert.match(inspect(frame), /NVDA/);
});

test("group by aggregates", () => {
  const query = trades().lazy();
  assert.ok(query instanceof Query);
  const result = query
    .groupBy(["symbol"], ["sum(size) AS volume", "max(price) AS high"])
    .sort("volume", true)
    .collect();
  assert.deepEqual(result.column("symbol"), ["AAPL", "MSFT", "NVDA"]);
  assert.deepEqual(result.column("volume"), [600, 50, 10]);
});

test("a chain of operations", () => {
  const result = trades()
    .lazy()
    .filter("size >= 100")
    .withColumns(["price * size AS notional"])
    .select(["symbol", "notional"])
    .sort("notional", true)
    .limit(1)
    .collect();
  assert.deepEqual(result.column("notional"), [189.75 * 300]);
});

test("sql on a frame", () => {
  const result = trades().sql("select symbol, count(*) as n from self group by symbol order by symbol");
  assert.deepEqual(result.column("n"), [3, 1, 1]);
});

test("sql across frames", () => {
  const session = new Session();
  session.register("trades", trades());
  session.register("names", Frame.fromRows([{ symbol: "NVDA", name: "Nvidia" }]));
  assert.deepEqual(session.tables.toSorted(), ["names", "trades"]);
  const result = session.sql("select n.name, t.size from trades t join names n on t.symbol = n.symbol");
  assert.deepEqual(result.toRows(), [{ name: "Nvidia", size: 10 }]);
});

test("join", () => {
  const names = Frame.fromRows([{ symbol: "NVDA", name: "Nvidia" }]);
  assert.equal(trades().join(names, ["symbol"]).length, 1);
  assert.equal(trades().join(names, ["symbol"], "left").length, 5);
});

test("a JavaScript function maps a column", () => {
  const result = trades().mapColumn("size", "doubled", (value) => value * 2);
  assert.deepEqual(result.column("doubled").slice(0, 2), [200, 100]);
});

test("an exception reaches the caller", () => {
  assert.throws(
    () =>
      trades().mapColumn("size", "x", () => {
        throw new Error("no values today");
      }),
    (error) => error instanceof FrameError && error.code === "function" && /no values today/.test(error.details.message),
  );
});

test("for of over batches", () => {
  const frame = new Frame(tableFromArrays({ n: new Int32Array(Array.from({ length: 25 }, (_, i) => i)) }));
  const batches = frame.batches(10);
  assert.ok(batches instanceof Batches);
  assert.deepEqual([...batches].map((batch) => batch.numRows), [10, 10, 5]);
});

test("errors are typed", () => {
  assert.throws(
    () => trades().lazy().select(["nope"]).collect(),
    (error) => error instanceof FrameError && error.code === "missing_column",
  );
  assert.throws(
    () => trades().sql("select from where"),
    (error) => error instanceof FrameError && error.code === "invalid_query",
  );
});

test("a million rows group", () => {
  const k = new Int32Array(1_000_000);
  const v = new Float64Array(1_000_000);
  for (let i = 0; i < k.length; i++) {
    k[i] = i % 100;
    v[i] = i;
  }
  const result = new Frame(tableFromArrays({ k, v })).lazy().groupBy(["k"], ["sum(v) AS total"]).sort("k").collect();
  assert.equal(result.length, 100);
});

test("collect from an async call", async () => {
  const result = await trades().lazy().filter("size > 100").collectAsync();
  assert.equal(result.length, 2);
});
