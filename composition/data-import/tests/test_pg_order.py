from __future__ import annotations

import unittest
from pathlib import Path

from wximport.model import Column, ImportPlan, TablePlan
from wximport.pg import PgColumn, PgReference, PgRelation, apply_catalog


def col(name: str, kind: str = "text") -> Column:
    return Column(name, kind.upper(), kind, True)


def pg_col(name: str, kind: str = "text") -> PgColumn:
    return PgColumn(name, kind, kind, True, None, "")


def plan(*tables: TablePlan) -> ImportPlan:
    return ImportPlan(
        source_path=Path("source.sqlite"),
        source_sha256="a" * 64,
        source_data_version=1,
        tables=list(tables),
    )


def table(source: str, *references: str) -> TablePlan:
    return TablePlan(
        source=source,
        target=source,
        mode="table",
        columns=(col("id"),),
        primary_key=("id",),
    )


class TargetOrderTest(unittest.TestCase):
    def test_target_foreign_key_parent_is_moved_before_child(self) -> None:
        child = table("child")
        parent = table("parent")
        catalog = {
            "child": PgRelation(
                "child", "r", (pg_col("id"),), (PgReference("parent"),)
            ),
            "parent": PgRelation("parent", "r", (pg_col("id"),)),
        }
        result = apply_catalog(plan(child, parent), catalog)
        self.assertEqual([t.source for t in result.tables], ["parent", "child"])
        self.assertIn("reordered", result.warnings[-1])

    def test_compatibility_view_base_table_is_an_alias(self) -> None:
        meta = table("identity_session_meta")
        users = table("users")
        catalog = {
            "identity_session_meta": PgRelation(
                "identity_session_meta", "r", (pg_col("id"),), (PgReference("identity_user"),)
            ),
            "users": PgRelation("users", "v", (pg_col("id"),)),
        }
        result = apply_catalog(plan(meta, users), catalog)
        self.assertEqual([t.source for t in result.tables], ["users", "identity_session_meta"])

    def test_identity_root_users_runs_first_even_without_a_visible_fk(self) -> None:
        sessions = table("sessions")
        users = table("users")
        catalog = {
            "sessions": PgRelation("sessions", "v", (pg_col("id"),)),
            "users": PgRelation("users", "v", (pg_col("id"),)),
        }
        result = apply_catalog(plan(sessions, users), catalog)
        self.assertEqual([t.source for t in result.tables], ["users", "sessions"])
        self.assertTrue(any("identity root" in warning for warning in result.warnings))

    def test_cycle_keeps_every_table_and_warns(self) -> None:
        a = table("a")
        b = table("b")
        catalog = {
            "a": PgRelation("a", "r", (pg_col("id"),), (PgReference("b"),)),
            "b": PgRelation("b", "r", (pg_col("id"),), (PgReference("a"),)),
        }
        result = apply_catalog(plan(a, b), catalog)
        self.assertEqual([t.source for t in result.tables], ["a", "b"])
        self.assertTrue(any("cycle" in warning for warning in result.warnings))
