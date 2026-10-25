#!/usr/bin/env python3
"""Validate real-device evidence against the frozen device matrix.

This is deliberately strict: missing device/feature, failed status, placeholder
text, an empty evidence file, or a hash that does not match the signed artifact
all fail. A release without real device runs is not publishable.
"""

from __future__ import annotations

import argparse
import json
import sys
from datetime import datetime
from pathlib import Path

PLACEHOLDERS = ("tbd", "todo", "example", "changeme", "placeholder", "xxx")
REQUIRED_RESULT_FIELDS = ("feature", "status", "evidence", "observed_at")


def load(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SystemExit(f"cannot read {path}: {exc}")


def is_placeholder(value: str) -> bool:
    low = value.strip().lower()
    return not low or any(token in low for token in PLACEHOLDERS)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--release-evidence", type=Path)
    args = parser.parse_args(argv)

    evidence = load(args.evidence)
    matrix = load(args.matrix)
    problems: list[str] = []

    if evidence.get("schema_version") != 1:
        problems.append("schema_version must be 1")
    build = evidence.get("build") or {}
    git_sha = str(build.get("git_sha", ""))
    artifact_hash = str(build.get("artifact_sha256", ""))
    if len(git_sha) != 40 or any(c not in "0123456789abcdef" for c in git_sha):
        problems.append("build.git_sha must be a 40-hex commit")
    if len(artifact_hash) != 64 or any(c not in "0123456789abcdef" for c in artifact_hash):
        problems.append("build.artifact_sha256 must be a 64-hex digest")

    if args.release_evidence:
        release = load(args.release_evidence)
        if release.get("sha256") != artifact_hash:
            problems.append(
                f"evidence artifact_sha256 {artifact_hash} does not match signed artifact {release.get('sha256')}"
            )
        if release.get("version") and not str(build.get("version", "")).startswith(str(release["version"])):
            problems.append(f"evidence version {build.get('version')} does not match artifact {release['version']}")

    required_devices = {d["id"]: d for d in matrix.get("devices", [])}
    required_features = set(matrix.get("features", []))
    observed_devices = {d.get("id"): d for d in evidence.get("devices", []) if isinstance(d, dict)}

    base = args.evidence.parent
    for device_id, expected in required_devices.items():
        device = observed_devices.get(device_id)
        if device is None:
            problems.append(f"missing device {device_id} ({expected['model']} / {expected['os_version']})")
            continue
        for field in ("id", "platform", "model", "os_version"):
            if is_placeholder(str(device.get(field, ""))):
                problems.append(f"{device_id}: {field} is empty or placeholder")
        if device.get("platform") != expected["platform"]:
            problems.append(f"{device_id}: platform {device.get('platform')} != {expected['platform']}")
        results = device.get("results") or []
        seen = set()
        for result in results:
            missing = [f for f in REQUIRED_RESULT_FIELDS if not result.get(f)]
            if missing:
                problems.append(f"{device_id}: result missing {missing}")
                continue
            feature = result["feature"]
            seen.add(feature)
            if result["status"] != "pass":
                problems.append(f"{device_id}/{feature}: status is {result['status']}, not pass")
            if is_placeholder(str(result["notes"]) if result.get("notes") else ""):
                pass  # notes optional
            evidence_path = base / str(result["evidence"])
            if not evidence_path.is_file() or evidence_path.stat().st_size == 0:
                problems.append(f"{device_id}/{feature}: evidence file missing or empty: {evidence_path}")
            try:
                datetime.fromisoformat(str(result["observed_at"]).replace("Z", "+00:00"))
            except ValueError:
                problems.append(f"{device_id}/{feature}: observed_at is not ISO-8601")
        missing_features = sorted(required_features - seen)
        if missing_features:
            problems.append(f"{device_id}: no evidence for {missing_features}")

    for device_id in sorted(set(observed_devices) - set(required_devices)):
        problems.append(f"device {device_id} is not in the frozen matrix")

    for problem in problems:
        print(f"ERROR {problem}", file=sys.stderr)
    if problems:
        print(f"device evidence rejected: {len(problems)} problem(s)")
        return 1
    print(f"device evidence accepted: {len(required_devices)} devices x {len(required_features)} features")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
