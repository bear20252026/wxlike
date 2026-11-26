"""Shared records for the SQLite -> PostgreSQL import planner.

The importer never invents accounting or message semantics: it only moves rows.
These records keep the source shape and the target shape in one place so the
planner, exporter, loader and verifier cannot drift apart.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal

# ``decimal`` is kept separate from ``float`` so a PostgreSQL ``numeric`` column
# never gets rounded through binary floating point on its way back.
Kind = Literal["text", "int", "float", "bytes", "bool", "decimal"]


@dataclass(frozen=True)
class Column:
    """A physical, insertable column."""

    name: str
    declared_type: str
    kind: Kind
    not_null: bool
    pk: int = 0
    hidden: int = 0


@dataclass(frozen=True)
class ForeignKey:
    table: str
    column: str
    ref_table: str
    ref_column: str


@dataclass
class SourceTable:
    name: str
    columns: list[Column]
    primary_key: tuple[str, ...]
    foreign_keys: list[ForeignKey] = field(default_factory=list)

    @property
    def insertable_columns(self) -> list[Column]:
        return [column for column in self.columns if column.hidden == 0]


@dataclass(frozen=True)
class TablePlan:
    """One source table and its reviewed PostgreSQL target."""

    source: str
    target: str
    mode: Literal["table", "view"]
    columns: tuple[Column, ...]
    primary_key: tuple[str, ...]
    reason: str = ""

    @property
    def column_names(self) -> tuple[str, ...]:
        return tuple(column.name for column in self.columns)


@dataclass
class ImportPlan:
    source_path: Path
    source_sha256: str
    source_data_version: int
    tables: list[TablePlan] = field(default_factory=list)
    skipped: list[tuple[str, str]] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
