"""Oxc through the generated Python package."""

import json
import unittest

from jstools import (
    Language,
    Module,
    OutputOptions,
    OxcError,
    Statements,
    check,
    declarations,
    minify,
    transform,
)

TYPESCRIPT = """
import { readFile } from "fs";
export interface Point { x: number; y: number }
export function add(a: number, b: number): number { return a + b; }
const secret: string = "hidden";
add(1, 2);
"""


class ToolTests(unittest.TestCase):
    def test_transform_removes_types(self) -> None:
        output = transform(TYPESCRIPT)
        self.assertNotIn("interface", output.code)
        self.assertNotIn(": number", output.code)
        self.assertIn("function add(a, b)", output.code)
        self.assertIsNone(output.map)

    def test_transform_writes_a_source_map(self) -> None:
        output = transform(TYPESCRIPT, options=OutputOptions(source_map=True))
        self.assertEqual(json.loads(output.map)["version"], 3)

    def test_minify_shrinks(self) -> None:
        source = "function longName(argumentOne) { return argumentOne + 1 }\nconsole.log(longName(2));"
        mangled = minify(source, options=OutputOptions(mangle=True))
        self.assertLess(len(mangled.code), len(minify(source).code))
        self.assertLess(len(minify(source).code), len(source))

    def test_declarations(self) -> None:
        declared = declarations(TYPESCRIPT)
        self.assertIn("export declare function add(a: number, b: number): number;", declared)

    def test_check_reports_without_raising(self) -> None:
        self.assertEqual(check("let a = 1;"), [])
        problems = check("let a = 1; let a = 2;")
        self.assertTrue(problems[0].fatal)
        self.assertIn("a", problems[0].message)
        self.assertIsNotNone(problems[0].start)

    def test_a_syntax_error_carries_its_diagnostics(self) -> None:
        with self.assertRaises(OxcError) as caught:
            transform("const = 1", Language.Js)
        self.assertEqual(caught.exception.code, "syntax")
        self.assertTrue(caught.exception.details.diagnostics[0].fatal)


class ModuleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = Module(TYPESCRIPT, Language.Ts)

    def test_the_constructor_fails_with_diagnostics(self) -> None:
        with self.assertRaises(OxcError) as caught:
            Module("const = 1")
        self.assertEqual(caught.exception.code, "syntax")

    def test_ast_is_estree(self) -> None:
        ast = self.module.ast()
        self.assertEqual(ast["type"], "Program")
        self.assertEqual(ast["body"][0]["type"], "ImportDeclaration")

    def test_ast_text_is_the_same_tree(self) -> None:
        self.assertEqual(json.loads(self.module.ast_text()), self.module.ast())

    def test_a_large_ast_crosses(self) -> None:
        source = "\n".join(f"const v{index} = {{ a: [1, 2, {index}] }};" for index in range(5000))
        self.assertEqual(len(Module(source).ast()["body"]), 5000)

    def test_symbols(self) -> None:
        by_name = {symbol.name: symbol for symbol in self.module.symbols()}
        self.assertEqual(by_name["add"].references, 1)
        self.assertTrue(by_name["add"].top_level)
        self.assertFalse(by_name["a"].top_level)

    def test_in_uses_contains(self) -> None:
        self.assertIn("add", self.module)
        self.assertNotIn("a", self.module)

    def test_source_is_a_property_and_repr_describes(self) -> None:
        self.assertEqual(self.module.source, TYPESCRIPT)
        self.assertIn("Module(Ts", repr(self.module))

    def test_for_over_statements(self) -> None:
        statements = self.module.statements()
        self.assertIsInstance(statements, Statements)
        kinds = [statement.kind for statement in statements]
        self.assertEqual(kinds[0], "ImportDeclaration")
        self.assertEqual(len(kinds), 5)

    def test_a_visitor_runs_and_stops(self) -> None:
        seen = []

        def visitor(statement) -> bool:
            seen.append(statement.kind)
            return len(seen) < 2

        self.assertEqual(self.module.visit(visitor), 2)
        self.assertEqual(seen, ["ImportDeclaration", "ExportNamedDeclaration"])

    def test_an_exception_in_a_visitor_reaches_the_caller(self) -> None:
        def visitor(statement) -> bool:
            raise KeyError("stop")

        with self.assertRaisesRegex(OxcError, "stop"):
            self.module.visit(visitor)


if __name__ == "__main__":
    unittest.main()
