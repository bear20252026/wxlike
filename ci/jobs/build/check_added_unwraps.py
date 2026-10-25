#!/usr/bin/env python3
"""Block newly added ``unwrap()``/``expect()`` in critical production paths.

The repository-wide baseline is enforced by ``scripts/count_unwraps.py
--fail-on-increase``; this script is the stricter per-diff gate. A line inside
a ``#[cfg(test)]`` module of the same file is ignored (tests are not shipped).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT / "ci" / "jobs"))

from lib.gitdiff import GitError, added_line_text, changed_files  # noqa: E402
from lib.paths import GlobSet  # noqa: E402

UNWRAP_RE = re.compile(r"\.unwrap\s*\(\s*\)|\.expect\s*\(")


def test_ranges(path: Path) -> list[tuple[int, int]]:
    """Line ranges covered by `#[cfg(test)] mod ... { ... }`.

    Brace counting is a heuristic but sufficient for this repository's style:
    the attribute is followed by a module declaration on the same or next line.
    """
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return []
    ranges: list[tuple[int, int]] = []
    idx = 0
    while idx < len(lines):
        if "#[cfg(test)]" not in lines[idx]:
            idx += 1
            continue
        start = idx + 1
        decl = None
        for probe in range(idx + 1, min(idx + 4, len(lines))):
            if "mod " in lines[probe] and "{" in lines[probe]:
                decl = probe
                break
        if decl is None:
            idx += 1
            continue
        depth = 0
        end = decl
        for cursor in range(decl, len(lines)):
            depth += lines[cursor].count("{") - lines[cursor].count("}")
            end = cursor
            if depth <= 0:
                break
        ranges.append((start + 1, end + 1))
        idx = end + 1
    return ranges


def in_ranges(line_no: int, ranges: list[tuple[int, int]]) -> bool:
    return any(start <= line_no <= end for start, end in ranges)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--base", default=os.environ.get("CI_BASE_SHA", "origin/main"))
    parser.add_argument("--head", default=os.environ.get("CI_HEAD_SHA", "HEAD"))
    parser.add_argument("--changed-file", action="append", default=[])
    parser.add_argument("--changed-files-file")
    parser.add_argument("--policy", type=Path, default=HERE.parent / "policy" / "unwrap-critical.json")
    args = parser.parse_args(argv)

    policy = json.loads(args.policy.read_text(encoding="utf-8"))
    critical = GlobSet(policy["paths"])

    if args.changed_file:
        files = list(args.changed_file)
    elif args.changed_files_file:
        files = [line.strip() for line in Path(args.changed_files_file).read_text(encoding="utf-8").splitlines() if line.strip()]
    else:
        try:
            files = changed_files(args.root, args.base, args.head)
        except GitError as err:
            print(f"ERROR cannot compute diff: {err}", file=sys.stderr)
            return 1

    targets = [path for path in files if critical.any(path)]
    if not targets:
        print("unwrap gate: no critical production files changed")
        return 0

    violations: list[str] = []
    for path in targets:
        try:
            added = added_line_text(args.root, args.base, args.head, path)
        except GitError as err:
            print(f"ERROR cannot diff {path}: {err}", file=sys.stderr)
            return 1
        ranges = test_ranges(args.root / path)
        for line_no, text in sorted(added.items()):
            if in_ranges(line_no, ranges):
                continue
            if UNWRAP_RE.search(text):
                violations.append(f"{path}:{line_no}: {text.strip()}")

    if violations:
        print("ERROR new unwrap()/expect() in critical production paths:", file=sys.stderr)
        for violation in violations:
            print(f"  {violation}", file=sys.stderr)
        return 1
    print(f"unwrap gate: scanned {len(targets)} critical files, no added unwrap/expect")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
