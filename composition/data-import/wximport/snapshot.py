"""Build a shape-only SQLite database from the platform schema snapshot.

The platform's `schema_snapshot.txt` is the reviewed inventory of every SQLite
table and column. It contains no foreign keys, so a database built from it is a
catalog fixture, not a data fixture: it exists to prove that all 110 source
tables have a PostgreSQL landing shape before anyone points the importer at a
production snapshot.
"""

from __future__ import annotations

import sqlite3
from dataclasses import dataclass
from pathlib import Path

from .model import Kind
from .sqlite_source import list_tables


class SnapshotError(ValueError):
    pass


@dataclass(frozen=True)
class SnapshotColumn:
    name: str
    kind: Kind
    not_null: bool
    primary_key_order: int


@dataclass(frozen=True)
class SnapshotTable:
    name: str
    columns: tuple[SnapshotColumn, ...]


def _kind(declared: str) -> Kind:
    lower = declared.strip().upper()
    if lower in {"INTEGER", "BIGINT", "SMALLINT"}:
        return "int"
    if lower in {"TEXT", "VARCHAR", "CHAR", "CLOB"}:
        return "text"
    if lower in {"REAL", "DOUBLE", "FLOAT"}:
        return "float"
    if lower in {"BLOB"}:
        return "bytes"
    if lower in {"BOOLEAN", "BOOL"}:
        return "bool"
    raise SnapshotError(f"unsupported snapshot type {declared!r}")


def parse_snapshot(text: str) -> tuple[SnapshotTable, ...]:
    """Parse `table.column TYPE notnull=0|1 pk=0|1|2` lines.

    Blank lines and `#` comments are ignored. Table and column names are
    validated for duplicates; primary-key positions must start at 1 and be
    contiguous within a table.
    """

    tables: dict[str, list[SnapshotColumn]] = {}
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) < 2:
            raise SnapshotError(f"malformed snapshot line: {line!r}")
        qualified = parts[0]
        if "." not in qualified:
            raise SnapshotError(f"snapshot column is not qualified: {qualified!r}")
        table, column = qualified.split(".", 1)
        if not table or not column:
            raise SnapshotError(f"empty table or column in {qualified!r}")
        attrs = {}
        for extra in parts[2:]:
            if "=" not in extra:
                raise SnapshotError(f"malformed attribute {extra!r} in {line!r}")
            key, value = extra.split("=", 1)
            attrs[key] = value
        try:
            pk_order = int(attrs.get("pk", "0"))
        except ValueError as exc:
            raise SnapshotError(f"bad pk order in {line!r}") from exc
        not_null = attrs.get("notnull", "0") == "1"
        bucket = tables.setdefault(table, [])
        if any(existing.name == column for existing in bucket):
            raise SnapshotError(f"duplicate column {table}.{column}")
        bucket.append(
            SnapshotColumn(
                name=column,
                kind=_kind(parts[1]),
                not_null=not_null,
                primary_key_order=pk_order,
            )
        )

    if not tables:
        raise SnapshotError("snapshot has no tables")

    result: list[SnapshotTable] = []
    for table in sorted(tables):
        columns = tuple(tables[table])
        positions = sorted(c.primary_key_order for c in columns if c.primary_key_order)
        if positions and positions != list(range(1, len(positions) + 1)):
            raise SnapshotError(f"{table}: primary-key positions are not contiguous")
        result.append(SnapshotTable(name=table, columns=columns))
    return tuple(result)


def build_snapshot_database(snapshot_path: Path, sqlite_path: Path) -> tuple[SnapshotTable, ...]:
    """Create or replace `sqlite_path` with the empty 110-table shape.

    The destination is removed first so a stale database cannot silently make a
    census look better than the snapshot. No rows are inserted.
    """

    tables = parse_snapshot(snapshot_path.read_text(encoding="utf-8"))
    if sqlite_path.exists():
        sqlite_path.unlink()
    sqlite_path.parent.mkdir(parents=True, exist_ok=True)

    connection = sqlite3.connect(sqlite_path)
    try:
        for table in tables:
            definitions: list[str] = []
            for column in table.columns:
                definition = f'"{column.name}" {column.kind.upper()}'
                if column.not_null:
                    definition += " NOT NULL"
                definitions.append(definition)
            primary_key = [
                column.name
                for column in sorted(
                    (c for c in table.columns if c.primary_key_order),
                    key=lambda c: c.primary_key_order,
                )
            ]
            if primary_key:
                definitions.append(
                    "PRIMARY KEY (" + ", ".join(f'"{name}"' for name in primary_key) + ")"
                )
            connection.execute(f'CREATE TABLE "{table.name}" (' + ", ".join(definitions) + ")")
        connection.commit()
    finally:
        connection.close()

    reopened = sqlite3.connect(sqlite_path)
    try:
        actual = list_tables(reopened)
    finally:
        reopened.close()
    expected = [table.name for table in tables]
    if actual != expected:
        raise SnapshotError(
            f"snapshot database tables differ: expected {len(expected)}, got {len(actual)}"
        )
    return tables
