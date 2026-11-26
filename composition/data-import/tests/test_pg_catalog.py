from __future__ import annotations

import io
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

from wximport.pg import (
    PgColumn,
    PgRelation,
    PlanError,
    _iter_csv_rows,
    apply_catalog,
    kind_for_pg_type,
    parse_catalog,
)
from wximport.plan import build_plan


CATALOG = json.dumps(
    [
        {
            "name": "items",
            "kind": "v",
            "columns": [
                {"name": "id", "type": "text", "not_null": False, "default": None, "identity": ""},
                {"name": "n", "type": "bigint", "not_null": False, "default": None, "identity": ""},
                {"name": "note", "type": "text", "not_null": False, "default": None, "identity": ""},
                {
                    "name": "rowid",
                    "type": "text",
                    "not_null": False,
                    "default": "ctid::text",
                    "identity": "",
                },
            ],
        },
        {
            "name": "items_table",
            "kind": "r",
            "columns": [
                {"name": "id", "type": "text", "not_null": True, "default": None, "identity": ""},
                {"name": "n", "type": "bigint", "not_null": True, "default": "0", "identity": ""},
                {"name": "note", "type": "text", "not_null": True, "default": "''", "identity": ""},
            ],
        },
    ]
)


class PgCatalogTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "source.db"
        conn = sqlite3.connect(self.path)
        conn.executescript(
            """
            CREATE TABLE items (
                id TEXT PRIMARY KEY,
                n INTEGER NOT NULL,
                note TEXT
            );
            """
        )
        conn.execute("INSERT INTO items VALUES ('i1', 2, '')")
        conn.commit()
        conn.close()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def plan(self):
        return build_plan(self.path)

    def test_parses_relation_kinds_and_column_types(self) -> None:
        catalog = parse_catalog(CATALOG)
        self.assertEqual(catalog["items"].mode, "view")
        self.assertEqual(catalog["items_table"].mode, "table")
        self.assertEqual(catalog["items"].columns[1].kind, "int")
        self.assertEqual(catalog["items"].columns[0].kind, "text")
        self.assertEqual(kind_for_pg_type("numeric(20,0)"), "decimal")
        self.assertEqual(kind_for_pg_type("bytea"), "bytes")

    def test_copy_stream_preserves_embedded_carriage_return(self) -> None:
        rows = list(_iter_csv_rows(io.BytesIO(b'"id","line1\r\nline2"\n')))
        self.assertEqual(rows, [["id", "line1\r\nline2"]])

    def test_view_target_can_have_a_derived_column(self) -> None:
        catalog = parse_catalog(CATALOG)
        plan = self.plan()
        plan.tables[0].target = "items"
        result = apply_catalog(plan, catalog)
        self.assertEqual(result.tables[0].mode, "view")
        self.assertTrue(any("rowid" in warning for warning in result.warnings))

    def test_missing_target_is_rejected(self) -> None:
        with self.assertRaises(PlanError):
            apply_catalog(self.plan(), {})

    def test_source_only_column_is_rejected(self) -> None:
        catalog = {
            "items": PgRelation(
                name="items",
                relkind="r",
                columns=(
                    PgColumn("id", "text", "text", True, None, ""),
                    PgColumn("n", "bigint", "int", True, None, ""),
                ),
            )
        }
        with self.assertRaises(PlanError):
            apply_catalog(self.plan(), catalog)

    def test_integer_source_can_fill_numeric_target(self) -> None:
        catalog = {
            "items": PgRelation(
                name="items",
                relkind="r",
                columns=(
                    PgColumn("id", "text", "text", True, None, ""),
                    PgColumn("n", "numeric(20,0)", "decimal", True, None, ""),
                    PgColumn("note", "text", "text", False, None, ""),
                ),
            )
        }
        result = apply_catalog(self.plan(), catalog)
        self.assertEqual(result.tables[0].mode, "table")

    def test_incompatible_type_is_rejected(self) -> None:
        catalog = {
            "items": PgRelation(
                name="items",
                relkind="r",
                columns=(
                    PgColumn("id", "text", "text", True, None, ""),
                    PgColumn("n", "text", "text", True, None, ""),
                    PgColumn("note", "text", "text", True, None, ""),
                ),
            )
        }
        with self.assertRaises(PlanError):
            apply_catalog(self.plan(), catalog)

    def test_not_null_without_default_is_rejected_on_a_table(self) -> None:
        catalog = {
            "items": PgRelation(
                name="items",
                relkind="r",
                columns=(
                    PgColumn("id", "text", "text", True, None, ""),
                    PgColumn("n", "bigint", "int", True, None, ""),
                    PgColumn("note", "text", "text", True, None, ""),
                    PgColumn("must", "text", "text", True, None, ""),
                ),
            )
        }
        with self.assertRaises(PlanError):
            apply_catalog(self.plan(), catalog)


if __name__ == "__main__":
    unittest.main()
