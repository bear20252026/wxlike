"""Catalog-only census: every source table must have a writable target shape.

This is intentionally cheaper and safer than a row import. It answers the
question that blocked the first full-snapshot attempt: do all 110 SQLite tables
have a PostgreSQL landing object with compatible columns before any COPY runs?
"""

from __future__ import annotations

import argparse
import json
import tempfile
from pathlib import Path

from wximport.model import ImportPlan
from wximport.pg import PlanError, Psql, apply_catalog
from wximport.plan import build_plan
from wximport.snapshot import build_snapshot_database


def census_payload(plan: ImportPlan, catalog) -> dict:
    failures: list[dict[str, str]] = []
    warnings: list[str] = []
    for table in plan.tables:
        single = ImportPlan(
            source_path=plan.source_path,
            source_sha256=plan.source_sha256,
            source_data_version=plan.source_data_version,
            tables=[table],
            warnings=[],
        )
        try:
            apply_catalog(single, catalog)
        except PlanError as exc:
            failures.append({"source": table.source, "target": table.target, "error": str(exc)})
        else:
            warnings.extend(single.warnings)
    return {
        "source": str(plan.source_path),
        "source_sha256": plan.source_sha256,
        "source_data_version": plan.source_data_version,
        "tables": len(plan.tables),
        "skipped": [{"table": table, "reason": reason} for table, reason in plan.skipped],
        "verified": len(plan.tables) - len(failures),
        "failures": failures,
        "warnings": warnings,
        "ok": not failures,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="census SQLite tables against PostgreSQL")
    parser.add_argument("--snapshot", type=Path)
    parser.add_argument("--sqlite", type=Path)
    parser.add_argument("--mapping", type=Path)
    parser.add_argument("--dsn", required=True)
    parser.add_argument("--psql", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    if args.snapshot is None and args.sqlite is None:
        parser.error("one of --snapshot or --sqlite is required")

    temporary = None
    sqlite_path = args.sqlite
    if args.snapshot is not None:
        if sqlite_path is None:
            handle = tempfile.NamedTemporaryFile(suffix=".sqlite", delete=False)
            handle.close()
            sqlite_path = Path(handle.name)
            temporary = sqlite_path
        tables = build_snapshot_database(args.snapshot, sqlite_path)
        print(f"snapshot: {len(tables)} tables -> {sqlite_path}")

    assert sqlite_path is not None
    plan = build_plan(sqlite_path, args.mapping)
    binary = str(args.psql) if args.psql else None
    payload = census_payload(plan, Psql(args.dsn, binary=binary).catalog())
    if args.output:
        args.output.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    print(
        f"census: tables={payload['tables']} verified={payload['verified']} "
        f"failures={len(payload['failures'])} warnings={len(payload['warnings'])}"
    )
    for failure in payload["failures"]:
        print(f"FAIL {failure['source']}: {failure['error']}")
    if temporary is not None:
        temporary.unlink(missing_ok=True)
    return 0 if payload["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
