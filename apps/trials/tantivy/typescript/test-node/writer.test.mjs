// The threaded writer, which needs the Node.js addon: a browser has no threads.
import assert from "node:assert/strict";
import test from "node:test";

import { FieldKind, Index, Writer } from "search";

const FIELDS = [{ name: "title", kind: FieldKind.Text, stored: true }];

test("disposal rolls back what was not committed", () => {
  const index = new Index(FIELDS);
  {
    using writer = index.writer();
    assert.ok(writer instanceof Writer);
    writer.add({ title: "kept" });
    writer.commit();
    writer.add({ title: "dropped" });
  }
  assert.equal(index.searcher().numDocuments(), 1n);
});

test("a throwing block leaves nothing behind", () => {
  const index = new Index(FIELDS);
  assert.throws(() => {
    using writer = index.writer();
    writer.add({ title: "dropped" });
    throw new Error("boom");
  }, /boom/);
  assert.equal(index.searcher().numDocuments(), 0n);
});

test("an index survives on disk", async () => {
  const { mkdtempSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const directory = mkdtempSync(join(tmpdir(), "search-"));
  try {
    {
      using index = Index.create(directory, FIELDS);
      using writer = index.writer();
      writer.add({ title: "kept" });
      writer.commit();
    }
    using index = Index.open(directory);
    assert.equal(index.searcher().numDocuments(), 1n);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
