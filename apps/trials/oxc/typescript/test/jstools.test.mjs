// Oxc through the generated npm package, on Node.js.
import assert from "node:assert/strict";
import test from "node:test";

import { Language, Module, OxcError, Statements, check, declarations, minify, transform } from "jstools";

const TYPESCRIPT = `
import { readFile } from "fs";
export interface Point { x: number; y: number }
export function add(a: number, b: number): number { return a + b; }
const secret: string = "hidden";
add(1, 2);
`;

test("transform removes types", () => {
  const output = transform(TYPESCRIPT);
  assert.ok(!output.code.includes("interface"));
  assert.match(output.code, /function add\(a, b\)/);
  assert.equal(output.map, null);
});

test("transform writes a source map", () => {
  const output = transform(TYPESCRIPT, Language.Ts, { source_map: true, mangle: false });
  assert.equal(JSON.parse(output.map).version, 3);
});

test("minify shrinks", () => {
  const source = "function longName(argumentOne) { return argumentOne + 1 }\nconsole.log(longName(2));";
  assert.ok(minify(source).code.length < source.length);
});

test("declarations", () => assert.match(declarations(TYPESCRIPT), /export declare function add/));

test("check reports without throwing", () => {
  assert.deepEqual(check("let a = 1;"), []);
  assert.equal(check("let a = 1; let a = 2;")[0].fatal, true);
});

test("a syntax error carries its diagnostics", () => {
  assert.throws(
    () => new Module("const = 1"),
    (error) => error instanceof OxcError && error.code === "syntax" && error.details.diagnostics[0].fatal,
  );
});

test("ast is ESTree", () => {
  const ast = new Module(TYPESCRIPT, Language.Ts).ast();
  assert.equal(ast.type, "Program");
  assert.equal(ast.body[0].type, "ImportDeclaration");
});

test("astText is the same tree", () => {
  const module = new Module(TYPESCRIPT, Language.Ts);
  assert.deepEqual(JSON.parse(module.astText()), module.ast());
});

test("a large ast crosses", () => {
  const source = Array.from({ length: 5000 }, (_, index) => `const v${index} = { a: [1, 2, ${index}] };`).join("\n");
  assert.equal(new Module(source).ast().body.length, 5000);
});

test("for of over statements", () => {
  const statements = new Module(TYPESCRIPT, Language.Ts).statements();
  assert.ok(statements instanceof Statements);
  assert.equal([...statements].length, 5);
});

test("a visitor runs and stops", () => {
  const seen = [];
  const module = new Module(TYPESCRIPT, Language.Ts);
  assert.equal(module.visit((statement) => (seen.push(statement.kind), seen.length < 2)), 2);
  assert.deepEqual(seen, ["ImportDeclaration", "ExportNamedDeclaration"]);
});

test("an exception in a visitor reaches the caller", () => {
  const module = new Module(TYPESCRIPT, Language.Ts);
  assert.throws(
    () =>
      module.visit(() => {
        throw new Error("stop");
      }),
    /stop/,
  );
});
