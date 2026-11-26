from __future__ import annotations

import sqlite3
import tempfile
import unittest
from pathlib import Path

import preflight
from wximport.pg import PgColumn, PgRelation


class FakePsql:
    def __init__(self, version: str = "160015", control: str = "import_run|import_table"):
        self.version = version
        self.control = control

    def run(self, sql: str) -> str:
        if "server_version_num" in sql:
            return self.version
        if "current_database" in sql:
            return "wxlike"
        if "current_user" in sql:
            return "artex"
        if "to_regclass" in sql:
            return self.control
        raise AssertionError(f"unexpected SQL: {sql}")

    def catalog(self) -> dict:
        return {
            "users": PgRelation(
                "users", "v", (PgColumn("id", "text", "text", True, None, ""),)
            )
        }


class PreflightTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "source.db"
        conn = sqlite3.connect(self.path)
        conn.execute("CREATE TABLE users (id TEXT PRIMARY KEY)")
        conn.commit()
        conn.close()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_clean_preflight(self) -> None:
        payload = preflight.preflight_payload(
            self.path, FakePsql(), min_pg_version=150000
        )
        self.assertTrue(payload["ok"])
        self.assertEqual(payload["source"]["tables"], 1)
        self.assertEqual(payload["verified"], 1)
        self.assertEqual(payload["target"]["server_version_num"], 160015)
        self.assertFalse(payload["failures"])

    def test_old_server_is_a_failure(self) -> None:
        payload = preflight.preflight_payload(
            self.path, FakePsql(version="150000"), min_pg_version=160000
        )
        self.assertFalse(payload["ok"])
        self.assertIn("below required", payload["failures"][0])

    def test_missing_import_control_is_a_failure(self) -> None:
        payload = preflight.preflight_payload(
            self.path, FakePsql(control=""), min_pg_version=150000
        )
        self.assertFalse(payload["ok"])
        self.assertTrue(any("M023" in failure for failure in payload["failures"]))

    def test_missing_target_is_reported(self) -> None:
        # Rename the source so the fake catalog no longer matches.
        conn = sqlite3.connect(self.path)
        conn.execute("ALTER TABLE users RENAME TO missing")
        conn.commit()
        conn.close()
        payload = preflight.preflight_payload(
            self.path, FakePsql(), min_pg_version=150000
        )
        self.assertFalse(payload["ok"])
        self.assertTrue(any("missing" in failure for failure in payload["failures"]))
