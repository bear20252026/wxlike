from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from wximport.state import ImportState
from wximport.window import StopWriteWindow


class FakePsql:
    def __init__(self, responses: list[str] | None = None) -> None:
        self.responses = list(responses or [])
        self.calls: list[str] = []

    def run(self, sql: str, stdin_path=None) -> str:
        self.calls.append(sql)
        return self.responses.pop(0) if self.responses else ""


class WindowTest(unittest.TestCase):
    def test_quiet_window(self) -> None:
        window = StopWriteWindow("abc", 7)
        window.record_table("items", 12)
        window.close("abc", 7)
        self.assertTrue(window.quiescent)
        self.assertGreaterEqual(window.duration_ms, 0)
        self.assertEqual(window.table_ms, {"items": 12})

    def test_changed_source_is_not_quiet(self) -> None:
        window = StopWriteWindow("abc", 7)
        window.close("def", 7)
        self.assertFalse(window.quiescent)
        self.assertFalse(window.to_dict()["quiescent"])


class StateTest(unittest.TestCase):
    def test_new_run_and_table_transitions(self) -> None:
        psql = FakePsql(["", "", ""])
        state = ImportState(psql, "run-1")
        plan = type("P", (), {"source_sha256": "abc", "source_data_version": 3, "source_path": Path("x.db"), "tables": []})()
        info = state.ensure_run(plan, resume=False)
        self.assertEqual(info.status, "running")
        self.assertTrue(any("INSERT INTO import_run" in call for call in psql.calls))
        state.finish("verified", True, 12, "ok")
        self.assertTrue(any("status = 'verified'" in call for call in psql.calls))
        self.assertTrue(any("quiescent = TRUE" in call for call in psql.calls))

    def test_existing_run_requires_resume_and_same_hash(self) -> None:
        psql = FakePsql(['{"run_id":"run-1","source_sha256":"abc","started_at":1,"status":"failed"}'])
        state = ImportState(psql, "run-1")
        plan = type("P", (), {"source_sha256": "abc", "source_data_version": 3, "source_path": Path("x.db"), "tables": []})()
        with self.assertRaises(RuntimeError):
            state.ensure_run(plan, resume=False)
        psql.responses.append('{"run_id":"run-1","source_sha256":"abc","started_at":1,"status":"failed"}')
        info = state.ensure_run(plan, resume=True)
        self.assertEqual(info.status, "failed")

    def test_register_marks_each_table_pending(self) -> None:
        psql = FakePsql(["[]", "", ""])
        state = ImportState(psql, "run-1")
        table_a = type("T", (), {"source": "a", "target": "a"})()
        table_b = type("T", (), {"source": "b", "target": "b"})()
        plan = type("P", (), {"tables": [table_a, table_b]})()
        state.register(plan)
        self.assertEqual({key: row["status"] for key, row in state.rows.items()}, {"a": "pending", "b": "pending"})
        self.assertTrue(any("INSERT INTO import_table" in call for call in psql.calls))

    def test_loaded_then_verified_stores_both_digests(self) -> None:
        psql = FakePsql(["", ""])
        state = ImportState(psql, "run-1")
        state.rows["a"] = {
            "source_table": "a",
            "target_table": "a",
            "source_rows": -1,
            "loaded_rows": -1,
            "source_digest": "",
            "target_digest": "",
            "status": "pending",
            "started_at": 0,
            "finished_at": 0,
            "error": "",
        }
        state.loaded("a", 3, "src")
        state.verified("a", "dst")
        self.assertEqual(state.rows["a"]["loaded_rows"], 3)
        self.assertEqual(state.rows["a"]["source_digest"], "src")
        self.assertEqual(state.rows["a"]["target_digest"], "dst")
        self.assertEqual(state.rows["a"]["status"], "verified")
        self.assertTrue(any("source_digest = 'src'" in call for call in psql.calls))


if __name__ == "__main__":
    unittest.main()
