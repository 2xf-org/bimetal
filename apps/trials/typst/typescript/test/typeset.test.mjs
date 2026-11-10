// Typst through the generated npm package, on Node.js.
import assert from "node:assert/strict";
import test from "node:test";

import { Compiler, Document, TypstError, typstVersion } from "typeset";

const SOURCE = `
#set page(width: 10cm, height: auto)
= Hello from Bimetal
$ integral_0^1 x^2 dif x = 1/3 $
#pagebreak()
Second page.
`;

function compiler() {
  const compiler = new Compiler();
  compiler.addSource("main.typ", SOURCE);
  return compiler;
}

test("version", () => assert.match(typstVersion(), /^\d+\.\d+/));

test("bundled fonts load", () => assert.ok(compiler().fontCount > 10));

test("compile returns a document", () => {
  const document = compiler().compile("main.typ");
  assert.ok(document instanceof Document);
  assert.equal(document.pageCount(), 2);
  assert.deepEqual(document.warnings, []);
});

test("pdf, svg, and png", () => {
  const document = compiler().compile("main.typ");
  assert.equal(new TextDecoder().decode(document.pdf().slice(0, 5)), "%PDF-");
  assert.match(document.svg(0), /<svg/);
  assert.equal(document.png(1)[1], 0x50);
  assert.ok(document.png(0, 1).length < document.png(0, 3).length);
});

test("for of over pages", () => {
  const pages = [...compiler().compile("main.typ").pages()];
  assert.equal(pages.length, 2);
  assert.ok(pages.every((page) => page.includes("<svg")));
});

test("using closes the document", () => {
  let kept;
  {
    using document = compiler().compile("main.typ");
    kept = document;
    assert.equal(document.pageCount(), 2);
  }
  assert.throws(() => kept.pageCount(), /closed/);
});

test("a compile error carries its diagnostics", () => {
  const compiling = compiler();
  compiling.addSource("bad.typ", "#let x = \n#unknown()");
  assert.throws(
    () => compiling.compile("bad.typ"),
    (error) => {
      assert.ok(error instanceof TypstError);
      assert.equal(error.code, "compile");
      assert.equal(error.details.diagnostics[0].level, "error");
      assert.equal(error.details.diagnostics[0].path, "/bad.typ");
      return true;
    },
  );
});

test("a page past the end is a typed error", () => {
  const document = compiler().compile("main.typ");
  assert.throws(
    () => document.svg(9),
    (error) => error.code === "page_out_of_range" && error.details.count === 2,
  );
});
