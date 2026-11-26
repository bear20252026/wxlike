"""Build a deterministic, reviewable table plan before any row is copied."""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict
from pathlib import Path

from .model import ImportPlan, SourceTable, TablePlan
from .sqlite_source import (
    data_version,
    file_sha256,
    list_tables,
    open_readonly,
    source_table,
)

# Money is not a plain row copy: M013 derives ledger accounts from immutable
# entries. Copying `wallets`/`ledger` directly would create a second balance
# source and defeat the reconciliation. Everything else defaults to a same-name
# copy; the PG catalog check later decides whether the target is a table or a
# writable compatibility view.
DEFAULT_SKIP = {
    "wallets": "M013 derives ledger_accounts from immutable ledger entries",
    "ledger": "M013 replays ledger entries into journals, never copies rows",
}


def load_mapping(path: Path | None) -> dict:
    if path is None:
        return {"skip": {}, "targets": {}}
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError("mapping must be a JSON object")
    skip = payload.get("skip", {})
    targets = payload.get("targets", {})
    if not isinstance(skip, dict) or not isinstance(targets, dict):
        raise ValueError("mapping.skip and mapping.targets must be objects")
    for table, reason in skip.items():
        if not isinstance(reason, str) or not reason:
            raise ValueError(f"mapping.skip[{table!r}] needs a reason")
    for table, target in targets.items():
        if not isinstance(target, str) or not target:
            raise ValueError(f"mapping.targets[{table!r}] must be a table name")
    return {"skip": dict(skip), "targets": dict(targets)}


def dependency_order(tables: dict[str, SourceTable]) -> tuple[list[str], list[str]]:
    """Parents before children; break a cycle deterministically and warn.

    A forced break is safer than silently reversing rows: the loader will still
    fail on the actual foreign key, and the warning points at the tables that
    need a deliberate decision.
    """

    children: dict[str, set[str]] = {name: set() for name in tables}
    indegree: dict[str, int] = {name: 0 for name in tables}
    for name, table in tables.items():
        parents = {fk.ref_table for fk in table.foreign_keys if fk.ref_table in tables}
        for parent in parents:
            if parent == name:
                continue
            if name not in children[parent]:
                children[parent].add(name)
                indegree[name] += 1

    remaining = set(tables)
    order: list[str] = []
    warnings: list[str] = []
    while remaining:
        ready = sorted(name for name in remaining if indegree[name] == 0)
        if not ready:
            forced = min(remaining)
            warnings.append(f"foreign-key cycle broken at {forced}")
            ready = [forced]
        for name in ready:
            if name not in remaining:
                continue
            remaining.remove(name)
            order.append(name)
            for child in sorted(children[name]):
                if child in remaining:
                    indegree[child] -= 1
    return order, warnings


def build_plan(
    sqlite_path: Path,
    mapping_path: Path | None = None,
) -> ImportPlan:
    mapping = load_mapping(mapping_path)
    source = sqlite_path.resolve()
    connection = open_readonly(source)
    try:
        tables = {name: source_table(connection, name) for name in list_tables(connection)}
        order, warnings = dependency_order(tables)
        plan = ImportPlan(
            source_path=source,
            source_sha256=file_sha256(source),
            source_data_version=data_version(connection),
            warnings=list(warnings),
        )
        skip = dict(DEFAULT_SKIP)
        skip.update(mapping["skip"])
        targets = mapping["targets"]
        for name in order:
            if name in skip:
                plan.skipped.append((name, skip[name]))
                continue
            table = tables[name]
            target = targets.get(name, name)
            plan.tables.append(
                TablePlan(
                    source=name,
                    target=target,
                    mode="table",
                    columns=tuple(table.insertable_columns),
                    primary_key=table.primary_key,
                )
            )
        for name, reason in skip.items():
            if name not in tables:
                plan.warnings.append(f"mapping skips unknown table {name}: {reason}")
        for name in targets:
            if name not in tables:
                plan.warnings.append(f"mapping targets unknown table {name}")
    finally:
        connection.close()
    return plan


def plan_payload(plan: ImportPlan) -> dict:
    return {
        "source": {
            "path": str(plan.source_path),
            "sha256": plan.source_sha256,
            "data_version": plan.source_data_version,
        },
        "tables": [
            {
                "source": table.source,
                "target": table.target,
                "mode": table.mode,
                "columns": [asdict(column) for column in table.columns],
                "primary_key": list(table.primary_key),
                "reason": table.reason,
            }
            for table in plan.tables
        ],
        "skipped": [{"table": table, "reason": reason} for table, reason in plan.skipped],
        "warnings": plan.warnings,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="plan a SQLite -> PostgreSQL row import")
    parser.add_argument("--sqlite", required=True, type=Path)
    parser.add_argument("--mapping", type=Path)
    parser.add_argument("--out", type=Path, help="write JSON here instead of stdout")
    args = parser.parse_args(argv)

    plan = build_plan(args.sqlite, args.mapping)
    text = json.dumps(plan_payload(plan), ensure_ascii=False, indent=2) + "\n"
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(text, encoding="utf-8")
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
