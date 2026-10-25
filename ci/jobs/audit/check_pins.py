#!/usr/bin/env python3
"""Action-pin and workflow-policy checks (C10).

Two independent rules, both blocking:

* every ``uses:`` is a 40-hex commit (or a local ``./`` action, or a
  ``docker://...@sha256:`` reference);
* the composed workflow cannot escape a required check through
  ``continue-on-error`` and every required job is reachable from ``gate``.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

import yaml

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
POLICY_DIR = ROOT / "ci" / "jobs" / "policy"
USES_RE = re.compile(r"^\s*(?:-\s*)?uses:\s*['\"]?([^'\"\s#]+)")
SHA_RE = re.compile(r"@[0-9a-f]{40}$")
DOCKER_SHA_RE = re.compile(r"docker://.+@sha256:[0-9a-f]{64}$")


def scan_files(paths: list[Path]) -> list[tuple[Path, int, str]]:
    rows = []
    for path in paths:
        for no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            m = USES_RE.match(line)
            if m:
                rows.append((path, no, m.group(1)))
    return rows


def pin_index(pins: dict[str, str]) -> dict[str, dict[str, str]]:
    index: dict[str, dict[str, str]] = {}
    for key, sha in pins.items():
        repo, _, tag = key.partition("@")
        index.setdefault(repo, {})[sha] = tag
    return index


def check_uses(ref: str, pins: dict[str, str], index: dict[str, dict[str, str]] | None = None) -> str | None:
    if ref.startswith("./"):
        return None
    if ref.startswith("docker://"):
        return None if DOCKER_SHA_RE.match(ref) else f"docker image is not pinned by digest: {ref}"
    if not SHA_RE.search(ref):
        return f"action is not pinned to a 40-hex SHA: {ref}"
    repo, sha = ref.rsplit("@", 1)
    index = index if index is not None else pin_index(pins)
    allowed = index.get(repo)
    if not allowed:
        return f"action {repo} is not in policy/action-pins.json"
    if sha not in allowed:
        expected = ", ".join(f"{tag}@{digest}" for digest, tag in sorted(allowed.items()))
        return f"action {repo} pinned to {sha}, policy allows {expected}"
    return None


def _walk_continue_on_error(node, path="job") -> list[str]:
    found = []
    if isinstance(node, dict):
        for key, value in node.items():
            here = f"{path}.{key}"
            if key == "continue-on-error" and value is True:
                found.append(here)
            found.extend(_walk_continue_on_error(value, here))
    elif isinstance(node, list):
        for idx, value in enumerate(node):
            found.extend(_walk_continue_on_error(value, f"{path}[{idx}]"))
    return found


def check_workflow(path: Path, pins: dict[str, str], required: dict) -> list[str]:
    problems: list[str] = []
    text = path.read_text(encoding="utf-8")
    data = yaml.safe_load(text) or {}
    jobs = data.get("jobs") or {}
    if not isinstance(jobs, dict) or not jobs:
        return [f"{path}: no jobs"]

    top_permissions = data.get("permissions")
    if not isinstance(top_permissions, dict) or top_permissions.get("contents") != "read":
        problems.append(f"{path}: top-level permissions must be exactly contents: read")

    if "pull_request_target" in (data.get(True) or {}):
        problems.append(f"{path}: pull_request_target is forbidden")

    is_required_workflow = path.name == required.get("workflow", "ci.yml")
    required_jobs = required.get("required", [])
    if is_required_workflow:
        for name in required_jobs:
            if name not in jobs:
                problems.append(f"{path}: required job {name} is missing")
                continue
            escapes = _walk_continue_on_error(jobs[name], name)
            for escape in escapes:
                problems.append(f"{path}: required job {name} uses continue-on-error ({escape})")

    for name, job in jobs.items():
        for need in job.get("needs", []) if isinstance(job.get("needs"), list) else ([job["needs"]] if job.get("needs") else []):
            if need not in jobs:
                problems.append(f"{path}: job {name} needs unknown job {need}")

    env_map = required.get("environments", {})
    for name, spec in env_map.items():
        if isinstance(spec, str):
            spec = {"workflow": required.get("workflow", "ci.yml"), "environment": spec}
        if spec.get("workflow") != path.name:
            continue
        job = jobs.get(name)
        if job is None:
            problems.append(f"{path}: job {name} must exist (declared environment {spec['environment']})")
            continue
        if job.get("environment") != spec["environment"]:
            problems.append(f"{path}: job {name} must use protected environment {spec['environment']}")

    gate = jobs.get("gate")
    if gate is not None and is_required_workflow:
        gate_needs = set(gate.get("needs") or [])
        missing = [name for name in required_jobs if name not in gate_needs and name != "gate"]
        if missing:
            problems.append(f"{path}: gate does not depend on {', '.join(missing)}")

    index = pin_index(pins)
    for source, line, ref in scan_files([path]):
        err = check_uses(ref, pins, index)
        if err:
            problems.append(f"{source}:{line}: {err}")

    return problems


def default_sources(root: Path, include_fragments: bool = False) -> list[Path]:
    files = list((root / ".github" / "workflows").glob("*.yml"))
    files += list((root / ".github" / "workflows").glob("*.yaml"))
    if include_fragments:
        files += list((root / "ci" / "jobs").rglob("*.yml"))
    else:
        files += list((root / "ci" / "jobs" / "generated").glob("*.yml"))
    return sorted(set(files))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--pins", type=Path, default=POLICY_DIR / "action-pins.json")
    parser.add_argument("--required", type=Path, default=POLICY_DIR / "required-checks.json")
    parser.add_argument("--workflow", type=Path, action="append")
    parser.add_argument("--pins-only", action="store_true")
    parser.add_argument("--all-fragments", action="store_true", help="pin-check fragments as well as composed workflows")
    args = parser.parse_args(argv)

    pins = json.loads(args.pins.read_text(encoding="utf-8")).get("actions", {})
    required = json.loads(args.required.read_text(encoding="utf-8"))
    if args.workflow:
        sources = args.workflow
    else:
        sources = default_sources(args.root, include_fragments=args.pins_only or args.all_fragments)
    if not sources:
        print("no workflow files found; compose the workflows before running the policy check")
        return 1

    problems: list[str] = []
    for path in sources:
        if args.pins_only:
            for source, line, ref in scan_files([path]):
                err = check_uses(ref, pins)
                if err:
                    problems.append(f"{source}:{line}: {err}")
        else:
            problems.extend(check_workflow(path, pins, required))

    for problem in problems:
        print(f"ERROR {problem}", file=sys.stderr)
    print(f"checked {len(sources)} workflow/fragment files; {len(problems)} policy violations")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
