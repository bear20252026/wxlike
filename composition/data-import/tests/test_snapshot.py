from __future__ import annotations

import sqlite3
import tempfile
import unittest
from pathlib import Path

from wximport.snapshot import SnapshotError, build_snapshot_database, parse_snapshot

ROOT = Path(__file__).resolve().parents[3]


class ParseSnapshotTest(unittest.TestCase):
    def test_composite_primary_key_and_not_null(self) -> None:
        tables = parse_snapshot(
            """
            # comment
            users.id TEXT notnull=1 pk=1
            users.name TEXT notnull=1 pk=0
            users.age INTEGER notnull=0 pk=0
            """
        )
        self.assertEqual([table.name for table in tables], ["users"])
        self.assertEqual([column.name for column in tables[0].columns], ["id", "name", "age"])
        self.assertEqual(tables[0].columns[0].primary_key_order, 1)
        self.assertTrue(tables[0].columns[0].not_null)

    def test_composite_primary_key_positions_must_be_contiguous(self) -> None:
        with self.assertRaisesRegex(SnapshotError, "contiguous"):
            parse_snapshot("t.a TEXT pk=1\nt.b TEXT pk=3\n")

    def test_duplicate_column_is_rejected(self) -> None:
        with self.assertRaisesRegex(SnapshotError, "duplicate"):
            parse_snapshot("t.a TEXT pk=0\nt.a TEXT pk=0\n")

    def test_unknown_type_is_rejected(self) -> None:
        with self.assertRaisesRegex(SnapshotError, "unsupported"):
            parse_snapshot("t.a JSON pk=0\n")


class SnapshotDatabaseTest(unittest.TestCase):
    def test_builds_an_empty_database_with_the_declared_pk(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            snapshot = Path(temp) / "snapshot.txt"
            sqlite_path = Path(temp) / "source.sqlite"
            snapshot.write_text(
                "parent.id TEXT notnull=1 pk=1\nparent.name TEXT notnull=1 pk=0\n"
                "child.id TEXT notnull=1 pk=1\nchild.parent_id TEXT notnull=1 pk=0\n",
                encoding="utf-8",
            )
            tables = build_snapshot_database(snapshot, sqlite_path)
            self.assertEqual(len(tables), 2)
            conn = sqlite3.connect(sqlite_path)
            try:
                self.assertEqual(conn.execute("SELECT COUNT(*) FROM parent").fetchone()[0], 0)
                info = conn.execute("PRAGMA table_info(parent)").fetchall()
            finally:
                conn.close()
            self.assertEqual([row[1] for row in info], ["id", "name"])
            self.assertEqual([row[5] for row in info], [1, 0])

    def test_platform_snapshot_has_all_110_tables(self) -> None:
        tables = parse_snapshot(
            (ROOT / "crates" / "platform" / "tests" / "schema_snapshot.txt").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(len(tables), 110)
        self.assertEqual(sum(len(table.columns) for table in tables), 704)
        names = {table.name for table in tables}
        self.assertIn("identity_session_token", names)
        self.assertIn("wallets", names)
