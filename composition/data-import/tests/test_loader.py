from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from wximport.loader import (
    atomic_script,
    clear_sql,
    clear_targets,
    copy_sql,
    load_all_atomic,
    load_table,
    sequence_sql,
    stage_ddl,
    view_insert_sql,
    view_stage,
)
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


def relation(relkind: str = "r") -> PgRelation:
    return PgRelation(
        name="items",
        relkind=relkind,
        columns=(
            PgColumn("id", "bigint", "int", True, None, "d"),
            PgColumn("seq", "bigint", "int", True, "nextval('items_seq'::regclass)", ""),
            PgColumn("note", "text", "text", False, None, ""),
        ),
    )


class FakePsql:
    def __init__(self) -> None:
        self.calls: list[tuple[str, Path | None]] = []
        self.scripts: list[str] = []

    def run(self, sql: str, stdin_path: Path | None = None) -> str:
        self.calls.append((sql, stdin_path))
        return ""

    def run_script(self, script: str) -> None:
        self.scripts.append(script)


class LoaderTest(unittest.TestCase):
    def test_copy_is_null_safe_and_column_scoped(self) -> None:
        sql = copy_sql(plan())
        self.assertIn('COPY "items" ("id", "seq", "note") FROM STDIN', sql)
        self.assertIn("NULL '\\N'", sql)
        self.assertIn("HEADER false", sql)

    def test_clear_uses_delete_for_views_and_tables(self) -> None:
        self.assertEqual(clear_sql(plan()), 'DELETE FROM "items"')
        self.assertEqual(clear_sql(plan("view")), 'DELETE FROM "items"')

    def test_clear_targets_reverses_order_and_skips_verified(self) -> None:
        parent = plan()
        parent.source, parent.target = "parent", "parent"
        child = plan()
        child.source, child.target = "child", "child"
        psql = FakePsql()
        clear_targets(
            psql,
            [parent, child],
            {"parent": {"status": "verified"}, "child": {"status": "failed"}},
        )
        self.assertEqual([call[0] for call in psql.calls], ['DELETE FROM "child"'])

    def test_sequence_reset_covers_identity_and_nextval(self) -> None:
        statements = sequence_sql(plan(), relation())
        self.assertEqual(len(statements), 2)
        self.assertTrue(all("setval(" in statement for statement in statements))
        self.assertTrue(any('"seq"' in statement for statement in statements))
        self.assertTrue(all("GREATEST(" in statement for statement in statements))
        self.assertFalse(any("MAX(\"id\") FROM \"items\"), 0) + 1" in statement for statement in statements))

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

    def test_view_is_staged_then_inserted_once(self) -> None:
        psql = FakePsql()
        with tempfile.TemporaryDirectory() as temp:
            csv_path = Path(temp) / "items.csv"
            csv_path.write_text('"1","1",""\\n', encoding="utf-8")
            load_table(psql, plan("view"), csv_path, relation("v"))
        stage = view_stage(plan("view"))
        self.assertEqual(len(psql.calls), 5)
        self.assertTrue(psql.calls[0][0].startswith("DELETE FROM"))
        self.assertEqual(psql.calls[1][0], f'DROP TABLE IF EXISTS "{stage}"')
        self.assertEqual(
            psql.calls[2][0],
            stage_ddl(plan("view"), relation("v"), stage),
        )
        self.assertEqual(psql.calls[3][0], copy_sql(plan("view"), stage))
        self.assertEqual(psql.calls[3][1], csv_path)
        self.assertEqual(
            psql.calls[4][0],
            view_insert_sql(plan("view"), stage) + '; DROP TABLE "' + stage + '"',
        )

    def test_atomic_script_clears_children_first_and_commits_once(self) -> None:
        parent = plan()
        parent.source, parent.target = "parent", "parent"
        child = plan()
        child.source, child.target = "child", "child"
        with tempfile.TemporaryDirectory() as temp:
            parent_csv = Path(temp) / "parent.csv"
            child_csv = Path(temp) / "child.csv"
            parent_csv.write_text('"1","1",""\n', encoding="utf-8")
            child_csv.write_text('"2","2",""\n', encoding="utf-8")
            script = atomic_script(
                [(parent, relation(), parent_csv), (child, relation(), child_csv)]
            )
        lines = script.splitlines()
        self.assertEqual(lines[0], "BEGIN;")
        self.assertEqual(lines[-1], "COMMIT;")
        self.assertEqual(lines[1], 'DELETE FROM "child";')
        self.assertEqual(lines[2], 'DELETE FROM "parent";')
        self.assertEqual(script.count("BEGIN;"), 1)
        self.assertEqual(script.count("COMMIT;"), 1)
        self.assertIn('\\copy "parent" ("id", "seq", "note")', script)
        self.assertIn('\\copy "child" ("id", "seq", "note")', script)

    def test_atomic_view_uses_one_staging_table(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            csv_path = Path(temp) / "items.csv"
            csv_path.write_text('"1","1",""\n', encoding="utf-8")
            script = atomic_script([(plan("view"), relation("v"), csv_path)])
        stage = view_stage(plan("view"))
        self.assertIn(f'DROP TABLE IF EXISTS "{stage}";', script)
        self.assertIn(f'CREATE TABLE "{stage}"', script)
        self.assertIn(f'\\copy "{stage}"', script)
        self.assertIn(view_insert_sql(plan("view"), stage) + ";", script)
        self.assertIn(f'DROP TABLE "{stage}";', script)

    def test_load_all_atomic_checks_every_csv_before_the_script(self) -> None:
        psql = FakePsql()
        with tempfile.TemporaryDirectory() as temp:
            csv_path = Path(temp) / "items.csv"
            csv_path.write_text('"1","1",""\n', encoding="utf-8")
            load_all_atomic(psql, [(plan(), relation(), csv_path)])
        self.assertEqual(len(psql.scripts), 1)
        self.assertIn('\\copy "items"', psql.scripts[0])

    def test_load_all_atomic_rejects_missing_csv_before_any_write(self) -> None:
        psql = FakePsql()
        with self.assertRaises(FileNotFoundError):
            load_all_atomic(psql, [(plan(), relation(), Path("does-not-exist.csv"))])
        self.assertEqual(psql.scripts, [])

    def test_missing_csv_is_rejected_before_any_write(self) -> None:
        psql = FakePsql()
        with self.assertRaises(FileNotFoundError):
            load_table(psql, plan(), Path("does-not-exist.csv"), relation())
        self.assertEqual(psql.calls, [])


if __name__ == "__main__":
    unittest.main()
