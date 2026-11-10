// The TypeScript samples of docs/guides, so that they run.

import assert from "node:assert/strict";
import { test } from "node:test";

import { tableFromArrays } from "apache-arrow";

import {
  Ledger,
  Mailbox,
  ParseError,
  Priority,
  countWords,
  echoSettings,
  nativeReady,
  parsePriority,
  retrace,
  reverse,
  runningTotal,
} from "showcase";
import { split } from "showcase/columns";
import { Shelf, ShelfError, offset } from "showcase/shelf";
import { phrase } from "showcase/text";

function books(titles: string[] = []): Shelf {
  const value = new Shelf("fiction", 3);
  for (const title of titles) value.add(title);
  return value;
}

test("the package is ready", async () => {
  await nativeReady;
});

test("functions", () => {
  assert.equal(phrase("one two").wordCount, 2);
});

test("default arguments", () => {
  assert.equal(offset(4), 1.5);
  assert.equal(offset(4, undefined, 1), 3);
});

test("models", () => {
  assert.equal(echoSettings({ version: "v1" }).verbose, false);
  assert.equal(Priority.High, "high");
  assert.deepEqual(retrace(["lift", { move_to: { x: 1, y: 2 } }]), [{ move_to: { x: 1, y: 2 } }, "lift"]);
});

test("errors", () => {
  assert.throws(
    () => parsePriority(""),
    (error: unknown) => error instanceof ParseError && error.code === "empty",
  );
});

test("error details", () => {
  const full = new Shelf("fiction", 1);
  full.add("a");
  assert.throws(
    () => full.add("c"),
    (error: unknown) => {
      assert.ok(error instanceof ShelfError);
      assert.equal(error.code, "full");
      assert.deepEqual(error.details, { limit: 1, title: "c" });
      return true;
    },
  );
});

test("a resource", () => {
  using ledger = new Ledger("Ada");
  assert.equal(ledger.deposit(5n), 5n);
  assert.equal(Ledger.withBalance("Ada", 5n).balance(), 5n);
});

test("consuming a resource", () => {
  const value = books(["a", "b"]);
  assert.deepEqual(value.seal(), ["a", "b"]);
});

test("properties", () => {
  const value = books();
  value.name = "history";
  assert.equal(value.name, "history");
  assert.equal(value.length, 0);
});

test("using with an explicit commit", () => {
  const value = books();
  {
    using session = value.session();
    session.add("a");
    session.commit();
  }
  assert.equal(value.length, 1);
});

test("iteration", () => {
  const value = books(["a", "b"]);
  assert.deepEqual([...value.titles()], ["a", "b"]);
});

test("async calls", async () => {
  assert.equal(await countWords("one two three"), 3);
  await using mailbox = await Mailbox.open("Ada");
  assert.equal(await mailbox.deliver("hello"), 1);
});

test("callbacks", () => {
  const value = books(["a", "b"]);
  assert.equal(value.visit((_title, index) => index < 0), 1);
});

test("bytes and buffers", () => {
  assert.deepEqual(reverse(new Uint8Array([1, 2, 3])), new Uint8Array([3, 2, 1]));
  assert.deepEqual(runningTotal(new BigInt64Array([1n, 2n, 3n])), new BigInt64Array([1n, 3n, 6n]));
});

test("arrow", () => {
  const batch = tableFromArrays({ price: new Float64Array([1.5, 2.5]) }).batches[0]!;
  assert.equal(split(batch, 1).numRows, 2);
});
