"""Drive one resumable SQLite -> PostgreSQL import run."""

from __future__ import annotations

import json
import re
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

from .exporter import export_table
from .loader import load_table
from .model import ImportPlan
from .pg import Psql, apply_catalog, parse_catalog
from .plan import build_plan, plan_payload
from .sqlite_source import data_version, file_sha256, open_readonly
from .state import ImportState, now_ms
from .verifier import verify_table
from .window import StopWriteWindow

_SAFE_FILE = re.compile(r"[^A-Za-z0-9_.-]+")


@dataclass
class RunnerConfig:
    sqlite: Path
    dsn: str
    work_dir: Path
    run_id: str | None = None
    mapping: Path | None = None
    resume: bool = False
    verify_only: bool = False
    plan_only: bool = False
    require_quiescence: bool = True
    max_window_seconds: int | None = None
    plan_out: Path | None = None


def default_run_id(plan: ImportPlan) -> str:
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    return f"dt-{stamp}-{plan.source_sha256[:8]}"


def csv_path(work_dir: Path, source: str) -> Path:
    safe = _SAFE_FILE.sub("_", source)
    return work_dir / "csv" / f"{safe}.csv"


def run_import(config: RunnerConfig, psql: Psql | None = None) -> dict:
    plan = build_plan(config.sqlite, config.mapping)
    if config.plan_out:
        config.plan_out.parent.mkdir(parents=True, exist_ok=True)
        config.plan_out.write_text(
            json.dumps(plan_payload(plan), ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
    if config.plan_only:
        return {"status": "planned", "tables": len(plan.tables), "warnings": plan.warnings}

    client = psql or Psql(config.dsn)
    catalog = client.catalog()
    # The catalog text is parsed once here so a test can pass a fake client
    # without a real PostgreSQL server.
    if isinstance(catalog, str):
        catalog = parse_catalog(catalog)
    plan = apply_catalog(plan, catalog)

    run_id = config.run_id or default_run_id(plan)
    config.work_dir.mkdir(parents=True, exist_ok=True)
    state = ImportState(client, run_id)
    state.ensure_run(plan, config.resume)
    state.register(plan)

    source_sha = file_sha256(config.sqlite)
    if source_sha != plan.source_sha256:
        raise RuntimeError("SQLite source changed after planning; restart without --resume")
    connection = open_readonly(config.sqlite)
    window = StopWriteWindow(source_sha, data_version(connection))
    failures: list[str] = []
    try:
        for table in plan.tables:
            row = state.rows[table.source]
            status = row["status"]
            if status == "verified":
                continue
            path = csv_path(config.work_dir, table.source)
            started = now_ms()
            try:
                if config.verify_only:
                    if not path.is_file():
                        raise FileNotFoundError(
                            f"verify-only needs the prior CSV for {table.source}: {path}"
                        )
                else:
                    state.begin(table.source, table.target)
                    result = export_table(connection, table, path)
                    state.loaded(table.source, result.rows, result.digest)
                    load_table(client, table, path, catalog[table.target])
                result = verify_table(
                    client,
                    table,
                    int(state.rows[table.source]["source_rows"]),
                    str(state.rows[table.source]["source_digest"]),
                )
                if not result.ok:
                    raise RuntimeError(f"{table.source}: {result.reason}")
                state.verified(table.source, result.actual_digest)
            except Exception as err:  # noqa: BLE001 - the state must record any failure
                state.failed(table.source, str(err))
                failures.append(table.source)
                raise
            finally:
                window.record_table(table.source, max(0, now_ms() - started))
            if config.require_quiescence and data_version(connection) != window.source_data_version:
                raise RuntimeError("SQLite writes were observed during the import window")
    except Exception as err:  # noqa: BLE001 - report the run instead of leaving it running
        failures.append(str(err))
    finally:
        after_version = data_version(connection)
        connection.close()
        window.close(file_sha256(config.sqlite), after_version)

    elapsed = window.duration_ms
    if config.max_window_seconds and elapsed > config.max_window_seconds * 1000:
        failures.append(f"window {elapsed}ms exceeds {config.max_window_seconds}s")
    if config.require_quiescence and not window.quiescent:
        failures.append("source file or data_version changed during the window")
    status = "verified" if state.all_verified() and not failures else "failed"
    state.finish(status, window.quiescent, elapsed, "; ".join(failures))
    report = {
        "status": status,
        "run_id": run_id,
        "tables": len(plan.tables),
        "failures": failures,
        "window": window.to_dict(),
        "warnings": plan.warnings,
    }
    (config.work_dir / "window.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    return report
