#!/usr/bin/env python3
"""Turn a Trivy image JSON report into a job summary, and gate it on demand.

Landing a scanner inside a required job with a blocking exit code is how every
PR in the repository turns red overnight, so the gate starts off: the report and
the counts are published first, and `--fail-on` is flipped once the baseline is
known and fixed. The gap stays registered in ci/jobs/policy/known-gaps.json.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

SEVERITIES = ("CRITICAL", "HIGH", "MEDIUM", "LOW")


def summarize(report: dict) -> tuple[dict[str, int], dict[str, int]]:
    by_severity = dict.fromkeys(SEVERITIES, 0)
    unfixed_by_severity = dict.fromkeys(SEVERITIES, 0)
    for result in report.get("Results") or []:
        for vuln in result.get("Vulnerabilities") or []:
            severity = vuln.get("Severity", "UNKNOWN")
            if severity in by_severity:
                by_severity[severity] += 1
                if not vuln.get("FixedVersion"):
                    unfixed_by_severity[severity] += 1
    return by_severity, unfixed_by_severity


def write_summary(path: str | None, line: str, rows: list[dict], limit: int = 40) -> None:
    if not path:
        return
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(f"### {line}\n\n")
        for vuln in rows[:limit]:
            handle.write(
                f"- `{vuln['VulnerabilityID']}` {vuln['Severity']} in `{vuln['PkgName']}` "
                f"{vuln.get('InstalledVersion', '')} -> fix `{vuln.get('FixedVersion') or 'none'}`\n"
            )
        if len(rows) > limit:
            handle.write(f"- …另 {len(rows) - limit} 条见 trivy-server-image.json\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument(
        "--fail-on",
        default="none",
        choices=("none", "critical", "critical-high"),
        help="severities that must block the job; none while the baseline is unknown",
    )
    args = parser.parse_args(argv)

    if not args.report.is_file() or args.report.stat().st_size == 0:
        print(f"ERROR {args.report} missing or empty: the scan never produced evidence", file=sys.stderr)
        return 1
    report = json.loads(args.report.read_text(encoding="utf-8"))
    counts, unfixed = summarize(report)
    rows = [
        vuln
        for result in report.get("Results") or []
        for vuln in result.get("Vulnerabilities") or []
    ]
    line = "image scan: " + " ".join(
        f"{name}={counts[name]} (unfixed {unfixed[name]})" for name in ("CRITICAL", "HIGH")
    )
    print(line)
    write_summary(os.environ.get("GITHUB_STEP_SUMMARY"), line, rows)

    blocking = {"none": (), "critical": ("CRITICAL",), "critical-high": ("CRITICAL", "HIGH")}[args.fail_on]
    hit = [name for name in blocking if counts[name] > 0]
    if hit:
        for name in hit:
            print(f"::error::{name} findings present ({counts[name]}) and --fail-on includes {name}", file=sys.stderr)
        return 1
    if args.fail_on == "none" and counts["CRITICAL"]:
        print(f"report-only: {counts['CRITICAL']} CRITICAL finding(s) still unaddressed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
