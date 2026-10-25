#!/usr/bin/env python3
"""C11 security-path coverage gate.

The repository has a long tail of uncovered code; demanding a global jump
would only produce `#[ignore]`s. This gate instead asks: *the security-sensitive
lines this PR touched, are they exercised?* It also enforces a modest per-file
floor so a file cannot rot while every PR touches two covered lines.

Missing lcov while security files changed is a failure, not a skip.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT / "ci" / "jobs"))

from lib.gitdiff import GitError, added_lines, changed_files  # noqa: E402
from lib.paths import GlobSet, normalize  # noqa: E402


def parse_lcov(path: Path) -> dict[str, dict[int, int]]:
    files: dict[str, dict[int, int]] = {}
    current: str | None = None
    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw.strip()
        if line.startswith("SF:"):
            current = normalize(line[3:])
            files.setdefault(current, {})
        elif line.startswith("DA:") and current:
            body = line[3:].split(",")
            if len(body) >= 2:
                try:
                    files[current][int(body[0])] = int(body[1])
                except ValueError:
                    continue
        elif line == "end_of_record":
            current = None
    return files


def match_lcov(files: dict[str, dict[int, int]], rel: str) -> tuple[str, dict[int, int]] | None:
    rel = normalize(rel)
    if rel in files:
        return rel, files[rel]
    suffix = "/" + rel
    for key, value in files.items():
        if key.endswith(suffix):
            return key, value
    return None


def ratio(hit: int, total: int) -> float:
    return 1.0 if total == 0 else hit / total


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--lcov", type=Path, default=Path("lcov.info"))
    parser.add_argument("--base", default=os.environ.get("CI_BASE_SHA", "origin/main"))
    parser.add_argument("--head", default=os.environ.get("CI_HEAD_SHA", "HEAD"))
    parser.add_argument("--changed-file", action="append", default=[])
    parser.add_argument("--policy", type=Path, default=HERE.parent / "policy" / "coverage.json")
    args = parser.parse_args(argv)

    policy = json.loads(args.policy.read_text(encoding="utf-8"))
    critical = GlobSet(policy["security_paths"])
    min_changed = float(policy["min_changed_line_coverage"])
    min_file = float(policy["min_file_coverage"])

    if args.changed_file:
        files = list(args.changed_file)
    else:
        try:
            files = changed_files(args.root, args.base, args.head)
        except GitError as err:
            print(f"ERROR cannot compute diff: {err}", file=sys.stderr)
            return 1

    targets = [path for path in files if critical.any(path)]
    if not targets:
        print("coverage gate: no security-sensitive files changed")
        return 0

    lcov_path = args.lcov if args.lcov.is_absolute() else args.root / args.lcov
    if not lcov_path.exists():
        print(f"ERROR coverage gate: {lcov_path} missing but security files changed", file=sys.stderr)
        return 1
    coverage = parse_lcov(lcov_path)

    failures: list[str] = []
    print(f"coverage gate: {len(targets)} security-sensitive file(s) changed")
    for path in targets:
        try:
            changed = added_lines(args.root, args.base, args.head, path)
        except GitError as err:
            print(f"ERROR cannot diff {path}: {err}", file=sys.stderr)
            return 1
        if not changed:
            continue
        matched = match_lcov(coverage, path)
        if matched is None:
            message = f"{path}: changed lines {sorted(changed)[:5]} but no lcov record"
            if policy.get("fail_if_uninstrumented", True):
                failures.append(message)
            else:
                print(f"WARN {message}")
            continue
        _, rows = matched
        instrumented = [line for line in changed if line in rows]
        hit = [line for line in instrumented if rows[line] > 0]
        changed_ratio = ratio(len(hit), len(instrumented))
        file_ratio = ratio(sum(1 for value in rows.values() if value > 0), len(rows))
        print(
            f"  {path}: changed {len(hit)}/{len(instrumented)} = {changed_ratio:.0%}, "
            f"file {file_ratio:.0%}"
        )
        if not instrumented:
            failures.append(f"{path}: changed lines are not instrumented")
        elif changed_ratio < min_changed:
            failures.append(f"{path}: changed-line coverage {changed_ratio:.0%} < {min_changed:.0%}")
        if file_ratio < min_file:
            failures.append(f"{path}: file coverage {file_ratio:.0%} < {min_file:.0%}")

    if failures:
        for failure in failures:
            print(f"ERROR {failure}", file=sys.stderr)
        return 1
    print("coverage gate: passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
