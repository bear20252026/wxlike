#!/usr/bin/env python3
"""Validate a built APK and emit machine-readable evidence.

This reads the real artifact: it asks ``aapt`` for the manifest version and
``apksigner`` for the signer certificate. Debug-signed CI builds are allowed,
release jobs must prove a non-debug signer or fail.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

VERSION_RE = re.compile(r"versionName='([^']+)'")
# apksigner 的证书标签随版本变过：老版按序号打 `Signer #1 certificate DN:`
# （`#1` 后没有冒号），新版按签名方案打 `V2 Signer: certificate DN:`。只认一种
# 就会把**已签名**的 APK 判成"没有证书"（CI 上红的就是这个）。
DN_RE = re.compile(r"(?:Signer #\d+|V\d+ Signer):? certificate DN: (.+)")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def find_sdk_tool(name: str) -> str | None:
    found = shutil.which(name)
    if found:
        return found
    # build-tools ships `apksigner` and `apksigner.bat` side by side; the newest
    # version sort hands a Linux runner the Windows batch file, so pick the
    # extension this OS can actually exec.
    native_exts = (".bat", ".cmd", ".exe") if os.name == "nt" else ("",)
    for root in (os.environ.get("ANDROID_HOME"), os.environ.get("ANDROID_SDK_ROOT")):
        if not root:
            continue
        for candidate in sorted(Path(root, "build-tools").glob(f"*/{name}*"), reverse=True):
            if candidate.is_file() and candidate.suffix.lower() in native_exts:
                return str(candidate)
    return None


def run(tool: str, *args: str) -> str:
    result = subprocess.run([tool, *args], capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(f"{tool} failed ({result.returncode}): {result.stderr.strip() or result.stdout.strip()}")
    return result.stdout


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apk", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--symbols-dir", type=Path)
    parser.add_argument("--require-release-key", action="store_true")
    parser.add_argument(
        "--require-native-lib",
        action="append",
        default=[],
        help="lib/<abi>/<name> must be packaged for every ABI present in the APK",
    )
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)

    if not args.apk.is_file() or args.apk.stat().st_size < 1_000_000:
        raise SystemExit(f"APK missing or implausibly small: {args.apk}")

    aapt = find_sdk_tool("aapt")
    apksigner = find_sdk_tool("apksigner")
    if not aapt or not apksigner:
        raise SystemExit("aapt/apksigner not found; set ANDROID_HOME to an SDK with build-tools")

    badging = run(aapt, "dump", "badging", str(args.apk))
    match = VERSION_RE.search(badging)
    if not match:
        raise SystemExit(f"aapt did not report a versionName for {args.apk}")
    version = match.group(1)
    if version != args.expected_version:
        raise SystemExit(f"APK version {version} does not match expected {args.expected_version}")

    with zipfile.ZipFile(args.apk) as apk:
        entries = set(apk.namelist())
    # "能跑的 ABI" = 带 Dart 快照 libapp.so 的那些。插件 AAR 常把自家 .so 铺到全部
    # ABI 目录，但那些目录没有 libflutter/libapp，本来就跑不起来，不该拿它们来
    # 要求我们的 ffi 库。
    lib_abis = {name.split("/")[1] for name in entries if name.startswith("lib/") and len(name.split("/")) > 2}
    abis = sorted({a for a in lib_abis if f"lib/{a}/libapp.so" in entries} or lib_abis)
    for lib in args.require_native_lib:
        missing = [abi for abi in abis if f"lib/{abi}/{lib}" not in entries]
        if missing or not abis:
            raise SystemExit(
                f"{lib} is missing for {missing or 'every ABI'} (apk runs on {abis or ['nothing']}): "
                "the native cross-compile step did not run"
            )

    signer_out = run(apksigner, "verify", "--verbose", "--print-certs", str(args.apk))
    dn_match = DN_RE.search(signer_out)
    if not dn_match:
        raise SystemExit(f"apksigner found no signer certificate:\n{signer_out}")
    signer = dn_match.group(1).strip()
    if args.require_release_key and "Android Debug" in signer:
        raise SystemExit(f"release build is signed with the debug key: {signer}")

    symbols = []
    if args.symbols_dir:
        symbols = sorted(str(p) for p in args.symbols_dir.rglob("*.symbols"))
        if not symbols:
            raise SystemExit(f"no native symbol files under {args.symbols_dir}; obfuscated builds cannot be symbolised")

    evidence = {
        "artifact": args.apk.name,
        "sha256": sha256(args.apk),
        "size": args.apk.stat().st_size,
        "version": version,
        "signer": signer,
        "debug_key": "Android Debug" in signer,
        "symbols": symbols,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"APK {args.apk.name} version={version} signer={signer} sha256={evidence['sha256']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
