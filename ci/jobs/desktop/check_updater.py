#!/usr/bin/env python3
"""Verify Tauri update signatures with minisign, including a negative test.

``--self-test`` proves the verifier rejects a tampered artefact. ``--verify``
checks a real update package against the configured public key. minisign is
installed by the desktop job; the script fails closed if it is missing.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, **kwargs)


def minisign() -> str:
    path = shutil.which("minisign")
    if not path:
        raise SystemExit("minisign not found (install it in the job before running the updater check)")
    return path


def pubkey_text(path: Path) -> str:
    lines = [
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.startswith("untrusted comment:")
    ]
    if not lines:
        raise SystemExit(f"{path} contains no public key")
    return lines[-1]


def self_test() -> int:
    tool = minisign()
    with tempfile.TemporaryDirectory(prefix="wxlike-updater-") as tmp:
        work = Path(tmp)
        artifact = work / "wxlike_0.0.1_x64.AppImage"
        artifact.write_bytes(os.urandom(4096))
        pub, sec = work / "test.pub", work / "test.sec"
        generated = run([tool, "-G", "-W", "-p", str(pub), "-s", str(sec)])
        if generated.returncode != 0:
            print(generated.stderr, file=sys.stderr)
            return 1
        signature = work / "artifact.sig"
        signed = run([tool, "-S", "-s", str(sec), "-m", str(artifact), "-x", str(signature)])
        if signed.returncode != 0:
            print(signed.stderr, file=sys.stderr)
            return 1
        public_key = pubkey_text(pub)
        ok = run([tool, "-V", "-m", str(artifact), "-x", str(signature), "-P", public_key])
        if ok.returncode != 0:
            print("valid signature was rejected", file=sys.stderr)
            print(ok.stderr, file=sys.stderr)
            return 1
        artifact.write_bytes(artifact.read_bytes() + b"tampered")
        bad = run([tool, "-V", "-m", str(artifact), "-x", str(signature), "-P", public_key])
        if bad.returncode == 0:
            print("ERROR tampered artefact was accepted", file=sys.stderr)
            return 1
        print("updater self-test: valid accepted, tampered rejected")
    return 0


def verify(artifact: Path, signature: Path, pubkey: str) -> int:
    tool = minisign()
    result = run([tool, "-V", "-m", str(artifact), "-x", str(signature), "-P", pubkey.strip()])
    if result.returncode != 0:
        print(result.stderr, file=sys.stderr)
        print(f"ERROR signature rejected for {artifact}", file=sys.stderr)
        return 1
    print(f"signature ok: {artifact}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--signature", type=Path)
    parser.add_argument("--public-key", default=os.environ.get("WXLIKE_UPDATER_PUBKEY", ""))
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()
    if not (args.artifact and args.signature and args.public_key):
        parser.error("--artifact, --signature and --public-key (or WXLIKE_UPDATER_PUBKEY) are required")
    return verify(args.artifact, args.signature, args.public_key)


if __name__ == "__main__":
    raise SystemExit(main())
