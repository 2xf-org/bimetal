// Tantivy through the generated npm package, on Node.js, with the same
// single-threaded writer a browser uses.
import assert from "node:assert/strict";
import test from "node:test";

import { BulkWriter, FieldKind, Index, SearchError, firstText } from "search";

const FIELDS = [
  { name: "title", kind: FieldKind.Text, stored: true },
  { name: "body", kind: FieldKind.Text },
  { name: "year", kind: FieldKind.Integer, stored: true },
];
const BOOKS = [
  { title: "The Old Man and the Sea", body: "an old man who fished alone", year: 1952 },
  { title: "Of Mice and Men", body: "the river runs deep and green", year: 1937 },
  { title: "Frankenstein", body: "no disaster has accompanied the commencement", year: 1818 },
];

function index() {
  const writer = new BulkWriter(FIELDS);
  for (const book of BOOKS) writer.add(book);
  return writer.finish();
}

test("finish consumes the writer and returns an index", () => {
  const writer = new BulkWriter(FIELDS);
  writer.add(BOOKS[0]);
  assert.ok(writer.finish() instanceof Index);
  assert.throws(() => writer.add(BOOKS[0]), /closed/);
});

test("search returns hits and defaults its limit", () => {
  const searcher = index().searcher();
  const hits = searcher.search("sea");
  assert.equal(hits.length, 1);
  assert.equal(firstText(hits[0].document, "title"), "The Old Man and the Sea");
  assert.equal(searcher.search("the OR of").length, 3);
  assert.equal(searcher.search("the OR of", null, 1).length, 1);
});

test("for of over hits", () => {
  const titles = [...index().searcher().hits("the OR of", 2)].map((hit) => firstText(hit.document, "title"));
  assert.equal(titles.length, 3);
});

test("a bad query is typed and carries its reason", () => {
  assert.throws(
    () => index().searcher().search("title:"),
    (error) => error instanceof SearchError && error.code === "query" && error.details.reason.length > 0,
  );
});

test("fields is a getter", () => {
  assert.deepEqual(index().fields.map((field) => field.name), ["title", "body", "year"]);
});
