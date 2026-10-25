#!/usr/bin/env python3
"""Changed-path classification for CI (fail-safe: unknown => full matrix)."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

from lib.gitdiff import GitError, changed_files  # noqa: E402
from lib.paths import GlobSet, normalize  # noqa: E402

ROOT = HERE.parents[2]
POLICY = HERE.parent / "policy" / "paths.json"
TARGETS = ("rust", "contract", "ui", "mobile", "desktop", "ci")


def classify(files: list[str], policy: dict) -> dict:
    files = [normalize(f) for f in files]
    ignore = GlobSet(policy.get("ignore", []))
    full_triggers = GlobSet(policy.get("full_matrix_triggers", []))
    target_sets = {name: GlobSet(policy["targets"].get(name, [])) for name in TARGETS}

    targets = {name: False for name in TARGETS}
    full_reason = None
    unmatched: list[str] = []

    for path in files:
        trigger = full_triggers.match(path)
        if trigger:
            full_reason = full_reason or f"{path} matches full-matrix trigger {trigger}"
        for name, gs in target_sets.items():
            if gs.any(path):
                targets[name] = True
        known = any(gs.any(path) for gs in target_sets.values()) or ignore.any(path)
        if not known:
            unmatched.append(path)

    if unmatched:
        full_reason = full_reason or f"unclassified paths: {', '.join(sorted(unmatched)[:5])}"

    if full_reason:
        targets = {name: True for name in TARGETS}

    return {
        "files": files,
        "targets": targets,
        "full": full_reason is not None,
        "reason": full_reason or ("no code targets" if not files else "path-selected"),
    }


def read_changed(args, root: Path) -> tuple[list[str], str | None]:
    if args.changed_file:
        return list(args.changed_file), None
    if args.changed_files_file:
        path = Path(args.changed_files_file)
        return [line.strip() for line in path.read_text(encoding="utf-8").splitlines() if line.strip()], None
    try:
        base = args.base
        head = args.head
        if base is None or head is None:
            return [], "base/head not supplied"
        return changed_files(root, base, head), None
    except GitError as err:
        return [], str(err)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--changed-file", action="append", default=[])
    parser.add_argument("--changed-files-file")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--github-output", help="append key=value outputs to this file")
    parser.add_argument("--policy", type=Path, default=POLICY)
    args = parser.parse_args(argv)

    policy = json.loads(args.policy.read_text(encoding="utf-8"))
    files, git_error = read_changed(args, args.root)
    result = classify(files, policy)
    if git_error:
        result["full"] = True
        result["reason"] = f"cannot compute diff: {git_error}"
        result["targets"] = {name: True for name in TARGETS}

    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print(f"select: {result['reason']}")
        for name in TARGETS:
            print(f"  {name}: {'true' if result['targets'][name] else 'false'}")

    if args.github_output:
        lines = [f"{name}={str(result['targets'][name]).lower()}" for name in TARGETS]
        lines.append(f"full={str(result['full']).lower()}")
        lines.append(f"reason={result['reason']}")
        with open(args.github_output, "a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as fh:
            fh.write(f"### C0 path selection\n\n{result['reason']}\n\n")
            for name in TARGETS:
                fh.write(f"- `{name}`: {result['targets'][name]}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
