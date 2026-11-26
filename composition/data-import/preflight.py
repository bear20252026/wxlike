#!/usr/bin/env python3
"""Read-only preflight before a SQLite -> PostgreSQL cutover import.

This does not copy rows. It proves the source is the file the plan will read,
the target server is the expected major version, the M023 control tables exist,
and every planned table has a compatible target shape. Run it immediately before
opening the stop-write window and keep its JSON next to the import report.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import census
from wximport.pg import Psql
from wximport.plan import build_plan
from wximport.sqlite_source import data_version, list_tables, open_readonly


def _scalar(connection, sql: str):
    row = connection.execute(sql).fetchone()
    return row[0] if row else None


def preflight_payload(
    sqlite_path: Path,
    psql,
    mapping: Path | None = None,
    min_pg_version: int = 160000,
) -> dict:
    failures: list[str] = []
    warnings: list[str] = []
    checks: dict[str, object] = {}

    plan = build_plan(sqlite_path, mapping)
    connection = open_readonly(sqlite_path)
    try:
        tables = list_tables(connection)
        journal_mode = str(_scalar(connection, "PRAGMA journal_mode"))
        page_count = int(_scalar(connection, "PRAGMA page_count") or 0)
        freelist = int(_scalar(connection, "PRAGMA freelist_count") or 0)
        source_version = data_version(connection)
    finally:
        connection.close()

    checks["source"] = {
        "path": str(sqlite_path.resolve()),
        "bytes": sqlite_path.stat().st_size,
        "sha256": plan.source_sha256,
        "data_version": source_version,
        "tables": len(tables),
        "journal_mode": journal_mode,
        "page_count": page_count,
        "freelist_count": freelist,
    }
    checks["plan"] = {
        "tables": len(plan.tables),
        "skipped": [table for table, _ in plan.skipped],
        "warnings": list(plan.warnings),
    }
    warnings.extend(plan.warnings)

    wal_path = Path(str(sqlite_path) + "-wal")
    if journal_mode.lower() == "wal" and wal_path.exists():
        warnings.append(f"WAL file present ({wal_path.stat().st_size} bytes); checkpoint before the stop-write window")

    if len(tables) != 110:
        warnings.append(f"source has {len(tables)} tables; the reviewed platform snapshot has 110")
    if plan.skipped and len(plan.tables) != 107:
        warnings.append(f"plan has {len(plan.tables)} tables, expected 107")

    version_text = psql.run("SHOW server_version_num").strip()
    try:
        server_version = int(version_text)
    except ValueError:
        failures.append(f"cannot parse PostgreSQL server_version_num: {version_text!r}")
        server_version = 0
    checks["target"] = {
        "server_version_num": server_version,
        "current_database": psql.run("SELECT current_database()").strip(),
        "current_user": psql.run("SELECT current_user").strip(),
    }
    if server_version < min_pg_version:
        failures.append(
            f"PostgreSQL {server_version} is below required {min_pg_version}"
        )

    control = psql.run(
        "SELECT to_regclass('public.import_run')::text || '|' || "
        "COALESCE(to_regclass('public.import_table')::text, '')"
    ).strip()
    checks["import_control"] = control
    if not control.startswith("import_run|") or "import_table" not in control:
        failures.append("M023 import_run/import_table is missing; run the PostgreSQL migrations first")

    catalog = psql.catalog()
    checks["pg_relations"] = len(catalog)
    census_report = census.census_payload(plan, catalog)
    checks["census"] = {
        "verified": census_report["verified"],
        "failures": census_report["failures"],
    }
    warnings.extend(census_report["warnings"])
    for failure in census_report["failures"]:
        failures.append(f"{failure['source']}: {failure['error']}")

    return {
        "ok": not failures,
        "source": checks["source"],
        "plan": checks["plan"],
        "target": checks["target"],
        "import_control": control,
        "pg_relations": len(catalog),
        "verified": census_report["verified"],
        "failures": failures,
        "warnings": warnings,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="read-only SQLite -> PostgreSQL cutover preflight")
    parser.add_argument("--sqlite", required=True, type=Path)
    parser.add_argument("--dsn", required=True)
    parser.add_argument("--mapping", type=Path)
    parser.add_argument("--psql", type=Path)
    parser.add_argument("--min-pg-version", type=int, default=160000)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)

    psql = Psql(args.dsn, binary=str(args.psql) if args.psql else None)
    payload = preflight_payload(args.sqlite, psql, args.mapping, args.min_pg_version)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    print(
        f"preflight: source_tables={payload['source']['tables']} "
        f"planned={payload['plan']['tables']} verified={payload['verified']} "
        f"failures={len(payload['failures'])} warnings={len(payload['warnings'])}"
    )
    for failure in payload["failures"]:
        print(f"FAIL {failure}")
    return 0 if payload["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
