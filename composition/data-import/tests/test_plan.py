from __future__ import annotations

import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

from wximport.plan import build_plan, dependency_order, plan_payload
from wximport.sqlite_source import source_table


class PlanTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "source.db"
        conn = sqlite3.connect(self.path)
        conn.executescript(
            """
            PRAGMA foreign_keys = ON;
            CREATE TABLE parent (id TEXT PRIMARY KEY, name TEXT NOT NULL);
            CREATE TABLE child (
                id TEXT PRIMARY KEY,
                parent_id TEXT NOT NULL REFERENCES parent(id),
                body TEXT
            );
            CREATE TABLE audit (id INTEGER PRIMARY KEY, note TEXT);
            """
        )
        conn.execute("INSERT INTO parent VALUES ('p1', 'P')")
        conn.execute("INSERT INTO child VALUES ('c1', 'p1', '')")
        conn.commit()
        conn.close()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_parent_is_planned_before_child(self) -> None:
        plan = build_plan(self.path)
        names = [table.source for table in plan.tables]
        self.assertLess(names.index("parent"), names.index("child"))
        self.assertIn("audit", names)
        self.assertEqual(plan.skipped, [])
        self.assertEqual(plan.tables[0].mode, "table")

    def test_mapping_can_skip_and_rename(self) -> None:
        mapping = Path(self.temp.name) / "map.json"
        mapping.write_text(
            json.dumps({"skip": {"audit": "derived elsewhere"}, "targets": {"child": "child_v2"}}),
            encoding="utf-8",
        )
        plan = build_plan(self.path, mapping)
        self.assertEqual([(name, reason) for name, reason in plan.skipped], [("audit", "derived elsewhere")])
        child = next(table for table in plan.tables if table.source == "child")
        self.assertEqual(child.target, "child_v2")

    def test_payload_contains_both_shapes(self) -> None:
        payload = plan_payload(build_plan(self.path))
        self.assertEqual(payload["source"]["sha256"], build_plan(self.path).source_sha256)
        child = next(table for table in payload["tables"] if table["source"] == "child")
        self.assertEqual([column["name"] for column in child["columns"]], ["id", "parent_id", "body"])


class DependencyOrderTest(unittest.TestCase):
    def test_cycle_is_broken_deterministically_with_a_warning(self) -> None:
        conn = sqlite3.connect(":memory:")
        conn.executescript(
            """
            CREATE TABLE a (id TEXT PRIMARY KEY, b_id TEXT REFERENCES b(id));
            CREATE TABLE b (id TEXT PRIMARY KEY, a_id TEXT REFERENCES a(id));
            """
        )
        tables = {"a": source_table(conn, "a"), "b": source_table(conn, "b")}
        first, warnings_a = dependency_order(tables)
        second, warnings_b = dependency_order(tables)
        conn.close()
        self.assertEqual(first, second)
        self.assertEqual(warnings_a, warnings_b)
        self.assertTrue(any("cycle" in warning for warning in warnings_a))
        self.assertEqual(sorted(first), ["a", "b"])


if __name__ == "__main__":
    unittest.main()
