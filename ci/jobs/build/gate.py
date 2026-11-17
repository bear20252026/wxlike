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


# R3（第二轮审计 §3）：选路标志**从工作流自动提取**，不再手工镜像。
# 手工表两次漂移（web-v2/desktop-package 漏映射、unwrap 漏 contract），
# 证明"人记得同步"不可靠。真实事实源是 generated/ci.yml 里每个作业的
# `if:` 表达式——gate 与作业条件同源，漂移在结构上不可能。
# 下面的静态表仅作**兜底**（工作流文件缺失/解析失败时用），并由
# test_ci 的对账测试保证兜底表与真实条件一致（兜底也不许漂移）。
_FALLBACK_SELECT_FLAGS = {
    "fmt": ["rust"],
    "clippy": ["rust"],
    "test": ["rust"],
    "native": ["rust"],
    "doc": ["rust"],
    "wasm": ["rust"],
    "unwrap": ["rust", "contract"],
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

_SELECT_FLAG_RE = None  # lazy compile


def job_select_flags() -> dict[str, list[str]]:
    """从 generated/ci.yml 提取每个作业的 select 驱动标志（真实事实源）。

    只认 ``needs.select.outputs.<flag>`` 引用；if:false / 无 if 的作业不在
    表里（前者走退役台账，后者视为恒跑）。解析失败 → 退回静态兜底表并
    打印警告（fail-closed 方向：兜底表若也缺该作业，跳过仍判未解释）。
    """
    global _SELECT_FLAG_RE
    import re

    if _SELECT_FLAG_RE is None:
        _SELECT_FLAG_RE = re.compile(r"needs\.select\.outputs\.(\w+)")
    path = ROOT / "ci/jobs/generated/ci.yml"
    try:
        import yaml

        workflow = yaml.safe_load(path.read_text(encoding="utf-8"))
        flags: dict[str, list[str]] = {}
        for name, job in (workflow.get("jobs") or {}).items():
            expr = job.get("if")
            if not isinstance(expr, str):
                continue
            found = sorted(set(_SELECT_FLAG_RE.findall(expr)))
            if found:
                flags[name] = found
        if flags:
            return flags
        print("WARNING no select flags parsed from ci.yml; using fallback table", file=sys.stderr)
    except Exception as exc:  # noqa: BLE001 — 兜底路径要宽捕获
        print(f"WARNING cannot parse ci.yml select flags ({exc}); using fallback table", file=sys.stderr)
    return dict(_FALLBACK_SELECT_FLAGS)


# 兼容旧名（测试与外部引用）：动态提取，不再是死表。
JOB_SELECT_FLAGS = job_select_flags()


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
