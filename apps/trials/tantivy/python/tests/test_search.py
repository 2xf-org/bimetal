"""Tantivy through the generated Python package."""

import tempfile
import unittest

from search import (
    BulkWriter,
    FieldKind,
    FieldSpec,
    Index,
    SearchError,
    Searcher,
    Writer,
    first_text,
)

FIELDS = [
    FieldSpec(name="title", kind=FieldKind.Text, stored=True),
    FieldSpec(name="body", kind=FieldKind.Text),
    FieldSpec(name="year", kind=FieldKind.Integer, stored=True),
]
BOOKS = [
    {"title": "The Old Man and the Sea", "body": "an old man who fished alone", "year": 1952},
    {"title": "Of Mice and Men", "body": "the river runs deep and green", "year": 1937},
    {"title": "Frankenstein", "body": "no disaster has accompanied the commencement", "year": 1818},
]


class WithTests(unittest.TestCase):
    def test_a_finished_with_block_commits(self) -> None:
        index = Index(FIELDS)
        with index.writer() as writer:
            self.assertIsInstance(writer, Writer)
            for book in BOOKS:
                writer.add(book)
        self.assertEqual(len(index.searcher()), 3)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            writer.add(BOOKS[0])

    def test_a_raising_with_block_rolls_back(self) -> None:
        index = Index(FIELDS)
        with index.writer() as writer:
            writer.add(BOOKS[0])
        with self.assertRaisesRegex(ValueError, "boom"):
            with index.writer() as writer:
                writer.add(BOOKS[1])
                raise ValueError("boom")
        self.assertEqual(len(index.searcher()), 1)

    def test_a_closed_writer_skips_the_exit_hook(self) -> None:
        index = Index(FIELDS)
        with index.writer() as writer:
            writer.add(BOOKS[0])
            writer.close()
        self.assertEqual(len(index.searcher()), 0)

    def test_an_exit_error_reaches_the_caller_as_its_type(self) -> None:
        index = Index(FIELDS)
        with self.assertRaises(SearchError) as caught:
            with index.writer() as writer:
                writer.add({"year": "not a number"})
        self.assertEqual(caught.exception.code, "document")


class SearchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.index = Index(FIELDS)
        with self.index.writer() as writer:
            for book in BOOKS:
                writer.add(book)
        self.searcher = self.index.searcher()

    def test_the_searcher_is_a_resource(self) -> None:
        self.assertIsInstance(self.searcher, Searcher)

    def test_search_returns_typed_hits(self) -> None:
        hits = self.searcher.search("sea")
        self.assertEqual(len(hits), 1)
        self.assertEqual(first_text(hits[0].document, "title"), "The Old Man and the Sea")
        self.assertGreater(hits[0].score, 0)

    def test_limit_has_a_default_and_a_keyword(self) -> None:
        self.assertEqual(len(self.searcher.search("the OR of")), 3)
        self.assertEqual(len(self.searcher.search("the OR of", limit=1)), 1)
        self.assertEqual(len(self.searcher.search("sea", ["title"], 1)), 1)

    def test_for_over_hits(self) -> None:
        titles = [
            first_text(hit.document, "title")
            for hit in self.searcher.hits("the OR of", page_size=2)
        ]
        self.assertEqual(sorted(titles), sorted(book["title"] for book in BOOKS))

    def test_a_bad_query_is_typed_and_carries_its_reason(self) -> None:
        with self.assertRaises(SearchError) as caught:
            self.searcher.search("title:")
        self.assertEqual(caught.exception.code, "query")
        self.assertTrue(caught.exception.details.reason)

    def test_an_unknown_field_carries_its_name(self) -> None:
        with self.assertRaises(SearchError) as caught:
            self.searcher.search("sea", ["missing"])
        self.assertEqual(caught.exception.details.name, "missing")

    def test_fields_is_a_property(self) -> None:
        self.assertEqual([field.name for field in self.index.fields], ["title", "body", "year"])

    def test_reload_sees_later_commits(self) -> None:
        with self.index.writer() as writer:
            writer.add({"title": "Dune", "body": "spice", "year": 1965})
        self.assertEqual(len(self.searcher), 3)
        self.searcher.reload()
        self.assertEqual(len(self.searcher), 4)


class BulkTests(unittest.TestCase):
    def test_finish_consumes_the_writer_and_returns_an_index(self) -> None:
        writer = BulkWriter(FIELDS)
        for book in BOOKS:
            writer.add(book)
        index = writer.finish()
        self.assertIsInstance(index, Index)
        self.assertEqual(index.searcher().count("man"), 1)
        with self.assertRaisesRegex(RuntimeError, "closed"):
            writer.add(BOOKS[0])


class DirectoryTests(unittest.TestCase):
    def test_an_index_survives_on_disk(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with Index.create(directory, FIELDS) as index, index.writer() as writer:
                writer.add(BOOKS[0])
            with Index.open(directory) as index:
                self.assertEqual(len(index.searcher()), 1)


if __name__ == "__main__":
    unittest.main()
