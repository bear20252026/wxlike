#!/usr/bin/env python3
"""Validate the shared contract artefacts that CI can see today.

Blocks on structural breakage (invalid JSON, duplicate IDs, seed drift). The
full action registry is owned by G00-03; until it exists this checker reports
``PENDING`` and only ``--require-registry`` (used by the release gate) fails.
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
# Canonical registry schema frozen by G00-03 (`composition/entries.json`).
REQUIRED_ENTRY_FIELDS = {
    "entry_id",
    "seed_id",
    "scope",
    "platform_design",
    "action_id",
    "test_id",
    "owners",
    "states",
    "review_status",
}
REQUIRED_STATE_KEYS = {
    "success",
    "empty_result",
    "param_error",
    "unauthorized",
    "timeout_or_offline",
    "cancel",
    "write_idempotency_required",
}
REVIEW_STATES = {"owner_confirmed", "requires_domain_review", "requires_ui_review"}


def load_json(path: Path) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as err:
        raise ValueError(f"{path}: {err}") from err


def check_contracts(root: Path) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    notes: list[str] = []
    contracts = root / "contracts"
    if not contracts.exists():
        return errors, ["PENDING contracts/ does not exist yet"]

    for path in sorted(contracts.rglob("*.json")):
        try:
            data = load_json(path)
        except ValueError as err:
            errors.append(str(err))
            continue
        if path.name.endswith(".schema.json"):
            if not isinstance(data, dict) or data.get("type") is None:
                errors.append(f"{path}: schema has no type")
            if not str(data.get("$schema", "")).startswith("https://json-schema.org/"):
                errors.append(f"{path}: missing json-schema $schema marker")
    return errors, notes


def check_seed(root: Path) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    notes: list[str] = []
    seed = root / "docs" / "full-platform-delivery" / "entry_seed.csv"
    if not seed.exists():
        return ["entry_seed.csv missing"], []
    with seed.open(encoding="utf-8", newline="") as fh:
        rows = list(csv.DictReader(fh))
    ids = [row.get("entry_id", "") for row in rows]
    dupes = [key for key, count in Counter(ids).items() if count > 1]
    if dupes:
        errors.append(f"entry_seed.csv duplicate entry_id: {dupes[:5]}")
    if any(not entry for entry in ids):
        errors.append("entry_seed.csv has empty entry_id rows")
    product = [row for row in rows if row.get("platform") != "showcase"]
    notes.append(f"seed rows={len(rows)} product={len(product)} showcase={len(rows) - len(product)}")
    return errors, notes


def check_registry(root: Path, require: bool) -> tuple[list[str], list[str]]:
    errors: list[str] = []
    notes: list[str] = []
    path = root / "composition" / "entries.json"
    seed_path = root / "docs" / "full-platform-delivery" / "entry_seed.csv"
    if not path.exists():
        message = "PENDING composition/entries.json (G00-01/G00-03) not present"
        (errors if require else notes).append(message)
        return errors, notes

    try:
        data = load_json(path)
    except ValueError as err:
        return [str(err)], notes
    rows = data.get("entries") if isinstance(data, dict) else data
    if not isinstance(rows, list):
        return [f"{path}: expected an entries array"], notes

    ids: list[str] = []
    seed_ids_registered: list[str] = []
    for idx, row in enumerate(rows):
        if not isinstance(row, dict):
            errors.append(f"{path}[{idx}]: entry must be an object")
            continue
        missing = REQUIRED_ENTRY_FIELDS - row.keys()
        if missing:
            errors.append(f"{path}[{idx}]: missing fields {sorted(missing)}")
        ids.append(str(row.get("entry_id", "")))
        seed_value = row.get("seed_id")
        if seed_value is not None:
            if not str(seed_value).strip():
                errors.append(f"{path}[{idx}]: empty seed_id")
            else:
                seed_ids_registered.append(str(seed_value))
        if row.get("scope") not in {"product", "showcase"}:
            errors.append(f"{path}[{idx}]: invalid scope {row.get('scope')!r}")
        if row.get("review_status") not in REVIEW_STATES:
            errors.append(f"{path}[{idx}]: invalid review_status {row.get('review_status')!r}")
        owners = row.get("owners") or {}
        if not isinstance(owners, dict) or not owners.get("domain") or not owners.get("ui"):
            errors.append(f"{path}[{idx}]: owners.domain/owners.ui are required")
        states = row.get("states") or {}
        missing_states = REQUIRED_STATE_KEYS - (states.keys() if isinstance(states, dict) else set())
        if missing_states:
            errors.append(f"{path}[{idx}]: states missing {sorted(missing_states)}")
        action_id = str(row.get("action_id", ""))
        test_id = str(row.get("test_id", ""))
        if not action_id or action_id.startswith("TO_"):
            if row.get("scope") == "product":
                errors.append(f"{path}[{idx}]: product entry has no frozen action_id ({action_id!r})")
            else:
                notes.append(f"{path}[{idx}]: showcase entry keeps placeholder action_id")
        if not test_id or test_id.startswith("TO_"):
            errors.append(f"{path}[{idx}]: entry has no test_id ({test_id!r})")

    dupes = [key for key, count in Counter(ids).items() if key and count > 1]
    if dupes:
        errors.append(f"{path}: duplicate entry_id {dupes[:5]}")

    if seed_path.exists():
        with seed_path.open(encoding="utf-8", newline="") as fh:
            seed_ids = {row["entry_id"] for row in csv.DictReader(fh) if row.get("entry_id")}
        missing = sorted(seed_ids - set(seed_ids_registered))
        if missing:
            errors.append(f"{path}: seed ids not registered: {missing[:8]} (+{max(0, len(missing) - 8)})")
        extra = sorted(set(seed_ids_registered) - seed_ids)
        if extra:
            notes.append(f"{path}: registry seed_ids without a seed row: {extra[:5]}")
    if isinstance(data, dict):
        counts = data.get("counts") or {}
        if counts.get("total") not in (None, len(rows)):
            errors.append(f"{path}: counts.total={counts.get('total')} != {len(rows)} rows")
    notes.append(f"registry entries={len(rows)}")
    return errors, notes


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--require-registry", action="store_true")
    args = parser.parse_args(argv)

    all_errors: list[str] = []
    all_notes: list[str] = []
    for fn in (check_contracts, check_seed):
        errors, notes = fn(args.root)
        all_errors += errors
        all_notes += notes
    errors, notes = check_registry(args.root, args.require_registry)
    all_errors += errors
    all_notes += notes

    for note in all_notes:
        print(f"NOTE {note}")
    for error in all_errors:
        print(f"ERROR {error}", file=sys.stderr)
    print(f"contract check: {len(all_errors)} error(s), {len(all_notes)} note(s)")
    return 1 if all_errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
