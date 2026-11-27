"""PostgreSQL catalog introspection and target-shape checks.

The importer talks to PostgreSQL through ``psql`` rather than a second Python
driver. That keeps the dependency surface to the client already used by the
restore drill, and makes every statement visible in CI logs.
"""

from __future__ import annotations

import csv
import io
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
           ), '[]'::json) AS columns,
           COALESCE((
               SELECT json_agg(json_build_object('target_table', rc.relname)
                               ORDER BY rc.relname)
               FROM pg_constraint con
               JOIN pg_class rc ON rc.oid = con.confrelid
               JOIN pg_namespace rn ON rn.oid = rc.relnamespace
               WHERE con.conrelid = c.oid
                 AND con.contype = 'f'
                 AND rn.nspname = 'public'
           ), '[]'::json) AS references
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


def _iter_csv_rows(stream) -> Iterator[list[str]]:
    """Decode a UTF-8 COPY stream without rewriting embedded CR/LF bytes.

    ``subprocess`` text mode uses universal newlines, which would turn a
    quoted ``\r\n`` inside a text field into ``\n`` and make the target digest
    disagree with the source. Wrapping the raw pipe with ``newline=""`` keeps
    the field bytes intact while ``csv.reader`` still understands quoted
    newlines.
    """

    wrapper = io.TextIOWrapper(stream, encoding="utf-8", newline="")
    try:
        yield from csv.reader(wrapper)
    finally:
        wrapper.close()


@dataclass(frozen=True)
class PgColumn:
    name: str
    declared_type: str
    kind: Kind
    not_null: bool
    default: str | None
    identity: str


@dataclass(frozen=True)
class PgReference:
    target_table: str


@dataclass(frozen=True)
class PgRelation:
    name: str
    relkind: str
    columns: tuple[PgColumn, ...]
    references: tuple[PgReference, ...] = ()

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
    # SQLite commonly stores integer minor units in a column whose PG target is
    # NUMERIC; widening an integer to an exact decimal is lossless.
    if source == "int" and target == "decimal":
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
        relations[name] = PgRelation(
            name=name,
            relkind=str(entry["kind"]),
            columns=columns,
            references=tuple(
                PgReference(target_table=str(reference["target_table"]))
                for reference in entry.get("references", [])
            ),
        )
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
    return order_by_target_dependencies(plan, catalog)


# A compatibility view renames a physical base table. A foreign key in another
# target table still points at the base table, so dependency resolution must
# translate it back to the source table that fills the view.
_TARGET_ALIASES = {
    "identity_user": "users",
}


def order_by_target_dependencies(
    plan: ImportPlan, catalog: dict[str, PgRelation]
) -> ImportPlan:
    """Put target foreign-key parents before children, preserving plan order.

    SQLite's dependency order is the primary signal, but it cannot see a target
    foreign key that SQLite never declared (for example the legacy identity
    sidecar) or a table that the importer renamed. A cycle is reported and the
    remaining tables keep their original order; the loader will still fail on
    the actual constraint rather than silently reverse rows.
    """

    original = [table.source for table in plan.tables]
    by_source = {table.source: table for table in plan.tables}
    by_target = {table.target: table.source for table in plan.tables}
    dependencies: dict[str, set[str]] = {source: set() for source in original}
    children: dict[str, set[str]] = {source: set() for source in original}
    indegree = {source: 0 for source in original}

    def source_for_target(target: str) -> str | None:
        if target in by_target:
            return by_target[target]
        alias = _TARGET_ALIASES.get(target)
        return by_target.get(alias) if alias else None

    for table in plan.tables:
        relation = catalog.get(table.target)
        if relation is None:
            continue
        for reference in relation.references:
            parent = source_for_target(reference.target_table)
            if parent is None or parent == table.source:
                continue
            if parent not in dependencies[table.source]:
                dependencies[table.source].add(parent)
                children[parent].add(table.source)
                indegree[table.source] += 1

    remaining = set(original)
    order: list[str] = []
    while remaining:
        ready = [source for source in original if source in remaining and indegree[source] == 0]
        if not ready:
            forced = [source for source in original if source in remaining]
            plan.warnings.append(
                "target foreign-key cycle broken at " + ", ".join(forced[:3])
            )
            ready = forced
        for source in ready:
            if source not in remaining:
                continue
            remaining.remove(source)
            order.append(source)
            for child in sorted(children[source]):
                if child in remaining:
                    indegree[child] -= 1

    # The users view is the only source of identity_user rows. Sessions and
    # refresh rows reference identity_user through physical base tables that the
    # view catalog cannot expose, so the root must be loaded first explicitly.
    if "users" in by_source and order and order[0] != "users":
        order.remove("users")
        order.insert(0, "users")
        plan.warnings.append("users moved first as the identity root")
    if order != original:
        plan.warnings.append("target foreign keys reordered the table plan")
    plan.tables = [by_source[source] for source in order]
    return plan


class Psql:
    def __init__(self, dsn: str, binary: str | None = None, timeout: int = 600):
        self.dsn = dsn
        self.binary = binary or shutil.which("psql") or ""
        if not self.binary:
            raise PsqlError("psql is not on PATH; install the PostgreSQL client tools")
        self.timeout = timeout

    @staticmethod
    def _arg(sql: str) -> str:
        # A Windows .cmd shim cannot carry a multi-line argument reliably; the
        # statements here have no meaningful line breaks outside literals.
        return " ".join(line.strip() for line in sql.splitlines() if line.strip())

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
            self._arg(sql),
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

    def run_script(self, script: str) -> None:
        r"""Run one multi-statement psql script with ``ON_ERROR_STOP``.

        The script is fed through ``-f -`` so psql executes ``\copy`` meta-
        commands in file order. A failed statement exits non-zero; the caller
        relies on the open transaction being rolled back when the connection
        closes.
        """

        command = [
            self.binary,
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-d",
            self.dsn,
            "-f",
            "-",
        ]
        result = subprocess.run(
            command,
            input=script,
            text=True,
            capture_output=True,
            timeout=self.timeout,
        )
        if result.returncode != 0:
            detail = (result.stderr or result.stdout).strip().splitlines()
            suffix = f": {detail[-1]}" if detail else ""
            raise PsqlError(f"psql script failed ({result.returncode}){suffix}")

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
            self._arg(sql),
        ]
        stderr = tempfile.TemporaryFile(mode="w+", encoding="utf-8")
        process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=stderr,
        )
        assert process.stdout is not None
        try:
            for row in _iter_csv_rows(process.stdout):
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
