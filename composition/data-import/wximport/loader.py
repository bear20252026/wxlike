"""Load exported CSV files into PostgreSQL through ``COPY``."""

from __future__ import annotations

from pathlib import Path

from .model import TablePlan
from .pg import PgRelation, Psql


def ident(name: str) -> str:
    if "\x00" in name:
        raise ValueError("identifier contains NUL")
    return '"' + name.replace('"', '""') + '"'


def literal(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


def copy_sql(plan: TablePlan) -> str:
    columns = ", ".join(ident(column.name) for column in plan.columns)
    return (
        f"COPY {ident(plan.target)} ({columns}) FROM STDIN "
        "WITH (FORMAT csv, HEADER false, NULL '\\N')"
    )


def sequence_sql(plan: TablePlan, relation: PgRelation) -> list[str]:
    """Reset identity/serial sequences after explicit IDs were imported.

    Without this, the first post-cutover insert either collides with an imported
    id or jumps backwards. ``setval(..., max+1, true)`` leaves the next value
    exactly at max+1; an empty table gets ``1, false``.
    """

    statements: list[str] = []
    if plan.mode == "view":
        return statements
    source_columns = {column.name for column in plan.columns}
    for column in relation.columns:
        if column.name not in source_columns:
            continue
        is_serial = column.identity in {"a", "d"} or bool(
            column.default and "nextval(" in column.default
        )
        if not is_serial:
            continue
        table = ident(plan.target)
        column_sql = ident(column.name)
        table_literal = literal(ident(plan.target))
        column_literal = literal(column.name)
        statements.append(
            "SELECT setval("
            f"pg_get_serial_sequence({table_literal}, {column_literal}), "
            f"COALESCE((SELECT MAX({column_sql}) FROM {table}), 0) + 1, "
            f"COALESCE((SELECT MAX({column_sql}) FROM {table}), 0) > 0"
            ");"
        )
    return statements


def clear_sql(plan: TablePlan) -> str:
    # DELETE is used for both tables and views. TRUNCATE would reject a parent
    # that still has an empty child referencing it, and would bypass the
    # INSTEAD OF triggers on the users/sessions/media compatibility views.
    return f"DELETE FROM {ident(plan.target)}"


def load_table(psql: Psql, plan: TablePlan, csv_path: Path, relation: PgRelation) -> None:
    if not csv_path.is_file():
        raise FileNotFoundError(f"export file is missing: {csv_path}")
    psql.run(clear_sql(plan))
    psql.run(copy_sql(plan), stdin_path=csv_path)
    for statement in sequence_sql(plan, relation):
        psql.run(statement)
