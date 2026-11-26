"""Read-only SQLite inventory for the cutover importer."""

from __future__ import annotations

import hashlib
import sqlite3
from pathlib import Path

from .model import Column, ForeignKey, Kind, SourceTable


def open_readonly(path: Path) -> sqlite3.Connection:
    """Open an existing SQLite file without any write capability.

    A plain ``sqlite3.connect`` creates a missing file and takes a write lock.
    The cutover importer must fail on a typo instead of silently producing an
    empty source, so this refuses missing paths and uses ``mode=ro``.
    """

    path = path.resolve()
    if not path.is_file():
        raise FileNotFoundError(f"SQLite source does not exist: {path}")
    uri = path.as_uri() + "?mode=ro"
    connection = sqlite3.connect(uri, uri=True)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA foreign_keys = ON")
    return connection


def quote_ident(name: str) -> str:
    """Quote a SQLite identifier; reject NUL bytes rather than truncating them."""

    if "\x00" in name:
        raise ValueError("SQLite identifier contains NUL")
    return '"' + name.replace('"', '""') + '"'


def list_tables(connection: sqlite3.Connection) -> list[str]:
    """Physical user tables, excluding SQLite's own bookkeeping tables."""

    rows = connection.execute(
        "SELECT name FROM sqlite_master "
        "WHERE type = 'table' AND name NOT LIKE 'sqlite_%' "
        "ORDER BY name"
    ).fetchall()
    return [str(row[0]) for row in rows]


def kind_for_sqlite_type(declared_type: str) -> Kind:
    """Map SQLite affinity to the importer's value families."""

    upper = declared_type.upper()
    if "INT" in upper:
        return "int"
    if any(token in upper for token in ("REAL", "FLOA", "DOUBLE")):
        return "float"
    if "BLOB" in upper:
        return "bytes"
    if "BOOL" in upper:
        return "bool"
    if any(token in upper for token in ("NUMERIC", "DECIMAL")):
        return "decimal"
    return "text"


def table_columns(connection: sqlite3.Connection, table: str) -> list[Column]:
    rows = connection.execute(f"PRAGMA table_xinfo({quote_ident(table)})").fetchall()
    columns: list[Column] = []
    for row in rows:
        hidden = int(row[6])
        # hidden=1 is a virtual-table hidden column; hidden=2/3 are generated
        # columns. Neither can be inserted through a plain INSERT/COPY.
        if hidden != 0:
            continue
        declared = str(row[2] or "")
        columns.append(
            Column(
                name=str(row[1]),
                declared_type=declared,
                kind=kind_for_sqlite_type(declared),
                not_null=bool(row[3]),
                pk=int(row[5]),
                hidden=hidden,
            )
        )
    return columns


def table_primary_key(connection: sqlite3.Connection, table: str) -> tuple[str, ...]:
    columns = table_columns(connection, table)
    ordered = sorted((column for column in columns if column.pk > 0), key=lambda c: c.pk)
    return tuple(column.name for column in ordered)


def table_foreign_keys(connection: sqlite3.Connection, table: str) -> list[ForeignKey]:
    rows = connection.execute(f"PRAGMA foreign_key_list({quote_ident(table)})").fetchall()
    result: list[ForeignKey] = []
    pk_by_table: dict[str, tuple[str, ...]] = {}
    for row in rows:
        ref_table = str(row[2])
        ref_column = str(row[4] or "")
        if not ref_column:
            if ref_table not in pk_by_table:
                pk_by_table[ref_table] = table_primary_key(connection, ref_table)
            if len(pk_by_table[ref_table]) != 1:
                raise ValueError(
                    f"foreign key {table}.{row[3]} -> {ref_table} has no single target column"
                )
            ref_column = pk_by_table[ref_table][0]
        result.append(
            ForeignKey(
                table=table,
                column=str(row[3]),
                ref_table=ref_table,
                ref_column=ref_column,
            )
        )
    return result


def source_table(connection: sqlite3.Connection, table: str) -> SourceTable:
    return SourceTable(
        name=table,
        columns=table_columns(connection, table),
        primary_key=table_primary_key(connection, table),
        foreign_keys=table_foreign_keys(connection, table),
    )


def data_version(connection: sqlite3.Connection) -> int:
    """SQLite's cross-connection write counter.

    The counter only moves when another connection commits a write. The
    importer uses it as a cheap "source stayed quiet" signal while it exports.
    """

    row = connection.execute("PRAGMA data_version").fetchone()
    return int(row[0])


def file_sha256(path: Path, chunk_size: int = 1 << 20) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(chunk_size):
            digest.update(chunk)
    return digest.hexdigest()
