from __future__ import annotations

import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

from wximport.runner import RunnerConfig, run_import


class RunnerTest(unittest.TestCase):
    def test_plan_only_writes_a_reviewable_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            db = root / "source.db"
            conn = sqlite3.connect(db)
            conn.executescript(
                """
                CREATE TABLE parent (id TEXT PRIMARY KEY, name TEXT NOT NULL);
                CREATE TABLE child (id TEXT PRIMARY KEY, parent_id TEXT REFERENCES parent(id));
                """
            )
            conn.close()
            out = root / "plan.json"
            config = RunnerConfig(
                sqlite=db,
                dsn="",
                work_dir=root / "work",
                plan_only=True,
                plan_out=out,
            )
            report = run_import(config)
            self.assertEqual(report["status"], "planned")
            self.assertEqual(report["tables"], 2)
            payload = json.loads(out.read_text(encoding="utf-8"))
            self.assertEqual([table["source"] for table in payload["tables"]], ["parent", "child"])


if __name__ == "__main__":
    unittest.main()
