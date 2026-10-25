#!/usr/bin/env python3
"""C6 WASM compile gate.

There is no WASM crate yet (G01-03). Pretending to compile nothing would be a
green check with no evidence, so the job records the absence in
``policy/known-gaps.json`` and passes daily CI; the release gate fails while the
gap is open. As soon as one workspace crate declares a wasm32 build, this
script compiles exactly those packages and failure is fatal.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
MARKERS = ("wasm-bindgen", 'target_arch = "wasm32"', "wasm32-unknown-unknown")


def wasm_packages(root: Path) -> list[tuple[str, Path]]:
    found = []
    for manifest in root.glob("crates/*/Cargo.toml"):
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        package = data.get("package", {})
        name = package.get("name")
        if not name:
            continue
        text = manifest.read_text(encoding="utf-8")
        lib = data.get("lib", {})
        crate_types = " ".join(lib.get("crate-type", []))
        if any(marker in text for marker in MARKERS) or "cdylib" in crate_types and "wasm" in text:
            found.append((name, manifest))
    return found


def run(cmd: list[str], root: Path) -> int:
    print("+", " ".join(cmd))
    proc = subprocess.run(cmd, cwd=root)
    return proc.returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args(argv)

    packages = wasm_packages(args.root)
    if not packages:
        print("NOT_APPLICABLE: no workspace crate declares a wasm32 target (G01-03).")
        print("Recorded in ci/jobs/policy/known-gaps.json; release is blocked until it exists.")
        return 0

    if run(["rustup", "target", "add", "wasm32-unknown-unknown"], args.root) != 0:
        return 1
    cmd = ["cargo", "build", "--locked", "--target", "wasm32-unknown-unknown"]
    for name, _ in packages:
        cmd += ["-p", name]
    return run(cmd, args.root)


if __name__ == "__main__":
    raise SystemExit(main())
