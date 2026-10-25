#!/usr/bin/env python3
"""Validate a signed IPA and emit evidence.

The IPA is opened as a zip: the app's Info.plist supplies the version, and
``codesign`` on the extracted app supplies the signing authority. A debug or
ad-hoc signature is rejected for release.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import plistlib
import subprocess
import tempfile
import zipfile
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ipa", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)

    if not args.ipa.is_file():
        raise SystemExit(f"IPA missing: {args.ipa}")

    with tempfile.TemporaryDirectory() as tmp:
        with zipfile.ZipFile(args.ipa) as archive:
            infos = [name for name in archive.namelist() if name.startswith("Payload/") and name.endswith(".app/Info.plist")]
            if len(infos) != 1:
                raise SystemExit(f"expected exactly one Payload/*.app/Info.plist, found {infos}")
            info_name = infos[0]
            info = plistlib.loads(archive.read(info_name))
            app_name = info_name.split("/")[1]
            archive.extractall(tmp)

        version = str(info.get("CFBundleShortVersionString", ""))
        build = str(info.get("CFBundleVersion", ""))
        if version != args.expected_version:
            raise SystemExit(f"IPA version {version} does not match expected {args.expected_version}")
        if not build:
            raise SystemExit("IPA has no CFBundleVersion")

        app = Path(tmp, "Payload", app_name)
        codesign = subprocess.run(
            ["codesign", "-dv", "--verbose=4", str(app)],
            capture_output=True,
            text=True,
        )
        details = codesign.stdout + codesign.stderr
        if codesign.returncode != 0:
            raise SystemExit(f"codesign could not read the signature: {details.strip()}")
        if "Authority=Apple Distribution" not in details and "Authority=iPhone Distribution" not in details:
            raise SystemExit(f"IPA is not distribution-signed:\n{details.strip()}")
        authority = next(
            (line.split("=", 1)[1] for line in details.splitlines() if line.startswith("Authority=")),
            "",
        )

        evidence = {
            "artifact": args.ipa.name,
            "sha256": sha256(args.ipa),
            "size": args.ipa.stat().st_size,
            "version": version,
            "build": build,
            "bundle_id": info.get("CFBundleIdentifier", ""),
            "authority": authority,
            "distribution_signed": True,
        }
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8", newline="\n")
        print(f"IPA {args.ipa.name} version={version}+{build} authority={authority} sha256={evidence['sha256']}")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
