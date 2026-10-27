#!/usr/bin/env python3
"""Validate a built APK (and its AAB twin) and emit machine-readable evidence.

This reads the real artifact: it asks ``aapt`` for the manifest version and
``apksigner`` for the signer certificate. Debug-signed CI builds are allowed,
release jobs must prove a non-debug signer or fail.

The native-library gate runs on the raw zip before any SDK tool is invoked, so
a missing ``libwxlike_ffi.so`` is reported as such even on a runner without
``aapt`` — that crash-at-startup bug is the one this script must never miss.
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
# 原生库存放处：APK 是 `lib/<abi>/`，AAB 是 `<模块>/lib/<abi>/`（Flutter 只有 `base`）。
# 同一条规则两边都要能用，否则 release 的 AAB 是一个没人看的旁路产物。
LIB_ENTRY_RE = re.compile(r"^(?:(?P<module>[^/]+)/)?lib/(?P<abi>[^/]+)/(?P<name>[^/]+)$")
# "能跑的 ABI" 的定义：带 Dart 快照的那个 ABI 目录。
DART_SNAPSHOT = "libapp.so"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def zip_entries(path: Path, kind: str) -> set[str]:
    if not path.is_file() or path.stat().st_size < 1_000_000:
        raise SystemExit(f"{kind} missing or implausibly small: {path}")
    try:
        with zipfile.ZipFile(path) as archive:
            return set(archive.namelist())
    except zipfile.BadZipFile:
        raise SystemExit(f"{kind} is not a readable zip archive: {path}") from None


def runtime_lib_dirs(entries: set[str]) -> list[str]:
    """The `.../lib/<abi>` prefixes an app can actually run on.

    插件 AAR 常把自家 .so 铺到全部 ABI 目录，但那些目录没有 libflutter/libapp，
    本来就跑不起来，不该拿它们来要求我们的 ffi 库——所以只认带 `libapp.so` 的。
    """
    dirs: set[str] = set()
    snapshots: set[str] = set()
    for entry in entries:
        m = LIB_ENTRY_RE.match(entry)
        if not m:
            continue
        prefix = f"lib/{m['abi']}" if not m["module"] else f"{m['module']}/lib/{m['abi']}"
        dirs.add(prefix)
        if m["name"] == DART_SNAPSHOT:
            snapshots.add(prefix)
    # 没有任何 libapp.so（例如 fixture）时退回全部 ABI 目录，别把"查不到"当成"不需要"。
    return sorted(snapshots or dirs)


def require_native_libs(artifact: Path, entries: set[str], libs: list[str]) -> list[str]:
    """Fail unless every runtime ABI of `artifact` packages every lib in `libs`."""
    lib_dirs = runtime_lib_dirs(entries)
    for lib in libs:
        missing = [d for d in lib_dirs if f"{d}/{lib}" not in entries]
        if missing or not lib_dirs:
            raise SystemExit(
                f"{lib} is missing for {missing or 'every ABI'} in {artifact.name} "
                f"(it runs on {lib_dirs or ['nothing']}): the native cross-compile step did not run"
            )
    return lib_dirs


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
    parser.add_argument(
        "--aab",
        type=Path,
        help="the AAB built from the same sources; --require-native-lib applies to it too (base/lib/<abi>/...)",
    )
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

    apk_entries = zip_entries(args.apk, "APK")
    # 先查打包、再动 SDK：这条门要能在没有 aapt 的机器上也判得红。
    lib_dirs = require_native_libs(args.apk, apk_entries, args.require_native_lib)
    bundle_dirs: list[str] = []
    if args.aab:
        bundle_dirs = require_native_libs(args.aab, zip_entries(args.aab, "AAB"), args.require_native_lib)

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
        "native_libs": args.require_native_lib,
        "abi_dirs": lib_dirs,
    }
    if args.aab:
        evidence["bundle"] = {
            "artifact": args.aab.name,
            "sha256": sha256(args.aab),
            "size": args.aab.stat().st_size,
            "abi_dirs": bundle_dirs,
        }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(
        f"APK {args.apk.name} version={version} signer={signer} sha256={evidence['sha256']} "
        f"abis={','.join(lib_dirs)}" + (f" bundle={args.aab.name} abis={','.join(bundle_dirs)}" if args.aab else "")
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
