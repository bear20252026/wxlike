#!/usr/bin/env python3
"""Compose workflow fragments into `.github/workflows/*.yml`.

G00 owns `.github/`; this script is the handoff format. ``--check`` renders in
memory, runs the workflow policy and (when present) compares against
``ci/jobs/generated/`` so drift is visible in CI before a merge.

Requires PyYAML (installed by the audit job).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tempfile
from pathlib import Path

import yaml

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE / "audit"))

from check_pins import check_workflow  # noqa: E402


def load_yaml(path: Path) -> dict:
    data = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"{path}: expected a mapping")
    return data


def compose_one(base_path: Path, pins: dict, required: dict, name: str) -> tuple[str, list[str]]:
    base = load_yaml(base_path)
    fragments = base.pop("x-wxlike-fragments", None)
    if not fragments:
        raise ValueError(f"{base_path}: missing x-wxlike-fragments")
    jobs = base.setdefault("jobs", {})
    if jobs:
        raise ValueError(f"{base_path}: base must not declare jobs; fragments own them")

    for fragment_name in fragments:
        fragment_path = HERE / fragment_name / "job.yml"
        fragment = load_yaml(fragment_path)
        fragment_jobs = fragment.get("jobs", fragment)
        for job_id, body in fragment_jobs.items():
            if job_id in jobs:
                raise ValueError(f"duplicate job {job_id} in {fragment_path}")
            jobs[job_id] = body

    for job_id, body in jobs.items():
        needs = body.get("needs", [])
        for need in [needs] if isinstance(needs, str) else needs:
            if need not in jobs:
                raise ValueError(f"job {job_id} needs unknown job {need}")

    rendered = yaml.safe_dump(base, sort_keys=False, allow_unicode=True, width=4096)
    with tempfile.TemporaryDirectory() as tmpdir:
        # check_pins keys workflow-specific rules by file name.
        tmp = Path(tmpdir, name)
        tmp.write_text(rendered, encoding="utf-8")
        problems = check_workflow(tmp, pins, required)
    return rendered, problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--check-live", action="store_true", help="require .github/workflows to match the rendered output")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--pins", type=Path, default=HERE / "policy" / "action-pins.json")
    parser.add_argument("--required", type=Path, default=HERE / "policy" / "required-checks.json")
    args = parser.parse_args(argv)

    pins = json.loads(args.pins.read_text(encoding="utf-8")).get("actions", {})
    required = json.loads(args.required.read_text(encoding="utf-8"))
    bases = sorted((HERE / "workflows").glob("*.workflow.yml"))
    if not bases:
        print("ERROR no workflow bases", file=sys.stderr)
        return 1

    outputs: dict[str, str] = {}
    problems: list[str] = []
    for base_path in bases:
        name = base_path.name.removesuffix(".workflow.yml") + ".yml"
        try:
            rendered, issues = compose_one(base_path, pins, required, name)
        except ValueError as err:
            print(f"ERROR {err}", file=sys.stderr)
            return 1
        outputs[name] = rendered
        problems.extend(issues)
        print(f"composed {name}: {len(yaml.safe_load(rendered)['jobs'])} jobs")

    for problem in problems:
        print(f"ERROR {problem}", file=sys.stderr)
    if problems:
        return 1

    if args.check:
        generated = HERE / "generated"
        for name, rendered in outputs.items():
            path = generated / name
            if not path.exists():
                print(f"ERROR {path} is missing; rerun compose.py --output-dir ci/jobs/generated", file=sys.stderr)
                return 1
            if path.read_text(encoding="utf-8") != rendered:
                print(f"ERROR {path} is stale; rerun compose.py --output-dir ci/jobs/generated", file=sys.stderr)
                return 1
            print(f"check {name}: ok")

    if args.check_live:
        live = ROOT / ".github" / "workflows"
        for name, rendered in outputs.items():
            path = live / name
            if not path.exists():
                print(f"ERROR {path} has not been applied by G00", file=sys.stderr)
                return 1
            if path.read_text(encoding="utf-8") != rendered:
                print(f"ERROR {path} differs from the composed output; G00 must re-apply it", file=sys.stderr)
                return 1
            print(f"live {name}: ok")

    if args.output_dir:
        args.output_dir.mkdir(parents=True, exist_ok=True)
        lines = []
        for name, rendered in outputs.items():
            path = args.output_dir / name
            path.write_text(rendered, encoding="utf-8", newline="\n")
            digest = hashlib.sha256(rendered.encode("utf-8")).hexdigest()
            lines.append(f"{digest}  {name}")
        (args.output_dir / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
        print(f"wrote {len(outputs)} workflow(s) to {args.output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
