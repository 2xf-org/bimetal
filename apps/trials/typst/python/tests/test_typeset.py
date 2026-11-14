"""
Typst through the generated Python package.
"""

from __future__ import annotations

import unittest

from typeset import Compiler, Document, Level, TypstError, typst_version

SOURCE = """
#set page(width: 10cm, height: auto)
= Hello from Bimetal
Typst #sys.version compiled this. $ integral_0^1 x^2 dif x = 1/3 $
#pagebreak()
Second page.
"""


class CompileTests(unittest.TestCase):
    def setUp(self) -> None:
        self.compiler = Compiler()
        self.compiler.add_source("main.typ", SOURCE)

    def test_version(self) -> None:
        self.assertRegex(typst_version(), r"^\d+\.\d+")

    def test_bundled_fonts_load(self) -> None:
        self.assertGreater(self.compiler.font_count, 10)

    def test_compile_returns_a_document(self) -> None:
        document = self.compiler.compile("main.typ")
        self.assertIsInstance(document, Document)
        self.assertEqual(len(document), 2)
        self.assertEqual(document.warnings, [])

    def test_pdf(self) -> None:
        pdf = self.compiler.compile("main.typ").pdf()
        self.assertTrue(pdf.startswith(b"%PDF-"))
        self.assertGreater(len(pdf), 1000)

    def test_svg_and_png(self) -> None:
        document = self.compiler.compile("main.typ")
        self.assertIn("<svg", document.svg(0))
        self.assertEqual(document.png(1)[:4], b"\x89PNG")
        self.assertLess(len(document.png(0, 1.0)), len(document.png(0, 3.0)))

    def test_for_over_pages(self) -> None:
        pages = list(self.compiler.compile("main.typ").pages())
        self.assertEqual(len(pages), 2)
        self.assertTrue(all("<svg" in page for page in pages))

    def test_with_closes_the_document(self) -> None:
        with self.compiler.compile("main.typ") as document:
            self.assertEqual(len(document), 2)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            len(document)

    def test_a_compile_error_carries_its_diagnostics(self) -> None:
        self.compiler.add_source("bad.typ", "#let x = \n#unknown()")
        with self.assertRaises(TypstError) as caught:
            self.compiler.compile("bad.typ")
        self.assertEqual(caught.exception.code, "compile")
        diagnostics = caught.exception.details.diagnostics
        self.assertGreaterEqual(len(diagnostics), 1)
        self.assertEqual(diagnostics[0].level, Level.Error)
        self.assertEqual(diagnostics[0].path, "/bad.typ")
        self.assertIsNotNone(diagnostics[0].line)

    def test_a_page_past_the_end_is_a_typed_error(self) -> None:
        document = self.compiler.compile("main.typ")
        with self.assertRaises(TypstError) as caught:
            document.svg(9)
        self.assertEqual(caught.exception.code, "page_out_of_range")
        self.assertEqual(caught.exception.details.count, 2)

    def test_files_and_fonts(self) -> None:
        with self.assertRaises(TypstError) as caught:
            self.compiler.add_font(b"not a font")
        self.assertEqual(caught.exception.code, "invalid_font")
        self.compiler.add_file("data.csv", b"a,b\n1,2\n")
        self.compiler.add_source("csv.typ", '#csv("data.csv").len()')
        self.assertEqual(len(self.compiler.compile("csv.typ")), 1)


if __name__ == "__main__":
    unittest.main()
