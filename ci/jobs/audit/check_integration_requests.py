#!/usr/bin/env python3
"""integration-requests handoff files must stay machine-readable (C10, #306).

``integration-requests/G*/**.yaml`` is the only cross-group handoff
protocol in this repo: G00 reads these files to execute root-manifest,
route, command-mount and schema requests. For months 22 of the 106
files were not parseable YAML at all (Chinese prose sitting bare in a
mapping, Markdown backticks in unquoted scalars), and nothing in CI
ever parsed one, so nobody was told.

The gate is deliberately format-level, not schema-level: the corpus
spans several generations of the handoff format, so there is no
single mandatory key set. What every file must satisfy:

* ``yaml.safe_load`` succeeds;
* the top level is a mapping;
* the mapping carries at least one identity key (`task`, `issue`,
  `task_issue`, `request_id`, `card`, `group_issue`) so a reader can
  tell which handoff it is holding.

Semantic review of a request stays with G00; this gate only promises
the file can be read by a machine at all.
"""

from __future__ import annotations

import sys
from pathlib import Path

import yaml

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
REQUESTS_DIR = ROOT / "integration-requests"
IDENTITY_KEYS = {"task", "issue", "task_issue", "request_id", "card", "group_issue"}


def check_file(path: Path) -> str | None:
    """Return a one-line problem description, or None when the file is fine."""
    try:
        doc = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        first = str(exc).strip().splitlines()[0] if str(exc).strip() else "YAML error"
        return f"does not parse: {first}"
    if not isinstance(doc, dict):
        return f"top level is {type(doc).__name__}, not a mapping"
    if not IDENTITY_KEYS.intersection(doc):
        return "no identity key (one of task/issue/task_issue/request_id/card/group_issue)"
    return None


def main() -> int:
    files = sorted(REQUESTS_DIR.rglob("*.yaml")) + sorted(REQUESTS_DIR.rglob("*.yml"))
    problems = [(p, check_file(p)) for p in files]
    failures = [(p, why) for p, why in problems if why]
    for path, why in failures:
        print(f"FAIL {path.relative_to(ROOT)}: {why}")
    print(f"integration-requests: {len(files) - len(failures)}/{len(files)} machine-readable")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
