#!/usr/bin/env python3
"""Assemble and validate the release evidence manifest.

``--require-clean`` is the publish gate: every blocking gap must be closed,
every desktop platform must be signed, the SBOM must exist, the image must be
referenced by digest, and mobile evidence must be present. A dry run may omit
some of these but records exactly what was missing.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from datetime import datetime, timezone
from pathlib import Path

SEMVER = re.compile(r"^\d+\.\d+\.\d+$")
HEX40 = re.compile(r"^[0-9a-f]{40}$")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load(path: Path | None, default=None):
    if path is None:
        return default
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SystemExit(f"cannot read {path}: {exc}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--image-digest", default="")
    parser.add_argument("--sbom", type=Path)
    parser.add_argument("--desktop-manifest", type=Path)
    parser.add_argument("--android-evidence", type=Path)
    parser.add_argument("--ios-evidence", type=Path)
    parser.add_argument("--device-evidence", type=Path)
    parser.add_argument("--device-evidence-verified", action="store_true")
    parser.add_argument("--gaps-json", type=Path, required=True)
    parser.add_argument("--published", action="store_true")
    parser.add_argument("--require-clean", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)

    problems: list[str] = []
    if not SEMVER.match(args.version):
        problems.append(f"version {args.version!r} is not x.y.z")
    if not HEX40.match(args.commit):
        problems.append(f"commit {args.commit!r} is not a 40-hex sha")

    digest = args.image_digest
    if args.require_clean and not DIGEST.match(digest):
        problems.append(f"image must be pinned by digest for publication, got {digest!r}")

    sbom = None
    if args.sbom and args.sbom.is_file() and args.sbom.stat().st_size > 0:
        sbom = {"path": str(args.sbom), "sha256": sha256(args.sbom), "size": args.sbom.stat().st_size}
    elif args.require_clean:
        problems.append("SBOM is missing or empty")

    desktop = load(args.desktop_manifest, {})
    platforms = desktop.get("platforms", {}) if desktop else {}
    if args.require_clean:
        for expected in ("linux-x86_64", "windows-x86_64", "darwin-aarch64"):
            entry = platforms.get(expected)
            if not entry:
                problems.append(f"desktop platform {expected} missing from the update manifest")
            elif not entry.get("signature"):
                problems.append(f"desktop platform {expected} is unsigned")
    if desktop and desktop.get("version") and desktop["version"] != args.version:
        problems.append(f"desktop manifest version {desktop['version']} != {args.version}")

    mobile = {}
    for name, path, flag in (
        ("android", args.android_evidence, args.require_clean),
        ("ios", args.ios_evidence, args.require_clean),
    ):
        if path and path.is_file():
            data = json.loads(path.read_text(encoding="utf-8"))
            if data.get("debug_key"):
                problems.append(f"{name} evidence shows a debug-key signature")
            mobile[name] = {"path": str(path), "sha256": sha256(path), **data}
        elif flag:
            problems.append(f"{name} release evidence is missing")

    device = None
    if args.device_evidence and args.device_evidence.is_file():
        device = {
            "path": str(args.device_evidence),
            "sha256": sha256(args.device_evidence),
            "verified": args.device_evidence_verified,
        }
        if args.require_clean and not args.device_evidence_verified:
            problems.append("device evidence exists but was not validated")
    elif args.require_clean:
        problems.append("real-device evidence is missing")
    if args.device_evidence_verified and not device:
        problems.append("device evidence flag set but no evidence file was supplied")

    gaps = load(args.gaps_json, {})
    open_blocking = gaps.get("open_blocking", [])
    if args.require_clean and open_blocking:
        problems.append("open release-blocking gaps: " + ", ".join(g["id"] for g in open_blocking))
    if args.require_clean and not args.published:
        problems.append("publication was demanded but --published was not set")

    payload = {
        "schema_version": 1,
        "kind": "wxlike-release",
        "version": args.version,
        "commit": args.commit,
        "built_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "published": bool(args.published and not problems),
        "image": {"reference": args.image, "digest": digest or None, "sbom": sbom},
        "desktop": {"manifest_version": desktop.get("version"), "platforms": platforms},
        "mobile": mobile,
        "device_evidence": device,
        "gaps": {
            "total": gaps.get("total", 0),
            "open_blocking": [g.get("id") for g in open_blocking],
        },
        "problems": problems,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8", newline="\n")

    for problem in problems:
        print(f"ERROR {problem}")
    print(f"release manifest {'written' if not problems else 'written with blockers'}: {args.output}")
    return 1 if args.require_clean and problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
