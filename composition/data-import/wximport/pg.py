"""PostgreSQL catalog introspection and target-shape checks.

The importer talks to PostgreSQL through ``psql`` rather than a second Python
driver. That keeps the dependency surface to the client already used by the
restore drill, and makes every statement visible in CI logs.
"""

from __future__ import annotations

import csv
import json
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator

from .model import ImportPlan, Kind, TablePlan

CATALOG_SQL = """
SELECT COALESCE(json_agg(row_to_json(t))::text, '[]')
FROM (
    SELECT c.relname AS name,
           c.relkind::text AS kind,
           COALESCE((
               SELECT json_agg(json_build_object(
                          'name', a.attname,
                          'type', format_type(a.atttypid, a.atttypmod),
                          'not_null', a.attnotnull,
                          'default', pg_get_expr(d.adbin, d.adrelid),
                          'identity', a.attidentity
                      ) ORDER BY a.attnum)
               FROM pg_attribute a
               LEFT JOIN pg_attrdef d
                      ON d.adrelid = a.attrelid AND d.adnum = a.attnum
               WHERE a.attrelid = c.oid
                 AND a.attnum > 0
                 AND NOT a.attisdropped
           ), '[]'::json) AS columns
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public'
      AND c.relkind IN ('r', 'v', 'm', 'p')
    ORDER BY c.relname
) t;
"""


class PsqlError(RuntimeError):
    pass


class PlanError(ValueError):
    pass


@dataclass(frozen=True)
class PgColumn:
    name: str
    declared_type: str
    kind: Kind
    not_null: bool
    default: str | None
    identity: str


@dataclass(frozen=True)
class PgRelation:
    name: str
    relkind: str
    columns: tuple[PgColumn, ...]

    @property
    def mode(self) -> str:
        if self.relkind == "v":
            return "view"
        if self.relkind == "m":
            return "materialized-view"
        return "table"


def kind_for_pg_type(declared_type: str) -> Kind:
    lower = declared_type.lower().split("(", 1)[0].strip()
    if lower in {"bigint", "integer", "smallint"}:
        return "int"
    if lower == "boolean":
        return "bool"
    if lower == "bytea":
        return "bytes"
    if lower in {"double precision", "real"}:
        return "float"
    if lower in {"numeric", "decimal", "money"}:
        return "decimal"
    return "text"


def _kind_compatible(source: Kind, target: Kind) -> bool:
    if source == target:
        return True
    # A SQLite REAL can land in numeric without changing the value family.
    if source == "float" and target == "decimal":
        return True
    if source == "decimal" and target == "float":
        return True
    # SQLite has no native boolean; INTEGER 0/1 and BOOL both occur.
    if source == "bool" and target == "int":
        return True
    if source == "int" and target == "bool":
        return True
    return False


def parse_catalog(text: str) -> dict[str, PgRelation]:
    payload = json.loads(text or "[]")
    if not isinstance(payload, list):
        raise ValueError("catalog payload must be a JSON array")
    relations: dict[str, PgRelation] = {}
    for entry in payload:
        name = str(entry["name"])
        columns = tuple(
            PgColumn(
                name=str(column["name"]),
                declared_type=str(column["type"]),
                kind=kind_for_pg_type(str(column["type"])),
                not_null=bool(column["not_null"]),
                default=column.get("default"),
                identity=str(column.get("identity") or ""),
            )
            for column in entry.get("columns", [])
        )
        relations[name] = PgRelation(name=name, relkind=str(entry["kind"]), columns=columns)
    return relations


def apply_catalog(plan: ImportPlan, catalog: dict[str, PgRelation]) -> ImportPlan:
    """Validate every planned target before a single row is copied."""

    checked: list[TablePlan] = []
    for table in plan.tables:
        relation = catalog.get(table.target)
        if relation is None:
            raise PlanError(f"target {table.target!r} is missing from PostgreSQL public schema")
        if relation.mode == "materialized-view":
            raise PlanError(f"target {table.target!r} is a materialized view; it is not writable")
        target_columns = {column.name: column for column in relation.columns}
        source_names = set(table.column_names)
        source_only = sorted(source_names - set(target_columns))
        if source_only:
            raise PlanError(
                f"target {table.target!r} has no columns for source data: {source_only}"
            )
        for name, column in target_columns.items():
            if name in source_names:
                source_column = next(c for c in table.columns if c.name == name)
                if not _kind_compatible(source_column.kind, column.kind):
                    raise PlanError(
                        f"{table.target}.{name}: source {source_column.kind} cannot fill "
                        f"target {column.kind} ({column.declared_type})"
                    )
                continue
            if relation.mode == "view" or not column.not_null or column.default is not None:
                plan.warnings.append(
                    f"{table.target}.{name} is filled by PostgreSQL defaults/triggers"
                )
                continue
            raise PlanError(
                f"{table.target}.{name} is NOT NULL without a default; "
                "the source cannot supply it"
            )
        checked.append(
            TablePlan(
                source=table.source,
                target=table.target,
                mode="view" if relation.mode == "view" else "table",
                columns=table.columns,
                primary_key=table.primary_key,
                reason=table.reason,
            )
        )
    plan.tables = checked
    return plan


class Psql:
    def __init__(self, dsn: str, binary: str | None = None, timeout: int = 600):
        self.dsn = dsn
        self.binary = binary or shutil.which("psql") or ""
        if not self.binary:
            raise PsqlError("psql is not on PATH; install the PostgreSQL client tools")
        self.timeout = timeout

    def run(self, sql: str, stdin_path: Path | None = None) -> str:
        command = [
            self.binary,
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-d",
            self.dsn,
            "-A",
            "-t",
            "-c",
            sql,
        ]
        stdin_handle = stdin_path.open("r", encoding="utf-8", newline="") if stdin_path else None
        try:
            result = subprocess.run(
                command,
                stdin=stdin_handle,
                text=True,
                capture_output=True,
                timeout=self.timeout,
            )
        finally:
            if stdin_handle:
                stdin_handle.close()
        if result.returncode != 0:
            detail = (result.stderr or result.stdout).strip().splitlines()
            suffix = f": {detail[-1]}" if detail else ""
            raise PsqlError(f"psql failed ({result.returncode}){suffix}")
        return result.stdout

    def catalog(self) -> dict[str, PgRelation]:
        return parse_catalog(self.run(CATALOG_SQL))

    def iter_copy_rows(self, sql: str) -> Iterator[list[str]]:
        """Stream ``COPY ... TO STDOUT`` rows without buffering the table.

        ``csv.reader`` is fed the live pipe, so a text field containing a
        newline stays one row. stderr goes to a temporary file to keep it out
        of the CSV stream and avoid a full-pipe deadlock.
        """

        command = [
            self.binary,
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-d",
            self.dsn,
            "-c",
            sql,
        ]
        stderr = tempfile.TemporaryFile(mode="w+", encoding="utf-8")
        process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=stderr,
            text=True,
            encoding="utf-8",
        )
        assert process.stdout is not None
        try:
            for row in csv.reader(process.stdout):
                yield row
            process.wait(timeout=self.timeout)
            if process.returncode != 0:
                stderr.seek(0)
                detail = stderr.read().strip().splitlines()
                suffix = f": {detail[-1]}" if detail else ""
                raise PsqlError(f"psql COPY OUT failed ({process.returncode}){suffix}")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            stderr.close()
