#!/usr/bin/env python3
"""Aggregate the required jobs into one merge-blocking check.

``gate`` always runs. ``success`` is green. ``skipped`` is green ONLY when the
skip is *explained*: either the job's C0 select output is false (path-selected
skip), or the job is registered in ``policy/disabled-jobs.json`` (a declared
retirement). An *unexplained* skip is red — this closes the hole where a job
set to ``if: false`` silently passed as green forever (2026-11-16 audit).

The result set is a whitelist, not a blacklist, because GitHub keeps inventing
result strings: ``abandoned`` is what ``needs`` reports when one matrix leg is
cancelled, and a blacklist of ``failure``/``cancelled`` let a build that never
finished (``native (macos-latest)``, 2026-10-26) pass as green.

Inputs (env): ``NEEDS_JSON`` = toJSON(needs) (job→result); ``SELECT_JSON`` =
toJSON(needs.select.outputs) (C0 path-selection flags). The retirement ledger
is read from disk. ``check_pins.py`` statically enforces that every ``if: false``
job is ledgered; this runtime check enforces that every *observed* skip is
explained. Together they make "green" mean "verified or explicitly retired".
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]

GREEN_RESULTS = {"success", "skipped"}


def load_disabled() -> set[str]:
    """Retired job names (fail-closed: a malformed ledger is an error, not a pass)."""
    path = ROOT / "ci/jobs/policy/disabled-jobs.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    return {entry["job"] for entry in data.get("disabled", [])}


# 每个作业受哪些 C0 选路标志驱动（镜像各 job.yml 的 `if:` 条件）。
# 作业在**任一**标志为 true 时运行；只有全部标志都 false 才是路径跳过。
# 新增/改作业 if 条件时要同步这里——否则该作业的合法跳过会被误判为"未解释"。
JOB_SELECT_FLAGS = {
    "fmt": ["rust"],
    "clippy": ["rust"],
    "test": ["rust"],
    "native": ["rust"],
    "doc": ["rust"],
    "wasm": ["rust"],
    "unwrap": ["rust"],
    "coverage": ["rust", "contract"],
    "audit": ["rust", "ci"],
    "contract": ["contract", "rust"],
    "ui": ["ui", "desktop"],
    "web-v2": ["ui"],
    "desktop": ["desktop", "ui"],
    "desktop-package": ["desktop", "ui"],
    "image": ["rust", "ci"],
    "ingest-image": ["rust", "ci"],
    "e2e": ["rust", "ci"],
    "performance": ["rust", "ci"],
    "ios": ["mobile", "contract"],
    # flutter / android 已退役（disabled-jobs.json），不走选路判定。
}


def select_skips_job(name: str, select: dict) -> bool:
    """Whether C0 path-selection deselects this job.

    A job is path-skipped only if **all** its driving select flags are explicitly
    false (mirrors the `if: a == 'true' || b == 'true'` condition). Unknown jobs
    are NOT assumed path-skipped, so an unexplained skip stays red.
    """
    domain = name.split(" ")[0].split("(")[0].strip()
    flags = JOB_SELECT_FLAGS.get(domain)
    if not flags:
        return False
    for flag in flags:
        value = select.get(flag)
        if isinstance(value, str) and value.lower() == "true":
            return False  # 任一标志为 true → 作业本应运行，不是路径跳过
    return True


def main() -> int:
    required = json.loads((ROOT / "ci/jobs/policy/required-checks.json").read_text(encoding="utf-8"))["required"]
    needs = json.loads(os.environ.get("NEEDS_JSON", "{}"))
    select = json.loads(os.environ.get("SELECT_JSON", "{}"))
    disabled = load_disabled()

    missing = [name for name in required if name != "gate" and name not in needs]
    not_green = {name: data.get("result") for name, data in needs.items() if data.get("result") not in GREEN_RESULTS}

    path_skipped, retired, unexplained = [], [], []
    for name, data in needs.items():
        if data.get("result") != "skipped":
            continue
        if name in disabled:
            retired.append(name)
        elif select_skips_job(name, select):
            path_skipped.append(name)
        else:
            unexplained.append(name)

    print("C0-C11 gate")
    for name in sorted(needs):
        print(f"  {name}: {needs[name].get('result')}")
    if path_skipped:
        print(f"  path-selected skips: {', '.join(sorted(path_skipped))}")
    if retired:
        print(f"  declared retirements (disabled-jobs.json): {', '.join(sorted(retired))}")
    if missing:
        print(f"ERROR required jobs absent from gate.needs: {sorted(missing)}", file=sys.stderr)
    if not_green:
        print(f"ERROR required jobs are not green (only success/skipped pass): {sorted(not_green.items())}", file=sys.stderr)
    if unexplained:
        print(
            f"ERROR unexplained skips (neither path-selected nor ledgered): {sorted(unexplained)}. "
            "Register a permanent retirement in ci/jobs/policy/disabled-jobs.json or fix the job.",
            file=sys.stderr,
        )
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as fh:
            fh.write("### C0-C11 gate\n\n| job | result |\n|---|---|\n")
            for name in sorted(needs):
                fh.write(f"| `{name}` | {needs[name].get('result')} |\n")
            if missing:
                fh.write(f"\n**Missing required jobs:** {', '.join(missing)}\n")
            if unexplained:
                fh.write(f"\n**Unexplained skips:** {', '.join(unexplained)}\n")
    if missing or not_green or unexplained:
        return 1
    print("gate: passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
