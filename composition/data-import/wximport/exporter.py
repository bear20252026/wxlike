"""Stream SQLite rows into COPY-ready CSV files."""

from __future__ import annotations

import sqlite3
from dataclasses import dataclass
from pathlib import Path

from .codec import RowDigest, coerce_source, encode_row
from .model import TablePlan
from .sqlite_source import quote_ident


@dataclass(frozen=True)
class ExportResult:
    source: str
    target: str
    csv_path: Path
    rows: int
    digest: str


def export_table(
    connection: sqlite3.Connection,
    plan: TablePlan,
    csv_path: Path,
) -> ExportResult:
    csv_path.parent.mkdir(parents=True, exist_ok=True)
    kinds = [column.kind for column in plan.columns]
    names = [column.name for column in plan.columns]
    select = "SELECT " + ", ".join(quote_ident(name) for name in names) + " FROM " + quote_ident(plan.source)
    digest = RowDigest()
    rows = 0
    with csv_path.open("w", encoding="utf-8", newline="") as handle:
        for row in connection.execute(select):
            values = [coerce_source(row[index], kind) for index, kind in enumerate(kinds)]
            handle.write(encode_row(values, kinds))
            digest.add(values, kinds)
            rows += 1
    return ExportResult(
        source=plan.source,
        target=plan.target,
        csv_path=csv_path,
        rows=rows,
        digest=digest.finish(),
    )
