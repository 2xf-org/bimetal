// DataFusion through the generated npm package, on Node.js.
import assert from "node:assert/strict";
import test from "node:test";

import { Float64, Int32, RecordBatch, Table, tableFromArrays } from "apache-arrow";

import { Batches, Frame, QueryError, Session } from "query";

function trades() {
  return tableFromArrays({
    symbol: ["AAPL", "MSFT", "AAPL", "NVDA", "AAPL"],
    price: new Float64Array([190.5, 410.25, 191.0, 880.0, 189.75]),
    size: new Int32Array([100, 50, 200, 10, 300]),
  });
}

function session() {
  const result = new Session();
  result.addTable("trades", trades());
  return result;
}

test("query returns an apache-arrow table", async () => {
  const table = await session().query("select symbol, sum(size) as volume from trades group by symbol order by symbol");
  assert.ok(table instanceof Table);
  assert.deepEqual([...table.getChild("symbol")], ["AAPL", "MSFT", "NVDA"]);
});

test("sql returns a frame that refines", async () => {
  const frame = await session().sql("select * from trades");
  assert.ok(frame instanceof Frame);
  const narrowed = frame.filter("symbol = 'AAPL'").select(["price"]).limit(2, 1);
  assert.equal(await narrowed.count(), 2n);
  assert.equal((await narrowed.collect()).numRows, 2);
});

test("schema is a getter", async () => {
  const frame = await session().sql("select symbol, price from trades");
  assert.deepEqual(frame.schema.fields.map((field) => field.name), ["symbol", "price"]);
});

test("for await over batches", async () => {
  const frame = await session().sql("select * from trades");
  const batches = await frame.stream();
  assert.ok(batches instanceof Batches);
  let rows = 0;
  for await (const batch of batches) {
    assert.ok(batch instanceof RecordBatch);
    rows += batch.numRows;
  }
  assert.equal(rows, 5);
});

test("a bad query is typed", async () => {
  await assert.rejects(session().query("select nope from trades"), (error) => {
    assert.ok(error instanceof QueryError);
    assert.equal(error.code, "plan");
    assert.match(error.details.message, /nope/);
    return true;
  });
});

test("a frame can register as a table", async () => {
  const open = session();
  const frame = await open.sql("select * from trades where size > 100");
  await open.addFrame("big", frame);
  assert.equal((await open.query("select * from big")).numRows, 2);
});

test("a JavaScript function runs in SQL", async () => {
  const open = session();
  const seen = [];
  open.addFunction("notional", (price, size) => {
    seen.push(price);
    return price * size;
  });
  const table = await open.query("select notional(price, cast(size as double)) as n from trades where symbol = 'NVDA'");
  assert.deepEqual([...table.getChild("n")], [8800]);
  assert.deepEqual(seen, [880]);
});
