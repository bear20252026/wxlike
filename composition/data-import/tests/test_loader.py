from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from wximport.loader import clear_sql, copy_sql, load_table, sequence_sql
from wximport.model import Column, TablePlan
from wximport.pg import PgColumn, PgRelation


def plan(mode: str = "table") -> TablePlan:
    return TablePlan(
        source="items",
        target="items",
        mode=mode,
        columns=(
            Column("id", "BIGINT", "int", True),
            Column("seq", "BIGINT", "int", True),
            Column("note", "TEXT", "text", False),
        ),
        primary_key=("id",),
    )


def relation() -> PgRelation:
    return PgRelation(
        name="items",
        relkind="r",
        columns=(
            PgColumn("id", "bigint", "int", True, None, "d"),
            PgColumn("seq", "bigint", "int", True, "nextval('items_seq'::regclass)", ""),
            PgColumn("note", "text", "text", False, None, ""),
        ),
    )


class FakePsql:
    def __init__(self) -> None:
        self.calls: list[tuple[str, Path | None]] = []

    def run(self, sql: str, stdin_path: Path | None = None) -> str:
        self.calls.append((sql, stdin_path))
        return ""


class LoaderTest(unittest.TestCase):
    def test_copy_is_null_safe_and_column_scoped(self) -> None:
        sql = copy_sql(plan())
        self.assertIn('COPY "items" ("id", "seq", "note") FROM STDIN', sql)
        self.assertIn("NULL '\\N'", sql)
        self.assertIn("HEADER false", sql)

    def test_clear_uses_delete_for_views_and_tables(self) -> None:
        self.assertEqual(clear_sql(plan()), 'DELETE FROM "items"')
        self.assertEqual(clear_sql(plan("view")), 'DELETE FROM "items"')

    def test_sequence_reset_covers_identity_and_nextval(self) -> None:
        statements = sequence_sql(plan(), relation())
        self.assertEqual(len(statements), 2)
        self.assertTrue(all("setval(" in statement for statement in statements))
        self.assertTrue(any('"seq"' in statement for statement in statements))

    def test_sequence_reset_is_skipped_for_views(self) -> None:
        self.assertEqual(sequence_sql(plan("view"), relation()), [])

    def test_load_order_is_clear_copy_sequence(self) -> None:
        psql = FakePsql()
        with tempfile.TemporaryDirectory() as temp:
            csv_path = Path(temp) / "items.csv"
            csv_path.write_text('"1","1",""\n', encoding="utf-8")
            load_table(psql, plan(), csv_path, relation())
        self.assertEqual(len(psql.calls), 4)
        self.assertTrue(psql.calls[0][0].startswith("DELETE FROM"))
        self.assertTrue(psql.calls[1][0].startswith("COPY"))
        self.assertEqual(psql.calls[1][1], csv_path)
        self.assertTrue(psql.calls[2][0].startswith("SELECT setval("))
        self.assertTrue(psql.calls[3][0].startswith("SELECT setval("))

    def test_missing_csv_is_rejected_before_any_write(self) -> None:
        psql = FakePsql()
        with self.assertRaises(FileNotFoundError):
            load_table(psql, plan(), Path("does-not-exist.csv"), relation())
        self.assertEqual(psql.calls, [])


if __name__ == "__main__":
    unittest.main()
