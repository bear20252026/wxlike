"""Load exported CSV files into PostgreSQL through ``COPY``."""

from __future__ import annotations

import hashlib
from pathlib import Path

from .model import TablePlan
from .pg import PgRelation, Psql


def ident(name: str) -> str:
    if "\x00" in name:
        raise ValueError("identifier contains NUL")
    return '"' + name.replace('"', '""') + '"'


def literal(text: str) -> str:
    return "'" + text.replace("'", "''") + "'"


def copy_sql(plan: TablePlan, target: str | None = None) -> str:
    columns = ", ".join(ident(column.name) for column in plan.columns)
    destination = ident(target or plan.target)
    return (
        f"COPY {destination} ({columns}) FROM STDIN "
        "WITH (FORMAT csv, HEADER false, NULL '\\N')"
    )


def view_stage(plan: TablePlan) -> str:
    """Stable, <=63-byte staging name for a compatibility view.

    PostgreSQL cannot COPY FROM a view even when it has INSTEAD OF triggers.
    The loader stages rows in a plain table, then one INSERT SELECT fires the
    view triggers. The name is derived from the target so a failed run can
    clean up the same table next time.
    """

    digest = hashlib.sha1(plan.target.encode("utf-8")).hexdigest()[:12]
    return f"_wximport_stage_{digest}"


def stage_ddl(plan: TablePlan, relation: PgRelation, stage: str) -> str:
    relation_columns = {column.name: column for column in relation.columns}
    definitions = []
    for column in plan.columns:
        target = relation_columns.get(column.name)
        if target is None:
            raise ValueError(f"{plan.target}.{column.name} is missing from the catalog")
        definitions.append(f"{ident(column.name)} {target.declared_type}")
    return f"CREATE TABLE {ident(stage)} ({', '.join(definitions)})"


def view_insert_sql(plan: TablePlan, stage: str) -> str:
    columns = ", ".join(ident(column.name) for column in plan.columns)
    return (
        f"INSERT INTO {ident(plan.target)} ({columns}) "
        f"SELECT {columns} FROM {ident(stage)}"
    )


def sequence_sql(plan: TablePlan, relation: PgRelation) -> list[str]:
    """Reset identity/serial sequences after explicit IDs were imported.

    Without this, the first post-cutover insert either collides with an imported
    id or jumps backwards. ``setval(max, true)`` leaves max as the last used
    value and makes the next insert max+1; an empty table gets ``1, false``.
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
            f"GREATEST(COALESCE((SELECT MAX({column_sql}) FROM {table}), 0), 1), "
            f"COALESCE((SELECT MAX({column_sql}) FROM {table}), 0) > 0"
            ");"
        )
    return statements


def clear_sql(plan: TablePlan) -> str:
    # DELETE is used for both tables and views. TRUNCATE would reject a parent
    # that still has an empty child referencing it, and would bypass the
    # INSTEAD OF triggers on the users/sessions/media compatibility views.
    return f"DELETE FROM {ident(plan.target)}"


def clear_targets(psql: Psql, tables: list[TablePlan], statuses: dict[str, dict]) -> None:
    """Empty pending targets children-first before the load loop.

    Loading parent rows first is required for foreign keys, but clearing a
    parent while an old child row still points at it is forbidden. A full
    import therefore clears in reverse dependency order; verified tables in a
    resumed run are left untouched.
    """

    for table in reversed(tables):
        if statuses.get(table.source, {}).get("status") == "verified":
            continue
        psql.run(clear_sql(table))


def load_table(psql: Psql, plan: TablePlan, csv_path: Path, relation: PgRelation) -> None:
    if not csv_path.is_file():
        raise FileNotFoundError(f"export file is missing: {csv_path}")
    psql.run(clear_sql(plan))
    if relation.mode == "view":
        stage = view_stage(plan)
        psql.run(f"DROP TABLE IF EXISTS {ident(stage)}")
        psql.run(stage_ddl(plan, relation, stage))
        psql.run(copy_sql(plan, stage), stdin_path=csv_path)
        # DROP runs only after a successful INSERT; a failed trigger leaves the
        # staging table for diagnosis and the next attempt drops it first.
        psql.run(view_insert_sql(plan, stage) + ";" + f" DROP TABLE {ident(stage)}")
        return
    psql.run(copy_sql(plan), stdin_path=csv_path)
    for statement in sequence_sql(plan, relation):
        psql.run(statement)
