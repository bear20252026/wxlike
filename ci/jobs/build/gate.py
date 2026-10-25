#!/usr/bin/env python3
"""Aggregate the required jobs into one merge-blocking check.

``gate`` always runs. Any ``failure``/``cancelled`` dependency fails. A
``skipped`` dependency is accepted only because every job is skipped through
its C0 selection condition; a job skipped because its dependency failed is
already covered by that dependency's failure.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]


def main() -> int:
    required = json.loads((ROOT / "ci/jobs/policy/required-checks.json").read_text(encoding="utf-8"))["required"]
    raw = os.environ.get("NEEDS_JSON", "{}")
    needs = json.loads(raw)
    missing = [name for name in required if name != "gate" and name not in needs]
    failed = [name for name, data in needs.items() if data.get("result") in {"failure", "cancelled"}]
    skipped = [name for name, data in needs.items() if data.get("result") == "skipped"]

    print("C0-C11 gate")
    for name in sorted(needs):
        print(f"  {name}: {needs[name].get('result')}")
    if skipped:
        print(f"  path-selected skips: {', '.join(sorted(skipped))}")
    if missing:
        print(f"ERROR required jobs absent from gate.needs: {sorted(missing)}", file=sys.stderr)
    if failed:
        print(f"ERROR failed or cancelled required jobs: {sorted(failed)}", file=sys.stderr)
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as fh:
            fh.write("### C0-C11 gate\n\n| job | result |\n|---|---|\n")
            for name in sorted(needs):
                fh.write(f"| `{name}` | {needs[name].get('result')} |\n")
            if missing:
                fh.write(f"\n**Missing required jobs:** {', '.join(missing)}\n")
    if missing or failed:
        return 1
    print("gate: passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
