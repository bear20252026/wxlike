from __future__ import annotations

import unittest
from pathlib import Path

import census
from wximport.model import Column, ImportPlan, TablePlan
from wximport.pg import PgColumn, PgRelation


def plan_with(*tables: TablePlan) -> ImportPlan:
    return ImportPlan(
        source_path=Path("source.sqlite"),
        source_sha256="a" * 64,
        source_data_version=1,
        tables=list(tables),
    )


class CensusTest(unittest.TestCase):
    def test_all_tables_verified(self) -> None:
        table = TablePlan(
            source="users",
            target="users",
            mode="table",
            columns=(Column("id", "TEXT", "text", True),),
            primary_key=("id",),
        )
        catalog = {
            "users": PgRelation(
                name="users",
                relkind="r",
                columns=(PgColumn("id", "text", "text", True, None, ""),),
            )
        }
        payload = census.census_payload(plan_with(table), catalog)
        self.assertTrue(payload["ok"])
        self.assertEqual(payload["verified"], 1)
        self.assertEqual(payload["failures"], [])

    def test_missing_target_is_reported_per_table(self) -> None:
        table = TablePlan(
            source="audit",
            target="audit",
            mode="table",
            columns=(Column("id", "INTEGER", "int", True),),
            primary_key=("id",),
        )
        payload = census.census_payload(plan_with(table), {})
        self.assertFalse(payload["ok"])
        self.assertEqual(payload["verified"], 0)
        self.assertIn("missing", payload["failures"][0]["error"])

    def test_source_only_column_is_reported(self) -> None:
        table = TablePlan(
            source="channel_role",
            target="channel_role",
            mode="table",
            columns=(
                Column("channel_id", "TEXT", "text", True),
                Column("created_at", "INTEGER", "int", True),
            ),
            primary_key=("channel_id",),
        )
        catalog = {
            "channel_role": PgRelation(
                name="channel_role",
                relkind="r",
                columns=(PgColumn("channel_id", "text", "text", True, None, ""),),
            )
        }
        payload = census.census_payload(plan_with(table), catalog)
        self.assertFalse(payload["ok"])
        self.assertIn("created_at", payload["failures"][0]["error"])
