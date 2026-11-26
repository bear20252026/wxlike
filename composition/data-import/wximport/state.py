"""Resumable import ledger in PostgreSQL (M023)."""

from __future__ import annotations

import json
import time
from dataclasses import dataclass
from pathlib import Path

from .loader import literal
from .model import ImportPlan
from .pg import Psql


def now_ms() -> int:
    return int(time.time() * 1000)


@dataclass
class RunInfo:
    run_id: str
    source_sha256: str
    started_at: int
    status: str


class ImportState:
    def __init__(self, psql: Psql, run_id: str) -> None:
        if not run_id:
            raise ValueError("run_id is required")
        self.psql = psql
        self.run_id = run_id
        self.info: RunInfo | None = None
        self.rows: dict[str, dict] = {}

    def _run_row(self) -> dict | None:
        sql = (
            "SELECT row_to_json(t)::text FROM ("
            "SELECT run_id, source_sha256, started_at, status "
            f"FROM import_run WHERE run_id = {literal(self.run_id)}"
            ") t;"
        )
        raw = self.psql.run(sql).strip()
        return json.loads(raw) if raw else None

    def ensure_run(self, plan: ImportPlan, resume: bool) -> RunInfo:
        existing = self._run_row()
        if existing:
            if existing["source_sha256"] != plan.source_sha256:
                raise RuntimeError(
                    f"run {self.run_id} was started for another source hash; "
                    "use a new --run-id"
                )
            if not resume:
                raise RuntimeError(
                    f"run {self.run_id} already exists; pass --resume or a new --run-id"
                )
            self.info = RunInfo(
                run_id=str(existing["run_id"]),
                source_sha256=str(existing["source_sha256"]),
                started_at=int(existing["started_at"]),
                status=str(existing["status"]),
            )
        else:
            started = now_ms()
            sql = (
                "INSERT INTO import_run (run_id, source_path, source_sha256, "
                "source_data_version, started_at, status, notes) VALUES ("
                f"{literal(self.run_id)}, {literal(str(plan.source_path))}, "
                f"{literal(plan.source_sha256)}, {plan.source_data_version}, "
                f"{started}, 'running', '')"
            )
            self.psql.run(sql)
            self.info = RunInfo(
                run_id=self.run_id,
                source_sha256=plan.source_sha256,
                started_at=started,
                status="running",
            )
        return self.info

    def _load_tables(self) -> None:
        sql = (
            "SELECT COALESCE(json_agg(row_to_json(t) ORDER BY t.source_table), '[]'::json)::text "
            "FROM (SELECT source_table, target_table, source_rows, loaded_rows, "
            "source_digest, target_digest, status, started_at, finished_at, error "
            f"FROM import_table WHERE run_id = {literal(self.run_id)}) t;"
        )
        raw = self.psql.run(sql).strip()
        rows = json.loads(raw or "[]")
        self.rows = {str(row["source_table"]): row for row in rows}

    def register(self, plan: ImportPlan) -> None:
        self._load_tables()
        planned = {table.source: table.target for table in plan.tables}
        extra = sorted(set(self.rows) - set(planned))
        if extra:
            raise RuntimeError(f"run {self.run_id} has tables outside the current plan: {extra}")
        for table in plan.tables:
            if table.source in self.rows:
                if self.rows[table.source]["target_table"] != table.target:
                    raise RuntimeError(
                        f"{table.source}: target changed from "
                        f"{self.rows[table.source]['target_table']!r} to {table.target!r}"
                    )
                continue
            self.psql.run(
                "INSERT INTO import_table (run_id, source_table, target_table, status) "
                f"VALUES ({literal(self.run_id)}, {literal(table.source)}, "
                f"{literal(table.target)}, 'pending')"
            )
            self.rows[table.source] = {
                "source_table": table.source,
                "target_table": table.target,
                "source_rows": -1,
                "loaded_rows": -1,
                "source_digest": "",
                "target_digest": "",
                "status": "pending",
                "started_at": 0,
                "finished_at": 0,
                "error": "",
            }

    def _update(self, source: str, fields: dict) -> None:
        row = self.rows[source]
        row.update(fields)
        values = []
        for key in (
            "source_rows",
            "loaded_rows",
            "source_digest",
            "target_digest",
            "status",
            "started_at",
            "finished_at",
            "error",
        ):
            value = row[key]
            if isinstance(value, bool):
                values.append("TRUE" if value else "FALSE")
            elif isinstance(value, int):
                values.append(str(value))
            elif value is None:
                values.append("NULL")
            else:
                values.append(literal(str(value)))
        sql = (
            "UPDATE import_table SET "
            "source_rows = " + values[0] + ", "
            "loaded_rows = " + values[1] + ", "
            "source_digest = " + values[2] + ", "
            "target_digest = " + values[3] + ", "
            "status = " + values[4] + ", "
            "started_at = " + values[5] + ", "
            "finished_at = " + values[6] + ", "
            "error = " + values[7] + " "
            f"WHERE run_id = {literal(self.run_id)} "
            f"AND source_table = {literal(source)}"
        )
        self.psql.run(sql)

    def begin(self, source: str, target: str) -> None:
        self._update(
            source,
            {"target_table": target, "status": "loading", "started_at": now_ms(), "error": ""},
        )

    def loaded(self, source: str, rows: int, digest: str) -> None:
        self._update(
            source,
            {
                "source_rows": rows,
                "loaded_rows": rows,
                "source_digest": digest,
                "status": "loaded",
                "finished_at": now_ms(),
            },
        )

    def verified(self, source: str, digest: str) -> None:
        self._update(
            source,
            {"target_digest": digest, "status": "verified", "finished_at": now_ms(), "error": ""},
        )

    def failed(self, source: str, error: str) -> None:
        self._update(
            source,
            {"status": "failed", "finished_at": now_ms(), "error": error[:2000]},
        )

    def all_verified(self) -> bool:
        return bool(self.rows) and all(row["status"] == "verified" for row in self.rows.values())

    def finish(self, status: str, quiescent: bool, window_ms: int, notes: str = "") -> None:
        sql = (
            "UPDATE import_run SET "
            f"finished_at = {now_ms()}, status = {literal(status)}, "
            f"quiescent = {'TRUE' if quiescent else 'FALSE'}, "
            f"window_ms = {window_ms}, notes = {literal(notes)} "
            f"WHERE run_id = {literal(self.run_id)}"
        )
        self.psql.run(sql)
