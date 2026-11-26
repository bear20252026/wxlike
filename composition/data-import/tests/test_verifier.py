from __future__ import annotations

import unittest

from wximport.codec import RowDigest
from wximport.model import Column, TablePlan
from wximport.verifier import copy_out_sql, decode_target, verify_table


KINDS = ["text", "int", "bytes", "text"]


def plan() -> TablePlan:
    return TablePlan(
        source="items",
        target="items",
        mode="table",
        columns=(
            Column("id", "TEXT", "text", True),
            Column("n", "INTEGER", "int", True),
            Column("payload", "BLOB", "bytes", False),
            Column("note", "TEXT", "text", False),
        ),
        primary_key=("id",),
    )


def expected_digest() -> str:
    digest = RowDigest()
    digest.add(["a", 1, b"\x00\xff", ""], KINDS)
    digest.add(["b", 2, b"", None], KINDS)
    return digest.finish()


class FakePsql:
    def __init__(self, rows: list[list[str]]) -> None:
        self.rows = rows
        self.sql = ""

    def iter_copy_rows(self, sql: str):
        self.sql = sql
        for row in self.rows:
            yield row


class VerifierTest(unittest.TestCase):
    def test_decode_distinguishes_null_empty_and_bytes(self) -> None:
        self.assertIsNone(decode_target("\\N", "text"))
        self.assertEqual(decode_target("", "text"), "")
        self.assertEqual(decode_target("\\x00ff", "bytes"), b"\x00\xff")
        self.assertEqual(decode_target("t", "bool"), True)
        self.assertEqual(decode_target("2", "int"), 2)

    def test_copy_out_selects_only_planned_columns(self) -> None:
        sql = copy_out_sql(plan())
        self.assertIn('COPY (SELECT "id", "n", "payload", "note" FROM "items")', sql)
        self.assertIn("NULL '\\N'", sql)

    def test_identical_rows_pass(self) -> None:
        psql = FakePsql([["a", "1", "\\x00ff", ""], ["b", "2", "\\x", "\\N"]])
        result = verify_table(psql, plan(), 2, expected_digest())
        self.assertTrue(result.ok)
        self.assertEqual(result.target_rows, 2)

    def test_missing_row_fails_on_count(self) -> None:
        psql = FakePsql([["a", "1", "\\x00ff", ""]])
        result = verify_table(psql, plan(), 2, expected_digest())
        self.assertFalse(result.ok)
        self.assertIn("row count", result.reason)

    def test_changed_value_fails_on_digest(self) -> None:
        psql = FakePsql([["a", "1", "\\x00ff", "changed"], ["b", "2", "\\x", "\\N"]])
        result = verify_table(psql, plan(), 2, expected_digest())
        self.assertFalse(result.ok)
        self.assertEqual(result.reason, "row digest differs")


if __name__ == "__main__":
    unittest.main()
