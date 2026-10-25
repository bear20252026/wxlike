#!/usr/bin/env python3
"""Build a Tauri updater manifest from real artefacts.

The manifest is intentionally boring JSON so the release job can diff it. When
``--require-signature`` is set, every platform must have a ``.sig`` produced by
the Tauri signer (minisign format); an unsigned build is allowed for PRs but
can never be published.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from datetime import datetime, timezone
from pathlib import Path

PLATFORM_ALIASES = {
    "windows": "windows-x86_64",
    "macos-aarch64": "darwin-aarch64",
    "macos-x86_64": "darwin-x86_64",
    "macos": "darwin-aarch64",
    "linux": "linux-x86_64",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def updater_candidates(directory: Path, platform: str) -> list[Path]:
    files = [p for p in directory.rglob("*") if p.is_file() and p.name.lower() != "latest.json"]
    if platform.startswith("windows"):
        patterns = (".nsis.zip", ".msi.zip", ".msi", ".exe")
    elif platform.startswith("darwin"):
        patterns = (".app.tar.gz", ".dmg")
    else:
        patterns = (".appimage.tar.gz", ".appimage", ".deb", ".rpm")
    candidates = [p for p in files if p.name.lower().endswith(patterns)]
    # Prefer the updater archive (the only artefact the updater consumes).
    candidates.sort(key=lambda p: (not p.name.lower().endswith(".zip") and not p.name.lower().endswith(".tar.gz"), -p.stat().st_size))
    return candidates


def pick_artifact(directory: Path, platform: str) -> Path:
    candidates = updater_candidates(directory, platform)
    if not candidates:
        raise SystemExit(f"no Tauri artefact for {platform} under {directory}")
    return candidates[0]


def merge(inputs: list[Path], output: Path, require_signature: bool) -> int:
    merged = {"version": "", "notes": "", "pub_date": "", "platforms": {}, "artifacts": []}
    for path in inputs:
        data = json.loads(path.read_text(encoding="utf-8"))
        if merged["version"] and data.get("version") != merged["version"]:
            raise SystemExit(f"version mismatch: {path} has {data.get('version')}, expected {merged['version']}")
        merged["version"] = merged["version"] or data.get("version", "")
        merged["pub_date"] = max(merged["pub_date"], data.get("pub_date", ""))
        for platform, entry in (data.get("platforms") or {}).items():
            if platform in merged["platforms"]:
                raise SystemExit(f"duplicate platform {platform} in {path}")
            if require_signature and not entry.get("signature"):
                raise SystemExit(f"unsigned platform {platform} in {path}; refusing to publish")
            merged["platforms"][platform] = entry
        merged["artifacts"].extend(data.get("artifacts") or [])
    if not merged["platforms"]:
        raise SystemExit("no platforms in the input manifests")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(merged, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"merged {len(inputs)} manifests -> {output} ({', '.join(sorted(merged['platforms']))})")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--merge", nargs="+", type=Path, help="merge per-platform manifests")
    parser.add_argument("--artifacts-dir", type=Path)
    parser.add_argument("--platform")
    parser.add_argument("--version")
    parser.add_argument("--notes", default="")
    parser.add_argument("--base-url")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--require-signature", action="store_true")
    args = parser.parse_args(argv)

    if args.merge:
        return merge(args.merge, args.output, args.require_signature)
    missing = [name for name in ("artifacts_dir", "platform", "version", "base_url") if not getattr(args, name)]
    if missing:
        parser.error(f"missing {', '.join('--' + name.replace('_', '-') for name in missing)}")

    platform = PLATFORM_ALIASES.get(args.platform, args.platform)
    artifact = pick_artifact(args.artifacts_dir, platform)
    signature_path = artifact.with_name(artifact.name + ".sig")
    signature = signature_path.read_text(encoding="utf-8").strip() if signature_path.exists() else ""
    if args.require_signature and not signature:
        raise SystemExit(f"signature missing for {artifact}; refusing to publish")

    entry = {
        "url": args.base_url.rstrip("/") + "/" + artifact.name,
        "size": artifact.stat().st_size,
        "sha256": sha256(artifact),
    }
    if signature:
        entry["signature"] = signature

    payload = {
        "version": args.version,
        "notes": args.notes,
        "pub_date": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": {platform: entry},
        "artifacts": [{"name": artifact.name, "sha256": entry["sha256"], "size": entry["size"]}],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"manifest {args.output}: {platform} <- {artifact} ({entry['size']} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
